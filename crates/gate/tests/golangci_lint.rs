//! The `gate` binary with the real golangci-lint, over a Go module of no dependencies in a subdirectory of the
//! repository, with golangci-lint's default linters.
//!
//! No test here has golangci-lint outlive its timeout: that is a constant of ten minutes, and process.rs's own tests
//! hold that a process past its timeout is killed with what it started.

use std::path::Path;

use common::Repo;

mod common;

const GO_MOD: &str = "module example.com/calc\n\ngo 1.22\n";
const CALC: &str = "package calc\n\nimport \"os\"\n\n// Add adds.\nfunc Add(a, b int) int {\n\treturn a + b\n}\n";
/// A function whose error return no one checks, errcheck's finding at its line 3.
const CLEAN: &str = "\n// Clean removes x.\nfunc Clean() {\n\tos.Remove(\"x\")\n}\n";

fn module_repo() -> Repo {
    Repo::with(&[
        ("pyproject.toml", b"[tool.ruff]\n"),
        ("calc/go.mod", GO_MOD.as_bytes()),
        ("calc/calc.go", format!("{CALC}\nvar _ = os.Getpid\n").as_bytes()),
    ])
}

/// The exit status and standard output of `gate --base HEAD` in `repo`, with `bin` ahead of PATH when given.
fn gate(repo: &Repo, bin: Option<&Path>) -> (i32, String) {
    let mut command = repo.gate_command("HEAD");
    if let Some(bin) = bin {
        command.env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()));
    }
    let done = command.output().unwrap();
    (done.status.code().unwrap(), String::from_utf8(done.stdout).unwrap())
}

#[test]
fn a_finding_on_an_added_line_is_an_issue() {
    let repo = module_repo();
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- 1 findings on lines the change added; each is listed below.\n"), "{out}");
    assert!(out.contains("1. `calc/calc.go:12` error golangci-lint errcheck: Error return value of `os.Remove` is not \
                          checked\n"), "{out}");
}

#[test]
fn a_finding_on_an_old_line_counts_as_elsewhere() {
    let repo = module_repo();
    repo.commit(&[("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes())], "a finding");
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}\n// Two is 2.\nfunc Two() int {{\n\treturn 2\n}}\n").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 0, "{out}");
    assert!(out.contains("1 other open issues in the project"), "{out}");
}

#[test]
fn code_that_does_not_compile_fails_the_gate_naming_the_type_checker_s_first_error() {
    let repo = module_repo();
    let two = "\n// Two is 2.\nfunc Two() int {\n\treturn Add(1, 1)\n}\n";
    repo.commit(&[("calc/calc.go", format!("{CALC}{two}").as_bytes())], "a caller");
    repo.put("calc/calc.go", format!("package calc\n{two}").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- `golangci-lint` could not compile the module of `calc/go.mod`: `calc/calc.go:1` \
                          golangci-lint typecheck: ./calc.go:5:9: undefined: Add\n"), "{out}");
    assert!(out.contains("## Issues on changed lines\n\nNone.\n"), "{out}");
}

#[test]
fn a_module_under_testdata_is_not_linted() {
    let repo = module_repo();
    repo.commit(&[
        ("calc/testdata/broken/go.mod", b"module example.com/broken\n\ngo 1.22\n"),
        ("calc/testdata/broken/b.go", b"package broken\n\nfunc F() int {\n\treturn nope\n}\n"),
    ], "a test's input");
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- 1 findings on lines the change added; each is listed below.\n"), "{out}");
    assert!(!out.contains("testdata"), "{out}");
}

#[test]
fn a_configuration_golangci_lint_cannot_read_fails_naming_it() {
    let repo = module_repo();
    repo.put("calc/.golangci.yml", b"linters: [\n");
    repo.git(&["add", "calc/.golangci.yml"]);
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- `golangci-lint` failed with exit status 3 in `calc/go.mod`: "), "{out}");
    assert!(out.contains("can't load config"), "{out}");
}

#[test]
fn a_repository_without_go_files_runs_no_golangci_lint() {
    let repo = module_repo();
    repo.git(&["rm", "--quiet", "calc/calc.go"]);
    let ran = repo.outside.join("golangci-lint-ran");
    let bin = repo.stub("golangci-lint", &format!("echo ran > '{}'; exit 3", ran.display()));
    let (exit_code, out) = gate(&repo, Some(&bin));
    assert_eq!(exit_code, 0, "{out}");
    assert!(!ran.exists());
}

#[test]
fn a_golangci_lint_that_prints_no_report_fails_naming_what_it_said() {
    let repo = module_repo();
    let bin = repo.stub("golangci-lint", "echo 'level=error msg=\"Running error: no go files\"' >&2; exit 1");
    let (exit_code, out) = gate(&repo, Some(&bin));
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- `golangci-lint` failed with exit status 1 in `calc/go.mod`: level=error \
                          msg=\"Running error: no go files\"\n"), "{out}");
}
