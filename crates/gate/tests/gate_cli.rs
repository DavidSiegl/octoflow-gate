//! The `gate` binary run on a git repository of its own, as gate.py's TestGate runs gate.py (tests/test_gate.py):
//! the whole report it writes held to gate.py's golden files, and its exit statuses. The linters are stubs, as
//! TestGate's are ([`Repo::stub_linters`]).

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use common::Repo;
use gate::report::GATE_VERSION;

mod common;

/// TestGate's BASE_FILES: rules for ruff, and f.py of three lines.
fn test_gate_repo() -> Repo {
    Repo::with(&[("pyproject.toml", b"[tool.ruff]\n"), ("f.py", b"line1 = 1\nline2 = 2\nline3 = 3\n")])
}

/// TestGate's junit: a pytest JUnit report, results.xml in the tree, of test_ok passing and `failing`, each a test
/// and its failure's message.
fn junit(repo: &Repo, failing: &[(&str, &str)]) {
    let case = |(name, message): &(&str, &str)| {
        let failure = format!("<failure message=\"{message}\">trace</failure>");
        format!("<testcase classname=\"tests.T\" name=\"{name}\">{failure}</testcase>")
    };
    let cases: String = failing.iter().map(case).collect();
    let report = format!(
        "<?xml version=\"1.0\"?><testsuites><testsuite name=\"pytest\">\
         <testcase classname=\"tests.T\" name=\"test_ok\"/>{cases}</testsuite></testsuites>"
    );
    repo.put("results.xml", report.as_bytes());
}

/// TestGate's coverage: a Cobertura report, coverage.xml in the tree, of f.py, with each of `hits` a line and how
/// often it ran. Gives back its path.
fn coverage(repo: &Repo, hits: &[(u32, u32)]) -> PathBuf {
    let lines: String = hits.iter().map(|(n, hits)| format!("<line number=\"{n}\" hits=\"{hits}\"/>")).collect();
    let report = format!(
        "<?xml version=\"1.0\" ?><coverage><sources><source>{}</source></sources><packages><package name=\".\">\
         <classes><class name=\"f.py\" filename=\"f.py\"><lines>{lines}</lines></class>\
         </classes></package></packages></coverage>",
        repo.repo.display()
    );
    repo.put("coverage.xml", report.as_bytes());
    repo.repo.join("coverage.xml")
}

/// Runs `gate --base base --tests results.xml --coverage coverage --out <outside>/quality.md` in `repo` with the
/// linters' stubs; gives back the finished process and the quality file and quality.json it wrote.
fn golden_run(repo: &Repo, base: &str, coverage: &Path) -> (Output, [String; 2]) {
    let out = repo.outside.join("quality.md");
    let mut gate = repo.gate_command(base);
    gate.args(["--python".as_ref(), common::python().as_os_str()]);
    gate.args(["--tests".as_ref(), "results.xml".as_ref(), "--coverage".as_ref(), coverage.as_os_str()]);
    let done = gate.arg("--out").arg(&out).env("PATH", repo.stub_linters()).output().unwrap();
    let written = [out, repo.outside.join("quality.json")].map(|p| std::fs::read_to_string(p).unwrap());
    (done, written)
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
    junit(&repo, &[]);
    let (done, written) = golden_run(&repo, "HEAD", &coverage(&repo, &[(4, 1)]));
    assert_eq!(done.status.code(), Some(0), "{}", String::from_utf8_lossy(&done.stderr));
    assert_eq!(done.stdout, b"");
    let (out, json) = (repo.outside.join("quality.md"), repo.outside.join("quality.json"));
    let said = format!("GATE: OK, in {} and {}\n", out.display(), json.display());
    assert_eq!(String::from_utf8_lossy(&done.stderr), said);
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    assert_golden("gate_ok", written, &[(head, "<head>")]);
}

/// gate.py's test of this change runs test-integrity as its only check, the others running pytest, whose words are
/// the host's. This binary runs them all, and flaky-tests has one finding to weigh, of the test the change skips,
/// which is taken out of both files before they are held to the golden ones.
#[test]
fn the_file_of_a_failing_change_is_golden() {
    let repo = test_gate_repo();
    let tests = "def test_one():\n    assert 1\n\n\ndef test_two():\n    assert 2\n";
    repo.commit(&[("tests/test_f.py", tests.as_bytes()), ("uv.lock", b"version = 1\n")], "tests");
    let base = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    repo.commit(&[("README.md", b"Read me.\n")], "head");
    let head = repo.git(&["rev-parse", "HEAD"]).trim().to_string();
    repo.put("tests/test_f.py", b"import pytest\n\n\n@pytest.mark.skip\ndef test_one():\n    assert 1\n");
    repo.put("uv.lock", b"version = 2\n");
    let new: String = (4..23).map(|k| format!("new{k} = {k}\n")).collect();
    repo.put("f.py", format!("line1 = 1\nline2 = 2\nline3 = 3\n{new}import os  # noqa: F401\n").as_bytes());
    junit(&repo, &[("test_limit", "AssertionError: 3 != 2")]);
    let hits: Vec<(u32, u32)> = (1..24).map(|n| (n, u32::from(n < 14))).collect();
    let found = "f.py:2:1: F401 `os` imported but unused\n\
                 f.py:4:7: B023 Function definition does not bind loop variable `x`\nFound 2 errors.\n";
    repo.says("ruff", found.as_bytes());
    let (done, [file, json]) = golden_run(&repo, &base, &coverage(&repo, &hits));
    assert_eq!(done.status.code(), Some(1), "{}", String::from_utf8_lossy(&done.stderr));

    let skipped = "`tests/test_f.py:5` warning flaky-tests not-rerun: `test_one` ";
    let (before, rest) = file.split_once(&format!("4. {skipped}")).unwrap_or_else(|| panic!("{file}"));
    let file = format!("{before}{}", rest.split_once('\n').unwrap().1);
    let (findings, rest) = json.split_once("\n  ],\n  \"findings_total\": 4,").unwrap_or_else(|| panic!("{json}"));
    let (kept, last) = findings.rsplit_once(",\n    {\n").unwrap();
    assert!(last.contains("\"rule\": \"flaky-tests not-rerun\""), "{last}");
    let json = format!("{kept}\n  ],\n  \"findings_total\": 3,{rest}");
    assert_golden("gate_error", [file, json], &[(base, "<base>"), (head, "<head>")]);
}

#[test]
fn a_finding_on_an_added_line_fails_the_gate_and_the_report_goes_to_standard_output() {
    let repo = test_gate_repo();
    // A shell script shellcheck reads, run as a stub that finds something on the line the change adds and the one
    // before it, which only the first counts against.
    let bin = repo.stub("shellcheck", "echo 'run.sh:1:1: warning: Old. [SC1000]'\n\
                                       echo 'run.sh:2:1: warning: New. [SC2000]'\nexit 1");
    repo.put("run.sh", b"#!/bin/sh\n");
    repo.git(&["add", "run.sh"]);
    repo.git(&["commit", "--quiet", "-m", "script"]);
    repo.put("run.sh", b"#!/bin/sh\necho new\n");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let done = repo.gate_command("HEAD").env("PATH", path).output().unwrap();
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
    // A pyproject.toml that is there, beside the settings, but is no file to read rules from.
    std::os::unix::fs::symlink("nowhere.toml", repo.outside.join("pyproject.toml")).unwrap();
    std::fs::write(repo.outside.join("gate.toml"), "").unwrap();
    let unruled = repo.outside.join("gate.toml");
    for (args, said) in [
        (&["--config", "missing/gate.toml"][..], "cannot read the gate's settings"),
        (&["--config", unruled.to_str().unwrap()][..], "pyproject.toml is not a file"),
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
    let done = repo.gate("HEAD", &[OsStr::new("--config"), OsStr::new("pyproject.toml")]);
    assert_eq!(done.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&done.stderr).contains("gate: error: argument --config: names the gate's settings"));
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
