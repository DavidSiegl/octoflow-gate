//! ruff and mypy as the `gate` binary runs them over a real git repository, with stubs on PATH that print what each
//! one does, as gate.py's TestGate runs gate.py (tests/test_gate.py): what they are given, and how what ruff prints
//! of a Python file is placed on the lines git counts.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use common::Repo;
use gate::text::one_line;

mod common;

/// TestGate's BASE_FILES: rules for ruff, and f.py of three lines.
fn repo() -> Repo {
    Repo::with(&[("pyproject.toml", b"[tool.ruff]\n"), ("f.py", b"line1 = 1\nline2 = 2\nline3 = 3\n")])
}

/// f.py's base with `more` after it.
fn appended(more: &[u8]) -> Vec<u8> {
    [b"line1 = 1\nline2 = 2\nline3 = 3\n", more].concat()
}

/// The exit status and the quality file of `gate --base HEAD` in `repo`, with the linters' stubs and `env` in its
/// environment.
fn gate(repo: &Repo, env: &[(&str, &str)]) -> (Option<i32>, String) {
    let mut command = repo.gate_command("HEAD");
    command.arg("--python").arg(common::python()).env("PATH", repo.stub_linters());
    let done = command.envs(env.iter().copied()).output().unwrap();
    (done.status.code(), String::from_utf8(done.stdout).unwrap())
}

#[test]
fn ruff_is_given_the_rules_and_every_python_file_and_mypy_the_rules_alone() {
    let repo = repo();
    repo.put("sub/g.py", b"g = 1\n");
    repo.put("notes.txt", b"no Python\n");
    repo.git(&["add", "sub/g.py", "notes.txt"]);
    repo.put("f.py", &appended(b"new4 = 4\n"));
    let rules = repo.repo.join("pyproject.toml");
    let rules = rules.to_str().unwrap();
    // Whatever syntax the environment gives git's pathspecs, every tracked file is listed.
    for name in ["GIT_LITERAL_PATHSPECS", "GIT_GLOB_PATHSPECS", "GIT_NOGLOB_PATHSPECS"] {
        assert_eq!(gate(&repo, &[(name, "1")]).0, Some(0), "{name}");
        let given = ["check", "--config", rules, "--output-format", "concise", "--no-cache", "f.py", "sub/g.py"];
        assert_eq!(repo.argv("ruff"), given, "{name}");
        let given = ["--config-file", rules, "--show-column-numbers", "--no-error-summary", "--cache-dir", "/dev/null"];
        assert_eq!(repo.argv("mypy"), given, "{name}");
    }
}

#[test]
fn mypy_s_error_on_an_added_line_fails_the_gate_and_its_note_does_not() {
    let repo = repo();
    repo.put("f.py", &appended(b"tokens = []\n"));
    repo.says("mypy", b"f.py:4:1: error: Need type annotation for \"tokens\"  [var-annotated]\n\
                        f.py:4:1: note: See https://x\n");
    let (status, file) = gate(&repo, &[]);
    assert_eq!(status, Some(1));
    assert!(file.contains("- 1 findings on lines the change added; each is listed below.\n"), "{file}");
    assert!(file.contains("1. `f.py:4` error mypy var-annotated: Need type annotation for \"tokens\"\n"), "{file}");
    assert!(!file.contains("See https://x"), "{file}");
}

#[test]
fn a_carriage_return_inside_an_added_python_line_fails_the_gate() {
    // Python ends a line at the lone \r and git does not, so ruff's line 6 is git's line 5, the second added line.
    // Without the condition, the finding sits on no added line and the gate passes. A \r\n is one end.
    let repo = repo();
    repo.put("f.py", &appended(b"crlf = 1\r\npass\rimport os\n"));
    repo.says("ruff", b"f.py:6:8: F401 [*] `os` imported but unused\n");
    let (status, file) = gate(&repo, &[]);
    assert_eq!(status, Some(1));
    assert!(file.contains("- `f.py:5` holds `\\r`, which a linter that reads the file ends a line at"), "{file}");
    assert!(!file.contains("`f.py:4`"), "{file}");
}

#[test]
fn a_file_name_that_holds_a_line_separator_keeps_its_findings() {
    let repo = repo();
    let names = ["a\u{85}b.py", "a\u{2028}b.py", "a\u{1c}b.py", "a\rb.py"];
    let mut printed = String::new();
    for name in names {
        repo.put(name, b"x = 1\n");
        repo.git(&["add", name]);
        printed += &format!("{name}:1:1: F401 `os` imported but unused\n");
    }
    repo.says("ruff", printed.as_bytes());
    let (status, file) = gate(&repo, &[]);
    assert_eq!(status, Some(1));
    assert!(file.contains("- 4 findings on lines the change added"), "{file}");
    for name in names {
        assert!(file.contains(&format!("`{}:1` error ruff F401", one_line(name))), "{name:?} {file}");
    }
}

#[test]
fn a_file_name_that_is_not_utf8_is_judged() {
    let repo = repo();
    let name = OsStr::from_bytes(b"f\xe9.py");
    repo.put(name, b"a = 1\n");
    repo.git(&[OsStr::new("add"), name]);
    repo.git(&["commit", "--quiet", "-m", "latin-1 name"]);
    repo.put(name, b"a = 1\nb = 2  # noqa: E501\n");
    // ruff prints U+FFFD for each byte of the name that is not UTF-8; a tool may also print the bytes themselves.
    for printed in [&b"f\xe9.py"[..], "f\u{fffd}.py".as_bytes()] {
        repo.says("ruff", &[printed, b":2:1: E501 Line too long (130 > 120)\n"].concat());
        let (status, file) = gate(&repo, &[]);
        assert_eq!(status, Some(1));
        // The name's undecodable byte is written as an escape.
        assert!(file.contains("1. `f\\xe9.py:2` error ruff E501: Line too long (130 > 120)\n"), "{file}");
        assert!(!file.contains("none the gate could read"), "{file}");
        assert!(file.split_once("## Suppressions").unwrap().1.contains("f\\xe9.py:2"), "{file}");
    }
}
