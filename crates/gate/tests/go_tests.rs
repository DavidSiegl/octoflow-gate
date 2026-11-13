//! A Go module's tests run by a gate.toml `[[test]]` command through the real gotestsum, whose JUnit report gives the
//! failing tests and whose `go test -coverprofile` the coverage of new code, through the `gate` binary.

use common::Repo;

mod common;

/// The command a Go module at the top runs its tests with, writing junit.xml and cover.out at the top.
const GATE_TOML: &str = "[linters]\ngolangci-lint = []\n\n[[test]]\nname = \"go\"\n\
                         command = [\"gotestsum\", \"--junitfile\", \"junit.xml\", \"--\", \
                         \"-coverprofile=cover.out\", \"./...\"]\njunit = [\"junit.xml\"]\n\
                         coverage = [\"cover.out\"]\n";

const CALC: &str = "package calc\n\n// Add adds.\nfunc Add(a, b int) int {\n\treturn a + b\n}\n";

/// A module of example.com/calc at the top, its package in sub/, with a test of Add.
fn module_repo() -> Repo {
    Repo::with(&[
        ("gate.toml", GATE_TOML.as_bytes()),
        (".gitignore", b"junit.xml\ncover.out\n"),
        ("go.mod", b"module example.com/calc\n\ngo 1.22\n"),
        ("sub/calc.go", CALC.as_bytes()),
        (
            "sub/calc_test.go",
            b"package calc\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\tif Add(1, 2) != 3 {\n\
              \t\tt.Fatal(\"1 + 2 is not 3\")\n\t}\n}\n",
        ),
    ])
}

fn gate(repo: &Repo) -> (i32, String) {
    let done = repo.gate_command("HEAD").output().unwrap();
    (done.status.code().unwrap(), String::from_utf8(done.stdout).unwrap())
}

#[test]
fn a_failing_go_test_fails_the_gate_and_the_profile_gives_the_coverage_of_new_code() {
    let repo = module_repo();
    // Sub is run by no test; Big by its test, which fails.
    let added = "\n// Sub subtracts.\nfunc Sub(a, b int) int {\n\treturn a - b\n}\n\n\
                 // Big says whether a is big.\nfunc Big(a int) bool {\n\treturn a > 3\n}\n";
    repo.put("sub/calc.go", format!("{CALC}{added}").as_bytes());
    repo.put("sub/big_test.go", b"package calc\n\nimport \"testing\"\n\nfunc TestBig(t *testing.T) {\n\
                                  \tif !Big(2) {\n\t\tt.Errorf(\"2 is not big\")\n\t}\n}\n");
    repo.git(&["add", "-A"]);
    let (exit_code, out) = gate(&repo);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- 1 tests fail; each is listed below.\n"), "{out}");
    assert!(out.contains("## Failing tests\n\n- `example.com/calc/sub.TestBig`: big_test.go:7: 2 is not big\n"), "{out}");
    let coverage = "## Coverage on new code\n\n50.0% of 2 new lines to cover; 1 lines are not covered.\n";
    assert!(out.contains(coverage), "{out}");
}

#[test]
fn a_package_that_does_not_compile_is_a_failing_test() {
    let repo = module_repo();
    repo.put("sub/calc.go", format!("{CALC}\nfunc Broken() int {{\n\treturn nope\n}}\n").as_bytes());
    let (exit_code, out) = gate(&repo);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("## Failing tests\n\n- `TestMain`: FAIL\texample.com/calc/sub [build failed]\n"), "{out}");
}
