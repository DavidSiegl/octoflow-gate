//! The passes-at-base check over a real git repository's Go module, its changed tests run with the real go test in a
//! copy of the base, through the `gate` binary and its quality.json. golangci-lint is off, which would build the module
//! once more.

use common::Repo;

mod common;

/// Abs, wrong at the base for a negative number.
const ABS: &str = "package calc\n\n// Abs is a's absolute value.\nfunc Abs(a int) int {\n\treturn a\n}\n";

/// Abs, right.
const FIXED: &str = "package calc\n\n// Abs is a's absolute value.\nfunc Abs(a int) int {\n\tif a < 0 {\n\
                     \t\treturn -a\n\t}\n\treturn a\n}\n";

/// A test of Abs that passes at the base, its func on line 5, and one that fails there, on line 11.
const ABS_TESTS: &str = "package calc\n\nimport \"testing\"\n\nfunc TestAbsPositive(t *testing.T) {\n\
                         \tif Abs(3) != 3 {\n\t\tt.Fatal(\"wrong\")\n\t}\n}\n\n\
                         func TestAbsNegative(t *testing.T) {\n\tif Abs(-3) != 3 {\n\t\tt.Fatal(\"wrong\")\n\t}\n}\n";

/// A repository whose base holds the module example.com/calc at the top, its package calc, with [`ABS`], in `dir`, a
/// directory with a slash or the top when empty.
fn repo(dir: &str) -> Repo {
    let files = [
        ("gate.toml".to_string(), "[linters]\ngolangci-lint = []\n".to_string()),
        ("go.mod".to_string(), "module example.com/calc\n\ngo 1.22\n".to_string()),
        (format!("{dir}calc.go"), ABS.to_string()),
    ];
    let files: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (p.as_str(), t.as_bytes())).collect();
    Repo::with(&files)
}

/// A finding of the check: its path, line, own rule without the check's name, and message.
type Found = (Option<String>, Option<u32>, String, String);

/// The check's findings on the change from HEAD to the working tree of `repo`, every file in it tracked, with
/// `--base-run-budget` `budget` seconds, the default when None.
fn findings_within(repo: &Repo, budget: Option<u64>) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let mut command = repo.gate_command("HEAD");
    command.arg("--json").arg(&json);
    if let Some(budget) = budget {
        command.args(["--base-run-budget", &budget.to_string()]);
    }
    let done = command.output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        let Some(rule) = f["rule"].as_str().unwrap().strip_prefix("passes-at-base ") else { continue };
        let path = f["path"].as_str().map(String::from);
        let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
        out.push((path, line, rule.to_string(), f["message"].as_str().unwrap().to_string()));
    }
    out
}

/// The check's findings on the change from HEAD to the working tree of `repo` with the default budget.
fn findings(repo: &Repo) -> Vec<Found> {
    findings_within(repo, None)
}

#[test]
fn a_go_test_that_passes_at_the_base_is_found_and_one_that_needs_the_change_is_not() {
    let repo = repo("sub/");
    repo.put("sub/calc.go", FIXED.as_bytes());
    repo.put("sub/calc_test.go", ABS_TESTS.as_bytes());
    let found = findings(&repo);
    let message = "`TestAbsPositive` passes without the change, so it does not test it.";
    assert_eq!(
        found,
        [(Some("sub/calc_test.go".into()), Some(5), "passes-at-base".into(), message.into())],
        "{found:?}"
    );
}

#[test]
fn go_tests_that_do_not_build_at_the_base_need_the_change() {
    let repo = repo("");
    let double = "\n// Double is twice a.\nfunc Double(a int) int {\n\treturn 2 * a\n}\n";
    repo.put("calc.go", format!("{ABS}{double}").as_bytes());
    let tests = "package calc\n\nimport \"testing\"\n\nfunc TestDouble(t *testing.T) {\n\tif Double(2) != 4 {\n\
                 \t\tt.Fatal(\"wrong\")\n\t}\n}\n\nfunc TestAbs(t *testing.T) {\n\tif Abs(3) != 3 {\n\
                 \t\tt.Fatal(\"wrong\")\n\t}\n}\n";
    repo.put("calc_test.go", tests.as_bytes());
    assert_eq!(findings(&repo), []);
}

/// go before 1.24 marks a package whose tests do not build only in the package's output, with no `FailedBuild`.
#[test]
fn go_tests_that_do_not_build_at_the_base_under_go_1_23_need_the_change() {
    let repo = repo("");
    repo.put("calc.go", FIXED.as_bytes());
    repo.put("calc_test.go", ABS_TESTS.as_bytes());
    let go_1_23 = r#"printf '%s\n' '{"Action":"start","Package":"example.com/calc"}' \
        '{"Action":"output","Package":"example.com/calc","Output":"FAIL\texample.com/calc [build failed]\n"}' \
        '{"Action":"fail","Package":"example.com/calc","Elapsed":0}'
echo '# example.com/calc [example.com/calc.test]' >&2
exit 1"#;
    let bin = repo.stub("go", go_1_23);
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    let done = repo.gate_command("HEAD").env("PATH", path).arg("--json").arg(&json).output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let found: Vec<&serde_json::Value> = report["findings"].as_array().unwrap().iter()
        .filter(|f| f["rule"].as_str().unwrap().starts_with("passes-at-base ")).collect();
    assert_eq!(found, Vec::<&serde_json::Value>::new());
}

#[test]
fn a_change_to_go_tests_alone_runs_nothing_at_the_base() {
    let repo = repo("");
    repo.put("calc_test.go", ABS_TESTS.as_bytes());
    assert_eq!(findings(&repo), []);
}

/// The test sleeps at the base alone, where Abs(-1) is -1, so that flaky-tests' reruns of it in the working tree are
/// quick.
#[test]
fn a_go_test_run_the_budget_stops_is_a_finding_and_judges_nothing() {
    let repo = repo("");
    repo.put("calc.go", FIXED.as_bytes());
    let slow = "package calc\n\nimport (\n\t\"testing\"\n\t\"time\"\n)\n\nfunc TestAbsSlowly(t *testing.T) {\n\
                \tif Abs(-1) == -1 {\n\t\ttime.Sleep(30 * time.Second)\n\t}\n}\n";
    repo.put("calc_test.go", slow.as_bytes());
    let found = findings_within(&repo, Some(2));
    let message = "go test was stopped at 2 seconds (`--base-run-budget`) while it ran the changed tests of the \
                   package at the top level, so none of them was judged.";
    assert_eq!(found, [(None, None, "budget-reached".into(), message.into())], "{found:?}");
}
