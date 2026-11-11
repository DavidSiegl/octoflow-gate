//! The workspace's own tasks, run as `cargo xtask <task>` from anywhere in it (.cargo/config.toml):
//!
//! ```text
//! cargo xtask smoke-release TARBALL
//! ```
//!
//! `smoke-release` checks a release tarball that ci/package.sh built, before Jenkins's Release stage publishes it.
//! Unpacked into an empty directory, the release must:
//!
//! - answer `gate --help` with exit status 0;
//! - judge a fixture project as a project runs it, `gate --base REV --out quality.md`. The project has no
//!   pyproject.toml, its gate.toml lists one `[[test]]` command, and its change adds a Rust function over the
//!   cognitive-complexity limit. The gate must find that function and fail on that alone, read the command's JUnit
//!   report, and write a quality.json that the release's schema/quality-v1.json accepts.
//!
//! It exits 0 when all hold, saying so; 1, naming the first that does not on standard error, otherwise; and 2 when
//! the arguments are bad. It needs tar, git, sh, and cargo with clippy on PATH: without clippy the gate raises a
//! condition for it, which fails the check.

use std::ffi::OsStr;
use std::path::Path;
use std::process::{Command, ExitCode, Output};

use serde_json::{json, Value};

const USAGE: &str = "usage: cargo xtask smoke-release TARBALL";

/// The fixture project's settings: one test command, which writes a JUnit report of one passing test and exits 0.
const GATE_TOML: &str = r#"[[test]]
name = "smoke"
command = ["sh", "-c", """
echo '<testsuites><testsuite><testcase classname="smoke" name="test_ok"/></testsuite></testsuites>' > junit.xml"""]
junit = ["junit.xml"]
"#;

/// The fixture project's change: a new Rust function of cognitive complexity 21 (1 + 2 + ... + 6), over the limit
/// of 15.
const TANGLED_RS: &str = "\
pub fn tangled(a: u32) -> u32 {
    let mut n = 0;
    if a > 0 {
        if a > 1 {
            if a > 2 {
                if a > 3 {
                    if a > 4 {
                        if a > 5 {
                            n += 1;
                        }
                    }
                }
            }
        }
    }
    n
}
";

/// The one failed condition the fixture project's change must raise, in the gate's words for a check's findings; any
/// other, such as a check that could not run, fails the smoke check.
const EXPECTED_CONDITION: &str = "1 findings of the `cognitive-complexity` check; each is listed below.";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let (task, tarball, more) = (args.next(), args.next(), args.next());
    let (Some("smoke-release"), Some(tarball), None) = (task.as_deref().and_then(OsStr::to_str), tarball, more) else {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    };
    let tarball = Path::new(&tarball);
    match smoke_release(tarball) {
        Ok(()) => {
            println!("xtask smoke-release: {} passed", tarball.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("xtask smoke-release: {e}");
            ExitCode::from(1)
        }
    }
}

/// Runs every check of the module comment on the release in `tarball`, in a temporary directory it removes; errs
/// with the first check that fails and why.
fn smoke_release(tarball: &Path) -> Result<(), String> {
    let tmp = std::env::temp_dir().join(format!("xtask-smoke-release-{}", std::process::id()));
    // One already there is a killed earlier run's, of the same process id: it goes, so that nothing it unpacked can
    // stand in for what this tarball lacks.
    let made = match std::fs::create_dir(&tmp) {
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
            std::fs::remove_dir_all(&tmp).and_then(|()| std::fs::create_dir(&tmp))
        }
        made => made,
    };
    let (release, project) = (tmp.join("release"), tmp.join("project"));
    let checked = made
        .map_err(|e| format!("cannot make {}: {e}", tmp.display()))
        .and_then(|()| {
            [&release, &project]
                .into_iter()
                .try_for_each(|dir| std::fs::create_dir(dir).map_err(|e| format!("cannot make {}: {e}", dir.display())))
        })
        .and_then(|()| unpack(tarball, &release))
        .and_then(|()| check_help(&release))
        .and_then(|()| check_fixture_project(&release, &project));
    if let Err(e) = std::fs::remove_dir_all(&tmp) {
        eprintln!("xtask smoke-release: cannot remove {}: {e}", tmp.display());
    }
    checked
}

/// The finished run of `command`, its output captured, whatever its exit status; errs when it cannot start, naming
/// it as `what`.
fn run(command: &mut Command, what: &str) -> Result<Output, String> {
    command.output().map_err(|e| format!("cannot start {what}: {e}"))
}

/// What a finished process said: its standard error, or its standard output when that is empty, trimmed.
fn said(done: &Output) -> String {
    let stream = if done.stderr.is_empty() { &done.stdout } else { &done.stderr };
    String::from_utf8_lossy(stream).trim().to_string()
}

/// Unpacks `tarball` into the directory `release`.
fn unpack(tarball: &Path, release: &Path) -> Result<(), String> {
    let done = run(Command::new("tar").arg("-xzf").arg(tarball).arg("-C").arg(release), "tar")?;
    match done.status.success() {
        true => Ok(()),
        false => Err(format!("cannot unpack {}: {}", tarball.display(), said(&done))),
    }
}

/// Errs unless the gate of the release unpacked at `release` answers `--help` with exit status 0.
fn check_help(release: &Path) -> Result<(), String> {
    let done = run(Command::new(release.join("gate")).arg("--help"), "`gate --help`")?;
    match done.status.success() {
        true => Ok(()),
        false => Err(format!("`gate --help` ended with {}: {}", done.status, said(&done))),
    }
}

/// Makes `project`, an empty directory, a git repository whose one commit holds [`GATE_TOML`], and whose working
/// tree adds tangled.rs, [`TANGLED_RS`]; gives back that commit.
fn make_fixture_project(project: &Path) -> Result<String, String> {
    let git = |args: &[&str]| -> Result<String, String> {
        let mut git = Command::new("git");
        git.current_dir(project).args(["-c", "user.name=smoke", "-c", "user.email=smoke@localhost"]);
        let done = run(git.args(["-c", "commit.gpgSign=false"]).args(args), "git")?;
        match done.status.success() {
            true => Ok(String::from_utf8_lossy(&done.stdout).trim().to_string()),
            false => Err(format!("`git {}` failed in the fixture project: {}", args.join(" "), said(&done))),
        }
    };
    let put = |name: &str, text: &str| {
        std::fs::write(project.join(name), text).map_err(|e| format!("cannot write the fixture project's {name}: {e}"))
    };
    git(&["init", "--quiet", "--initial-branch=main"])?;
    put("gate.toml", GATE_TOML)?;
    put(".gitignore", "junit.xml\nquality*\n")?;
    git(&["add", "."])?;
    git(&["commit", "--quiet", "-m", "base"])?;
    put("tangled.rs", TANGLED_RS)?;
    git(&["add", "tangled.rs"])?;
    git(&["rev-parse", "HEAD"])
}

/// Errs unless the gate of the release at `release` judges [`make_fixture_project`]'s change, made in `project`,
/// GATE: ERROR for tangled.rs's function alone, with no failing test read from the test command's report, and
/// writes a quality.json that the release's schema accepts.
fn check_fixture_project(release: &Path, project: &Path) -> Result<(), String> {
    let base = make_fixture_project(project)?;
    let mut gate = Command::new(release.join("gate"));
    let done = run(gate.current_dir(project).args(["--base", &base, "--out", "quality.md"]), "the release's gate")?;
    let said = said(&done);
    if done.status.code() != Some(1) {
        return Err(format!("the fixture project was judged with {}, not 1 for GATE: ERROR: {said}", done.status));
    }
    let read = |path: &Path, what: &str| -> Result<Value, String> {
        let text =
            std::fs::read_to_string(path).map_err(|e| format!("cannot read {what}: {e}; the gate said: {said}"))?;
        serde_json::from_str(&text).map_err(|e| format!("{what} is not JSON: {e}"))
    };
    let report = read(&project.join("quality.json"), "the fixture project's quality.json")?;
    let schema = read(&release.join("schema/quality-v1.json"), "the release's schema/quality-v1.json")?;
    let validator = jsonschema::validator_for(&schema)
        .map_err(|e| format!("the release's schema/quality-v1.json is no JSON Schema: {e}"))?;
    validator
        .validate(&report)
        .map_err(|e| format!("the fixture project's quality.json does not follow schema/quality-v1.json: {e}"))?;
    // Of each finding, what places it; its message is the check's to word.
    let placed = |f: &Value| json!({"path": f["path"], "line": f["line"], "check": f["check"]});
    let findings = report["findings"].as_array();
    let found = Value::Array(findings.map(|all| all.iter().map(placed).collect()).unwrap_or_default());
    let expected = json!([{"path": "tangled.rs", "line": 1, "check": "cognitive-complexity"}]);
    if found != expected {
        return Err(format!("the fixture project's findings are {found}, not {expected}"));
    }
    let failing = &report["failing_tests"];
    if *failing != json!([]) {
        return Err(format!("the fixture project's test report was not read: failing_tests is {failing}, not []"));
    }
    let conditions = &report["failed_conditions"];
    if *conditions != json!([EXPECTED_CONDITION]) {
        return Err(format!("the fixture project's failed conditions are {conditions}, not [\"{EXPECTED_CONDITION}\"]"));
    }
    Ok(())
}
