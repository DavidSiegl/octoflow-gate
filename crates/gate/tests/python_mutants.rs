//! Python's half of the surviving-mutants check over a real git repository, through the `gate` binary and its
//! quality.json, as gate.py's TestSurvivingMutants runs gate.py's check (tests/test_gate.py): with the workspace's
//! own mutmut (common::python), or a `--python` that is a shell script standing in for it. Where gate.py's tests
//! patch the gate, these give the binary such a `--python`, or a TMPDIR; what a `--python` cannot vary, the gate's
//! own waits and a scratch directory that cannot be removed among it, is the unit tests' in python_mutants.rs,
//! copies.rs and process.rs. The gate runs its flaky-tests and passes-at-base checks on the same change with the same
//! Python, so a fake mutmut answers nothing to what is not mutmut's.

use std::collections::BTreeSet;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

use common::{fake_python, python_after, Repo};

mod common;

/// calc.py at the base. checked is on line 1, unchecked on 5, untouched on 9, orphan on 13, Box.size on 18,
/// Box.other on 21, describe on 25 and describe_tool on 30.
const BASE: &str = "def checked(a, b):\n    return b + a\n\n\n\
                    def unchecked(a, b):\n    return b * a\n\n\n\
                    def untouched(a, b):\n    return a - b\n\n\n\
                    def orphan():\n    return 1\n\n\n\
                    class Box:\n    def size(self):\n        return 3\n\n    def other(self):\n        return 4\n\n\n\
                    def describe(parser):\n    parser.add_argument('--count', help='how many'\n\
                    \x20                                    ' to make')\n\n\n\
                    def describe_tool(make):\n    return make(description='what the'\n\
                    \x20               ' tool does')\n";

/// tests/test_calc.py at the base. Each test calls one function; only test_checked looks at what it returns.
const TESTS: &str = "import calc\n\n\n\
                     def test_checked():\n    assert calc.checked(2, 3) == 5\n\n\n\
                     def test_unchecked():\n    calc.unchecked(2, 3)\n\n\n\
                     def test_untouched():\n    calc.untouched(2, 3)\n\n\n\
                     def test_box():\n    calc.Box().size()\n    calc.Box().other()\n";

/// How [`change_the_functions`] edits each function it edits: its name, what it holds, and what that becomes.
const EDITS: [(&str, &str, &str); 4] = [
    ("checked", "b + a", "a + b"),
    ("unchecked", "b * a", "a * b"),
    ("orphan", "return 1", "return 2"),
    ("Box.size", "return 3", "return 5"),
];

/// The findings of a change to all of [`EDITS`] under the base's rules: unchecked's test never looks at what it
/// returns, nor does Box.size's; checked's does, and no test calls orphan.
const WEAKLY_TESTED: [(&str, u32, &str); 3] =
    [("calc.py", 5, "survived"), ("calc.py", 13, "untested"), ("calc.py", 18, "survived")];

/// The workspace's own `[tool.mutmut]` do_not_mutate_patterns, as TOML, which keep mutmut off argparse's help and
/// description texts.
fn patterns() -> String {
    let own = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../pyproject.toml")).unwrap();
    let own: toml::Table = own.parse().unwrap();
    own["tool"]["mutmut"]["do_not_mutate_patterns"].to_string()
}

/// A repository whose base holds [`BASE`] as calc.py, [`TESTS`], and a pyproject.toml whose `[tool.mutmut]` mutates
/// calc.py by the tests under tests/, with the workspace's [`patterns`]; it runs neither ruff nor mypy.
fn calc() -> Repo {
    let rules = format!(
        "[tool.mutmut]\nsource_paths = [\"calc.py\"]\npytest_add_cli_args_test_selection = [\"tests/\"]\n\
         do_not_mutate_patterns = {}\n",
        patterns()
    );
    Repo::with(&[
        common::NO_PYTHON_LINTERS,
        ("pyproject.toml", rules.as_bytes()),
        ("calc.py", BASE.as_bytes()),
        ("tests/test_calc.py", TESTS.as_bytes()),
    ])
}

/// [`BASE`] with the functions `names` edited as [`EDITS`] says, all of them when `names` is empty.
fn changed(names: &[&str]) -> String {
    let mut text = BASE.to_string();
    for (name, old, new) in EDITS {
        if names.is_empty() || names.contains(&name) {
            text = text.replace(old, new);
        }
    }
    text
}

/// Edits the functions `names` in the working tree, by default checked, unchecked, orphan and Box.size, leaving
/// untouched and Box.other as they were.
fn change_the_functions(repo: &Repo, names: &[&str]) {
    repo.put("calc.py", changed(names).as_bytes());
}

/// A rules file outside the tree, as `gate --config` names one, with the TOML `table` as its `[tool.mutmut]`.
fn rules(repo: &Repo, table: &str) -> PathBuf {
    let path = repo.outside.join("pyproject.toml");
    std::fs::write(&path, format!("[tool.mutmut]\n{table}")).unwrap();
    path
}

/// A finding of the check: its path, line, own rule without the check's name, and message.
type Found = (Option<String>, Option<u32>, String, String);

/// How the gate is run beside the defaults.
#[derive(Default)]
struct Gate {
    /// The seconds mutmut may run; None for 60.
    budget: Option<u64>,
    /// The project's Python; None for the workspace's.
    python: Option<PathBuf>,
    /// `--config`: the pyproject.toml whose `[tool.mutmut]` judges the change; None for the tree's own.
    rules: Option<PathBuf>,
    /// `--test-contexts`.
    test_contexts: Option<PathBuf>,
    /// Variables of the gate's environment.
    env: Vec<(&'static str, OsString)>,
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
    command.args(["--base-run-budget", "1", "--mutation-budget", &how.budget.unwrap_or(60).to_string()]);
    if let Some(rules) = how.rules {
        command.arg("--config").arg(rules);
    }
    if let Some(test_contexts) = how.test_contexts {
        command.arg("--test-contexts").arg(test_contexts);
    }
    let done = command.envs(how.env).output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        let Some(rule) = f["rule"].as_str().unwrap().strip_prefix("surviving-mutants ") else { continue };
        assert_eq!(f["severity"], "warning", "{f}");
        let path = f["path"].as_str().map(String::from);
        let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
        out.push((path, line, rule.to_string(), f["message"].as_str().unwrap().to_string()));
    }
    out
}

/// The path, line and rule of each of `found`.
fn places(found: &[Found]) -> Vec<(Option<&str>, Option<u32>, &str)> {
    found.iter().map(|(path, line, rule, _)| (path.as_deref(), *line, rule.as_str())).collect()
}

/// Findings as [`places`] gives them, each at a line of a path.
fn at<'a>(places: &[(&'a str, u32, &'a str)]) -> Vec<(Option<&'a str>, Option<u32>, &'a str)> {
    places.iter().map(|&(path, line, rule)| (Some(path), Some(line), rule)).collect()
}

/// A finding as [`places`] gives it, on no path or line.
fn whole(rule: &str) -> (Option<&str>, Option<u32>, &str) {
    (None, None, rule)
}

/// `text` as one word of sh.
fn quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// What a fake mutmut does when asked for its results: prints this text, or runs this sh.
enum Listing<'a> {
    Prints(&'a str),
    Runs(&'a str),
}

/// A `--python` that is a fake mutmut: it answers `mutmut run` with the sh `run`, `results` as `listing` says, and
/// `show NAME` by printing the diff `shows` has for the name; and anything else, pytest among it, with nothing.
fn fake_mutmut(repo: &Repo, run: &str, listing: Listing, shows: &[(&str, &str)]) -> PathBuf {
    fake_python(repo, &mutmut_script(run, listing, shows))
}

/// The sh of [`fake_mutmut`].
fn mutmut_script(run: &str, listing: Listing, shows: &[(&str, &str)]) -> String {
    let listing = match listing {
        Listing::Prints(results) => format!("printf %s {}", quoted(results)),
        Listing::Runs(script) => script.to_string(),
    };
    let show = |(name, diff): &(&str, &str)| format!("{}) printf %s {};;\n", quoted(name), quoted(diff));
    let shows: String = shows.iter().map(show).collect();
    // The gate starts mutmut as `python -c CODE ARGS`, so the command is the third argument.
    format!("case \"$3\" in\nrun) {run};;\nresults) {listing};;\nshow) case \"$4\" in\n{shows}esac;;\nesac")
}

/// A fake mutmut whose run leaves the file [`ran`] names, and that lists no mutant.
fn mutmut_that_tells(repo: &Repo) -> PathBuf {
    fake_mutmut(repo, &format!("touch {}", quoted(ran(repo).to_str().unwrap())), Listing::Prints(""), &[])
}

/// The file [`mutmut_that_tells`] leaves when mutmut ran.
fn ran(repo: &Repo) -> PathBuf {
    repo.outside.join("ran")
}

/// A `--python` that is the workspace's, and the file that gets what its `mutmut results` printed.
fn listing_python(repo: &Repo) -> (PathBuf, PathBuf) {
    let listed = repo.outside.join("listed");
    let script = format!(
        "if [ \"$3\" = results ]; then '{}' \"$@\" | tee {}; exit 0; fi",
        common::python().display(),
        quoted(listed.to_str().unwrap())
    );
    (python_after(repo, &script), listed)
}

/// [`findings`] with the workspace's mutmut, and the functions it made mutants of, as it names them before
/// `__mutmut_<n>`.
fn found_and_made(repo: &Repo) -> (Vec<Found>, BTreeSet<String>) {
    let (python, listed) = listing_python(repo);
    let found = findings(repo, Gate::with_python(python));
    let listed = std::fs::read_to_string(listed).unwrap();
    let name = |line: &str| line.trim().split_once("__mutmut_").map(|(function, _)| function.to_string());
    (found, listed.lines().filter_map(name).collect())
}

/// Names of functions as [`found_and_made`] gives them.
fn functions(names: &[&str]) -> BTreeSet<String> {
    names.iter().map(|name| name.to_string()).collect()
}

#[test]
fn a_mutant_the_tests_do_not_notice_is_reported_and_a_killed_one_is_not() {
    let repo = calc();
    change_the_functions(&repo, &[]);
    let (found, made) = found_and_made(&repo);
    // untouched and Box.other are as weakly tested as unchecked, but the change left them alone, so mutmut made no
    // mutant of them.
    assert_eq!(made, functions(&["calc.x_checked", "calc.x_unchecked", "calc.x_orphan", "calc.xǁBoxǁsize"]));
    assert_eq!(places(&found), at(&WEAKLY_TESTED));
    assert_eq!(
        found[0].3,
        "1 of 1 mutants of `unchecked` survived its tests: line 6, `return a * b` became `return a / b`."
    );
    assert!(found[1].3.contains("`orphan`"), "{}", found[1].3);
    let opening = "1 of 1 mutants of `Box.size` survived its tests: line 19, `return 5` became ";
    assert!(found[2].3.starts_with(opening), "{}", found[2].3);
    assert_eq!(repo.git(&["status", "--porcelain", "--ignored"]), "M  calc.py\n");
}

#[test]
fn a_source_file_with_a_byte_order_mark_is_read_as_python_reads_it() {
    let repo = calc();
    repo.put("calc.py", format!("\u{feff}{}", changed(&[])).as_bytes());
    let (found, made) = found_and_made(&repo);
    // untouched and Box.other are marked to be skipped in the copy as in a file without the mark.
    assert_eq!(made, functions(&["calc.x_checked", "calc.x_unchecked", "calc.x_orphan", "calc.xǁBoxǁsize"]));
    assert_eq!(places(&found), at(&WEAKLY_TESTED));
}

#[test]
fn a_function_whose_edit_only_removes_a_guard_is_mutated() {
    let repo = calc();
    let guarded = BASE.replace("def unchecked(a, b):\n", "def unchecked(a, b):\n    if a is None:\n        return 0\n");
    repo.commit(&[("calc.py", guarded.as_bytes())], "guarded");
    repo.put("calc.py", BASE.as_bytes());
    let (found, made) = found_and_made(&repo);
    assert_eq!(made, functions(&["calc.x_unchecked"]));
    assert_eq!(places(&found), at(&[("calc.py", 5, "survived")]));
}

#[test]
fn the_rules_mutmut_table_judges_the_change_not_the_tree_s() {
    let repo = calc();
    change_the_functions(&repo, &[]);
    // The change tells mutmut to leave every line alone; the rules it is judged by do not.
    let own = std::fs::read_to_string(repo.repo.join("pyproject.toml")).unwrap();
    repo.put("pyproject.toml", own.replace("do_not_mutate_patterns = ", "do_not_mutate_patterns = ['.'] #").as_bytes());
    let rules = rules(&repo, "source_paths = [\"calc.py\"]\npytest_add_cli_args_test_selection = [\"tests/\"]\n");
    let found = findings(&repo, Gate { rules: Some(rules), ..Gate::default() });
    assert_eq!(places(&found), at(&WEAKLY_TESTED));
}

#[test]
fn a_function_outside_source_paths_is_not_mutated_and_said_so() {
    let repo = calc();
    repo.put("tools/extra.py", b"def extra():\n    return 1\n");
    let found = findings(&repo, Gate::with_python(mutmut_that_tells(&repo)));
    assert!(!ran(&repo).exists());
    assert_eq!(places(&found), at(&[("tools/extra.py", 1, "not-mutated")]));
    assert_eq!(
        found[0].3,
        "`extra` was not mutation-tested: it is outside [tool.mutmut] source_paths (calc.py)."
    );
}

#[test]
fn a_file_do_not_mutate_names_is_not_mutated_and_said_so() {
    let repo = calc();
    change_the_functions(&repo, &["orphan"]);
    let rules = rules(&repo, "source_paths = [\".\"]\ndo_not_mutate = [\"calc*\"]\n");
    let found = findings(&repo, Gate { rules: Some(rules), ..Gate::with_python(mutmut_that_tells(&repo)) });
    assert!(!ran(&repo).exists());
    assert_eq!(places(&found), at(&[("calc.py", 13, "not-mutated")]));
    assert_eq!(
        found[0].3,
        "`orphan` was not mutation-tested: [tool.mutmut] do_not_mutate excludes it (calc*)."
    );
}

#[test]
fn rules_that_name_no_source_paths_are_a_finding() {
    let repo = calc();
    change_the_functions(&repo, &[]);
    let rules = rules(&repo, "also_copy = [\"docs/\"]\n");
    let named = rules.display().to_string();
    let found = findings(&repo, Gate { rules: Some(rules), ..Gate::with_python(mutmut_that_tells(&repo)) });
    assert!(!ran(&repo).exists());
    assert_eq!(places(&found), [whole("not-run")]);
    assert_eq!(
        found[0].3,
        format!(
            "[tool.mutmut] in {named} names no source_paths as a list of paths, so what mutmut mutates is unknown; \
             the change's functions were not mutation-tested."
        )
    );
}

#[test]
fn a_function_mutmut_does_not_mutate_is_said_so() {
    // mutmut 3.8.0 mutates no decorated function but one decorated with @staticmethod or @classmethod alone, and
    // never __new__, __setattr__ or __getattribute__.
    let repo = calc();
    let text = format!(
        "{BASE}\n\nimport functools\n\n\n@functools.cache\ndef cached(a):\n    return a + 1\n\n\n\
         class Made:\n    def __new__(cls):\n        return super().__new__(cls)\n\n\
         \x20   @staticmethod\n    def make():\n        return 1\n"
    );
    repo.put("calc.py", text.as_bytes());
    let line = |wanted: &str| u32::try_from(text.split('\n').position(|line| line == wanted).unwrap() + 1).unwrap();
    // mutmut is run for Made.make, the one function it mutates, and lists its mutant.
    let run = format!("touch {}", quoted(ran(&repo).to_str().unwrap()));
    let python = fake_mutmut(&repo, &run, Listing::Prints("    calc.xǁMadeǁmake__mutmut_1: killed\n"), &[]);
    let found = findings(&repo, Gate::with_python(python));
    assert!(ran(&repo).exists());
    let (cached, new) = (line("def cached(a):"), line("    def __new__(cls):"));
    assert_eq!(places(&found), at(&[("calc.py", cached, "not-mutated"), ("calc.py", new, "not-mutated")]));
    let opening = "`cached` was not mutation-tested: mutmut does not mutate a decorated function";
    assert!(found[0].3.starts_with(opening), "{}", found[0].3);
    assert_eq!(found[1].3, "`Made.__new__` was not mutation-tested: mutmut never mutates `__new__`.");
}

#[test]
fn a_change_to_a_decorated_function_alone_runs_nothing_and_says_so() {
    let repo = calc();
    repo.put("calc.py", format!("{BASE}\n\n@property\ndef shown(self):\n    return 1\n").as_bytes());
    let found = findings(&repo, Gate::with_python(mutmut_that_tells(&repo)));
    assert_eq!(places(&found), at(&[("calc.py", 36, "not-mutated")]));
    assert!(!ran(&repo).exists());
}

#[test]
fn the_budget_counts_tried_mutants_as_each_function_does() {
    let repo = calc();
    change_the_functions(&repo, &["checked", "orphan", "unchecked"]);
    let results = "    calc.x_checked__mutmut_1: killed\n    calc.x_orphan__mutmut_1: no tests\n\
                   \x20   calc.x_unchecked__mutmut_1: not checked\n";
    let python = fake_mutmut(&repo, "exec sleep 30", Listing::Prints(results), &[]);
    let found = findings(&repo, Gate { budget: Some(1), ..Gate::with_python(python) });
    assert_eq!(places(&found), [whole("budget-reached"), (Some("calc.py"), Some(13), "untested")]);
    assert_eq!(
        found[0].3,
        "budget reached after 1 of 3 mutants: mutmut was stopped at 1 seconds (`--mutation-budget`), so the mutants \
         it did not try may survive too."
    );
    assert_eq!(found[1].3, "no test calls `orphan`, so none of its 1 mutants can make one fail.");
}

#[test]
fn a_directory_for_the_copy_that_cannot_be_made_is_a_finding() {
    let repo = calc();
    change_the_functions(&repo, &[]);
    let how = Gate { env: vec![("TMPDIR", repo.outside.join("missing").into())], ..Gate::default() };
    assert_eq!(
        findings(&repo, how),
        [(
            None,
            None,
            "not-run".to_string(),
            "could not make a directory to run mutmut on a copy of the tree in: No such file or directory (os error \
             2); nothing ran, so the check judged nothing."
                .to_string()
        )]
    );
}

#[test]
fn a_pytest_configuration_above_the_copy_is_not_read() {
    // The project has no pytest configuration of its own, so pytest under mutmut would look for one in every
    // directory above the copy, where another account may plant one, and a conftest.py beside it.
    let repo = calc();
    change_the_functions(&repo, &[]);
    let (above, planted_ran) = (repo.outside.join("tmp"), repo.outside.join("planted-ran"));
    std::fs::create_dir(&above).unwrap();
    std::fs::write(above.join("pytest.ini"), "[pytest]\n").unwrap();
    std::fs::write(above.join("conftest.py"), format!("open({planted_ran:?}, 'w').close()\n")).unwrap();
    let found = findings(&repo, Gate { env: vec![("TMPDIR", above.into())], ..Gate::default() });
    assert!(!planted_ran.exists());
    assert_eq!(places(&found), at(&WEAKLY_TESTED));
}

#[test]
fn a_pyproject_that_is_a_link_out_of_the_tree_is_read_and_not_written_through() {
    // The copy holds a tracked link as a link, and mutmut's table goes into the copy's pyproject.toml.
    let repo = calc();
    let (own, shared) = (repo.repo.join("pyproject.toml"), repo.outside.join("shared.toml"));
    std::fs::rename(&own, &shared).unwrap();
    std::os::unix::fs::symlink(&shared, &own).unwrap();
    repo.commit(&[], "rules elsewhere");
    let before = std::fs::read(&shared).unwrap();
    change_the_functions(&repo, &[]);
    assert_eq!(places(&findings(&repo, Gate::default())), at(&WEAKLY_TESTED));
    assert_eq!(String::from_utf8(std::fs::read(&shared).unwrap()), String::from_utf8(before));
}

#[test]
fn a_pyproject_that_is_a_relative_link_out_of_the_tree_is_read_from_the_tree_s_place() {
    // From the copy's place the same link names another file, here one that is not there.
    let repo = calc();
    let (own, shared) = (repo.repo.join("pyproject.toml"), repo.outside.join("shared.toml"));
    std::fs::rename(&own, &shared).unwrap();
    std::os::unix::fs::symlink("../outside/shared.toml", &own).unwrap();
    repo.commit(&[], "rules elsewhere");
    change_the_functions(&repo, &[]);
    assert_eq!(places(&findings(&repo, Gate::default())), at(&WEAKLY_TESTED));
}

#[test]
fn a_linked_pyproject_configures_the_tests_the_test_contexts_narrow_to() {
    // What the link names holds pytest's configuration too, by which the recorded tests are collected: its addopts
    // leave test_unchecked out. Taken for no configuration, the narrowing would collect both tests.
    let repo = calc();
    let (own, shared) = (repo.repo.join("pyproject.toml"), repo.outside.join("shared.toml"));
    let rules = std::fs::read_to_string(&own).unwrap();
    let options = "\n[tool.pytest.ini_options]\naddopts = \"-k test_checked\"\n";
    std::fs::write(&shared, format!("{rules}{options}")).unwrap();
    std::fs::remove_file(&own).unwrap();
    std::os::unix::fs::symlink("../outside/shared.toml", &own).unwrap();
    repo.commit(&[], "rules elsewhere");
    change_the_functions(&repo, &["checked", "unchecked"]);
    let recorded: [(&str, &[u32]); 2] =
        [("tests/test_calc.py::test_checked", &[1, 2]), ("tests/test_calc.py::test_unchecked", &[5, 6])];
    let data_file = contexts(&repo, "test", "calc.py", &recorded);
    let found = || findings(&repo, Gate { test_contexts: Some(data_file.clone()), ..Gate::default() });
    let linked = found();
    // The same settings in a file of the tree's own, then the rules without pytest's configuration.
    std::fs::remove_file(&own).unwrap();
    std::fs::copy(&shared, &own).unwrap();
    assert_eq!(linked, found());
    std::fs::write(&own, rules).unwrap();
    assert_ne!(linked, found());
}

#[test]
fn a_pyproject_that_links_to_no_file_is_a_finding() {
    // Its settings are somewhere the gate cannot read, which is not a project without the file. The rules are
    // --config's: a tree whose own rules cannot be read is not judged at all.
    let repo = calc();
    let (own, rules) = (repo.repo.join("pyproject.toml"), repo.outside.join("pyproject.toml"));
    std::fs::rename(&own, &rules).unwrap();
    std::os::unix::fs::symlink("../outside/shared.toml", &own).unwrap();
    repo.commit(&[], "settings nowhere");
    change_the_functions(&repo, &[]);
    let found = findings(&repo, Gate { rules: Some(rules), ..Gate::default() });
    assert_eq!(places(&found), [whole("not-run")]);
    assert_eq!(
        found[0].3,
        "could not start mutmut: `pyproject.toml` is a symbolic link, and what it names cannot be read: No such \
         file or directory (os error 2); the change's functions were not mutation-tested."
    );
}

#[test]
fn the_project_s_own_pytest_configuration_still_applies() {
    let repo = calc();
    change_the_functions(&repo, &[]);
    // Its addopts run only test_checked, so no test calls the other changed functions. Its pythonpath, relative to
    // the configuration's directory, must name mutmut's copy of the code, or checked's mutants go untested too.
    let own = std::fs::read_to_string(repo.repo.join("pyproject.toml")).unwrap();
    let options = "\n[tool.pytest.ini_options]\naddopts = \"-k test_checked\"\npythonpath = [\".\"]\n";
    repo.put("pyproject.toml", format!("{own}{options}").as_bytes());
    let found = findings(&repo, Gate::default());
    assert_eq!(places(&found), at(&WEAKLY_TESTED.map(|(path, line, _)| (path, line, "untested"))));
}

#[test]
fn a_mutmut_table_of_the_wrong_shape_is_a_finding_and_leaves_nothing_behind() {
    let repo = calc();
    change_the_functions(&repo, &[]);
    let tables = [
        ("only_mutate", "only_mutate = 5"),
        ("only_mutate", "only_mutate = \"calc.py\""),
        ("do_not_mutate", "do_not_mutate = [1]"),
        ("tests_dir", "tests_dir = \"tests/\""),
        ("pytest_add_cli_args_test_selection", "pytest_add_cli_args_test_selection = [1]"),
    ];
    let tmp = repo.outside.join("tmp");
    std::fs::create_dir(&tmp).unwrap();
    for (key, line) in tables {
        let rules = rules(&repo, &format!("source_paths = [\"calc.py\"]\n{line}\n"));
        let named = rules.display().to_string();
        let how = Gate {
            rules: Some(rules),
            env: vec![("TMPDIR", tmp.clone().into())],
            ..Gate::with_python(mutmut_that_tells(&repo))
        };
        let found = findings(&repo, how);
        assert_eq!(places(&found), [whole("not-run")], "{line}");
        assert_eq!(
            found[0].3,
            format!(
                "[tool.mutmut] {key} in {named} is not a list of strings; the change's functions were not \
                 mutation-tested."
            )
        );
        assert!(!ran(&repo).exists(), "{line}");
        assert_eq!(std::fs::read_dir(&tmp).unwrap().count(), 0, "{line}");
    }
}

#[test]
fn a_failing_test_that_prints_mutmut_s_words_is_not_taken_for_mutmut() {
    // The clean run fails, and mutmut prints the test's output before its own "failed to collect stats".
    let repo = calc();
    change_the_functions(&repo, &["unchecked"]);
    let loud = "\n\ndef test_loud():\n    calc.unchecked(1, 1)\n\
                \x20   print('could not find any test case for any mutant')\n    assert False\n";
    repo.put("tests/test_calc.py", format!("{TESTS}{loud}").as_bytes());
    let found = findings(&repo, Gate::default());
    assert_eq!(places(&found), [whole("not-run")]);
    assert!(found[0].3.contains("failed to collect stats"), "{}", found[0].3);
}

#[test]
fn a_failed_run_is_named_by_mutmut_s_message_not_its_config_warning() {
    // paths_to_mutate makes mutmut warn that it is deprecated, on standard error, before the failing test stops it.
    let repo = calc();
    change_the_functions(&repo, &["unchecked"]);
    repo.put("tests/test_calc.py", format!("{TESTS}\n\ndef test_failing():\n    assert False\n").as_bytes());
    let rules = rules(&repo, "paths_to_mutate = [\"calc.py\"]\npytest_add_cli_args_test_selection = [\"tests/\"]\n");
    let found = findings(&repo, Gate { rules: Some(rules), ..Gate::default() });
    assert_eq!(places(&found), [whole("not-run")]);
    let said = "exited with status 1: failed to collect stats. runner returned 1";
    assert!(found[0].3.contains(said), "{}", found[0].3);
}

#[test]
fn a_file_whose_coding_cookie_names_no_text_encoding_is_not_read() {
    // Python finds rot13, a codec of str to str, and undefined, which refuses to decode anything, by name, but
    // decodes no source with either.
    for codec in ["rot13", "undefined"] {
        let repo = calc();
        let (path, cookie) = (format!("tools/{codec}.py"), format!("# coding: {codec}\n"));
        repo.commit(&[(&path, cookie.as_bytes())], codec);
        // The changed file is the one that cannot be read.
        repo.put(&path, format!("{cookie}qrs s():\n    erghea 1\n").as_bytes());
        let found = findings(&repo, Gate::default());
        assert_eq!(places(&found), [(Some(path.as_str()), None, "not-run")], "{codec}");
        assert!(found[0].3.contains("could not be read"), "{}", found[0].3);
        // It is unchanged, and copied beside the changed one: left unmarked, it stops nothing.
        repo.put(&path, cookie.as_bytes());
        change_the_functions(&repo, &["checked"]);
        let python = fake_mutmut(&repo, "exit 0", Listing::Prints("    calc.x_checked__mutmut_1: killed\n"), &[]);
        assert_eq!(findings(&repo, Gate::with_python(python)), [], "{codec}");
    }
}

/// A repository like [`calc`]'s whose base also holds more.py, with the function g, and whose working tree changes
/// g, with `header_comment` after its header, and checked.
fn calc_and_more(header_comment: &str) -> Repo {
    let repo = calc();
    repo.commit(&[("more.py", b"def g(a):\n    return a + 1\n")], "more");
    repo.put("more.py", format!("def g(a):{header_comment}\n    return a + 2\n").as_bytes());
    change_the_functions(&repo, &["checked"]);
    repo
}

#[test]
fn a_run_whose_output_outlives_it_ends_within_the_stated_bound() {
    // mutmut ignores SIGINT here, so it is killed after the grace, and a helper in a session of its own keeps its
    // output open after that: the gate waits for neither longer than it says (TreeCopy::mutmut in python_mutants.rs).
    let repo = calc();
    change_the_functions(&repo, &[]);
    let helper_pid = repo.outside.join("helper.pid");
    let helper = format!("setsid sh -c 'echo $$ > \"$0\"; exec sleep 60' {} &", quoted(helper_pid.to_str().unwrap()));
    let python = fake_mutmut(&repo, &format!("trap '' INT; {helper} exec sleep 60"), Listing::Prints(""), &[]);
    let started = std::time::Instant::now();
    let found = findings(&repo, Gate { budget: Some(1), ..Gate::with_python(python) });
    let took = started.elapsed();
    let pid = std::fs::read_to_string(&helper_pid).unwrap();
    assert!(Command::new("kill").args(["-KILL", pid.trim()]).status().unwrap().success());
    assert_eq!(places(&found), [whole("budget-reached")]);
    // The budget of 1 second, a grace of 10 and a wait of 5 for the output, and the gate's other checks.
    assert!(took < std::time::Duration::from_secs(40), "{took:?}");
}

/// [`Gate`] with a budget of a second, rules that mutate calc.py and more.py, and a fake mutmut that runs the sh
/// `run` and lists checked's one mutant as not checked.
fn stopped_over_both(repo: &Repo, run: &str) -> Gate {
    let python = fake_mutmut(repo, run, Listing::Prints("    calc.x_checked__mutmut_1: not checked\n"), &[]);
    let rules = rules(repo, "source_paths = [\"calc.py\", \"more.py\"]\n");
    Gate { budget: Some(1), rules: Some(rules), ..Gate::with_python(python) }
}

#[test]
fn a_function_whose_file_the_budget_kept_mutmut_from_is_part_of_the_budget() {
    // mutmut makes and saves mutants file by file, so a run stopped while making them lists only some files'.
    let repo = calc_and_more("");
    let found = findings(&repo, stopped_over_both(&repo, "exec sleep 30"));
    assert_eq!(places(&found), [whole("budget-reached")]);
    let unreached = "; mutmut made no mutant of `g` in more.py before the budget ran out.";
    assert!(found[0].3.ends_with(unreached), "{}", found[0].3);
}

#[test]
fn a_pragma_mutmut_does_not_honour_is_not_given_as_the_cause() {
    // A bare `no mutate` on the def keeps only that line out; under mutate_only_covered_lines the function no test
    // calls gets no mutant for that reason instead.
    let repo = calc();
    let text = BASE.replace("def orphan():", "def orphan():  # pragma: no mutate").replace("return 1", "return 2");
    repo.put("calc.py", text.as_bytes());
    let rules = rules(
        &repo,
        "source_paths = [\"calc.py\"]\nmutate_only_covered_lines = true\n\
         pytest_add_cli_args_test_selection = [\"tests/\"]\n",
    );
    let found = findings(&repo, Gate { rules: Some(rules), ..Gate::default() });
    assert_eq!(places(&found), at(&[("calc.py", 13, "not-mutated")]));
    let opening = "`orphan` was not mutation-tested: mutmut made no mutant of it; either it has nothing mutmut mutates";
    assert!(found[0].3.starts_with(opening), "{}", found[0].3);
}

/// How a `not-mutated` finding ends that names a block pragma as the cause.
const BLOCK_PRAGMA: &str =
    "mutmut made no mutant of it; a `# pragma: no mutate block` where its header ends keeps it out.";

#[test]
fn a_block_pragma_where_a_long_header_ends_is_given_as_the_cause() {
    let repo = calc();
    let header = "def unchecked(\n        a, b):  # pragma: no mutate block";
    repo.put("calc.py", BASE.replace("def unchecked(a, b):", header).as_bytes());
    let found = findings(&repo, Gate::default());
    assert_eq!(places(&found), at(&[("calc.py", 5, "not-mutated")]));
    assert!(found[0].3.ends_with(BLOCK_PRAGMA), "{}", found[0].3);
}

#[test]
fn a_function_in_a_file_mutmut_listed_mutants_of_keeps_its_cause_after_a_budget_stop() {
    // The fake saves mutants/calc.py.meta, as mutmut does once it has made calc.py's mutants, which is how the gate
    // knows calc.py was reached.
    let repo = calc();
    let text = BASE
        .replace("b + a", "a + b")
        .replace("def unchecked(a, b):", "def unchecked(a, b):  # pragma: no mutate block");
    repo.put("calc.py", text.as_bytes());
    let run = "mkdir -p mutants; echo {} > mutants/calc.py.meta; exec sleep 30";
    let python = fake_mutmut(&repo, run, Listing::Prints("    calc.x_checked__mutmut_1: not checked\n"), &[]);
    let found = findings(&repo, Gate { budget: Some(1), ..Gate::with_python(python) });
    assert_eq!(places(&found), [whole("budget-reached"), (Some("calc.py"), Some(5), "not-mutated")]);
    assert!(!found[0].3.contains("before the budget ran out"), "{}", found[0].3);
    assert!(found[1].3.ends_with(BLOCK_PRAGMA), "{}", found[1].3);
}

#[test]
fn a_module_mutmut_saved_without_mutants_was_reached() {
    // mutmut saves a module's .meta once it has made its mutants, none or more.
    let repo = calc_and_more("  # pragma: no mutate block");
    let run = "mkdir -p mutants; for m in calc more; do echo {} > mutants/$m.py.meta; done; exec sleep 30";
    let found = findings(&repo, stopped_over_both(&repo, run));
    assert_eq!(places(&found), [whole("budget-reached"), (Some("more.py"), Some(1), "not-mutated")]);
    assert!(!found[0].3.contains("before the budget ran out"), "{}", found[0].3);
    assert!(found[1].3.ends_with(BLOCK_PRAGMA), "{}", found[1].3);
}

#[test]
fn mutmut_s_state_in_the_change_is_left_out() {
    let repo = calc();
    change_the_functions(&repo, &["unchecked"]);
    // The .meta mutmut writes for this change, kept from a first run, with every mutant marked killed.
    let kept = repo.outside.join("calc.py.meta");
    let keep = format!("[ \"$3\" != results ] || cp mutants/calc.py.meta {}", quoted(kept.to_str().unwrap()));
    let found = findings(&repo, Gate::with_python(python_after(&repo, &keep)));
    assert_eq!(places(&found), at(&[("calc.py", 5, "survived")]));
    let mut meta: serde_json::Value = serde_json::from_slice(&std::fs::read(kept).unwrap()).unwrap();
    let codes = meta["exit_code_by_key"].as_object_mut().unwrap();
    assert!(!codes.is_empty());
    codes.values_mut().for_each(|code| *code = 1.into());
    repo.put("mutants/calc.py.meta", meta.to_string().as_bytes());
    let found = findings(&repo, Gate::default());
    assert_eq!(places(&found), at(&[("calc.py", 5, "survived")]));
}

#[test]
fn a_function_under_mutmut_s_state_is_not_selected() {
    let repo = calc();
    repo.put("mutants/calc.py", b"def x_checked__mutmut_1(a, b):\n    return a - b\n");
    let rules = rules(&repo, "source_paths = [\".\"]\n");
    let found = findings(&repo, Gate { rules: Some(rules), ..Gate::with_python(mutmut_that_tells(&repo)) });
    assert_eq!(found, []);
    assert!(!ran(&repo).exists());
}

#[test]
fn unreached_functions_are_named_with_their_files() {
    let repo = calc();
    repo.commit(&[("a.py", b"def run():\n    return 1\n"), ("b.py", b"def run():\n    return 1\n")], "run twice");
    for name in ["a.py", "b.py"] {
        repo.put(name, b"def run():\n    return 2\n");
    }
    let python = fake_mutmut(&repo, "exec sleep 30", Listing::Prints(""), &[]);
    let rules = rules(&repo, "source_paths = [\"a.py\", \"b.py\"]\n");
    let found = findings(&repo, Gate { budget: Some(1), rules: Some(rules), ..Gate::with_python(python) });
    assert_eq!(places(&found), [whole("budget-reached")]);
    assert_eq!(
        found[0].3,
        "budget reached before mutmut had made its mutants: mutmut was stopped at 1 seconds (`--mutation-budget`), \
         so the mutants it did not try may survive too; mutmut made no mutant of `run` in a.py and `run` in b.py \
         before the budget ran out."
    );
}

/// The directory a `not-removed` finding's `message` names, made removable again and removed, and why the gate
/// could not remove it.
fn left_behind(message: &str) -> String {
    let named = message.strip_prefix("could not remove the directory to run mutmut on a copy of the tree in, `");
    let (dir, why) = named.expect(message).split_once("`: ").expect(message);
    assert!(Command::new("chmod").args(["-R", "u+rwX", dir]).status().unwrap().success());
    std::fs::remove_dir_all(dir).unwrap();
    why.to_string()
}

#[test]
fn a_copy_that_cannot_be_removed_is_a_finding_beside_the_others() {
    // mutmut leaves a directory in the copy that its owner cannot write, so what it holds cannot be removed. (Root
    // can remove it still: this test is for an account like the build's.)
    let repo = calc();
    change_the_functions(&repo, &["orphan"]);
    let run = "mkdir locked; touch locked/kept; chmod 500 locked";
    let python = fake_mutmut(&repo, run, Listing::Prints("    calc.x_orphan__mutmut_1: no tests\n"), &[]);
    let found = findings(&repo, Gate::with_python(python));
    assert_eq!(places(&found), [whole("not-removed"), (Some("calc.py"), Some(13), "untested")]);
    let why = left_behind(&found[0].3);
    assert!(why.contains("Permission denied"), "{}", found[0].3);
}

#[test]
fn a_save_the_stop_cut_off_loses_that_module_s_results_alone() {
    // mutmut rewrites a module's .meta in place after each mutant, so the stop can leave one cut off. The run below
    // is a real one, with more.py's .meta cut off at half once it ends, which then lasts until the budget stops it.
    let repo = calc();
    repo.commit(&[("more.py", b"def g(a):\n    return a + 1\n")], "more");
    repo.put("more.py", b"def g(a):\n    return a + 2\n");
    change_the_functions(&repo, &[]);
    let cut = format!(
        "[ \"$3\" != run ] || {{ '{}' \"$@\"; meta=mutants/more.py.meta; \
         head -c $(($(wc -c < $meta) / 2)) $meta > half; cat half > $meta; exec sleep 300; }}",
        common::python().display()
    );
    let rules = rules(
        &repo,
        "source_paths = [\"calc.py\", \"more.py\"]\npytest_add_cli_args_test_selection = [\"tests/\"]\n",
    );
    let how = Gate { budget: Some(40), rules: Some(rules), ..Gate::with_python(python_after(&repo, &cut)) };
    let found = findings(&repo, how);
    let mut expected = vec![whole("budget-reached")];
    expected.extend(at(&WEAKLY_TESTED));
    assert_eq!(places(&found), expected);
    let lost = "the stop cut off mutmut's save of its results for `g` in more.py, so they count as not reached";
    assert!(found[0].3.contains(lost), "{}", found[0].3);
    assert!(!found[0].3.contains("before the budget ran out"), "{}", found[0].3);
}

/// A fake `mutmut results` that fails while mutants/calc.py.meta is there, as mutmut's does on one cut off, and
/// lists nothing once it is gone.
const LISTS_WITHOUT_CALC_META: &str = "[ ! -e mutants/calc.py.meta ]";

/// A fake `mutmut run` that leaves mutants/calc.py.meta cut off.
const CUTS_OFF_CALC_META: &str = "mkdir -p mutants; printf '{\"exit' > mutants/calc.py.meta";

/// The findings on a change to checked of a fake mutmut that the budget stops, having run the sh `run` after
/// [`CUTS_OFF_CALC_META`], and that lists its results by the sh `listing`.
fn stopped_with_a_cut_off_save(repo: &Repo, run: &str, listing: &str) -> Vec<Found> {
    change_the_functions(repo, &["checked"]);
    let run = format!("{CUTS_OFF_CALC_META}; {run} exec sleep 30");
    let python = fake_mutmut(repo, &run, Listing::Runs(listing), &[]);
    findings(repo, Gate { budget: Some(1), ..Gate::with_python(python) })
}

#[test]
fn a_listing_that_fails_once_the_cut_off_save_is_removed_is_not_run_and_says_so() {
    let repo = calc();
    let listed = repo.outside.join("listed");
    let listing = format!("echo >> {}; echo broken >&2; exit 1", quoted(listed.to_str().unwrap()));
    let found = stopped_with_a_cut_off_save(&repo, "", &listing);
    assert_eq!(places(&found), [whole("not-run")]);
    assert_eq!(
        found[0].3,
        "mutmut could not list its results, exiting with status 1: broken after the gate removed the cut-off results \
         of calc.py; the change's functions were not mutation-tested."
    );
    assert_eq!(std::fs::read_to_string(listed).unwrap(), "\n");
}

#[test]
fn a_meta_that_cannot_be_opened_after_a_stop_is_left_to_mutmut() {
    let repo = calc();
    change_the_functions(&repo, &["checked"]);
    // mutmut reads a .meta that is not there as no results; this one is a dangling link, which the listing needs.
    let run = "mkdir -p mutants; ln -s nowhere mutants/calc.py.meta; exec sleep 30";
    let listing = "[ -L mutants/calc.py.meta ] && printf '    calc.x_checked__mutmut_1: killed\\n'";
    let python = fake_mutmut(&repo, run, Listing::Runs(listing), &[]);
    let found = findings(&repo, Gate { budget: Some(1), ..Gate::with_python(python) });
    assert_eq!(places(&found), [whole("budget-reached")]);
    assert!(found[0].3.starts_with("budget reached after 1 of 1 mutants:"), "{}", found[0].3);
}

#[test]
fn a_cut_off_save_after_a_stop_is_dropped_and_the_rest_listed() {
    let repo = calc();
    // notes.meta is the project's own file, which mutmut copies into mutants/ as it copies every file it finds.
    let listing = format!("[ -e mutants/notes.meta ] && {LISTS_WITHOUT_CALC_META}");
    let found = stopped_with_a_cut_off_save(&repo, "echo notes > mutants/notes.meta;", &listing);
    assert_eq!(places(&found), [whole("budget-reached")]);
    assert_eq!(
        found[0].3,
        "budget reached with none of the mutants mutmut made left to list: mutmut was stopped at 1 seconds \
         (`--mutation-budget`), so the mutants it did not try may survive too; the stop cut off mutmut's save of its \
         results for `checked` in calc.py, so they count as not reached."
    );
}

#[test]
fn a_cut_off_save_without_a_stop_is_not_dropped() {
    let repo = calc();
    change_the_functions(&repo, &["checked"]);
    let python = fake_mutmut(&repo, CUTS_OFF_CALC_META, Listing::Runs(LISTS_WITHOUT_CALC_META), &[]);
    let found = findings(&repo, Gate::with_python(python));
    assert_eq!(places(&found), [whole("not-run")]);
    assert!(found[0].3.starts_with("mutmut could not list its results, exiting with status 1: "), "{}", found[0].3);
}

/// What [`stopped_with_a_cut_off_save`] finds when the run ends with the sh `lock`, which takes a permission from
/// mutants/ that clearing the cut-off save needs: the message of its `not-run` finding, the copy it could then not
/// remove either removed. (Root needs no permission: these tests are for an account like the build's.)
fn stopped_with_a_locked_state(lock: &str) -> String {
    let repo = calc();
    let found = stopped_with_a_cut_off_save(&repo, lock, LISTS_WITHOUT_CALC_META);
    assert_eq!(places(&found), [whole("not-run"), whole("not-removed")]);
    left_behind(&found[1].3);
    found[0].3.clone()
}

#[test]
fn a_cut_off_save_that_cannot_be_removed_is_not_run_and_says_why() {
    let message = stopped_with_a_locked_state("chmod 500 mutants;");
    assert_eq!(
        message,
        "could not clear the results mutmut's stop cut off: Permission denied (os error 13); the change's functions \
         were not mutation-tested."
    );
}

#[test]
fn a_state_that_cannot_be_searched_after_a_stop_is_not_run_and_says_why() {
    let message = stopped_with_a_locked_state("chmod 300 mutants;");
    assert_eq!(
        message,
        "could not clear the results mutmut's stop cut off: Permission denied (os error 13); the change's functions \
         were not mutation-tested."
    );
}

#[test]
fn a_change_to_a_help_text_alone_makes_no_mutant() {
    // The second line of the help text does not say help=, but its string starts on the line that does.
    let repo = calc();
    repo.put("calc.py", BASE.replace(" to make", " to build").as_bytes());
    let (found, made) = found_and_made(&repo);
    assert_eq!(made, functions(&[]));
    // mutmut made no mutant of describe, and the finding says why rather than nothing.
    assert_eq!(places(&found), at(&[("calc.py", 25, "not-mutated")]));
    assert_eq!(
        found[0].3,
        "`describe` was not mutation-tested: mutmut made no mutant of it; the rules' do_not_mutate_patterns match \
         each statement in it."
    );
}

#[test]
fn a_change_to_a_parser_s_description_alone_makes_no_mutant() {
    let repo = calc();
    repo.put("calc.py", BASE.replace(" tool does", " tool is for").as_bytes());
    let (found, made) = found_and_made(&repo);
    assert_eq!(made, functions(&[]));
    assert_eq!(places(&found), at(&[("calc.py", 30, "not-mutated")]));
    let cause = "the rules' do_not_mutate_patterns match each statement in it.";
    assert!(found[0].3.ends_with(cause), "{}", found[0].3);
}

#[test]
fn a_function_a_mutmut_pragma_opts_out_is_said_so() {
    let repo = calc();
    let header = "def unchecked(a, b):  # pragma: no mutate block";
    repo.put("calc.py", BASE.replace("def unchecked(a, b):", header).as_bytes());
    let (found, made) = found_and_made(&repo);
    assert_eq!(made, functions(&[]));
    assert_eq!(places(&found), at(&[("calc.py", 5, "not-mutated")]));
    assert_eq!(found[0].3, format!("`unchecked` was not mutation-tested: {BLOCK_PRAGMA}"));
}

#[test]
fn a_change_to_untested_functions_alone_is_reported_as_untested() {
    // mutmut stops early when no test calls any function it mutated.
    let repo = calc();
    change_the_functions(&repo, &["orphan"]);
    assert_eq!(places(&findings(&repo, Gate::default())), at(&[("calc.py", 13, "untested")]));
}

#[test]
fn a_change_to_no_function_runs_nothing() {
    let repo = calc();
    repo.put("calc.py", format!("{BASE}\nLIMIT = 3\n").as_bytes());
    repo.put("tests/test_calc.py", format!("{TESTS}\n\ndef test_more():\n    calc.checked(1, 1)\n").as_bytes());
    assert_eq!(findings(&repo, Gate::with_python(mutmut_that_tells(&repo))), []);
    assert!(!ran(&repo).exists());
}

/// What `mutmut show` prints for a mutant of unchecked, which returns `old`, that returns `new`.
fn unchecked_mutant(old: &str, new: &str) -> String {
    format!(
        "# a mutant: survived\n--- calc.py\n+++ calc.py\n@@ -1,2 +1,2 @@\n def unchecked(a, b):\n-    return {old}\n\
         +    return {new}\n"
    )
}

#[test]
fn a_run_stopped_by_the_budget_says_so_with_what_it_found() {
    let repo = calc();
    change_the_functions(&repo, &["checked", "unchecked"]);
    let results = "    calc.x_checked__mutmut_1: killed\n    calc.x_unchecked__mutmut_1: survived\n\
                   \x20   calc.x_unchecked__mutmut_2: not checked\n";
    let shown = unchecked_mutant("a * b", "a / b");
    let shows = [("calc.x_unchecked__mutmut_1", shown.as_str())];
    let python = fake_mutmut(&repo, "exec sleep 30", Listing::Prints(results), &shows);
    let started = Instant::now();
    let found = findings(&repo, Gate { budget: Some(1), ..Gate::with_python(python) });
    assert!(started.elapsed() < Duration::from_secs(15), "{:?}", started.elapsed());
    assert_eq!(places(&found), [whole("budget-reached"), (Some("calc.py"), Some(5), "survived")]);
    assert!(found[0].3.starts_with("budget reached after 2 of 3 mutants"), "{}", found[0].3);
    assert!(found[1].3.starts_with("1 of 1 mutants of `unchecked`"), "{}", found[1].3);
}

/// The one finding, on no path or line and of rule `not-run`, of a change to all the functions under the fake
/// mutmut whose run is the sh `run`: its message.
fn not_run_by(run: &str) -> String {
    let repo = calc();
    change_the_functions(&repo, &[]);
    let found = findings(&repo, Gate::with_python(fake_mutmut(&repo, run, Listing::Prints(""), &[])));
    assert_eq!(places(&found), [whole("not-run")]);
    found[0].3.clone()
}

#[test]
fn a_run_that_fails_is_a_finding() {
    assert_eq!(
        not_run_by("echo 'failed to collect stats. runner returned 1'; exit 1"),
        "mutmut exited with status 1: failed to collect stats. runner returned 1; the change's functions were not \
         mutation-tested."
    );
}

#[test]
fn a_project_without_mutmut_gets_a_finding_naming_the_import_error() {
    let run = "echo 'Traceback (most recent call last):' >&2; \
               echo \"ModuleNotFoundError: No module named 'mutmut'\" >&2; exit 1";
    assert_eq!(
        not_run_by(run),
        "mutmut exited with status 1: ModuleNotFoundError: No module named 'mutmut'; the change's functions were not \
         mutation-tested."
    );
}

#[test]
fn the_survivors_of_a_function_are_one_finding_with_examples() {
    let repo = calc();
    change_the_functions(&repo, &["unchecked"]);
    let results = "    calc.x_unchecked__mutmut_1: survived\n    calc.x_unchecked__mutmut_2: killed\n\
                   \x20   calc.x_unchecked__mutmut_3: survived\n";
    let (first, third) = (unchecked_mutant("a * b", "a / b"), unchecked_mutant("a * b", "None"));
    let shows = [("calc.x_unchecked__mutmut_1", first.as_str()), ("calc.x_unchecked__mutmut_3", third.as_str())];
    let python = fake_mutmut(&repo, "exit 0", Listing::Prints(results), &shows);
    let found = findings(&repo, Gate::with_python(python));
    assert_eq!(places(&found), at(&[("calc.py", 5, "survived")]));
    assert_eq!(
        found[0].3,
        "2 of 3 mutants of `unchecked` survived its tests: line 6, `return a * b` became `return a / b`; line 6, \
         `return a * b` became `return None`."
    );
}

#[test]
fn an_example_keeps_a_line_that_holds_a_line_separator_whole() {
    // Only "\n" ends a line of mutmut's output, as of git's; U+2028 in a string is part of the line.
    let repo = calc();
    repo.put("calc.py", BASE.replace("return b * a", "return 'a\u{2028}b'").as_bytes());
    let shown = unchecked_mutant("'a\u{2028}b'", "'XXa\u{2028}bXX'");
    let shows = [("calc.x_unchecked__mutmut_1", shown.as_str())];
    let python = fake_mutmut(&repo, "exit 0", Listing::Prints("    calc.x_unchecked__mutmut_1: survived\n"), &shows);
    let found = findings(&repo, Gate::with_python(python));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0].3,
        "1 of 1 mutants of `unchecked` survived its tests: line 6, `return 'a\u{2028}b'` became `return \
         'XXa\u{2028}bXX'`."
    );
}

#[test]
fn a_function_s_first_three_survivors_are_its_examples() {
    let repo = calc();
    change_the_functions(&repo, &["unchecked"]);
    let names: Vec<String> = (1..5).map(|n| format!("calc.x_unchecked__mutmut_{n}")).collect();
    let results: String = names.iter().map(|name| format!("    {name}: survived\n")).collect();
    let shown: Vec<String> = (0..4).map(|n| unchecked_mutant("a * b", &n.to_string())).collect();
    let shows: Vec<(&str, &str)> = names.iter().map(String::as_str).zip(shown.iter().map(String::as_str)).collect();
    let python = fake_mutmut(&repo, "exit 0", Listing::Prints(&results), &shows);
    let found = findings(&repo, Gate::with_python(python));
    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(
        found[0].3,
        "4 of 4 mutants of `unchecked` survived its tests: line 6, `return a * b` became `return 0`; line 6, `return \
         a * b` became `return 1`; line 6, `return a * b` became `return 2`."
    );
}

#[test]
fn a_mutmut_that_cannot_start_is_a_finding() {
    let repo = calc();
    change_the_functions(&repo, &[]);
    let python = repo.outside.join("no-python");
    let found = findings(&repo, Gate::with_python(python.clone()));
    assert_eq!(places(&found), [whole("not-run")]);
    assert_eq!(
        found[0].3,
        format!(
            "could not start mutmut with the Python `{}`: No such file or directory (os error 2); the change's \
             functions were not mutation-tested.",
            python.display()
        )
    );
}

#[test]
fn a_python_that_cannot_start_to_read_the_test_contexts_is_named() {
    let repo = calc();
    change_the_functions(&repo, &[]);
    let python = repo.outside.join("no-python");
    let how = Gate { test_contexts: Some(repo.outside.join("test.coverage")), ..Gate::with_python(python.clone()) };
    let found = findings(&repo, how);
    let said = format!("coverage.py could not be started with the Python `{}` to read ", python.display());
    assert!(found.iter().any(|f| f.3.contains(&said)), "{found:?}");
}

#[test]
fn an_unparsable_source_file_is_a_finding() {
    let repo = calc();
    repo.put("calc.py", format!("{BASE}def broken(:\n").as_bytes());
    let found = findings(&repo, Gate::with_python(mutmut_that_tells(&repo)));
    assert!(!ran(&repo).exists());
    assert_eq!(places(&found), [(Some("calc.py"), None, "not-run")]);
    assert!(found[0].3.ends_with(", so its functions were not mutation-tested."), "{}", found[0].3);
}

#[test]
fn it_reports_without_failing_the_gate() {
    let repo = calc();
    change_the_functions(&repo, &["unchecked"]);
    repo.git(&["add", "-A"]);
    let python = fake_mutmut(&repo, "exit 0", Listing::Prints("    calc.x_unchecked__mutmut_1: survived\n"), &[]);
    let done = repo.gate_command("HEAD").arg("--python").arg(python).output().unwrap();
    let printed = String::from_utf8(done.stdout).unwrap();
    assert_eq!(done.status.code(), Some(0), "{printed}");
    assert!(printed.starts_with("GATE: OK\n"), "{printed}");
    assert!(printed.contains("1 of 1 mutants of `unchecked` survived its tests"), "{printed}");
}

/// A new coverage.py data file outside the tree, `name`.coverage, as `pytest --cov-context=test` writes one,
/// recording each test of `lines_by_test`, a node id, running its lines of the file at `path` in the tree.
fn contexts(repo: &Repo, name: &str, path: &str, lines_by_test: &[(&str, &[u32])]) -> PathBuf {
    let data_file = repo.outside.join(format!("{name}.coverage"));
    let program = "import json, sys\nfrom coverage import CoverageData\n\
                   recording = CoverageData(basename=sys.argv[1])\n\
                   for node_id, lines in json.loads(sys.argv[3]):\n\
                   \x20   recording.set_context(f'{node_id}|run')\n\
                   \x20   recording.add_lines({sys.argv[2]: lines})\n\
                   recording.write()\n";
    let measured = repo.repo.canonicalize().unwrap().join(path);
    let mut command = Command::new(common::python());
    command.args(["-c", program]).arg(&data_file).arg(measured).arg(serde_json::json!(lines_by_test).to_string());
    let done = command.output().unwrap();
    assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    data_file
}

/// A `--python` that is the workspace's, but for mutmut: its `run` keeps the pyproject.toml it was to run by as the
/// file this gives, and does nothing, and it lists no mutant. So the file is missing when the gate started no mutmut.
fn mutmut_that_keeps_its_table(repo: &Repo) -> (PathBuf, PathBuf) {
    let kept = repo.outside.join("kept.toml");
    let run = format!("cat pyproject.toml > {}; exit 0", quoted(kept.to_str().unwrap()));
    (python_after(repo, &mutmut_script(&run, Listing::Prints(""), &[])), kept)
}

/// The `[tool.mutmut]` [`mutmut_that_keeps_its_table`] kept in `kept`; None when no mutmut was started.
fn kept_table(kept: &Path) -> Option<toml::Table> {
    let whole: toml::Table = std::fs::read_to_string(kept).ok()?.parse().unwrap();
    Some(whole["tool"]["mutmut"].as_table().unwrap().clone())
}

/// The test selection of `table`, a `[tool.mutmut]`.
fn selection(table: &toml::Table) -> Vec<&str> {
    let selected = table["pytest_add_cli_args_test_selection"].as_array().unwrap();
    selected.iter().map(|test| test.as_str().unwrap()).collect()
}

#[test]
fn mutmut_runs_only_the_tests_the_contexts_record_reaching_the_changed_functions() {
    let repo = calc();
    change_the_functions(&repo, &["checked", "unchecked"]);
    // A parity test reaches checked too, but the rules leave tests/parity out, as this repository's once did; and
    // the tree has none, so pytest collects it from no selection.
    let recorded: [(&str, &[u32]); 4] = [
        ("tests/test_calc.py::test_checked", &[1, 2]),
        ("tests/test_calc.py::test_unchecked", &[5, 6]),
        ("tests/test_calc.py::test_untouched", &[9, 10]),
        ("tests/parity/test_p.py::test_p", &[2]),
    ];
    let data_file = contexts(&repo, "test", "calc.py", &recorded);
    let table = "source_paths = [\"calc.py\"]\npytest_add_cli_args_test_selection = [\"tests/\"]\n\
                 pytest_add_cli_args = [\"--ignore=tests/parity\"]\n";
    let keep = "[ \"$3\" != run ] || cat pyproject.toml > ../../kept.toml";
    let how = Gate {
        rules: Some(rules(&repo, table)),
        test_contexts: Some(data_file),
        env: vec![("TMPDIR", repo.outside.clone().into())],
        ..Gate::with_python(python_after(&repo, keep))
    };
    let found = findings(&repo, how);
    let kept = kept_table(&repo.outside.join("kept.toml")).unwrap();
    assert_eq!(selection(&kept), ["tests/test_calc.py::test_checked", "tests/test_calc.py::test_unchecked"]);
    // The same verdicts as with the whole selection: unchecked's test never looks at what it returns.
    assert_eq!(places(&found), at(&[("calc.py", 5, "survived")]));
}

/// Test files beside tests/test_calc.py, for pytest to collect by a selection: each holds one test, named for it.
const MORE_TESTS: [(&str, &str); 4] = [
    ("tests/test_calcs.py", "test_z"),
    ("tests/parity/test_p.py", "test_p"),
    ("tests/slow/test_w.py", "test_w"),
    ("other/test_o.py", "test_o"),
];

/// A repository like [`calc`]'s whose base also holds [`MORE_TESTS`] and a directory without tests, empty/, with a
/// change to checked, and a data file that records test_checked and each of [`MORE_TESTS`] running it.
fn calc_with_more_tests() -> (Repo, PathBuf) {
    let repo = calc();
    let more = MORE_TESTS.map(|(path, name)| (path, format!("def {name}():\n    pass\n")));
    let mut files: Vec<(&str, &[u8])> = more.iter().map(|(path, text)| (*path, text.as_bytes())).collect();
    files.push(("empty/notes.txt", b"no tests here\n"));
    repo.commit(&files, "more tests");
    change_the_functions(&repo, &["checked"]);
    let mut recorded = vec!["tests/test_calc.py::test_checked".to_string()];
    recorded.extend(MORE_TESTS.map(|(path, name)| format!("{path}::{name}")));
    let recorded: Vec<(&str, &[u32])> = recorded.iter().map(|test| (test.as_str(), &[2][..])).collect();
    let data_file = contexts(&repo, "test", "calc.py", &recorded);
    (repo, data_file)
}

/// What the gate makes of `table`, the TOML of a `[tool.mutmut]` beside its source_paths, on
/// [`calc_with_more_tests`]: the table mutmut was to run by, None when none was started, and the findings.
fn narrowed(table: &str) -> (Option<toml::Table>, Vec<Found>) {
    let (repo, data_file) = calc_with_more_tests();
    let (python, kept) = mutmut_that_keeps_its_table(&repo);
    let rules = rules(&repo, &format!("source_paths = [\"calc.py\"]\n{table}"));
    let how = Gate { rules: Some(rules), test_contexts: Some(data_file), ..Gate::with_python(python) };
    let found = findings(&repo, how);
    (kept_table(&kept), found)
}

#[test]
fn mutmut_is_given_the_recorded_tests_pytest_collects_by_its_selection_and_options() {
    // What pytest 9 collects, by its own rules, of each selection with each of mutmut's options.
    let cases: [(&str, &[&str], &[&str]); 5] = [
        (
            "pytest_add_cli_args_test_selection = [\"tests/\"]\n",
            &["--ignore=tests/parity"],
            &["tests/slow/test_w.py::test_w", "tests/test_calc.py::test_checked", "tests/test_calcs.py::test_z"],
        ),
        // A selection entry inside another is dropped before pytest collects; the outer one's walk applies the
        // glob below it.
        (
            "pytest_add_cli_args_test_selection = [\"tests\", \"tests/slow\"]\n",
            &["--ignore-glob=*/slow"],
            &["tests/parity/test_p.py::test_p", "tests/test_calc.py::test_checked", "tests/test_calcs.py::test_z"],
        ),
        // What the selection names pytest collects whatever --ignore and --ignore-glob say.
        (
            "pytest_add_cli_args_test_selection = [\"tests/slow\"]\n",
            &["--ignore-glob=*/slow"],
            &["tests/slow/test_w.py::test_w"],
        ),
        (
            "pytest_add_cli_args_test_selection = [\"tests/test_calc.py::test_checked\", \"tests/test_calcs.py\"]\n",
            &["--ignore=tests/test_calcs.py", "--ignore=tests"],
            &["tests/test_calc.py::test_checked", "tests/test_calcs.py::test_z"],
        ),
        (
            "pytest_add_cli_args_test_selection = [\"tests/\"]\n",
            &["--deselect=tests/test_calcs.py::test_z", "--ignore-glob=*/p*"],
            &["tests/slow/test_w.py::test_w", "tests/test_calc.py::test_checked"],
        ),
    ];
    for (selected, args, tests) in cases {
        let given: Vec<String> = args.iter().map(|arg| format!("{arg:?}")).collect();
        let (table, found) = narrowed(&format!("{selected}pytest_add_cli_args = [{}]\n", given.join(", ")));
        let table = table.expect(selected);
        assert_eq!(selection(&table), tests, "{selected} {args:?}");
        // mutmut's own options stay, ahead of the gate's.
        let options = table["pytest_add_cli_args"].as_array().unwrap();
        let options: Vec<&str> = options.iter().filter_map(|option| option.as_str()).collect();
        assert_eq!(&options[..args.len()], args, "{selected}");
        // The fake made no mutant of checked, and nothing is said of the narrowing.
        assert_eq!(places(&found), at(&[("calc.py", 1, "not-mutated")]), "{selected} {args:?}");
    }
}

#[test]
fn without_a_selection_mutmut_is_given_every_recorded_test_pytest_finds() {
    let (table, _) = narrowed("");
    let mut every = vec!["tests/test_calc.py::test_checked".to_string()];
    every.extend(MORE_TESTS.map(|(path, name)| format!("{path}::{name}")));
    every.sort();
    assert_eq!(selection(&table.unwrap()), every);
}

#[test]
fn tests_dir_mutmut_s_old_name_for_it_joins_the_selection() {
    let (table, _) = narrowed("tests_dir = [\"other/\"]\n");
    let table = table.unwrap();
    assert_eq!(selection(&table), ["other/test_o.py::test_o"]);
    assert!(!table.contains_key("tests_dir"), "{table}");
}

#[test]
fn a_selection_that_collects_none_of_the_recorded_tests_starts_no_mutmut() {
    // empty/ holds no test at all, which is no failed collection: mutmut has no test to run.
    for selected in ["other/", "empty/"] {
        let (repo, _) = calc_with_more_tests();
        let data_file = contexts(&repo, "few", "calc.py", &[("tests/test_calcs.py::test_z", &[2])]);
        let (python, kept) = mutmut_that_keeps_its_table(&repo);
        let table = format!("source_paths = [\"calc.py\"]\npytest_add_cli_args_test_selection = [{selected:?}]\n");
        let how =
            Gate { rules: Some(rules(&repo, &table)), test_contexts: Some(data_file), ..Gate::with_python(python) };
        let found = findings(&repo, how);
        assert_eq!(places(&found), at(&[("calc.py", 1, "untested")]), "{selected}");
        assert_eq!(kept_table(&kept), None, "{selected}");
    }
}

#[test]
fn a_collection_that_fails_leaves_mutmut_the_whole_selection_and_says_why() {
    let (table, found) = narrowed("pytest_add_cli_args_test_selection = [\"tests/missing.py\"]\n");
    assert_eq!(selection(&table.unwrap()), ["tests/missing.py"]);
    assert_eq!(places(&found), [whole("not-narrowed"), (Some("calc.py"), Some(1), "not-mutated")]);
    let why = "collecting [tool.mutmut]'s test selection, exited with status 4";
    assert!(found[0].3.contains(why), "{}", found[0].3);
}

#[test]
fn the_tree_s_own_modules_do_not_stand_in_for_the_reader_s() {
    // The reader runs at the tree's top, where git ignores these; the copy mutmut and pytest run in has neither.
    let repo = calc();
    repo.commit(&[(".gitignore", b"json.py\ncoverage.py\n")], "ignored");
    for module in ["json.py", "coverage.py"] {
        repo.put(module, b"raise SystemExit('the tree's own module ran')\n");
    }
    change_the_functions(&repo, &["checked"]);
    let data_file = contexts(&repo, "test", "calc.py", &[("tests/test_calc.py::test_checked", &[2])]);
    let (python, kept) = mutmut_that_keeps_its_table(&repo);
    let found = findings(&repo, Gate { test_contexts: Some(data_file), ..Gate::with_python(python) });
    assert_eq!(selection(&kept_table(&kept).unwrap()), ["tests/test_calc.py::test_checked"]);
    assert_eq!(places(&found), at(&[("calc.py", 1, "not-mutated")]));
}

#[test]
fn a_reader_output_of_the_wrong_shape_is_a_reason_not_a_crash() {
    let repo = calc();
    change_the_functions(&repo, &["checked"]);
    for reached in ["[]", "[[1]]", "[3]", "{}"] {
        // The gate runs the reader as `python -I -c PROGRAM`.
        let printed = format!("{{\"tests\": 1, \"busiest\": 1, \"reached\": {reached}}}");
        let script = format!(
            "[ \"$1\" != -I ] || {{ printf %s {}; exit 0; }}\n{}",
            quoted(&printed),
            mutmut_script("exit 0", Listing::Prints("    calc.x_checked__mutmut_1: killed\n"), &[])
        );
        let how = Gate { test_contexts: Some(repo.outside.join("test.coverage")), ..Gate::default() };
        let found = findings(&repo, Gate { python: Some(fake_python(&repo, &script)), ..how });
        assert_eq!(places(&found), [whole("not-narrowed")], "{reached}");
        assert!(found[0].3.contains("printed what the gate cannot read"), "{}", found[0].3);
    }
}

#[test]
fn without_test_contexts_mutmut_runs_the_whole_selection() {
    let repo = calc();
    change_the_functions(&repo, &["checked"]);
    let (python, kept) = mutmut_that_keeps_its_table(&repo);
    findings(&repo, Gate::with_python(python));
    assert_eq!(selection(&kept_table(&kept).unwrap()), ["tests/"]);
}

#[test]
fn functions_no_recorded_test_reaches_are_untested_without_mutmut() {
    let repo = calc();
    change_the_functions(&repo, &["checked", "orphan"]);
    let data_file = contexts(&repo, "test", "calc.py", &[("tests/test_calc.py::test_untouched", &[10])]);
    let (python, kept) = mutmut_that_keeps_its_table(&repo);
    let found = findings(&repo, Gate { test_contexts: Some(data_file), ..Gate::with_python(python) });
    assert_eq!(kept_table(&kept), None);
    assert_eq!(places(&found), at(&[("calc.py", 1, "untested"), ("calc.py", 13, "untested")]));
    assert_eq!(
        found[0].3,
        "no test of [tool.mutmut]'s selection runs `checked`, as `--test-contexts` records them, so mutmut was not \
         started."
    );
}

#[test]
fn contexts_that_cannot_tell_the_tests_leave_mutmut_the_whole_selection_and_say_why() {
    let repo = calc();
    change_the_functions(&repo, &["checked"]);
    // Eleven tests, none of the file's lines recorded for more than one: as the sys.monitoring core writes it.
    let tests: Vec<String> = (0..11).map(|i| format!("tests/test_calc.py::test_{i}")).collect();
    let lines: Vec<[u32; 1]> = (0..11).map(|i| [if i == 0 { 2 } else { 30 + i }]).collect();
    let under_recorded: Vec<(&str, &[u32])> =
        tests.iter().map(String::as_str).zip(lines.iter().map(|line| &line[..])).collect();
    let under_recorded = contexts(&repo, "under", "calc.py", &under_recorded);
    let cases = [
        ("records 11 tests but none of its lines for more than 1", under_recorded),
        (
            "did not measure `calc.py`",
            contexts(&repo, "other", "other.py", &[("tests/test_calc.py::test_checked", &[2])]),
        ),
        ("no such file", repo.outside.join("missing.coverage")),
    ];
    for (why, data_file) in cases {
        let (python, kept) = mutmut_that_keeps_its_table(&repo);
        let found = findings(&repo, Gate { test_contexts: Some(data_file), ..Gate::with_python(python) });
        assert_eq!(selection(&kept_table(&kept).unwrap()), ["tests/"], "{why}");
        std::fs::remove_file(kept).unwrap();
        assert_eq!(places(&found), [whole("not-narrowed"), (Some("calc.py"), Some(1), "not-mutated")], "{why}");
        assert!(found[0].3.contains(why), "{}", found[0].3);
    }
}
