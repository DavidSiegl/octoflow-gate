//! ruff and mypy as the `gate` binary runs them over a real git repository, with stubs on PATH that print what each
//! one does: what they are given, and how what ruff prints of a Python file is placed on the lines git counts.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use common::Repo;
use gate::text::one_line;

mod common;

/// A repository whose base holds rules for ruff, and f.py of three lines.
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
fn without_a_pyproject_toml_ruff_and_mypy_run_on_their_defaults_whatever_the_tree_configures() {
    // ruff.toml and mypy.ini are what each would read by itself.
    let repo = Repo::with(&[("f.py", b"line1 = 1\n"), ("ruff.toml", b"lint.ignore = [\"F401\"]\n"), ("mypy.ini", b"")]);
    repo.put("f.py", b"line1 = 1\nnew2 = 2\n");
    let (status, file) = gate(&repo, &[]);
    assert_eq!(status, Some(0), "{file}");
    assert_eq!(repo.argv("ruff"), ["check", "--isolated", "--output-format", "concise", "--no-cache", "f.py"]);
    let given = ["--config-file", "", "--show-column-numbers", "--no-error-summary", "--cache-dir", "/dev/null", "f.py"];
    assert_eq!(repo.argv("mypy"), given);
    assert!(file.contains("judged the working tree at"), "{file}");
    assert!(!file.contains("by the rules from"), "{file}");
}

#[test]
fn the_rules_are_those_beside_the_gate_toml_config_names_and_none_when_none_is_there() {
    let repo = repo();
    repo.put("f.py", &appended(b"new4 = 4\n"));
    let settings = repo.outside.join("gate.toml");
    std::fs::write(&settings, "").unwrap();
    let rules = repo.outside.join("pyproject.toml");
    let judged = |repo: &Repo| {
        let mut command = repo.gate_command("HEAD");
        command.arg("--config").arg(&settings).env("PATH", repo.stub_linters());
        let done = command.output().unwrap();
        assert_eq!(done.status.code(), Some(0), "{}", String::from_utf8_lossy(&done.stderr));
        String::from_utf8(done.stdout).unwrap()
    };
    // The tree's own pyproject.toml is not beside the settings, so it rules nothing.
    let file = judged(&repo);
    assert_eq!(repo.argv("ruff")[..2], ["check", "--isolated"]);
    let intro = format!(
        ", by the rules from the defaults (there is no `{}`), with the settings from `{}`.",
        rules.display(),
        settings.display()
    );
    assert!(file.contains(&intro), "{file}");
    std::fs::write(&rules, "[tool.ruff]\n").unwrap();
    let file = judged(&repo);
    assert_eq!(repo.argv("ruff")[..3], ["check", "--config", rules.to_str().unwrap()]);
    assert_eq!(repo.argv("mypy")[..2], ["--config-file", rules.to_str().unwrap()]);
    let intro = format!(", by the rules from `{}`, with the settings from `{}`.", rules.display(), settings.display());
    assert!(file.contains(&intro), "{file}");
}

#[test]
fn pymarkdown_runs_in_a_directory_of_its_own_on_absolute_paths() {
    // It would read the tree's pyproject.toml from its current directory beside the rules it is given.
    let repo = repo();
    repo.put("README.md", b"# Title\n");
    repo.git(&["add", "README.md"]);
    repo.git(&["commit", "--quiet", "-m", "readme"]);
    repo.put("README.md", b"# Title\n\nnew\n");
    let readme = repo.repo.join("README.md");
    let readme = readme.to_str().unwrap();
    repo.says("pymarkdown", format!("{readme}:3:1: MD047: Each file should end with a single newline\n").as_bytes());
    let path = repo.stub_linters();
    let cwd = repo.outside.join("pymarkdown.cwd");
    let stubs = repo.outside.join("stubs/pymarkdown");
    let script = format!(
        "pwd > '{}'\nprintf '%s\\n' \"$@\" > '{stubs}.argv'\ncat '{stubs}.out'\nexit 1",
        cwd.display(),
        stubs = stubs.display()
    );
    repo.stub("pymarkdown", &script);
    let done = repo.gate_command("HEAD").env("PATH", path).output().unwrap();
    let file = String::from_utf8(done.stdout).unwrap();
    assert_eq!(done.status.code(), Some(1), "{file}");
    assert!(file.contains("1. `README.md:3` error pymarkdown MD047: Each file should end"), "{file}");
    let rules = repo.repo.join("pyproject.toml");
    assert_eq!(repo.argv("pymarkdown"), ["--config", rules.to_str().unwrap(), "--strict-config", "scan", readme]);
    let ran_in = std::fs::read_to_string(cwd).unwrap();
    let ran_in = std::path::Path::new(ran_in.trim_end());
    assert!(!ran_in.starts_with(&repo.repo) && !ran_in.exists(), "{}", ran_in.display());
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
