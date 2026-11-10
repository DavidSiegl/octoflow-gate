//! The fixture `cargo xtask compare` judges: a git repository whose working tree changes its branch `main` so that
//! each linter and check has something to report. Its files are written here rather than committed to this
//! repository, whose own gate would judge their findings, stray line end and `mutants/` file as this repository's.

use std::path::Path;
use std::process::Command;

/// The files of `main`'s one commit, by path in the tree, then their text.
const BASE: &[(&str, &str)] = &[
    (".gitignore", "/junit.xml\n/coverage.xml\n"),
    ("gate.toml", GATE_TOML),
    ("pyproject.toml", PYPROJECT),
    ("versions.txt", "shellcheck 0.10.0\n"),
    ("run-tests.sh", "#!/bin/sh\n# No tests yet.\necho '<testsuite name=\"suite\" tests=\"0\"/>' > junit.xml\n"),
    ("README.md", "# Fixture\n\nThe change `cargo xtask compare` judges.\n"),
    (".github/workflows/ci.yml", WORKFLOW),
    ("tiny/Cargo.toml", "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
    // passes-at-base runs the tests with `--locked`.
    ("tiny/Cargo.lock", "version = 4\n\n[[package]]\nname = \"tiny\"\nversion = \"0.1.0\"\n"),
    ("tiny/src/lib.rs", TINY_LIB),
    ("tiny/tests/it.rs", TINY_IT),
    ("tiny/tests/golden/sum.txt", "3\n"),
    ("calc.py", CALC),
    ("test_calc.py", TEST_CALC),
];

/// The change the working tree makes since `main`: each file it adds or rewrites, by path in the tree, then its new
/// text, and what in it the gates report.
fn change() -> Vec<(&'static str, String)> {
    vec![
        // An edit to the tree's own settings, and to two files that hold build configuration: a ruleset of the
        // gate's own, and one gate.toml names.
        ("gate.toml", format!("# The fixture's settings.\n{GATE_TOML}")),
        ("pyproject.toml", PYPROJECT.replace("0.1.0", "0.2.0")),
        ("versions.txt", "shellcheck 0.11.0\n".into()),
        // shellcheck's SC2086 on line 2, and a suppression of it on line 3.
        ("deploy.sh", "#!/bin/sh\necho $1\n# shellcheck disable=SC2086\nrm $2\n".into()),
        // A carriage return inside line 2, where shellcheck ends a line and git does not.
        ("late.sh", "#!/bin/sh\necho one\recho two\n".into()),
        // A test that fails, and coverage of two of deploy.sh's added lines, one of them run.
        ("run-tests.sh", RUN_TESTS.into()),
        // pymarkdown's MD026 on line 5 and MD013 on line 9; line 7 suppresses MD013 on line 8.
        (
            "README.md",
            "# Fixture\n\nThe change `cargo xtask compare` judges.\n\n## Usage.\n\n\
             <!-- pyml disable-next-line md013 -->\n\
             This line is longer than the eighty characters pymarkdown allows, and its finding is suppressed.\n\
             This line is longer than the eighty characters pymarkdown allows, and its finding is reported.\n"
                .into(),
        ),
        // actionlint's finding of an untrusted value expanded in a script, on line 8.
        (".github/workflows/ci.yml", format!("{WORKFLOW}      - run: echo \"${{{{ {UNTRUSTED} }}}}\"\n")),
        // ruff's F401 on line 1, and mypy's error of an assignment on line 3.
        ("a.py", "import os\n\nx: int = \"a\"\n".into()),
        // What the parser checks report of Python, in a change to its code: a function over the cognitive-complexity
        // limit on lines 5-39 of calc.py, with a vague name on its line 6, which a comment for ruff follows; and in
        // its tests, a test removed, a skip added on line 12 of test_calc.py, and a constant an assertion reads
        // changed on line 5. test_sum, added on line 17, passes without the change, as passes-at-base reports.
        // mutmut finds no test that runs `big`, and the mutants of `scale`, on line 42, surviving test_scale.py.
        ("calc.py", calc_changed()),
        // The one test mutmut runs ([`PYPROJECT`]): it calls `scale` and does not look at what it gives.
        ("test_scale.py", "from calc import scale\n\n\ndef test_scale():\n    scale(2, 3)\n".into()),
        (
            "test_calc.py",
            TEST_CALC
                .replace("from calc", "import pytest\n\nfrom calc")
                .replace("EXPECTED = 3", "EXPECTED = 4")
                .replace("def test_zero():\n    assert add(0, 0) == 0\n\n\n", "")
                .replace("def test_negative", "@pytest.mark.skip\ndef test_negative")
                + "\n\ndef test_sum():\n    assert add(1, 2) == 3\n",
        ),
        // A test that fails on every second run, at line 6: flaky-tests reports its mixed results. It reports
        // test_negative too, which every rerun skips. The run at the base is its first, which passes.
        (
            "test_runs.py",
            "import os\n\nRUNS = os.path.join(os.path.dirname(__file__), 'runs')\n\n\n\
             def test_alternates():\n    with open(RUNS, 'a') as f:\n        f.write('x')\n    \
             with open(RUNS) as f:\n        assert len(f.read()) % 2 == 1\n"
                .into(),
        ),
        // What the parser checks report of Rust, in a change to its code: a function over the cognitive-complexity
        // limit, with a vague name in it, on lines 5-56 of lib.rs; and in its tests, a test removed, an `#[ignore]`
        // added on line 4 of it.rs, an expected value changed on line 6, and a golden file changed. No test calls
        // the function, so cargo-mutants finds its mutants surviving.
        ("tiny/src/lib.rs", tiny_lib_changed()),
        ("tiny/tests/it.rs", TINY_IT.replace("#[test]\nfn negative", "#[test]\n#[ignore]\nfn negative").replace(
            "-2);\n}\n\n#[test]\nfn zero() {\n    assert!(add(0, 0) == 0);\n}\n", "-3);\n}\n",
        )),
        ("tiny/tests/golden/sum.txt", "4\n".into()),
        // A test that passes without the change, at line 4: passes-at-base runs it at the base.
        ("tiny/tests/sums.rs", "use tiny::add;\n\n#[test]\nfn sums() {\n    assert_eq!(add(1, 2), 3);\n}\n".into()),
        // A file under mutmut's state.
        ("mutants/notes.txt", "left by a mutation run\n".into()),
    ]
}

/// The base's pyproject.toml: mypy checks a.py, and mutmut mutates calc.py, by test_scale.py alone, for
/// test_calc.py's tests do not all pass.
const PYPROJECT: &str = "[project]\nname = \"fixture\"\nversion = \"0.1.0\"\n\n[tool.mypy]\nfiles = [\"a.py\"]\n\n\
                         [tool.mutmut]\nsource_paths = [\"calc.py\"]\n\
                         pytest_add_cli_args_test_selection = [\"test_scale.py\"]\n";

const TINY_LIB: &str = "pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n";

/// lib.rs with `big`, a function of 16 ifs one after another, and so of cognitive complexity 16, added after `add`.
fn tiny_lib_changed() -> String {
    let ifs: String = (0..16).map(|k| format!("    if x == {k} {{\n        x += {k};\n    }}\n")).collect();
    format!("{TINY_LIB}\npub fn big(mut x: u32) -> u32 {{\n    let data = x;\n{ifs}    x + data\n}}\n")
}

const CALC: &str = "def add(a, b):\n    return a + b\n";

/// calc.py with two functions added after `add`: `big`, of 16 ifs one after another, and so of cognitive complexity
/// 16, and then `scale`.
fn calc_changed() -> String {
    let ifs: String = (0..16).map(|k| format!("    if x == {k}:\n        x += {k}\n")).collect();
    let scale = "def scale(a, b):\n    return a * b\n";
    format!("{CALC}\n\ndef big(x):\n    data = x  # noqa: F841\n{ifs}    return x\n\n\n{scale}")
}

const TEST_CALC: &str = "from calc import add\n\nEXPECTED = 3\n\n\n\
                         def test_add():\n    assert add(1, 2) == EXPECTED\n\n\n\
                         def test_zero():\n    assert add(0, 0) == 0\n\n\n\
                         def test_negative():\n    assert add(-1, -1) == -2\n";

const TINY_IT: &str = "use tiny::add;\n\n#[test]\nfn negative() {\n    assert_eq!(add(-1, -1), -2);\n}\n\n\
                       #[test]\nfn zero() {\n    assert!(add(0, 0) == 0);\n}\n";

const GATE_TOML: &str = r#"[[test]]
name = "suite"
command = ["sh", "run-tests.sh"]
junit = ["junit.xml"]
coverage = ["coverage.xml"]

[build-config]
"versions.txt" = "It pins the linters' versions."
"#;

/// A value of the event that whoever pushes chooses.
const UNTRUSTED: &str = "github.event.head_commit.message";

const WORKFLOW: &str =
    "name: ci\non: push\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n      - run: echo build\n";

const RUN_TESTS: &str = r#"#!/bin/sh
# The fixture's tests: one passes, one fails, and they run one of deploy.sh's two lines that can be covered.
cat > junit.xml <<'EOF'
<testsuite name="suite" tests="2" failures="1">
  <testcase classname="suite" name="test_pass"/>
  <testcase classname="suite" name="test_fail">
    <failure message="expected 2, got 3">expected 2, got 3</failure>
  </testcase>
</testsuite>
EOF
cat > coverage.xml <<'EOF'
<?xml version="1.0" ?>
<coverage version="7.6"><packages><package name="."><classes>
<class name="deploy.sh" filename="deploy.sh"><lines>
<line number="2" hits="1"/><line number="4" hits="0"/>
</lines></class>
</classes></package></packages></coverage>
EOF
"#;

/// Makes `dir`, which must not exist, the fixture's repository: `main` holds [`BASE`], and the working tree, on
/// `main`, holds [`change`] on top, staged, as the gates judge only the files git tracks. Errs with what could not
/// be done; `dir` may then be left half made.
pub fn make(dir: &Path) -> Result<(), String> {
    std::fs::create_dir(dir).map_err(|e| format!("cannot make {}: {e}", dir.display()))?;
    write(dir, BASE)?;
    git(dir, &["init", "--quiet", "--initial-branch", "main"])?;
    git(dir, &["add", "--all"])?;
    // Whatever the user's git configuration: no signing, no hooks, and the same identity and date every time.
    git(
        dir,
        &[
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
            "commit",
            "--quiet",
            "--no-verify",
            "--date=2026-01-01T00:00:00Z",
            "--message",
            "The fixture's base",
        ],
    )?;
    write(dir, &change())?;
    git(dir, &["add", "--all"])
}

/// Writes each of `files`, a path in the tree and its text, under `dir`, making the directories it needs.
fn write(dir: &Path, files: &[(&str, impl AsRef<str>)]) -> Result<(), String> {
    for (path, text) in files {
        let path = dir.join(path);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| format!("cannot make {}: {e}", parent.display()))?;
        }
        std::fs::write(&path, text.as_ref()).map_err(|e| format!("cannot write {}: {e}", path.display()))?;
    }
    Ok(())
}

/// Runs git with `args` in `dir`; errs unless it exits 0.
fn git(dir: &Path, args: &[&str]) -> Result<(), String> {
    let done = Command::new("git")
        .current_dir(dir)
        .env("GIT_COMMITTER_DATE", "2026-01-01T00:00:00Z")
        .args(args)
        .status()
        .map_err(|e| format!("cannot run git: {e}"))?;
    done.success().then_some(()).ok_or_else(|| format!("git {} failed: {done}", args.join(" ")))
}
