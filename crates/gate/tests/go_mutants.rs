//! The surviving-mutants check over a real git repository's Go module, through the `gate` binary and its
//! quality.json: with a real gremlins, or a stub `gremlins` ahead of PATH where a test needs it to do what the real one
//! cannot be made to. golangci-lint is off, which would build the module once more.

use std::ffi::OsString;

use common::Repo;

mod common;

const GO_MOD: &str = "module example.com/calc\n\ngo 1.22\n";
const CALC: &str = "package calc\n\n// Add adds.\nfunc Add(a, b int) int {\n\treturn a + b\n}\n";
const CALC_TEST: &str = "package calc\n\nimport \"testing\"\n\nfunc TestAdd(t *testing.T) {\n\
                         \tif Add(1, 2) != 3 {\n\t\tt.Fatal(\"1 + 2 is not 3\")\n\t}\n}\n";

/// Checked's test notices each of its mutants; Unchecked's calls it and checks nothing; no test calls Untested.
/// Their `func` lines are 9, 14 and 19 of calc.go with them.
const ADDED: &str = "\n// Checked doubles a.\nfunc Checked(a int) int {\n\treturn a * 2\n}\n\n\
                     // Unchecked says whether a is big.\nfunc Unchecked(a int) bool {\n\treturn a > 3\n}\n\n\
                     // Untested negates a.\nfunc Untested(a int) int {\n\treturn -a\n}\n";
const ADDED_TEST: &str = "package calc\n\nimport \"testing\"\n\nfunc TestChecked(t *testing.T) {\n\
                          \tif Checked(3) != 6 || Checked(0) != 0 {\n\t\tt.Fatal(\"wrong\")\n\t}\n}\n\n\
                          func TestUnchecked(t *testing.T) {\n\tUnchecked(1)\n}\n";

/// A repository whose base holds the module calc at `dir`, a directory with a slash or the top when empty.
fn repo(dir: &str) -> Repo {
    let files = [
        ("gate.toml".to_string(), "[linters]\ngolangci-lint = []\n".to_string()),
        (format!("{dir}go.mod"), GO_MOD.to_string()),
        (format!("{dir}calc.go"), CALC.to_string()),
        (format!("{dir}calc_test.go"), CALC_TEST.to_string()),
    ];
    let files: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (p.as_str(), t.as_bytes())).collect();
    Repo::with(&files)
}

/// A finding of the check: its path, line, own rule without the check's name, and message.
type Found = (Option<String>, Option<u32>, String, String);

/// The check's findings on the change from HEAD to the working tree of `repo`, every file in it tracked, with a
/// budget of `budget` seconds, the default when None, and `env` set for the gate.
fn findings(repo: &Repo, budget: Option<u64>, env: &[(&str, OsString)]) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let mut command = repo.gate_command("HEAD");
    command.arg("--json").arg(&json);
    if let Some(budget) = budget {
        command.args(["--mutation-budget", &budget.to_string()]);
    }
    command.envs(env.iter().map(|(k, v)| (k, v)));
    let done = command.output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        let Some(rule) = f["rule"].as_str().unwrap().strip_prefix("surviving-mutants ") else { continue };
        let path = f["path"].as_str().map(String::from);
        let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
        out.push((path, line, rule.to_string(), f["message"].as_str().unwrap().to_string()));
    }
    out
}

/// PATH with a `gremlins` that runs the sh `script` ahead of the rest.
fn gremlins_stub(repo: &Repo, script: &str) -> OsString {
    let bin = repo.stub("gremlins", script);
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()).into()
}

#[test]
fn a_function_whose_mutants_survive_is_found_and_one_no_test_runs_is_untested() {
    let repo = repo("");
    repo.put("calc.go", format!("{CALC}{ADDED}").as_bytes());
    repo.put("more_test.go", ADDED_TEST.as_bytes());
    let found = findings(&repo, None, &[]);
    let places: Vec<_> = found.iter().map(|(path, line, rule, _)| (path.as_deref(), *line, rule.as_str())).collect();
    assert_eq!(places, [(Some("calc.go"), Some(14), "survived"), (Some("calc.go"), Some(19), "untested")], "{found:?}");
    assert!(found[0].3.starts_with("2 of 2 mutants of `Unchecked` survived its tests: line 15, column 11: "),
            "{found:?}");
    assert_eq!(found[1].3, "2 mutants of `Untested` are on lines no test runs, so none of them can make one fail.");
}

#[test]
fn a_gremlins_configuration_in_the_module_changes_neither_what_it_prints_nor_its_exit() {
    let repo = repo("");
    // Silent, Killed lines alone, a threshold that Unchecked's survivors fail, which would exit 10, and a timeout so
    // short that every mutant would time out.
    let settings = "silent: true\nunleash:\n  output-statuses: k\n  timeout-coefficient: 1\n  threshold:\n    \
                    efficacy: 90\n";
    repo.commit(&[(".gremlins.yaml", settings.as_bytes())], "gremlins' settings");
    repo.put("calc.go", format!("{CALC}{ADDED}").as_bytes());
    repo.put("more_test.go", ADDED_TEST.as_bytes());
    let found = findings(&repo, None, &[]);
    let places: Vec<_> = found.iter().map(|(path, line, rule, _)| (path.as_deref(), *line, rule.as_str())).collect();
    assert_eq!(places, [(Some("calc.go"), Some(14), "survived"), (Some("calc.go"), Some(19), "untested")], "{found:?}");
}

#[test]
fn a_function_whose_edit_only_removes_lines_is_mutated_in_a_module_below_the_top() {
    let repo = repo("calc/");
    // Clamp's guard clause goes; what is left, a > 3, no test checks.
    let clamp = "\n// Clamp says whether a is big.\nfunc Clamp(a int) bool {\n\tif a < 0 {\n\t\treturn false\n\t}\n\
                 \treturn a > 3\n}\n";
    repo.commit(&[
        ("calc/calc.go", format!("{CALC}{clamp}").as_bytes()),
        ("calc/clamp_test.go", b"package calc\n\nimport \"testing\"\n\nfunc TestClamp(t *testing.T) {\n\
                                 \tClamp(1)\n}\n"),
    ], "a guard");
    let unguarded = clamp.replace("\tif a < 0 {\n\t\treturn false\n\t}\n", "");
    repo.put("calc/calc.go", format!("{CALC}{unguarded}").as_bytes());
    let found = findings(&repo, None, &[]);
    let places: Vec<_> = found.iter().map(|(path, line, rule, _)| (path.as_deref(), *line, rule.as_str())).collect();
    assert_eq!(places, [(Some("calc/calc.go"), Some(9), "survived")], "{found:?}");
}

#[test]
fn a_change_to_the_tests_alone_never_starts_gremlins() {
    let repo = repo("");
    repo.put("more_test.go", b"package calc\n\nimport \"testing\"\n\nfunc TestMore(t *testing.T) {\n\tAdd(1, 1)\n}\n");
    let ran = repo.outside.join("ran");
    let path = gremlins_stub(&repo, &format!("echo ran > '{}'; exit 1", ran.display()));
    assert_eq!(findings(&repo, None, &[("PATH", path)]), vec![]);
    assert!(!ran.exists());
}

#[test]
fn a_function_whose_edit_only_touches_its_comments_and_layout_is_not_mutated() {
    let repo = repo("");
    repo.put("calc.go", CALC.replace("\treturn a + b\n", "\t// The sum.\n\treturn a +\n\t\tb\n").as_bytes());
    let ran = repo.outside.join("ran");
    let path = gremlins_stub(&repo, &format!("echo ran > '{}'; exit 1", ran.display()));
    assert_eq!(findings(&repo, None, &[("PATH", path)]), vec![]);
    assert!(!ran.exists());
}

#[test]
fn a_function_whose_signature_alone_changes_is_mutated() {
    let repo = repo("");
    // Sub's parameters swap and its body stays; its test calls it and checks nothing.
    let sub = |params: &str| format!("{CALC}\n// Sub subtracts.\nfunc Sub({params} int) int {{\n\treturn a - b\n}}\n");
    repo.commit(&[
        ("calc.go", sub("a, b").as_bytes()),
        ("sub_test.go", b"package calc\n\nimport \"testing\"\n\nfunc TestSub(t *testing.T) {\n\tSub(3, 1)\n}\n"),
    ], "Sub");
    repo.put("calc.go", sub("b, a").as_bytes());
    let found = findings(&repo, None, &[]);
    let places: Vec<_> = found.iter().map(|(path, line, rule, _)| (path.as_deref(), *line, rule.as_str())).collect();
    assert_eq!(places, [(Some("calc.go"), Some(9), "survived")], "{found:?}");
}

#[test]
fn a_module_whose_tests_fail_is_not_run() {
    let repo = repo("");
    repo.put("calc.go", format!("{CALC}{ADDED}").as_bytes());
    let failing = b"package calc\n\nimport \"testing\"\n\nfunc TestNo(t *testing.T) {\n\tt.Fatal(\"no\")\n}\n";
    repo.put("more_test.go", failing);
    let found = findings(&repo, None, &[]);
    assert_eq!(found.len(), 1, "{found:?}");
    let (path, line, rule, message) = &found[0];
    assert_eq!((path.as_deref(), *line, rule.as_str()), (None, None, "not-run"), "{message}");
    assert!(message.starts_with("gremlins failed in the module at `.`, exiting with status 1: failed to gather \
                                 coverage"), "{message}");
    assert!(message.ends_with("; the change's Go was not mutation-tested."), "{message}");
}

#[test]
fn gremlins_runs_in_the_copy_on_the_gate_s_diff_and_is_stopped_with_sigint() {
    let repo = repo("calc/");
    repo.put("calc/calc.go", format!("{CALC}{ADDED}").as_bytes());
    let record = repo.outside.join("record");
    // The diff is what git gives when asked as gremlins asks; a SIGINT is recorded, and ends the stub.
    let script = format!(
        "{{ pwd; printf '%s\\n' \"$@\"; echo \"color=$NO_COLOR token=${{JENKINS_TOKEN-unset}} goflags=$GOFLAGS\"; \
         git diff --merge-base \"${{12}}\"; git --version; }} > '{0}'\n\
         echo '       LIVED CONDITIONALS_BOUNDARY at calc.go:15:11'\n\
         trap 'echo interrupted >> \"{0}\"; exit 0' INT\nsleep 30 & wait",
        record.display()
    );
    let env = [("PATH", gremlins_stub(&repo, &script)), ("JENKINS_TOKEN", "secret".into())];
    // 15 of the 18 seconds are the stop's.
    let found = findings(&repo, Some(18), &env);
    let rules: Vec<&str> = found.iter().map(|f| f.2.as_str()).collect();
    assert_eq!(rules, ["budget-reached", "survived"], "{found:?}");
    assert_eq!(found[0].3, "budget reached after 1 of the mutants gremlins made of the module at `calc`: it was \
                            stopped at 18 seconds (`--mutation-budget`), so the mutants it did not try may survive \
                            too.");
    assert_eq!(found[1].3, "1 of 1 mutants of `Unchecked` survived its tests: line 15, column 11: \
                            CONDITIONALS_BOUNDARY.");
    let recorded = std::fs::read_to_string(&record).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
    assert!(std::path::Path::new(lines[0]).ends_with("tree/calc"), "{recorded}");
    let base = repo.git(&["rev-parse", "HEAD"]);
    let argv = [
        "unleash", "--silent=false", "--output-statuses", "lctkvsr", "--threshold-efficacy", "0", "--threshold-mcover",
        "0", "--timeout-coefficient", "10", "--diff", base.trim(),
    ];
    assert_eq!(lines[1..13], argv);
    assert!(lines[13].starts_with("color=1 token=unset goflags=") && lines[13].ends_with(" -count=1"), "{recorded}");
    let diff = "diff --git a/calc.go b/calc.go\n--- a/calc.go\n+++ b/calc.go\n@@ -0,0 +9,3 @@\n+\n+\n+\n\
                @@ -0,0 +14,3 @@\n+\n+\n+\n@@ -0,0 +19,3 @@\n+\n+\n+\n";
    assert_eq!(lines[14..29].join("\n") + "\n", diff);
    assert!(lines[29].starts_with("git version "), "{recorded}");
    assert_eq!(lines.last(), Some(&"interrupted"));
}

#[test]
fn each_module_runs_in_turn_until_the_budget_runs_out() {
    let repo = repo("a/");
    repo.commit(&[("b/go.mod", b"module example.com/b\n\ngo 1.22\n"), ("b/calc.go", CALC.as_bytes())], "b");
    for dir in ["a/", "b/"] {
        repo.put(format!("{dir}calc.go"), format!("{CALC}{ADDED}").as_bytes());
    }
    assert_eq!(findings(&repo, Some(1), &[]), vec![
        (
            None,
            None,
            "budget-reached".into(),
            "budget reached before gremlins started on the module at `a`: the 1 seconds of `--mutation-budget` left \
             too little to run and stop it, so its mutants were not tried."
                .into(),
        ),
        (
            None,
            None,
            "not-run".into(),
            "the budget ran out before gremlins reached the module at `b`; the change's Go was not mutation-tested."
                .into(),
        ),
    ]);
}

#[test]
fn a_gremlins_that_is_missing_is_not_run() {
    let repo = repo("");
    repo.put("calc.go", format!("{CALC}{ADDED}").as_bytes());
    // PATH holds git and nothing else.
    let found = std::process::Command::new("sh").args(["-c", "command -v git"]).output().unwrap();
    let git = String::from_utf8(found.stdout).unwrap();
    let bin = repo.outside.join("git-only");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(git.trim(), bin.join("git")).unwrap();
    let found = findings(&repo, None, &[("PATH", bin.into_os_string())]);
    assert_eq!(found, vec![(
        None,
        None,
        "not-run".into(),
        "could not start gremlins: No such file or directory (os error 2); the change's Go was not mutation-tested."
            .into(),
    )]);
}
