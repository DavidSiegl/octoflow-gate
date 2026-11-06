//! The fixture `cargo xtask compare` judges: a git repository whose working tree changes its branch `main` so that
//! each part the Rust gate has ported has something to report. Its files are written here rather than committed to
//! this repository, whose own gate would judge their findings, stray line end and `mutants/` file as this
//! repository's.

use std::path::Path;
use std::process::Command;

/// The files of `main`'s one commit, by path in the tree, then their text.
const BASE: &[(&str, &str)] = &[
    (".gitignore", "/junit.xml\n/coverage.xml\n"),
    ("gate.toml", GATE_TOML),
    ("pyproject.toml", "[project]\nname = \"fixture\"\nversion = \"0.1.0\"\n"),
    ("versions.txt", "shellcheck 0.10.0\n"),
    ("run-tests.sh", "#!/bin/sh\n# No tests yet.\necho '<testsuite name=\"suite\" tests=\"0\"/>' > junit.xml\n"),
    ("README.md", "# Fixture\n\nThe change `cargo xtask compare` judges.\n"),
    (".github/workflows/ci.yml", WORKFLOW),
];

/// The change the working tree makes since `main`: each file it adds or rewrites, by path in the tree, then its new
/// text, and what in it the gates report.
fn change() -> Vec<(&'static str, String)> {
    vec![
        // An edit to the tree's own settings, and to two files that hold build configuration: a ruleset of the
        // gate's own, and one gate.toml names.
        ("gate.toml", format!("# The fixture's settings.\n{GATE_TOML}")),
        ("pyproject.toml", "[project]\nname = \"fixture\"\nversion = \"0.2.0\"\n".into()),
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
        // What gate.py's checks of Python report, which the Rust gate does not port yet and the comparison leaves
        // out: ruff's F401 on line 1, ahead of every ported finding in gate.py's list.
        ("a.py", "import os\n".into()),
        // A file under mutmut's state.
        ("mutants/notes.txt", "left by a mutation run\n".into()),
    ]
}

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
