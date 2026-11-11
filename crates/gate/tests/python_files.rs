//! How the checks that rerun, mutate and score read the Python files a change touches, end to end through the `gate`
//! binary over a real git repository, with the workspace's own pytest and mutmut (common::python).

use std::process::Command;

use common::Repo;

mod common;

/// `d` is a decorator that takes an argument, for the decorators below.
const CALC: &str = "def d(_):\n    return lambda f: f\n\n\ndef add(a, b):\n    return a + b\n";

/// A repository whose base holds [`CALC`] and a test of it, with rules that have mutmut mutate everything, and that
/// runs neither ruff nor mypy.
fn calc() -> Repo {
    Repo::with(&[
        common::NO_PYTHON_LINTERS,
        ("pyproject.toml", b"[tool.mutmut]\nsource_paths = [\".\"]\n"),
        ("calc.py", CALC.as_bytes()),
        ("tests/test_calc.py", b"import calc\n\n\ndef test_add():\n    assert calc.add(2, 3) == 5\n"),
    ])
}

/// A finding: its path, line, rule and message.
type Found = (String, Option<u32>, String, String);

/// The findings of `checks` on the change from HEAD to the working tree of `repo`, every file in it tracked, in the
/// report's order. The gate runs with at most `seconds` and 4 GiB of address space, so that a check that blocks or
/// reads without end, as on a named pipe or /dev/zero, fails the test rather than hanging the suite or taking the
/// host's memory.
fn findings(repo: &Repo, checks: &[&str], seconds: u32) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let mut command = Command::new("sh");
    command.args(["-c", "ulimit -v 4194304 && exec timeout \"$@\"", "sh", &seconds.to_string()]);
    command.arg(env!("CARGO_BIN_EXE_gate")).current_dir(&repo.repo).env_remove("NEXTEST_PROFILE");
    command.args(["--base", "HEAD", "--mutation-budget", "60", "--python"]).arg(common::python());
    let done = command.arg("--json").arg(&json).output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{done:?}");
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        if checks.contains(&f["check"].as_str().unwrap()) {
            let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
            let text = |key: &str| f[key].as_str().unwrap().to_string();
            out.push((text("path"), line, text("rule"), text("message")));
        }
    }
    out
}

/// The path, line and rule of each of `found`.
fn places(found: &[Found]) -> Vec<(&str, Option<u32>, &str)> {
    found.iter().map(|(path, line, rule, _)| (path.as_str(), *line, rule.as_str())).collect()
}

#[test]
fn a_changed_symlink_to_a_fifo_or_dev_zero_is_not_read_by_the_checks_that_rerun_or_mutate() {
    let repo = calc();
    let fifo = repo.outside.join("pipe");
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    for target in [fifo.as_path(), "/dev/zero".as_ref()] {
        for name in ["x.py", "tests/test_x.py"] {
            let link = repo.repo.join(name);
            if link.is_symlink() {
                std::fs::remove_file(&link).unwrap();
            }
            std::os::unix::fs::symlink(target, link).unwrap();
        }
        assert_eq!(findings(&repo, &["flaky-tests", "surviving-mutants"], 60), [], "{target:?}");
    }
}

#[test]
fn a_coding_cookie_naming_an_unknown_codec_is_the_same_reason_in_every_check() {
    let repo = calc();
    let source = b"# -*- coding: nonsense -*-\ndef test_x():\n    pass\n";
    repo.put("x.py", source);
    repo.put("tests/test_x.py", source);
    let mut found = findings(&repo, &["flaky-tests", "surviving-mutants", "cognitive-complexity"], 120);
    found.sort();
    assert_eq!(
        places(&found),
        [
            ("tests/test_x.py", None, "flaky-tests not-rerun"),
            ("x.py", None, "cognitive-complexity not-scored"),
            ("x.py", None, "surviving-mutants not-run"),
        ]
    );
    let reasons: Vec<&str> = found.iter().map(|(.., message)| message.split_once(", so ").unwrap().0).collect();
    assert!(reasons[0].starts_with("could not be read ("), "{reasons:?}");
    assert!(reasons.iter().all(|reason| *reason == reasons[0]), "{reasons:?}");
}

#[test]
fn a_python_script_with_no_py_is_read_as_python_and_said_not_mutated() {
    let repo = calc();
    repo.put("bin/tool", b"#!/usr/bin/env python3\ndef load(path):\n    data = path\n    return data\n");
    let found = findings(&repo, &["surviving-mutants", "vague-names"], 120);
    assert_eq!(
        places(&found),
        [("bin/tool", Some(2), "surviving-mutants not-mutated"), ("bin/tool", Some(3), "vague-names vague-name")]
    );
    assert!(found[0].3.contains("mutmut mutates only files named *.py"), "{}", found[0].3);
}

/// Decorators that Python's `ast.unparse` cannot write: a sum too deep for its recursion, and an int, as
/// hexadecimal, of more than `sys.get_int_max_str_digits()` decimal digits, which it writes in decimal.
fn deep_decorators() -> [String; 2] {
    [format!("@d({})", vec!["1"; 5000].join(" + ")), format!("@d(0x{})", "f".repeat(4200))]
}

#[test]
fn a_changed_test_with_a_decorator_too_deep_or_too_long_to_write_is_still_rerun() {
    let repo = calc();
    for decorator in deep_decorators() {
        let tests = format!(
            "import calc\n\n\ndef d(_):\n    return lambda f: f\n\n\n{decorator}\ndef test_add():\n    \
             assert calc.add(2, 3) == 6\n"
        );
        repo.put("tests/test_calc.py", tests.as_bytes());
        let found = findings(&repo, &["flaky-tests"], 300);
        assert_eq!(places(&found), [("tests/test_calc.py", Some(9), "flaky-tests failed-every-run")]);
    }
}

#[test]
fn a_changed_function_with_a_decorator_too_deep_or_too_long_to_write_is_not_mutated() {
    let repo = calc();
    for decorator in deep_decorators() {
        let add = format!("{decorator}\ndef add(a, b):\n    return b + a");
        let changed = CALC.replace("def add(a, b):\n    return a + b", &add);
        repo.put("calc.py", changed.as_bytes());
        let found = findings(&repo, &["surviving-mutants"], 300);
        assert_eq!(places(&found), [("calc.py", Some(6), "surviving-mutants not-mutated")]);
        assert!(found[0].3.contains("mutmut does not mutate a decorated function"), "{}", found[0].3);
    }
}
