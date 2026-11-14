//! The flaky-tests check over a real git repository's Go module, its changed tests rerun with the real `go test
//! -shuffle`, through the `gate` binary and its quality.json. golangci-lint is off, which would build the module once
//! more.

use common::Repo;

mod common;

const CALC: &str = "package calc\n\n// Add adds.\nfunc Add(a, b int) int {\n\treturn a + b\n}\n";

/// TestUnset, on line 11, passes only when it runs before TestSets: under `-shuffle` seeds 2 and 4, not 1, 3 and 5.
const ORDERED: &str = "package calc\n\nimport \"testing\"\n\nvar set bool\n\nfunc TestSets(t *testing.T) {\n\
                       \tset = true\n}\n\nfunc TestUnset(t *testing.T) {\n\tif set {\n\
                       \t\tt.Fatal(\"set by another test\")\n\t}\n}\n";

/// A repository whose base holds the module example.com/calc at the top and its package calc in sub/.
fn repo() -> Repo {
    Repo::with(&[
        ("gate.toml", b"[linters]\ngolangci-lint = []\n"),
        ("go.mod", b"module example.com/calc\n\ngo 1.22\n"),
        ("sub/calc.go", CALC.as_bytes()),
    ])
}

/// A finding of the check: its path, line, own rule without the check's name, and message.
type Found = (Option<String>, Option<u32>, String, String);

/// The check's findings on the change from HEAD to the working tree of `repo`, every file in it tracked.
fn findings(repo: &Repo) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let done = repo.gate_command("HEAD").arg("--json").arg(&json).output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        let Some(rule) = f["rule"].as_str().unwrap().strip_prefix("flaky-tests ") else { continue };
        let path = f["path"].as_str().map(String::from);
        let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
        out.push((path, line, rule.to_string(), f["message"].as_str().unwrap().to_string()));
    }
    out
}

#[test]
fn a_go_test_that_fails_in_one_order_is_reported_with_the_seeds_of_each_outcome() {
    let repo = repo();
    repo.put("sub/state_test.go", ORDERED.as_bytes());
    let found = findings(&repo);
    let expected = "`TestUnset` failed under seeds 1, 3 and 5 and passed under seeds 2 and 4: it depends on the order \
                    the tests run in, or on state or randomness that one run does not share with another.";
    assert_eq!(
        found,
        [(Some("sub/state_test.go".into()), Some(11), "mixed-results".into(), expected.into())],
        "{found:?}"
    );
}

#[test]
fn a_go_test_that_passes_in_every_order_is_not_reported() {
    let repo = repo();
    let stable = "package calc\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\tif Add(1, 2) != 3 {\n\
                  \t\tt.Fatal(\"1 + 2 is not 3\")\n\t}\n}\n\nfunc TestAddZero(t *testing.T) {\n\
                  \tif Add(0, 0) != 0 {\n\t\tt.Fatal(\"0 + 0 is not 0\")\n\t}\n}\n";
    repo.put("sub/calc_test.go", stable.as_bytes());
    assert_eq!(findings(&repo), []);
}

/// A stub go ahead of the real one makes a run with `-json` take longer than a rerun may, unless a `-c` build came
/// first: a package whose build alone outlasts a rerun's time.
#[test]
fn the_reruns_time_leaves_out_building_the_package() {
    let repo = repo();
    let stable = "package calc\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\tif Add(1, 2) != 3 {\n\
                  \t\tt.Fatal(\"1 + 2 is not 3\")\n\t}\n}\n";
    repo.put("sub/calc_test.go", stable.as_bytes());
    let which = std::process::Command::new("sh").args(["-c", "command -v go"]).output().unwrap();
    let real = String::from_utf8(which.stdout).unwrap();
    let built = repo.outside.join("built");
    let script = format!(
        "case \" $* \" in\n*\" -c \"*) : > '{0}' ;;\n*\" -json \"*) [ -f '{0}' ] || sleep 35 ;;\nesac\n\
         exec '{1}' \"$@\"",
        built.display(),
        real.trim()
    );
    let bin = repo.stub("go", &script);
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let done = repo.gate_command("HEAD").env("PATH", path).arg("--json").arg(&json).output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let found: Vec<&serde_json::Value> = report["findings"].as_array().unwrap().iter()
        .filter(|f| f["rule"].as_str().unwrap().starts_with("flaky-tests ")).collect();
    assert_eq!(found, Vec::<&serde_json::Value>::new());
}

#[test]
fn go_tests_that_do_not_build_are_said_not_to_be_rerun() {
    let repo = repo();
    let unbuilt = "package calc\n\nimport \"testing\"\n\nfunc TestSub(t *testing.T) {\n\tSub(1, 2)\n}\n";
    repo.put("sub/calc_test.go", unbuilt.as_bytes());
    let found = findings(&repo);
    let expected = "go test, rerunning them under seed 1, could not build the package's tests in the copy of the \
                    tracked files; the changed tests of the package at `sub` were not rerun.";
    assert_eq!(found, [(None, None, "not-rerun".into(), expected.into())], "{found:?}");
}
