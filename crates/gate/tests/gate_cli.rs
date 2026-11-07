//! The `gate` binary run on a git repository of its own, as gate.py's TestGate runs gate.py (tests/test_gate.py):
//! the whole report it writes held to gate.py's golden files, and its exit statuses.
//!
//! Only `gate_ok` is reproduced here. `gate_error` is a failing change whose findings are ruff's and the
//! test-integrity check's, which come with Python (gate-standalone/15, 16): until then golden_reports.rs renders it
//! from a report built to match, as it does the quality_report_* files.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;
use std::process::Command;

use common::Repo;
use gate::report::GATE_VERSION;

mod common;

/// TestGate's BASE_FILES: rules for ruff, which this binary does not run yet, and f.py of three lines.
fn test_gate_repo() -> Repo {
    Repo::with(&[("pyproject.toml", b"[tool.ruff]\n"), ("f.py", b"line1 = 1\nline2 = 2\nline3 = 3\n")])
}

/// Passes when `written`, the quality file and quality.json, are tests/golden/<name>.txt and .json, with each sha of
/// `shas` written as its name there and the JSON's gate_version as `<version>`.
fn assert_golden(name: &str, written: [String; 2], shas: &[(String, &str)]) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden");
    for (text, ext) in written.into_iter().zip(["txt", "json"]) {
        let version = format!("\"gate_version\": \"{GATE_VERSION}\"");
        let mut text = text.replace(&version, "\"gate_version\": \"<version>\"");
        for (sha, shown) in shas {
            text = text.replace(sha.as_str(), shown);
        }
        assert_eq!(text, std::fs::read_to_string(dir.join(format!("{name}.{ext}"))).unwrap(), "{name}.{ext}");
    }
}

#[test]
fn the_file_of_a_passing_change_is_golden() {
    let repo = test_gate_repo();
    let name = OsStr::from_bytes(b"m\xff.py");
    repo.put(name, b"m = 1\n");
    repo.git(&[OsStr::new("add"), name]);
    repo.put("f.py", b"line1 = 1\nline2 = 2\nline3 = 3\nnew4 = 4\n");
    repo.put("results.xml", b"<?xml version=\"1.0\"?><testsuites><testsuite name=\"pytest\">\
                               <testcase classname=\"tests.T\" name=\"test_ok\"/></testsuite></testsuites>");
    let coverage = format!(
        "<?xml version=\"1.0\" ?><coverage><sources><source>{}</source></sources><packages><package name=\".\">\
         <classes><class name=\"f.py\" filename=\"f.py\"><lines><line number=\"4\" hits=\"1\"/></lines></class>\
         </classes></package></packages></coverage>",
        repo.repo.display()
    );
    repo.put("coverage.xml", coverage.as_bytes());
    let out = repo.outside.join("quality.md");
    let coverage = repo.repo.join("coverage.xml");
    let done = repo.gate("HEAD", &[
        "--tests".as_ref(), "results.xml".as_ref(), "--coverage".as_ref(), coverage.as_os_str(),
        "--out".as_ref(), out.as_os_str(),
    ]);
    assert_eq!(done.status.code(), Some(0), "{}", String::from_utf8_lossy(&done.stderr));
    assert_eq!(done.stdout, b"");
    let json = repo.outside.join("quality.json");
    let said = format!("GATE: OK, in {} and {}\n", out.display(), json.display());
    assert_eq!(String::from_utf8_lossy(&done.stderr), said);
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    let written = [&out, &json].map(|p| std::fs::read_to_string(p).unwrap());
    assert_golden("gate_ok", written, &[(head, "<head>")]);
}

#[test]
fn a_finding_on_an_added_line_fails_the_gate_and_the_report_goes_to_standard_output() {
    let repo = test_gate_repo();
    // A shell script shellcheck reads, run as a stub that finds something on the line the change adds and the one
    // before it, which only the first counts against.
    let bin = repo.outside.join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let stub = bin.join("shellcheck");
    std::fs::write(&stub, "#!/bin/sh\necho 'run.sh:1:1: warning: Old. [SC1000]'\n\
                           echo 'run.sh:2:1: warning: New. [SC2000]'\nexit 1\n").unwrap();
    std::fs::set_permissions(&stub, std::os::unix::fs::PermissionsExt::from_mode(0o755)).unwrap();
    repo.put("run.sh", b"#!/bin/sh\n");
    repo.git(&["add", "run.sh"]);
    repo.git(&["commit", "--quiet", "-m", "script"]);
    repo.put("run.sh", b"#!/bin/sh\necho new\n");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let done = Command::new(env!("CARGO_BIN_EXE_gate")).current_dir(&repo.repo).env("PATH", path)
        .args(["--base", "HEAD"]).output().unwrap();
    assert_eq!(done.status.code(), Some(1), "{}", String::from_utf8_lossy(&done.stderr));
    let out = String::from_utf8(done.stdout).unwrap();
    assert!(out.starts_with("GATE: ERROR\n"), "{out}");
    assert!(out.contains("- 1 findings on lines the change added; each is listed below.\n"), "{out}");
    assert!(out.contains("1. `run.sh:2` warning shellcheck SC2000: New.\n"), "{out}");
    assert!(!out.contains("SC1000"), "{out}");
    assert!(out.contains("\n1 other open issues in the project are not on lines this change added, so they are not \
                          listed.\n"), "{out}");
    assert_eq!(done.stderr, b"");
}

#[test]
fn a_change_it_cannot_judge_writes_nothing_and_exits_2() {
    let repo = test_gate_repo();
    let out = repo.outside.join("quality.md");
    for (args, said) in [
        (&["--config", "missing/pyproject.toml"][..], "is not a file"),
        (&["--base", "no-such-rev"][..], "no-such-rev names no commit in"),
    ] {
        let done = repo.gate("HEAD", &[args, &["--out", out.to_str().unwrap()]].concat().iter().map(OsStr::new)
            .collect::<Vec<_>>());
        assert_eq!(done.status.code(), Some(2));
        let err = String::from_utf8_lossy(&done.stderr);
        assert!(err.starts_with("error: ") && err.contains(said), "{err}");
        assert!(!out.exists() && !repo.outside.join("quality.json").exists());
    }
    let done = repo.gate("HEAD", &[OsStr::new("--bogus")]);
    assert_eq!(done.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&done.stderr).contains("gate: error: unrecognized arguments: --bogus"));
}

#[test]
fn ctrl_c_writes_nothing_and_exits_130() {
    let repo = test_gate_repo();
    repo.put("gate.toml", b"[[test]]\nname = \"slow\"\ncommand = [\"sh\", \"-c\", \"touch started; sleep 30\"]\n\
                           junit = [\"results.xml\"]\n");
    let out = repo.outside.join("quality.md");
    // The gate leaves SIGINT ignored when it starts with it ignored, as a test run under Jenkins does; GNU env's
    // --default-signal gives it the default for this run whatever the test runner had.
    let mut gate = Command::new("env").current_dir(&repo.repo)
        .args(["--default-signal=INT", env!("CARGO_BIN_EXE_gate"), "--base", "HEAD", "--out"]).arg(&out)
        .spawn().unwrap();
    let start = std::time::Instant::now();
    while !repo.repo.join("started").exists() {
        assert!(start.elapsed() < std::time::Duration::from_secs(30), "the test command never started");
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    let sent = Command::new("kill").args(["-INT", &gate.id().to_string()]).status().unwrap();
    assert!(sent.success());
    let done = gate.wait().unwrap();
    assert_eq!(done.code(), Some(130));
    assert!(start.elapsed() < std::time::Duration::from_secs(20), "{:?}", start.elapsed());
    assert!(!out.exists() && !repo.outside.join("quality.json").exists());
}
