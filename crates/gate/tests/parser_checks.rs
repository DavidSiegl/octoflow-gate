//! The parser checks run on a real git repository's change (gate::parser_checks::run_checks): what the gate decides
//! over the whole change before a check judges it, which crates/lang-rust's tests of the checks take as given:
//! whether the change edits Rust code under test, by the Cargo packages and settings of the tree, and which golden
//! files go with it. TestRustWeakenedTests in tests/test_gate.py, for those cases.

use gate::judge::{judge_change, Change, Given};
use gate::parser_checks::run_checks;
use gate::settings::Settings;

use common::Repo;

mod common;

const LIB: &str = "\
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_small() {
        assert_eq!(add(1, 2), 3);
        assert_eq!(add(0, 0), 0);
    }

    #[test]
    fn adds_large() {
        assert_eq!(add(10, 20), 30);
    }
}
";

const IT: &str = "\
use tiny::add;

#[test]
fn negative() {
    assert_eq!(add(-1, -1), -2);
}

#[tokio::test]
async fn zero() {
    assert!(add(0, 0) == 0);
}
";

const MANIFEST: &str = "[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n";

/// A Cargo package under crate/, with unit tests in lib.rs's inline module and integration tests in tests/it.rs,
/// and `more`, (path, contents), committed with it as the base.
fn package(more: &[(&str, &str)]) -> Repo {
    let mut files = vec![("crate/Cargo.toml", MANIFEST), ("crate/src/lib.rs", LIB), ("crate/tests/it.rs", IT)];
    files.extend(more);
    let files: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (*p, t.as_bytes())).collect();
    Repo::with(&files)
}

/// Replaces the one `old` in the working tree's `path` with `new`.
fn edit(repo: &Repo, path: &str, old: &str, new: &str) {
    let text = std::fs::read_to_string(repo.repo.join(path)).unwrap();
    assert_eq!(text.matches(old).count(), 1, "{old:?} in {path}");
    repo.put(path, text.replacen(old, new, 1).as_bytes());
}

/// A finding as these tests compare it: its path, line, rule and severity.
type Found = (Option<String>, Option<u32>, String, String);

/// Each finding of the parser checks on the change from HEAD to the working tree, every file in it tracked, by the
/// default settings, in the order run_checks gives them.
fn found(repo: &Repo) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let change = Change::from_tree(&repo.repo, "HEAD", Settings::default()).unwrap();
    let by_check = run_checks(&change).unwrap();
    let mut out = vec![];
    for f in by_check.into_iter().flatten() {
        let path = f.path.as_ref().map(|p| p.to_string_lossy().into_owned());
        out.push((path, f.line, f.rule.clone(), f.severity.clone()));
    }
    out
}

/// The changed expected value of tests/it.rs's `negative`, at its line 5, as the check finds it beside a change to
/// the code under test.
fn negative_changed() -> Vec<Found> {
    vec![(
        Some("crate/tests/it.rs".into()),
        Some(5),
        "test-integrity expected-value-changed".into(),
        "warning".into(),
    )]
}

/// Changes the expected value of tests/it.rs's `negative`.
fn change_negative(repo: &Repo) {
    edit(repo, "crate/tests/it.rs", "assert_eq!(add(-1, -1), -2)", "assert_eq!(add(-1, -1), -3)");
}

#[test]
fn an_expected_value_changed_with_the_code_is_found_and_without_it_is_not() {
    let repo = package(&[]);
    change_negative(&repo);
    assert_eq!(found(&repo), vec![]);
    edit(&repo, "crate/src/lib.rs", "    a + b\n", "    a + b + 1\n");
    assert_eq!(found(&repo), negative_changed());
}

#[test]
fn an_expected_value_changed_with_only_the_inline_tests_is_not_found() {
    let repo = package(&[]);
    edit(&repo, "crate/src/lib.rs", "assert_eq!(add(10, 20), 30)", "assert_eq!(add(10, 20), 31)");
    edit(&repo, "crate/src/lib.rs", "mod tests {\n", "mod tests {\n    // the cases\n");
    edit(&repo, "crate/src/lib.rs", "    use super::*;\n", "");
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn an_expected_value_changed_with_rust_code_added_elsewhere_is_found() {
    for added in ["build.rs", "lib.rs"] {
        let repo = package(&[]);
        match added {
            "build.rs" => repo.put("crate/build.rs", b"fn main() {}\n"),
            _ => edit(&repo, "crate/src/lib.rs", "    }\n}\n", "    }\n}\n\npub fn three() -> i32 {\n    3\n}\n"),
        }
        change_negative(&repo);
        assert_eq!(found(&repo), negative_changed(), "{added}");
    }
}

#[test]
fn an_expected_value_changed_with_rust_code_renamed_deleted_or_a_line_of_it_removed_is_found() {
    let lib = format!("{LIB}\npub fn two() -> i32 {{\n    2\n}}\n");
    for how in ["renamed", "deleted", "line removed"] {
        let repo = package(&[("crate/src/util.rs", "pub fn one() -> i32 {\n    1\n}\n")]);
        repo.put("crate/src/lib.rs", lib.as_bytes());
        repo.git(&["commit", "--quiet", "-am", "more code"]);
        match how {
            "renamed" => drop(repo.git(&["mv", "crate/src/util.rs", "crate/src/helpers.rs"])),
            "deleted" => drop(repo.git(&["rm", "--quiet", "crate/src/util.rs"])),
            _ => edit(&repo, "crate/src/lib.rs", "\npub fn two() -> i32 {\n    2\n}\n", ""),
        }
        change_negative(&repo);
        assert_eq!(found(&repo), negative_changed(), "{how}");
    }
}

#[test]
fn rust_of_a_cargo_package_the_change_deletes_is_code_under_test() {
    let repo = package(&[("other/Cargo.toml", MANIFEST), ("other/src/lib.rs", "pub fn one() {}\n")]);
    repo.git(&["rm", "--quiet", "-r", "other"]);
    change_negative(&repo);
    assert_eq!(found(&repo), negative_changed());
}

#[test]
fn rust_outside_a_cargo_package_is_not_the_code_under_test() {
    let repo = package(&[("notes/README.md", "Sketches.\n")]);
    repo.put("notes/sketch.rs", b"fn sketch() {}\n");
    // A file the change deletes that is no Cargo.toml puts no package around it.
    repo.git(&["rm", "--quiet", "notes/README.md"]);
    change_negative(&repo);
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn a_golden_file_changed_with_rust_code_is_an_expected_value_changed() {
    let repo = package(&[("crate/tests/golden/sum.txt", "3\n")]);
    repo.put("crate/tests/golden/sum.txt", b"4\n");
    assert_eq!(found(&repo), vec![]);
    edit(&repo, "crate/src/lib.rs", "    a + b\n", "    a + b + 1\n");
    let golden = (
        Some("crate/tests/golden/sum.txt".to_string()),
        None,
        "test-integrity expected-value-changed".to_string(),
        "warning".to_string(),
    );
    assert_eq!(found(&repo), vec![golden]);
}

#[test]
fn golden_files_are_listed_before_the_findings_on_rust_whatever_their_paths() {
    let repo = package(&[("crate/tests/golden/sum.txt", "3\n")]);
    repo.put("crate/tests/golden/sum.txt", b"4\n");
    edit(&repo, "crate/src/lib.rs", "    a + b\n", "    a + b + 1\n");
    edit(&repo, "crate/src/lib.rs", "    #[test]\n    fn adds_large() {\n        assert_eq!(add(10, 20), 30);\n    }\n", "");
    let paths: Vec<Option<String>> = found(&repo).into_iter().map(|(path, ..)| path).collect();
    assert_eq!(paths, [Some("crate/tests/golden/sum.txt".into()), Some("crate/src/lib.rs".into())]);
}

#[test]
fn a_file_under_tests_that_is_not_rust_on_purpose_is_no_test_integrity_finding() {
    let broken = "// A fixture of a parser's error.\nfn broken( {\n";
    let repo = package(&[("crate/tests/fixtures/broken.rs", broken)]);
    edit(&repo, "crate/tests/fixtures/broken.rs", "fn broken(", "fn still_broken(");
    repo.put("crate/tests/fixtures/new.rs", broken.as_bytes());
    // Only vague-names, which reads every file the change adds lines to, says it could not read them.
    let rules: Vec<String> = found(&repo).into_iter().map(|(_, _, rule, _)| rule).collect();
    assert_eq!(rules, ["vague-names not-read", "vague-names not-read"]);
}

#[test]
fn a_symlinked_rust_file_is_not_read_in_the_working_tree() {
    let repo = package(&[]);
    // Read, it would bind a vague name on its line 1, the line the change adds to the symlink.
    repo.put("crate/vague.txt", b"pub fn f() -> u32 { let data = 1; data }\n");
    std::os::unix::fs::symlink("../vague.txt", repo.repo.join("crate/src/link.rs")).unwrap();
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn each_check_says_which_of_its_findings_fail_the_gate() {
    let repo = package(&[]);
    edit(&repo, "crate/tests/it.rs", "#[test]\nfn negative", "#[test]\n#[ignore]\nfn negative");
    edit(&repo, "crate/src/lib.rs", "    #[test]\n    fn adds_large() {\n        assert_eq!(add(10, 20), 30);\n    }\n", "");
    let found: Vec<(String, String)> = found(&repo).into_iter().map(|(_, _, rule, severity)| (rule, severity)).collect();
    assert_eq!(found, [
        ("test-integrity removed-test".into(), "warning".into()),
        ("test-integrity skip".into(), "error".into()),
    ]);
}

#[test]
fn a_check_s_errors_fail_the_gate_and_its_warnings_are_to_weigh() {
    let repo = package(&[("pyproject.toml", "[tool.ruff]\n")]);
    edit(&repo, "crate/tests/it.rs", "#[test]\nfn negative", "#[test]\n#[ignore]\nfn negative");
    let adds_large = "    #[test]\n    fn adds_large() {\n        assert_eq!(add(10, 20), 30);\n    }\n";
    edit(&repo, "crate/src/lib.rs", adds_large, "");
    repo.git(&["add", "-A"]);
    let report = judge_change(&repo.repo, "HEAD", &Given::default()).unwrap();
    assert_eq!(report.conditions, ["1 findings of the `test-integrity` check; each is listed below."]);
    let rules = |found: &[gate::report::Finding]| -> Vec<String> { found.iter().map(|f| f.rule.clone()).collect() };
    assert_eq!(rules(&report.issues), ["test-integrity skip"]);
    assert_eq!(rules(&report.to_weigh), ["test-integrity removed-test"]);
}

#[test]
fn the_checks_run_in_the_gate_s_order_one_list_each() {
    let repo = package(&[]);
    let change = Change::from_tree(&repo.repo, "HEAD", Settings::default()).unwrap();
    assert_eq!(run_checks(&change).unwrap(), vec![vec![], vec![], vec![]]);
    // A new fn over the limit, and a vague name, beside a skip.
    let ifs: String = (0..16).map(|k| format!("    if x == {k} {{\n        x += {k};\n    }}\n")).collect();
    repo.put("crate/src/big.rs", format!("pub fn big(mut x: u32) -> u32 {{\n{ifs}    let info = x;\n    info\n}}\n").as_bytes());
    edit(&repo, "crate/tests/it.rs", "#[test]\nfn negative", "#[test]\n#[ignore]\nfn negative");
    let rules: Vec<String> = found(&repo).into_iter().map(|(_, _, rule, _)| rule).collect();
    assert_eq!(rules, ["test-integrity skip", "cognitive-complexity over-limit", "vague-names vague-name"]);
}

#[test]
fn each_rs_file_the_change_touches_is_read_at_the_base_and_in_the_working_tree() {
    let test = |name: &str| format!("#[cfg(test)]\nmod tests {{\n    #[test]\n    fn {name}() {{\n        assert!(true);\n    }}\n}}\n");
    let (gone, moved, script) = (test("gone"), test("moved"), test("script"));
    let repo = Repo::with(&[
        ("src/gone.rs", gone.as_bytes()),
        ("src/moved.rs", moved.as_bytes()),
        ("src/script.rs", script.as_bytes()),
        ("notes.txt", b"notes\n"),
    ]);
    repo.git(&["rm", "--quiet", "src/gone.rs"]);
    repo.git(&["mv", "src/moved.rs", "src/renamed.rs"]);
    repo.git(&["mv", "src/script.rs", "src/script.txt"]);
    repo.put("src/new.rs", b"fn new() {\n    let data = 1;\n}\n");
    repo.put("notes.txt", b"more notes\n");
    // A file deleted, or no longer .rs, loses its tests; one renamed keeps them; one added is read for its names.
    let found: Vec<(Option<String>, String)> = found(&repo).into_iter().map(|(path, _, rule, _)| (path, rule)).collect();
    assert_eq!(found, [
        (Some("src/gone.rs".into()), "test-integrity removed-test".into()),
        (Some("src/script.rs".into()), "test-integrity removed-test".into()),
        (Some("src/new.rs".into()), "vague-names vague-name".into()),
    ]);
}

#[test]
fn a_base_file_is_read_as_git_stores_its_bytes() {
    // Not UTF-8 at the base, so its tests are not read there, and the test the working tree removes is no finding.
    let latin1 = b"#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n// caf\xe9\n";
    let repo = Repo::with(&[("src/lib.rs", latin1)]);
    repo.put("src/lib.rs", b"// caf\xc3\xa9\n");
    assert_eq!(found(&repo), vec![]);
    repo.put("src/lib.rs", b"#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n// caf\xe9\n// more\n");
    // Nor in the working tree, where the checks that read a file the change adds lines to say they could not.
    let rules: Vec<String> = found(&repo).into_iter().map(|(_, _, rule, _)| rule).collect();
    assert_eq!(rules, ["cognitive-complexity not-scored", "vague-names not-read"]);
}
