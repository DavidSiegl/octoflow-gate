//! Python's half of the passes-at-base check over a real git repository, its tests run with the workspace's own
//! pytest (common::python), through the `gate` binary and its quality.json, as gate.py's TestPassesAtBase runs
//! gate.py's check (tests/test_gate.py). Where gate.py's tests patch the gate, these give the binary a `--python`
//! that is a shell script or a virtual environment's, a TMPDIR, or a stub `tar` ahead of PATH; whether pytest ran at
//! the base is read from what a `--python` that logs its arguments was asked. The gate runs its flaky-tests check on
//! the same change with the same Python, so a test that must behave differently at the base asks [`AT_BASE`]. An
//! empty pytest configuration that cannot be written has no such route, and is pytest.rs's own test.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{fake_python, logging_python, Repo};

mod common;

/// calc.py at the base: add is wrong there; mul is right, and stays so.
const CALC: &str = "def add(a, b):\n    return a - b\n\n\ndef mul(a, b):\n    return a * b\n";

/// tests/test_calc.py at the base.
const TESTS: &str = "from calc import mul\n\n\ndef test_mul():\n    assert mul(2, 3) == 6\n";

/// A test that passes at the base, as [`fix_add`] takes it: its def is then on line 12.
const MUL_AGAIN: &str = "\n\ndef test_mul_again():\n    assert mul(1, 1) == 1\n";

/// A test of mul, which passes at the base, to follow [`TESTS`].
const MUL_ONE: &str = "\n\ndef test_mul_one():\n    assert mul(1, 1) == 1\n";

/// tests/b/test_two.py, a test that passes at the base wherever its conftest.py loads.
const TEST_TWO: &str = "from calc import mul\n\n\ndef test_two():\n    assert mul(2, 1) == 2\n";

/// A Python expression, for a module that imports sys, that is true in the check's run at the base and false in
/// the flaky-tests check's reruns: the run at the base loads the gate's plugin.
const AT_BASE: &str = "'_octoflow_gate_base_imports' in sys.modules";

/// A repository whose base holds [`CALC`], [`TESTS`] and a pytest.ini that puts the top on pytest's path.
fn calc() -> Repo {
    Repo::with(&[
        ("pyproject.toml", b"[tool.ruff]\n"),
        ("calc.py", CALC.as_bytes()),
        ("tests/test_calc.py", TESTS.as_bytes()),
        ("pytest.ini", b"[pytest]\npythonpath = .\n"),
    ])
}

/// [`CALC`] with add fixed, followed by `more`.
fn fixed_calc(more: &str) -> String {
    CALC.replace("a - b", "a + b") + more
}

/// Fixes add in the working tree, and adds test_add, which fails on the old add, then `tests`.
fn fix_add(repo: &Repo, tests: &str) {
    repo.put("calc.py", fixed_calc("").as_bytes());
    let tests = format!(
        "{}\n\ndef test_add():\n    assert add(2, 2) == 4\n{tests}",
        TESTS.replace("import mul", "import add, mul")
    );
    repo.put("tests/test_calc.py", tests.as_bytes());
}

/// Fixes add and gives calc `TWO`, which the base lacks, and adds test_mul_one to tests/test_calc.py.
fn add_two(repo: &Repo) {
    repo.put("calc.py", fixed_calc("\n\nTWO = 2\n").as_bytes());
    repo.put("tests/test_calc.py", format!("{TESTS}{MUL_ONE}").as_bytes());
}

/// A finding of the check: its path, line, own rule without the check's name, and message.
type Found = (Option<String>, Option<u32>, String, String);

/// How the gate is run beside the defaults: its budget for the run at the base, the project's Python, and variables
/// of its environment.
struct Gate {
    budget: u64,
    /// None for the workspace's.
    python: Option<PathBuf>,
    env: Vec<(&'static str, OsString)>,
}

impl Default for Gate {
    fn default() -> Gate {
        Gate { budget: 120, python: None, env: vec![] }
    }
}

impl Gate {
    fn with_python(python: PathBuf) -> Gate {
        Gate { python: Some(python), ..Gate::default() }
    }
}

/// The check's findings, each a warning, on the change from HEAD to the working tree of `repo`, every file in it
/// tracked, with the gate run as `how` says.
fn findings(repo: &Repo, how: Gate) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let mut command = repo.gate_command("HEAD");
    command.arg("--python").arg(how.python.unwrap_or_else(common::python)).arg("--json").arg(&json);
    command.arg("--base-run-budget").arg(how.budget.to_string()).args(["--mutation-budget", "1"]);
    let done = command.envs(how.env).output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        let Some(rule) = f["rule"].as_str().unwrap().strip_prefix("passes-at-base ") else { continue };
        assert_eq!(f["severity"], "warning", "{f}");
        let path = f["path"].as_str().map(String::from);
        let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
        out.push((path, line, rule.to_string(), f["message"].as_str().unwrap().to_string()));
    }
    out
}

/// The path, line and rule of each of [`findings`] with the defaults.
fn found(repo: &Repo) -> Vec<(Option<String>, Option<u32>, String)> {
    findings(repo, Gate::default()).into_iter().map(|(path, line, rule, _)| (path, line, rule)).collect()
}

/// A finding as [`found`] gives it, at `line` of `path`.
fn at(path: &str, line: u32, rule: &str) -> (Option<String>, Option<u32>, String) {
    (Some(path.into()), Some(line), rule.into())
}

/// The path of each of `found` and the test its message names, all of them `passes-at-base`.
fn passing(found: Vec<Found>) -> Vec<(String, String)> {
    let named = |(path, _, rule, message): Found| {
        assert_eq!(rule, "passes-at-base", "{message}");
        (path.unwrap(), message.split('`').nth(1).unwrap().to_string())
    };
    found.into_iter().map(named).collect()
}

/// [`passing`], of [`findings`] with the defaults.
fn passing_in(repo: &Repo) -> Vec<(String, String)> {
    passing(findings(repo, Gate::default()))
}

/// A test that passes at the base, as [`passing`] names it.
fn passes(path: &str, name: &str) -> (String, String) {
    (path.into(), name.into())
}

/// The one finding of `found`, which is on no path or line and of rule `not-run`: its message.
fn whole_not_run(found: Vec<Found>) -> String {
    assert_eq!(found.len(), 1, "{found:?}");
    let (path, line, rule, message) = found.into_iter().next().unwrap();
    assert_eq!((path, line, rule.as_str()), (None, None, "not-run"), "{message}");
    message
}

/// The pytest runs of the check on `repo`'s change, which finds nothing: how many collections at the base, and how
/// many runs of what they collected.
fn runs_at_base(repo: &Repo) -> (usize, usize) {
    let (python, log) = logging_python(repo);
    assert_eq!(findings(repo, Gate::with_python(python)), vec![]);
    let logged = std::fs::read_to_string(log).unwrap_or_default();
    let count = |option: &str| logged.lines().filter(|line| line.contains(option)).count();
    (count("--continue-on-collection-errors"), count("--gate-imports-root"))
}

/// A `--python` whose pytest runs the sh `collect` for a collection at the base and `run` for a run there, and
/// collects nothing for the flaky-tests check.
fn fake_pytest(repo: &Repo, collect: &str, run: &str) -> PathBuf {
    let script = format!(
        "case \"$*\" in\n*--continue-on-collection-errors*) {collect};;\n*--gate-imports-root*) {run};;\n\
         *) exit 5;;\nesac"
    );
    fake_python(repo, &script)
}

/// PATH with `bin` ahead of it.
fn path_after(bin: &Path) -> OsString {
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()).into()
}

/// What `python -c program` prints, without the line's end.
fn python_prints(python: &Path, program: &str) -> String {
    let done = Command::new(python).args(["-c", program]).output().unwrap();
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    String::from_utf8(done.stdout).unwrap().trim_end().to_string()
}

#[test]
fn a_test_of_code_the_change_left_alone_passes_at_the_base() {
    // The fixture comes from a conftest.py the change adds, so the test runs at the base only with it overlaid.
    let repo = calc();
    repo.put("tests/conftest.py", b"import pytest\n\n\n@pytest.fixture\ndef three():\n    return 3\n");
    let again = "\n\nimport pytest\n\n\n@pytest.mark.parametrize('n', [1, 2])\n\
                 def test_mul_again(n, three):\n    assert mul(n, three) == 3 * n\n";
    fix_add(&repo, again);
    repo.git(&["add", "-A"]);
    let status = repo.git(&["status", "--porcelain"]);
    let found = findings(&repo, Gate::default());
    let text = std::fs::read_to_string(repo.repo.join("tests/test_calc.py")).unwrap();
    let line = text.lines().position(|line| line == "def test_mul_again(n, three):").unwrap() + 1;
    assert_eq!(found, vec![(
        Some("tests/test_calc.py".into()),
        Some(u32::try_from(line).unwrap()),
        "passes-at-base".into(),
        "`test_mul_again` passes without the change, so it does not test it.".into(),
    )]);
    assert_eq!(repo.git(&["status", "--porcelain"]), status);
}

#[test]
fn a_test_that_fails_without_the_fix_gives_nothing() {
    let repo = calc();
    fix_add(&repo, "");
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn a_test_of_a_function_the_change_adds_gives_nothing() {
    // At the base, the test file cannot import sub, so pytest collects nothing from it.
    let repo = calc();
    repo.put("calc.py", format!("{CALC}\n\ndef sub(a, b):\n    return a - b\n").as_bytes());
    repo.put("tests/test_sub.py", b"from calc import sub\n\n\ndef test_sub():\n    assert sub(3, 1) == 2\n");
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn a_python_script_edited_or_deleted_outside_the_tests_is_python_source() {
    let script = b"#!/usr/bin/env python3\nprint(1)\n";
    let edited: &[u8] = b"#!/usr/bin/env python3\nprint(2)\n";
    for (edit, is_code) in [("bin/tool", true), ("bin/old", true), ("tests/bin/fake", false)] {
        let repo = calc();
        repo.commit(&[("bin/tool", script), ("bin/old", script), ("tests/bin/fake", script)], "scripts");
        if edit == "bin/old" {
            repo.git(&["rm", "--quiet", edit]);
        } else {
            repo.put(edit, edited);
        }
        // A test that fails at the base, so that the run there finds nothing.
        repo.put("tests/test_calc.py", format!("{TESTS}\n\ndef test_fails():\n    assert mul(0, 5) == 5\n").as_bytes());
        assert_eq!(runs_at_base(&repo) != (0, 0), is_code, "{edit}");
    }
}

#[test]
fn a_change_to_the_tests_alone_is_not_run() {
    let repo = calc();
    repo.put("tests/test_calc.py", format!("{TESTS}\n\ndef test_mul_zero():\n    assert mul(0, 5) == 0\n").as_bytes());
    assert_eq!(runs_at_base(&repo), (0, 0));
}

#[test]
fn a_change_to_docs_and_tests_is_not_run() {
    let repo = calc();
    repo.put("TODO.md", b"- nothing\n");
    repo.put("tests/test_calc.py", format!("{TESTS}\n\ndef test_mul_zero():\n    assert mul(0, 5) == 0\n").as_bytes());
    assert_eq!(runs_at_base(&repo), (0, 0));
}

#[test]
fn a_change_beside_the_tests_alone_is_not_run() {
    // A conftest.py anywhere and any file under tests/ go with the tests, not with the code they test.
    let repo = calc();
    repo.put("conftest.py", b"");
    repo.put("tests/data/input.txt", b"2 3\n");
    repo.put("tests/test_calc.py", format!("{TESTS}\n\ndef test_mul_zero():\n    assert mul(0, 5) == 0\n").as_bytes());
    assert_eq!(runs_at_base(&repo), (0, 0));
}

#[test]
fn no_room_for_the_run_at_the_base_is_a_finding() {
    let repo = calc();
    fix_add(&repo, "");
    let how = Gate { env: vec![("TMPDIR", repo.outside.join("missing").into())], ..Gate::default() };
    assert_eq!(
        whole_not_run(findings(&repo, how)),
        "could not make a directory to run the changed tests at the base in: No such file or directory (os error 2); \
         nothing ran, so the check judged nothing."
    );
}

#[test]
fn a_base_run_directory_left_behind_is_a_finding_beside_the_results() {
    // A test leaves a directory in the copy that its owner cannot write, so what it holds cannot be removed. (Root
    // can remove it still: this test is for an account like the build's.)
    let repo = calc();
    let locks = format!(
        "\n\ndef test_locks():\n    import os\n    import sys\n    if not ({AT_BASE}):\n        return\n    \
         locked = os.path.join(os.path.dirname(__file__), 'locked')\n    os.makedirs(locked)\n    \
         open(os.path.join(locked, 'kept'), 'w').close()\n    os.chmod(locked, 0o500)\n    assert False\n"
    );
    fix_add(&repo, &locks);
    let found = findings(&repo, Gate::default());
    assert_eq!(found.len(), 1, "{found:?}");
    let (path, line, rule, message) = &found[0];
    assert_eq!((path.as_deref(), *line, rule.as_str()), (None, None, "not-removed"), "{message}");
    let named = message.strip_prefix("could not remove the directory to run the changed tests at the base in, `");
    let (dir, why) = named.expect(message).split_once("`: ").expect(message);
    assert!(Command::new("chmod").args(["-R", "u+rwX", dir]).status().unwrap().success());
    std::fs::remove_dir_all(dir).unwrap();
    assert!(why.contains("Permission denied"), "{message}");
}

#[test]
fn a_base_that_cannot_be_copied_is_a_finding() {
    let repo = calc();
    fix_add(&repo, "");
    let bin = repo.stub("tar", "echo 'tar: no space' >&2; exit 2");
    let how = Gate { env: vec![("PATH", path_after(&bin))], ..Gate::default() };
    assert_eq!(
        whole_not_run(findings(&repo, how)),
        "could not copy the base to run the changed tests in: tar could not unpack the base: tar: no space; the \
         changed tests were not run there."
    );
}

#[test]
fn tests_with_no_pytest_configuration_run_at_the_base_with_an_empty_one() {
    let repo = calc();
    repo.git(&["rm", "--quiet", "pytest.ini"]);
    repo.commit(&[], "no pytest configuration");
    fix_add(&repo, MUL_AGAIN);
    assert_eq!(found(&repo), vec![at("tests/test_calc.py", 12, "passes-at-base")]);
}

#[test]
fn the_run_at_the_base_has_a_tmpdir_of_its_own_and_the_gate_s_pythonpath_after_the_copy() {
    let repo = calc();
    std::fs::write(repo.outside.join("extra_mod.py"), "TWO = 2\n").unwrap();
    // The first passes only in the run at the base: its TMPDIR lies beside the copy it runs in, which is its cwd.
    let tests = "\n\ndef test_tmpdir():\n    import os\n    \
                 assert os.path.dirname(os.environ['TMPDIR']) == os.path.dirname(os.getcwd())\n\
                 \n\ndef test_inherited_path():\n    from extra_mod import TWO\n    assert mul(TWO, 1) == 2\n";
    fix_add(&repo, tests);
    let how = Gate { env: vec![("PYTHONPATH", repo.outside.clone().into())], ..Gate::default() };
    let found: Vec<_> = findings(&repo, how).into_iter().map(|(_, line, rule, _)| (line, rule)).collect();
    assert_eq!(found, vec![(Some(12), "passes-at-base".into()), (Some(17), "passes-at-base".into())]);
}

#[test]
fn a_file_beside_the_tests_the_change_deletes_is_gone_from_the_run_at_the_base() {
    let repo = calc();
    repo.commit(&[("tests/conftest.py", b"raise RuntimeError('the base\\'s conftest')\n")], "a failing conftest");
    repo.git(&["rm", "--quiet", "tests/conftest.py"]);
    fix_add(&repo, MUL_AGAIN);
    assert_eq!(found(&repo), vec![at("tests/test_calc.py", 12, "passes-at-base")]);
}

#[test]
fn a_configuration_below_the_top_configures_the_run_at_the_base() {
    // The base's own configuration is tests/pytest.ini, whose pythonpath puts tests/src on the path for helper.
    let repo = calc();
    let nested: [(&str, &[u8]); 2] =
        [("tests/pytest.ini", b"[pytest]\npythonpath = src\n"), ("tests/src/helper.py", b"ONE = 1\n")];
    repo.commit(&nested, "nested config");
    fix_add(&repo, "\n\ndef test_mul_again():\n    from helper import ONE\n    assert mul(ONE, ONE) == 1\n");
    assert_eq!(found(&repo), vec![at("tests/test_calc.py", 12, "passes-at-base")]);
}

/// Moves calc.py to `place`/calc.py, a directory of `repo`, with a pytest.ini that puts nothing on the path, and
/// commits that as the base. Gives back the Python of a new virtual environment into which the project is installed
/// editable, as setuptools and hatchling write an editable install: a .pth file there puts the working tree's
/// `place` on the path, or with `finder`, imports a module that appends to sys.meta_path a finder of calc in the
/// working tree's `place`. The environment sees the workspace's packages, pytest among them, through another .pth
/// file. Making it takes the standard library alone, without pip, and no network.
fn installed_editable(repo: &Repo, place: &str, finder: bool) -> PathBuf {
    let venv = repo.outside.join("venv");
    let made = Command::new(common::python()).args(["-m", "venv", "--without-pip"]).arg(&venv).output().unwrap();
    assert!(made.status.success(), "{}", String::from_utf8_lossy(&made.stderr));
    let python = venv.join("bin/python");
    let purelib = "import sysconfig; print(sysconfig.get_path('purelib'))";
    let site = PathBuf::from(python_prints(&python, purelib));
    std::fs::write(site.join("suite.pth"), python_prints(&common::python(), purelib) + "\n").unwrap();
    let calc = repo.repo.join(place).join("calc.py");
    let finder_module = format!(
        "import importlib.util\nimport sys\n\n\nclass Finder:\n    @classmethod\n    \
         def find_spec(cls, name, path=None, target=None):\n        if name != 'calc':\n            return None\n\
         \x20       return importlib.util.spec_from_file_location(name, '{}')\n\n\ndef install():\n    \
         sys.meta_path.append(Finder)\n",
        calc.display()
    );
    std::fs::write(site.join("__editable___calc_finder.py"), finder_module).unwrap();
    let pth = if finder {
        "import __editable___calc_finder; __editable___calc_finder.install()\n".to_string()
    } else {
        format!("{}\n", calc.parent().unwrap().display())
    };
    std::fs::write(site.join("__editable__.calc-0.pth"), pth).unwrap();
    std::fs::create_dir_all(repo.repo.join(place)).unwrap();
    repo.git(&["mv", "calc.py", &format!("{place}/calc.py")]);
    repo.commit(&[("pytest.ini", b"[pytest]\n")], "the project's layout");
    python
}

/// Adds sub to `place`/calc.py, and test_sub, which calc.sub at the base cannot pass, to tests/test_calc.py.
fn add_sub(repo: &Repo, place: &str) {
    repo.put(format!("{place}/calc.py"), format!("{CALC}\n\ndef sub(a, b):\n    return a - b\n").as_bytes());
    let tests = format!("import calc\n\n{TESTS}\n\ndef test_sub():\n    assert calc.sub(3, 1) == 2\n");
    repo.put("tests/test_calc.py", tests.as_bytes());
}

/// The check's one finding with `python`, which says that the changed tests imported `named` from the working tree.
fn assert_imported_from_working_tree(repo: &Repo, python: PathBuf, named: &str) {
    assert_eq!(
        whole_not_run(findings(repo, Gate::with_python(python))),
        format!(
            "the changed tests imported {named} from the working tree, not from the copy of the base, as from a \
             project installed into the gate's interpreter, so they were not judged."
        )
    );
}

#[test]
fn an_editable_install_of_a_src_layout_imports_the_base() {
    // The run at the base puts the copy's src/ on the path, ahead of the installed working tree's.
    let repo = calc();
    let python = installed_editable(&repo, "src", false);
    add_sub(&repo, "src");
    assert_eq!(findings(&repo, Gate::with_python(python)), vec![]);
}

#[test]
fn an_editable_install_by_an_import_finder_imports_the_base() {
    // The finder comes after Python's own path search, which finds calc in the copy's src/ first.
    let repo = calc();
    let python = installed_editable(&repo, "src", true);
    add_sub(&repo, "src");
    assert_eq!(findings(&repo, Gate::with_python(python)), vec![]);
}

#[test]
fn a_package_imported_from_the_working_tree_cannot_be_judged() {
    // lib/ is no import root of the copy's, so calc comes from the working tree, through the editable install.
    let repo = calc();
    let python = installed_editable(&repo, "lib", false);
    add_sub(&repo, "lib");
    assert_imported_from_working_tree(&repo, python, "`calc` (`lib/calc.py`)");
}

#[test]
fn a_package_an_import_finder_takes_from_the_working_tree_cannot_be_judged() {
    let repo = calc();
    let python = installed_editable(&repo, "lib", true);
    add_sub(&repo, "lib");
    assert_imported_from_working_tree(&repo, python, "`calc` (`lib/calc.py`)");
}

#[test]
fn a_package_only_a_conftest_imports_from_the_working_tree_cannot_be_judged() {
    // The changed test reaches calc only through a fixture of the conftest.py, which imports it from lib/.
    let repo = calc();
    let python = installed_editable(&repo, "lib", false);
    let conftest = b"import pytest\n\n\n@pytest.fixture\ndef calc_module():\n    import calc\n    return calc\n";
    repo.commit(&[("tests/conftest.py", conftest)], "fixture");
    repo.put("lib/calc.py", format!("{CALC}\n\ndef sub(a, b):\n    return a - b\n").as_bytes());
    repo.put("tests/test_sub.py", b"def test_sub(calc_module):\n    assert calc_module.sub(3, 1) == 2\n");
    assert_imported_from_working_tree(&repo, python, "`calc` (`lib/calc.py`)");
}

#[test]
fn a_namespace_package_partly_in_the_working_tree_is_not_taken_for_one_imported_from_it() {
    // ns has no __init__.py, so its __path__ holds the copy's src/ns and, through the editable install, the working
    // tree's; its module comes from the copy.
    let repo = calc();
    let python = installed_editable(&repo, "src", false);
    repo.commit(&[("src/ns/helper.py", b"ONE = 1\n")], "namespace package");
    add_sub(&repo, "src");
    let one = "def test_mul_one():\n    assert mul(helper.ONE, 1) == 1\n";
    let tests = format!("from ns import helper\n\n{TESTS}\n\n{one}");
    repo.put("tests/test_calc.py", tests.as_bytes());
    let found = passing(findings(&repo, Gate::with_python(python)));
    // The whole file is not new, but its first line is, which moves test_mul without changing it.
    assert_eq!(found, vec![passes("tests/test_calc.py", "test_mul_one")]);
}

#[test]
fn a_module_a_namespace_package_finds_in_the_working_tree_cannot_be_judged() {
    // The change adds ns/extra.py, which the copy lacks; ns's __path__ finds it in the working tree's src/ns.
    let repo = calc();
    let python = installed_editable(&repo, "src", false);
    repo.commit(&[("src/ns/helper.py", b"ONE = 1\n")], "namespace package");
    repo.put("src/calc.py", fixed_calc("").as_bytes());
    repo.put("src/ns/extra.py", b"TWO = 2\n");
    let tests = format!("from ns import extra\n\n{TESTS}\n\ndef test_mul_two():\n    assert mul(extra.TWO, 1) == 2\n");
    repo.put("tests/test_calc.py", tests.as_bytes());
    assert_imported_from_working_tree(&repo, python, "`ns` (`src/ns/extra.py`)");
}

/// Two configurations, a/pytest.ini and b/pytest.ini, each over a test file to which the change adds a test, and
/// the fix of add.
fn two_configurations() -> Repo {
    let repo = calc();
    let old = "def test_old():\n    assert True\n";
    let new = format!("{old}\n\ndef test_new():\n    pass\n");
    for place in ["a", "b"] {
        repo.put(format!("{place}/pytest.ini"), b"[pytest]\n");
        repo.put(format!("{place}/test_{place}.py"), old.as_bytes());
    }
    repo.commit(&[], "two configs");
    repo.put("calc.py", fixed_calc("").as_bytes());
    for place in ["a", "b"] {
        repo.put(format!("{place}/test_{place}.py"), new.as_bytes());
    }
    repo
}

#[test]
fn a_budget_spent_on_one_configuration_says_the_later_ones_were_not_run() {
    let repo = two_configurations();
    let python = fake_pytest(&repo, "exec sleep 30", "exit 0");
    let found = findings(&repo, Gate { budget: 1, ..Gate::with_python(python) });
    let found: Vec<(&str, &str)> = found.iter().map(|(_, _, rule, said)| (rule.as_str(), said.as_str())).collect();
    assert_eq!(found, vec![
        (
            "budget-reached",
            "pytest was stopped at 1 seconds (`--base-run-budget`) while it collected the changed tests configured by \
             a/pytest.ini, so none of them was judged."
        ),
        (
            "not-run",
            "the run at the base used its 1 seconds (`--base-run-budget`) before it reached the changed tests \
             configured by b/pytest.ini, so they were not run there."
        ),
    ]);
}

#[test]
fn a_run_past_the_budget_says_how_many_test_cases_ran() {
    let repo = calc();
    fix_add(&repo, "\n\ndef test_more():\n    assert add(1, 1) == 2\n");
    let python = fake_pytest(
        &repo,
        "printf 'tests/test_calc.py::test_add\\ntests/test_calc.py::test_more\\n'",
        "printf '.'; exec sleep 30",
    );
    let found = findings(&repo, Gate { budget: 1, ..Gate::with_python(python) });
    assert_eq!(found, vec![(
        None,
        None,
        "budget-reached".into(),
        "pytest was stopped at 1 seconds (`--base-run-budget`) after 1 of the 2 test cases it was given, before it \
         reported them, so none of them was judged."
            .into(),
    )]);
}

#[test]
fn a_collection_past_the_budget_is_a_finding() {
    let repo = calc();
    fix_add(&repo, "");
    let python = fake_pytest(&repo, "exec sleep 30", "exit 0");
    let found = findings(&repo, Gate { budget: 1, ..Gate::with_python(python) });
    let messages: Vec<&str> = found.iter().map(|(_, _, _, message)| message.as_str()).collect();
    assert_eq!(messages, vec![
        "pytest was stopped at 1 seconds (`--base-run-budget`) while it collected the changed tests, so none of them \
         was judged."
    ]);
}

#[test]
fn a_test_pytest_does_not_collect_leaves_the_others_judged() {
    // pytest does not collect a test_* method of a class not named Test*; asked for it by id, it exits 4.
    let repo = calc();
    fix_add(&repo, &format!("\n\nclass CalcMixin:\n    def test_x(self):\n        assert True\n{MUL_ONE}"));
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_mul_one")]);
}

#[test]
fn a_conftest_that_cannot_be_imported_at_the_base_gives_nothing() {
    let repo = calc();
    repo.put("calc.py", fixed_calc("\n\nTWO = 2\n").as_bytes());
    let conftest = b"import pytest\n\nfrom calc import TWO\n\n\n@pytest.fixture\ndef two():\n    return TWO\n";
    repo.put("tests/conftest.py", conftest);
    let tests = format!("{TESTS}\n\ndef test_mul_two(two):\n    assert mul(two, 1) == 2\n");
    repo.put("tests/test_calc.py", tests.as_bytes());
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn a_conftest_that_cannot_be_imported_hides_only_the_tests_under_it() {
    let repo = calc();
    add_two(&repo);
    repo.put("tests/b/conftest.py", b"from calc import TWO\n");
    repo.put("tests/b/test_two.py", TEST_TWO.as_bytes());
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_mul_one")]);
}

#[test]
fn a_conftest_failing_through_a_helper_the_change_edits_hides_only_the_tests_under_it() {
    // The conftest.py is the base's, but the helper it imports is the working tree's, which needs calc.TWO.
    let repo = calc();
    repo.commit(&[("tests/b/conftest.py", b"import helpers\n"), ("tests/b/helpers.py", b"")], "helpers");
    add_two(&repo);
    repo.put("tests/b/helpers.py", b"import calc\n\nTWO = calc.TWO\n");
    repo.put("tests/b/test_two.py", TEST_TWO.as_bytes());
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_mul_one")]);
}

#[test]
fn a_conftest_that_is_a_link_the_change_edits_is_named_by_the_link() {
    // Neither file the link may point at is the change's; the change points it at the one that needs calc.TWO.
    let repo = calc();
    repo.put("shared/plain.py", b"");
    repo.put("shared/two.py", b"import calc\n\nTWO = calc.TWO\n");
    let link = repo.repo.join("tests/b/conftest.py");
    std::fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink("../../shared/plain.py", &link).unwrap();
    repo.commit(&[], "linked conftest");
    add_two(&repo);
    std::fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink("../../shared/two.py", &link).unwrap();
    repo.put("tests/b/test_two.py", TEST_TWO.as_bytes());
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_mul_one")]);
}

#[test]
fn a_conftest_the_change_edits_failing_otherwise_hides_only_the_tests_under_it() {
    // At the base, calc has no TWO, so the conftest.py the change edits raises AttributeError there.
    let repo = calc();
    repo.commit(&[("tests/b/conftest.py", b"import calc\n")], "conftest");
    add_two(&repo);
    repo.put("tests/b/conftest.py", b"import calc\n\nTWO = calc.TWO\n");
    repo.put("tests/b/test_two.py", TEST_TWO.as_bytes());
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_mul_one")]);
}

#[test]
fn a_conftest_the_change_does_not_edit_failing_is_a_finding() {
    let repo = calc();
    repo.put("calc.py", fixed_calc("").as_bytes());
    repo.put("tests/conftest.py", b"raise RuntimeError('broken')\n");
    repo.git(&["add", "tests/conftest.py"]);
    repo.git(&["commit", "--quiet", "-m", "broken conftest"]);
    repo.put("tests/test_calc.py", format!("{TESTS}{MUL_ONE}").as_bytes());
    assert_eq!(
        whole_not_run(findings(&repo, Gate::default())),
        "pytest, collecting the changed tests at the base, could not load `tests/conftest.py`, which fails through no \
         file the change edits, so the base fails there: RuntimeError: broken; the changed tests were not run there."
    );
}

#[test]
fn a_failing_root_conftest_costs_one_collection() {
    // Every changed test file is under it, so nothing is left to collect again, and nothing to run.
    let repo = calc();
    repo.put("calc.py", fixed_calc("\n\nTWO = 2\n").as_bytes());
    repo.put("conftest.py", b"import calc\n\nTWO = calc.TWO\n");
    for name in ["a", "b", "c"] {
        let test = b"from calc import mul\n\n\ndef test_one():\n    assert mul(1, 1) == 1\n";
        repo.put(format!("tests/test_{name}.py"), test);
    }
    assert_eq!(runs_at_base(&repo), (1, 0));
}

#[test]
fn a_failing_conftest_above_none_of_the_changed_tests_is_a_finding() {
    // Real pytest fails only at a conftest.py above a file it was given; a fake one names another, which ends the
    // collection rather than repeating it.
    let repo = calc();
    repo.put("elsewhere/conftest.py", b"");
    fix_add(&repo, "");
    let collect = "echo \"ImportError while loading conftest 'elsewhere/conftest.py'.\"; \
                   echo 'E   AttributeError: x'; exit 4";
    let python = fake_pytest(&repo, collect, "exit 0");
    assert_eq!(
        whole_not_run(findings(&repo, Gate::with_python(python))),
        "pytest, collecting the changed tests at the base, could not load `elsewhere/conftest.py`, above none of the \
         changed tests: AttributeError: x; the changed tests were not run there."
    );
}

#[test]
fn a_usage_error_without_a_conftest_is_a_finding() {
    let repo = calc();
    fix_add(&repo, "");
    let python = fake_pytest(&repo, "echo 'ERROR: usage: no such option'; exit 4", "exit 0");
    assert_eq!(
        whole_not_run(findings(&repo, Gate::with_python(python))),
        "pytest, collecting the changed tests at the base, exited with status 4: ERROR: usage: no such option; the \
         changed tests were not run there."
    );
}

#[test]
fn a_pytest_that_stops_before_it_collects_is_a_finding() {
    // pytest exits 1 for a plugin it cannot import, as for collection errors, but collects nothing.
    let repo = calc();
    repo.put("calc.py", fixed_calc("").as_bytes());
    repo.put("conftest.py", b"pytest_plugins = ['calc_plugin']\n");
    repo.put("calc_plugin.py", b"");
    repo.put("tests/test_calc.py", format!("{TESTS}{MUL_ONE}").as_bytes());
    let message = whole_not_run(findings(&repo, Gate::default()));
    assert!(message.starts_with("pytest stopped before it collected the changed tests at the base: "), "{message}");
    assert!(message.contains("calc_plugin"), "{message}");
}

#[test]
fn a_run_that_cannot_start_is_a_finding() {
    let repo = calc();
    fix_add(&repo, "");
    let message = whole_not_run(findings(&repo, Gate::with_python(repo.outside.join("no-python"))));
    assert!(message.starts_with("could not start pytest, collecting the changed tests at the base: "), "{message}");
}

/// Commits, as the base, a submodule at vendor/lib whose helper.py defines ONE, on pytest's path; then fixes add, and
/// adds test_mul_again, which imports ONE from it and passes at the base. Gives back the submodule's URL.
fn add_submodule(repo: &Repo) -> String {
    let lib = repo.outside.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let git = |args: &[&str]| {
        let done = Command::new("git").current_dir(&lib).args(args).output().unwrap();
        assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    };
    git(&["init", "--quiet", "-b", "main"]);
    std::fs::write(lib.join("helper.py"), "ONE = 1\n").unwrap();
    git(&["add", "helper.py"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "commit", "--quiet",
          "-m", "lib"]);
    let url = lib.to_str().unwrap().to_string();
    repo.git(&["-c", "protocol.file.allow=always", "submodule", "--quiet", "add", &url, "vendor/lib"]);
    repo.commit(&[("pytest.ini", b"[pytest]\npythonpath = . vendor/lib\n")], "submodule");
    fix_add(repo, "\n\ndef test_mul_again():\n    from helper import ONE\n    assert mul(ONE, ONE) == 1\n");
    url
}

/// What the finding on submodules the copy leaves empty opens with, for one of them.
const ONE_EMPTY: &str = "the copy of the base leaves a submodule empty, so a changed test that imports from it, if any \
                         does, was not judged: ";

#[test]
fn a_test_importing_from_a_submodule_is_judged_at_the_base() {
    let repo = calc();
    add_submodule(&repo);
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_mul_again")]);
}

#[test]
fn a_submodule_not_initialised_is_one_finding() {
    let repo = calc();
    add_submodule(&repo);
    repo.git(&["submodule", "--quiet", "deinit", "--force", "vendor/lib"]);
    assert_eq!(
        whole_not_run(findings(&repo, Gate::default())),
        format!("{ONE_EMPTY}`vendor/lib`, which is not initialised in the working tree.")
    );
}

#[test]
fn two_submodules_not_initialised_are_one_finding() {
    let repo = calc();
    let url = add_submodule(&repo);
    repo.git(&["-c", "protocol.file.allow=always", "submodule", "--quiet", "add", &url, "vendor/two"]);
    repo.git(&["commit", "--quiet", "-m", "a second submodule"]);
    repo.git(&["submodule", "--quiet", "deinit", "--force", "--all"]);
    assert_eq!(
        whole_not_run(findings(&repo, Gate::default())),
        "the copy of the base leaves submodules empty, so a changed test that imports from them, if any does, was not \
         judged: `vendor/lib`, which is not initialised in the working tree; `vendor/two`, which is not initialised \
         in the working tree."
    );
}

#[test]
fn a_submodule_marked_export_ignore_stays_out_of_the_copy() {
    let repo = calc();
    add_submodule(&repo);
    repo.put(".gitattributes", b"vendor/lib export-ignore\n");
    repo.git(&["add", ".gitattributes"]);
    repo.git(&["commit", "--quiet", "-m", "export-ignore"]);
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn a_submodule_without_the_base_s_commit_is_one_finding() {
    let repo = calc();
    add_submodule(&repo);
    // The base records a commit that the submodule's repository in the working tree never had.
    repo.git(&["update-index", "--cacheinfo", &format!("160000,{},vendor/lib", "1".repeat(40))]);
    repo.git(&["commit", "--quiet", "-m", "unknown submodule commit"]);
    assert_eq!(
        whole_not_run(findings(&repo, Gate::default())),
        format!(
            "{ONE_EMPTY}`vendor/lib`, whose repository in the working tree does not have the base's commit \
             111111111111."
        )
    );
}

#[test]
fn nothing_is_written_through_a_link_the_change_replaces() {
    let repo = calc();
    let elsewhere = repo.outside.join("elsewhere");
    std::fs::create_dir(&elsewhere).unwrap();
    repo.git(&["rm", "--quiet", "-r", "tests"]);
    std::os::unix::fs::symlink(&elsewhere, repo.repo.join("tests")).unwrap();
    repo.commit(&[], "tests elsewhere");
    std::fs::remove_file(repo.repo.join("tests")).unwrap();
    fix_add(&repo, MUL_ONE);
    // The whole test file is new to the change, so test_mul is changed too.
    let names: Vec<String> = passing_in(&repo).into_iter().map(|(_, name)| name).collect();
    assert_eq!(names, ["test_mul", "test_mul_one"]);
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}

#[test]
fn a_directory_the_change_makes_a_link_is_replaced() {
    let repo = calc();
    repo.commit(&[("tests/data/x.json", b"{}\n")], "data");
    repo.git(&["rm", "--quiet", "-r", "tests/data"]);
    repo.put("fixtures/x.json", b"{}\n");
    std::os::unix::fs::symlink("../fixtures", repo.repo.join("tests/data")).unwrap();
    fix_add(&repo, MUL_ONE);
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_mul_one")]);
}

#[test]
fn a_tracked_link_out_of_the_tree_does_not_stop_the_copy() {
    let repo = calc();
    std::os::unix::fs::symlink("/etc/hostname", repo.repo.join("host")).unwrap();
    repo.commit(&[], "link");
    fix_add(&repo, MUL_ONE);
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_mul_one")]);
}

#[test]
fn a_module_imported_through_a_tracked_link_out_of_the_tree_gets_no_bytecode_there() {
    let repo = calc();
    let far = repo.outside.join("far");
    std::fs::create_dir(&far).unwrap();
    std::fs::write(far.join("far.py"), "ONE = 1\n").unwrap();
    std::os::unix::fs::symlink(&far, repo.repo.join("far")).unwrap();
    repo.commit(&[], "link");
    // Only the run at the base imports it: the flaky-tests check's reruns do write bytecode.
    let test = format!(
        "\n\ndef test_far():\n    import sys\n    if {AT_BASE}:\n        from far.far import ONE\n        \
         assert mul(ONE, 1) == 1\n"
    );
    fix_add(&repo, &test);
    assert_eq!(passing_in(&repo), vec![passes("tests/test_calc.py", "test_far")]);
    let left: Vec<OsString> = std::fs::read_dir(&far).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    assert_eq!(left, ["far.py"]);
}

#[test]
fn an_unparsable_test_file_is_a_finding() {
    let repo = calc();
    repo.put("calc.py", fixed_calc("").as_bytes());
    repo.put("tests/test_calc.py", format!("{TESTS}def test_new(:\n").as_bytes());
    assert_eq!(findings(&repo, Gate::default()), vec![(
        Some("tests/test_calc.py".into()),
        None,
        "not-run".into(),
        "does not parse as Python, so its tests were not run at the base.".into(),
    )]);
}

#[test]
fn an_unparsable_test_file_leaves_the_others_judged() {
    let repo = calc();
    repo.put("calc.py", fixed_calc("").as_bytes());
    repo.put("tests/test_calc.py", format!("{TESTS}{MUL_AGAIN}").as_bytes());
    repo.put("tests/test_broken.py", b"def test_new(:\n");
    assert_eq!(found(&repo), vec![
        (Some("tests/test_broken.py".into()), None, "not-run".into()),
        at("tests/test_calc.py", 8, "passes-at-base"),
    ]);
}

#[test]
fn the_imports_plugin_writes_a_worker_s_report_beside_the_controller_s() {
    let repo = calc();
    let src = concat!(env!("CARGO_MANIFEST_DIR"), "/src");
    let report = repo.outside.join("imports.json");
    // A pytest the gate runs inside an xdist worker of its own inherits PYTEST_XDIST_WORKER, but is no worker.
    let driver = "\
import sys
from types import SimpleNamespace

src, report = sys.argv[1:]
sys.path.insert(0, src)
import _octoflow_gate_base_imports as plugin

options = {'gate_imports_root': src, 'gate_imports_report': report}
for config in (SimpleNamespace(getoption=options.get),
               SimpleNamespace(getoption=options.get, workerinput={'workerid': 'gw1'})):
    plugin.pytest_sessionfinish(SimpleNamespace(config=config))
";
    let mut command = Command::new(common::python());
    command.args(["-c", driver, src]).arg(&report);
    command.env("PYTEST_XDIST_WORKER", "gw7").env("PYTHONDONTWRITEBYTECODE", "1");
    let done = command.output().unwrap();
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    let mut written: Vec<OsString> =
        std::fs::read_dir(&repo.outside).unwrap().map(|entry| entry.unwrap().file_name()).collect();
    written.sort();
    assert_eq!(written, ["imports.json", "imports.json-gw1"]);
    let read: serde_json::Value = serde_json::from_slice(&std::fs::read(&report).unwrap()).unwrap();
    assert_eq!(read["_octoflow_gate_base_imports"], "_octoflow_gate_base_imports.py");
}
