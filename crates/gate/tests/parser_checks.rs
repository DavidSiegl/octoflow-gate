//! The parser checks run on a real git repository's change (gate::parser_checks): what the gate decides
//! over the whole change before a check judges it, which the front ends' tests of the checks (crates/lang-python,
//! crates/lang-rust) take as given: which files are a language's source, whether the change edits its code under
//! test, by the settings and, for Rust, the Cargo packages of the tree, and which golden files go with it.

use std::fs::OpenOptions;
use std::os::unix::fs::OpenOptionsExt;
use std::process::Command;
use std::time::Duration;

use gate::judge::{judge_change, Change, Given};
use gate::parser_checks::{
    dependency_cycles, duplicated_blocks, too_complex, vague_names, weakened_tests, Sources,
};
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

/// The findings of test-integrity, cognitive-complexity, dependency-cycles, duplicated-blocks and vague-names on
/// `change`, in that order.
fn run_checks(change: &Change) -> Vec<Vec<gate::report::Finding>> {
    let sources = Sources::read(change).unwrap();
    vec![
        weakened_tests(&sources),
        too_complex(&sources),
        dependency_cycles(&sources).unwrap(),
        duplicated_blocks(&sources).unwrap(),
        vague_names(&sources),
    ]
}

/// A finding as these tests compare it: its path, line, rule and severity.
type Found = (Option<String>, Option<u32>, String, String);

/// Each finding of the parser checks on the change from HEAD to the working tree, every file in it tracked, by the
/// default settings but for duplicated-blocks, which judges no file ([`without_copies`]), in the order the gate runs
/// the checks.
fn findings(repo: &Repo) -> Vec<gate::report::Finding> {
    findings_by(repo, without_copies())
}

/// The default settings, but with every file exempt from duplicated-blocks: the tests of the other checks move and
/// copy functions, which that check would report too.
fn without_copies() -> Settings {
    Settings { duplicates_exempt: gate::glob::Globs::new(&["**"]), ..Settings::default() }
}

/// Each finding of the parser checks on the change from HEAD to the working tree, every file in it tracked, by
/// `settings`, in the order the gate runs the checks.
fn findings_by(repo: &Repo, settings: Settings) -> Vec<gate::report::Finding> {
    repo.git(&["add", "-A"]);
    let change = Change::from_tree(&repo.repo, "HEAD", settings, None).unwrap();
    run_checks(&change).into_iter().flatten().collect()
}

/// The path, line, rule and severity of each of [`findings`].
fn found(repo: &Repo) -> Vec<Found> {
    found_by(repo, without_copies())
}

/// The path, line, rule and severity of each of [`findings_by`].
fn found_by(repo: &Repo, settings: Settings) -> Vec<Found> {
    let place = |f: gate::report::Finding| {
        (f.path.as_ref().map(|p| p.to_string_lossy().into_owned()), f.line, f.rule, f.severity)
    };
    findings_by(repo, settings).into_iter().map(place).collect()
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
fn a_rust_file_that_opens_with_an_inner_attribute_naming_python_is_not_read_as_python() {
    let python = "#![cfg(feature = \"python\")]\n\npub fn twice(n: i32) -> i32 {\n    n * 2\n}\n";
    let repo = package(&[("crate/src/python.rs", python)]);
    edit(&repo, "crate/src/python.rs", "n * 2", "n + n");
    assert_eq!(found(&repo), vec![]);
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
fn a_base_entry_that_is_no_regular_file_is_not_read() {
    let repo = package(&[]);
    // A symlink whose target, as git stores it, reads as a test; read, the change would remove it.
    std::os::unix::fs::symlink("#[test] fn t() {}", repo.repo.join("crate/tests/link.rs")).unwrap();
    // A submodule's entry, whose object is a commit that `git cat-file blob` refuses.
    let head = repo.git(&["rev-parse", "HEAD"]);
    repo.git(&["update-index", "--add", "--cacheinfo", &format!("160000,{},crate/src/sub.rs", head.trim())]);
    repo.git(&["add", "crate/tests/link.rs"]);
    repo.git(&["commit", "--quiet", "-m", "links"]);
    std::fs::remove_file(repo.repo.join("crate/tests/link.rs")).unwrap();
    repo.git(&["rm", "--quiet", "--cached", "crate/src/sub.rs"]);
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
    // clippy off: the package's tests use crates it does not depend on, which it would not compile.
    let repo = package(&[("pyproject.toml", "[tool.ruff]\n"), ("gate.toml", "[linters]\nclippy = []\n")]);
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
fn a_dependency_cycle_the_change_closes_fails_the_gate() {
    let repo = package(&[("pyproject.toml", "[tool.ruff]\n"), ("gate.toml", "[linters]\nclippy = []\n")]);
    edit(&repo, "crate/src/lib.rs", "pub fn add", "mod first;\nmod second;\n\npub fn add");
    repo.put("crate/src/first.rs", b"pub struct First;\n");
    repo.put("crate/src/second.rs", b"use crate::first::First;\n");
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "--quiet", "-m", "two modules"]);
    repo.put("crate/src/first.rs", b"use crate::second::Second;\n\npub struct First;\n");
    repo.git(&["add", "-A"]);
    let report = judge_change(&repo.repo, "HEAD", &Given::default()).unwrap();
    assert_eq!(report.conditions, ["1 findings of the `dependency-cycles` check; each is listed below."]);
    let found: Vec<_> = report.issues.iter().map(|f| (f.path.clone().unwrap(), f.line, f.rule.as_str())).collect();
    assert_eq!(found, [("crate/src/first.rs".into(), Some(1), "dependency-cycles")]);
}

#[test]
fn a_dependency_cycle_the_base_has_under_a_name_the_change_gives_a_file_is_not_found() {
    let body = "import a\n\nLIMIT = 10\nNAMES = ['first', 'second', 'third']\n";
    let repo = Repo::with(&[("a.py", b"import b\n"), ("b.py", body.as_bytes())]);
    repo.git(&["mv", "b.py", "c.py"]);
    repo.put("a.py", b"import c\n");
    assert_eq!(found(&repo), vec![]);
    // Without the rename, the same import closes a cycle.
    repo.put("d.py", body.as_bytes());
    repo.put("a.py", b"import c\nimport d\n");
    assert_eq!(found(&repo), vec![(Some("a.py".into()), Some(2), "dependency-cycles".into(), "error".into())]);
}

#[test]
fn the_checks_run_in_the_gate_s_order_one_list_each() {
    let repo = package(&[]);
    let change = Change::from_tree(&repo.repo, "HEAD", Settings::default(), None).unwrap();
    assert_eq!(run_checks(&change), vec![vec![], vec![], vec![], vec![], vec![]]);
    // A new fn over the limit, whose 16 ifs of one shape repeat each other, and a vague name, beside a skip.
    let ifs: String = (0..16).map(|k| format!("    if x == {k} {{\n        x += {k};\n    }}\n")).collect();
    repo.put("crate/src/big.rs", format!("pub fn big(mut x: u32) -> u32 {{\n{ifs}    let info = x;\n    info\n}}\n").as_bytes());
    edit(&repo, "crate/tests/it.rs", "#[test]\nfn negative", "#[test]\n#[ignore]\nfn negative");
    let rules: Vec<String> = found_by(&repo, Settings::default()).into_iter().map(|(_, _, rule, _)| rule).collect();
    let order = ["test-integrity skip", "cognitive-complexity over-limit", "duplicated-blocks"];
    assert_eq!(rules, [&order[..], &["vague-names vague-name"]].concat());
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
fn a_file_a_cfg_test_mod_declaration_takes_in_is_test_code() {
    // A fake whose fn is over the limit, beside a test of its own; lib.rs takes it in under `cfg(test)`.
    let ifs: String = (0..16).map(|k| format!("    if x == {k} {{\n        x += 1;\n    }}\n")).collect();
    let fakes = format!(
        "pub fn answer(mut x: u32) -> u32 {{\n{ifs}    x\n}}\n\n#[test]\nfn answers() {{\n    assert_eq!(answer(0), 1);\n}}\n"
    );
    let declared = |marked: bool| {
        let cfg = if marked { "#[cfg(test)]\n" } else { "" };
        format!("{cfg}mod fakes;\n\n{LIB}")
    };
    for marked in [true, false] {
        let repo = package(&[("crate/src/fakes.rs", "")]);
        repo.put("crate/src/lib.rs", declared(marked).as_bytes());
        repo.git(&["add", "-A"]);
        repo.git(&["commit", "--quiet", "-m", "fakes"]);
        repo.put("crate/src/fakes.rs", fakes.as_bytes());
        let complexity = ("crate/src/fakes.rs".into(), Some(1), "cognitive-complexity over-limit".into());
        let at = |f: Found| (f.0.unwrap_or_default(), f.1, f.2);
        let first: Vec<(String, Option<u32>, String)> = found(&repo).into_iter().map(at).collect();
        assert_eq!(first.contains(&complexity), !marked, "marked: {marked}, {first:?}");
        if marked {
            // Its test is a test: weakened beside a change to the code, it is found.
            repo.git(&["add", "-A"]);
            repo.git(&["commit", "--quiet", "-m", "fake"]);
            edit(&repo, "crate/src/fakes.rs", "#[test]\n", "#[test]\n#[ignore]\n");
            edit(&repo, "crate/src/lib.rs", "    a + b\n", "    b + a\n");
            let skip = ("crate/src/fakes.rs".into(), Some(54), "test-integrity skip".into());
            assert_eq!(found(&repo).into_iter().map(at).collect::<Vec<_>>(), vec![skip]);
        }
    }
}

#[test]
fn a_file_the_change_takes_out_of_the_tests_is_code_in_the_working_tree() {
    // lib.rs took fakes.rs in under `cfg(test)` at the base; the change takes it in as code and grows a fn there.
    let ifs: String = (0..16).map(|k| format!("    if x == {k} {{\n        x += 1;\n    }}\n")).collect();
    let repo = package(&[("crate/src/fakes.rs", "")]);
    repo.put("crate/src/lib.rs", format!("#[cfg(test)]\nmod fakes;\n\n{LIB}").as_bytes());
    repo.git(&["add", "-A"]);
    repo.git(&["commit", "--quiet", "-m", "fakes"]);
    repo.put("crate/src/lib.rs", format!("mod fakes;\n\n{LIB}").as_bytes());
    repo.put("crate/src/fakes.rs", format!("pub fn answer(mut x: u32) -> u32 {{\n{ifs}    x\n}}\n").as_bytes());
    let rules: Vec<(String, Option<u32>, String)> =
        found(&repo).into_iter().map(|f| (f.0.unwrap_or_default(), f.1, f.2)).collect();
    let complexity = ("crate/src/fakes.rs".to_string(), Some(1), "cognitive-complexity over-limit".to_string());
    assert!(rules.contains(&complexity), "{rules:?}");
}

#[test]
fn a_test_file_that_is_not_utf_8_on_a_side_fails_the_gate_as_unreadable() {
    // Among the tests by its place: its base cannot be decoded, so the gate cannot tell which tests the change removes.
    let latin1 = [IT.as_bytes(), b"// caf\xe9\n"].concat();
    let repo = Repo::with(&[("crate/Cargo.toml", MANIFEST.as_bytes()), ("crate/tests/it.rs", &latin1)]);
    repo.put("crate/tests/it.rs", b"use tiny::add;\n");
    let unreadable_at = |path: &str| -> Vec<Found> {
        vec![(Some(path.into()), None, "test-integrity unreadable".into(), "error".into())]
    };
    let unreadable = unreadable_at("crate/tests/it.rs");
    assert_eq!(found(&repo), unreadable);
    // And in the working tree.
    repo.put("crate/tests/it.rs", &[&latin1[..], b"// more\n"].concat());
    let integrity = |repo: &Repo| -> Vec<Found> {
        found(repo).into_iter().filter(|f| f.2.starts_with("test-integrity")).collect()
    };
    assert_eq!(integrity(&repo), unreadable);
    // A file with an inline test module at the base is a test file too, and the base's tests cannot be compared.
    let repo = package(&[]);
    repo.put("crate/src/lib.rs", &[LIB.as_bytes(), b"// caf\xe9\n"].concat());
    assert_eq!(integrity(&repo), unreadable_at("crate/src/lib.rs"));
}

#[test]
fn a_base_file_is_read_as_git_stores_its_bytes() {
    // Not UTF-8 at the base, so its tests are not read there, and the test the working tree removes is no finding: no
    // side of src/lib.rs is a test file that could be read, nor is it placed among the tests.
    let latin1 = b"#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n// caf\xe9\n";
    let repo = Repo::with(&[("src/lib.rs", latin1)]);
    repo.put("src/lib.rs", b"// caf\xc3\xa9\n");
    assert_eq!(found(&repo), vec![]);
    repo.put("src/lib.rs", b"#[cfg(test)]\nmod tests {\n    #[test]\n    fn t() {}\n}\n// caf\xe9\n// more\n");
    // Nor in the working tree, where the checks that read a file the change adds lines to say they could not.
    let rules: Vec<String> = found(&repo).into_iter().map(|(_, _, rule, _)| rule).collect();
    assert_eq!(rules, ["cognitive-complexity not-scored", "vague-names not-read"]);
}

/// A Python function named `name` that scores `score`: as many ifs, one after the other.
fn flat(name: &str, score: u32) -> String {
    let ifs: String = (0..score).map(|k| format!("    if x == {k}:\n        x += {k}\n")).collect();
    format!("def {name}(x):\n{ifs}    return x\n")
}

const CALC_PY: &str = "def add(a, b):\n    return a + b\n";
const TEST_CALC_PY: &str = "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n";

/// A Python module calc.py and its test tests/test_calc.py, and `more`, (path, contents), committed as the base.
fn python_project(more: &[(&str, &str)]) -> Repo {
    let mut files = vec![("calc.py", CALC_PY), ("tests/test_calc.py", TEST_CALC_PY)];
    files.extend(more);
    let files: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (*p, t.as_bytes())).collect();
    Repo::with(&files)
}

/// The path, line and rule of each of [`found`]'s findings.
fn found_at(repo: &Repo) -> Vec<(String, Option<u32>, String)> {
    found(repo).into_iter().map(|(path, line, rule, _)| (path.unwrap_or_default(), line, rule)).collect()
}

/// What [`found_at`] gives for a function over the limit at `line` of the file at `path`.
fn over_limit(path: &str, line: u32) -> (String, Option<u32>, String) {
    (path.into(), Some(line), "cognitive-complexity over-limit".into())
}

/// [`found_at`] once fresh.py, a new Python function over the limit, is added to `repo`'s working tree, but the
/// finding on fresh.py: so a test that expects no finding fails when the checks find nothing at all.
fn found_beside_fresh(repo: &Repo) -> Vec<(String, Option<u32>, String)> {
    repo.put("fresh.py", flat("fresh", 16).as_bytes());
    let mut beside = found_at(repo);
    let fresh = beside.iter().position(|at| *at == over_limit("fresh.py", 1)).expect("fresh.py's function is found");
    beside.remove(fresh);
    beside
}

#[test]
fn an_unchanged_python_tree_has_no_findings() {
    assert_eq!(found(&python_project(&[])), vec![]);
}

#[test]
fn a_script_whose_first_line_names_python_is_judged_and_one_that_names_a_shell_is_not() {
    let repo = python_project(&[]);
    repo.put("bin/tool", format!("#!/usr/bin/env python3\n{}", flat("fresh", 16)).as_bytes());
    repo.put("bin/run", format!("#!/bin/sh\n{}", flat("shell", 16)).as_bytes());
    assert_eq!(found_at(&repo), [over_limit("bin/tool", 2)]);
}

#[test]
fn a_vague_name_in_a_python_script_with_no_py_is_found() {
    let repo = python_project(&[]);
    repo.put("bin/tool", b"#!/usr/bin/env python3\ndef load(path):\n    data = path\n    return data\n");
    assert_eq!(found_at(&repo), [("bin/tool".into(), Some(3), "vague-names vague-name".into())]);
}

#[test]
fn a_python_script_the_change_deletes_lends_its_functions_bases_and_a_shell_script_does_not() {
    for (first, new) in [("#!/usr/bin/env python3\n", false), ("#!/bin/sh\n", true)] {
        let repo = python_project(&[("bin/tool", &format!("{first}{}", flat("helper", 20)))]);
        repo.git(&["rm", "--quiet", "bin/tool"]);
        // An edit of calc.py, so that git sees no rename.
        repo.put("calc.py", format!("def add(a, b):\n    return a + b\n\n\n{}", flat("helper", 20)).as_bytes());
        let rules: Vec<String> = found_at(&repo).into_iter().map(|(_, _, rule)| rule).collect();
        assert_eq!(rules, if new { vec!["cognitive-complexity over-limit"] } else { vec![] }, "{first}");
    }
}

#[test]
fn a_symlinked_python_file_is_not_read_on_either_side() {
    let repo = python_project(&[]);
    // Read in the working tree, it would bind a vague name on its line 1, the line the change adds to the symlink.
    repo.put("vague.txt", b"data = 1\n");
    std::os::unix::fs::symlink("vague.txt", repo.repo.join("link.py")).unwrap();
    // A symlink whose target, as git stores it, reads as a test; read at the base, the change would remove it.
    std::os::unix::fs::symlink("def test_t(): pass", repo.repo.join("tests/test_link.py")).unwrap();
    repo.git(&["add", "tests/test_link.py"]);
    repo.git(&["commit", "--quiet", "-m", "link"]);
    std::fs::remove_file(repo.repo.join("tests/test_link.py")).unwrap();
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn a_python_file_with_a_byte_order_mark_or_a_coding_cookie_is_judged_as_python_reads_it() {
    let repo = python_project(&[]);
    repo.put("bom.py", &[b"\xef\xbb\xbf", flat("marked", 16).as_bytes()].concat());
    let latin = [b"# -*- coding: latin-1 -*-\nCAFE = 'caf\xe9'\n", flat("declared", 16).as_bytes()].concat();
    repo.put("latin.py", &latin);
    assert_eq!(found_at(&repo), [over_limit("bom.py", 1), over_limit("latin.py", 3)]);
}

#[test]
fn a_python_base_with_a_byte_order_mark_or_a_coding_cookie_is_compared_as_python_reads_it() {
    let (bom, cookie) = (&b"\xef\xbb\xbf"[..], &b"# -*- coding: latin-1 -*-\nCAFE = 'caf\xe9'\n"[..]);
    let (marked, declared) = (flat("marked", 20), flat("declared", 20));
    let repo = Repo::with(&[
        ("bom.py", &[bom, marked.as_bytes()].concat()),
        ("latin.py", &[cookie, declared.as_bytes()].concat()),
    ]);
    // Edits that leave both at 20.
    repo.put("bom.py", &[bom, marked.replace("x += 3\n", "x += 30\n").as_bytes()].concat());
    repo.put("latin.py", &[cookie, declared.replace("x += 3\n", "x += 30\n").as_bytes()].concat());
    assert_eq!(found_beside_fresh(&repo), []);
}

#[test]
fn a_python_file_renamed_and_edited_is_compared_with_its_base_under_its_old_name() {
    let repo = python_project(&[("big.py", &flat("big", 20))]);
    repo.git(&["mv", "big.py", "lib.py"]);
    edit(&repo, "lib.py", "x += 19\n", "x += 190\n");
    assert_eq!(found_beside_fresh(&repo), []);
}

#[test]
fn a_python_script_the_change_renames_to_a_py_file_lends_its_functions_bases() {
    let repo = python_project(&[("bin/tool", &format!("#!/usr/bin/env python3\n{}", flat("helper", 20)))]);
    repo.git(&["rm", "--quiet", "bin/tool"]);
    // A new file, which git may pair with the deleted script as its rename.
    repo.put("lib.py", flat("helper", 20).as_bytes());
    assert_eq!(found_beside_fresh(&repo), []);
}

#[test]
fn a_python_file_renamed_from_any_name_is_compared_with_its_base() {
    // Python by its new name alone: the base has neither `.py` nor a `#!` line.
    let repo = python_project(&[("bin/tool", &flat("helper", 20))]);
    repo.git(&["mv", "bin/tool", "bin/tool.py"]);
    edit(&repo, "bin/tool.py", "x += 3\n", "x += 30\n");
    assert_eq!(found_beside_fresh(&repo), []);
}

#[test]
fn a_base_read_under_a_name_that_is_not_python_s_is_no_code_under_test() {
    // Renamed into the tests: its new path is a test file's, and its old one never held Python by name or first line.
    let repo = python_project(&[("scripts/check_calc", "def test_check():\n    assert True\n")]);
    repo.git(&["mv", "scripts/check_calc", "scripts/test_check_calc.py"]);
    edit(&repo, "tests/test_calc.py", "== 3\n", "== 4\n");
    assert_eq!(found_at(&repo), []);
    // With code under test edited too, the changed assertion is found.
    edit(&repo, "calc.py", "a + b", "b + a");
    let changed = ("tests/test_calc.py".into(), Some(5), "test-integrity expected-value-changed".into());
    assert_eq!(found_at(&repo), [changed]);
}

#[test]
fn a_python_function_out_of_tests_is_judged_as_new_as_tests_were_never_held_to_the_limit() {
    let (helper, other) = (flat("helper", 20), flat("other_helper", 20).replace("x +=", "x -="));
    let repo = python_project(&[("tests/helpers.py", &format!("{helper}\n\n{other}"))]);
    // Renamed, with both functions, and an edit that leaves helper at 20.
    std::fs::create_dir(repo.repo.join("lib")).unwrap();
    repo.git(&["mv", "tests/helpers.py", "lib/helpers.py"]);
    edit(&repo, "lib/helpers.py", "x += 3\n", "x += 30\n");
    assert_eq!(found_at(&repo), [over_limit("lib/helpers.py", 1)]);
    // lib/helpers.py's base is now in lib/, held to the limit: other_helper, moved out of it, keeps its base.
    repo.commit(&[], "renamed");
    repo.put("lib/helpers.py", helper.replace("x += 3\n", "x += 30\n").as_bytes());
    repo.put("lib/more.py", other.as_bytes());
    assert_eq!(found_beside_fresh(&repo), []);
}

/// big.py, a line and then a function over the limit, for [`python_project`]'s base, and the function alone.
fn big_py() -> (String, String) {
    let big = flat("big", 20);
    (format!("X = 0\n\n\n{big}"), big)
}

#[test]
fn a_function_moved_unchanged_into_a_python_script_keeps_its_base_from_a_copy_elsewhere() {
    let (big_py, big) = big_py();
    let repo = python_project(&[("big.py", &big_py)]);
    repo.put("big.py", b"X = 0\n");
    // The original, in a script with no .py, and a copy of it under another name.
    repo.put("bin/tool", format!("#!/usr/bin/env python3\n{big}").as_bytes());
    repo.put("lib.py", big.replace("def big(", "def copied(").as_bytes());
    assert_eq!(found_at(&repo), [over_limit("lib.py", 1)]);
}

#[test]
fn a_function_s_text_moved_into_a_file_that_is_not_python_leaves_its_base_to_its_copy() {
    for (name, first) in [("notes.txt", "# python, kept for reference\n"), ("bin/run", "#!/bin/sh\n")] {
        let (big_py, big) = big_py();
        let repo = python_project(&[("big.py", &big_py)]);
        repo.put("big.py", b"X = 0\n");
        repo.put(name, format!("{first}{big}").as_bytes());
        // The only function with big's body.
        repo.put("lib.py", big.replace("def big(", "def copied(").as_bytes());
        assert_eq!(found_beside_fresh(&repo), [], "{name}");
    }
}

#[test]
fn a_changed_symlink_is_not_read_so_one_to_a_fifo_cannot_block_the_checks() {
    let repo = python_project(&[]);
    let fifo = repo.outside.join("pipe");
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    std::os::unix::fs::symlink(&fifo, repo.repo.join("x.py")).unwrap();
    let (done, finished) = std::sync::mpsc::channel();
    std::thread::scope(|scope| {
        let repo = &repo;
        scope.spawn(move || done.send(found_beside_fresh(repo)).unwrap());
        let found = finished.recv_timeout(Duration::from_secs(10));
        if found.is_err() {
            // The FIFO's other end, opened and closed, ends the read blocked on it, so that the scope ends and the
            // test fails rather than hangs. It fails to open only when no read is blocked on it after all.
            drop(OpenOptions::new().write(true).custom_flags(libc::O_NONBLOCK).open(&fifo));
        }
        assert_eq!(found.ok(), Some(vec![]), "the checks read the FIFO a changed symlink points to");
    });
}

#[test]
fn a_python_module_renamed_with_a_symlink_left_under_its_old_name_keeps_its_functions_bases() {
    let repo = python_project(&[("big.py", &flat("big", 20))]);
    repo.git(&["mv", "big.py", "core.py"]);
    std::os::unix::fs::symlink("core.py", repo.repo.join("big.py")).unwrap();
    assert_eq!(found_beside_fresh(&repo), []);
}

#[test]
fn a_python_function_in_mutmut_s_state_is_not_judged() {
    let repo = python_project(&[]);
    repo.put("mutants/calc.py", flat("x_big__mutmut_1", 20).as_bytes());
    assert_eq!(found_beside_fresh(&repo), []);
}

#[test]
fn a_removed_python_test_is_found_whatever_syntax_the_environment_gives_git_s_pathspecs() {
    // Mutants/ is not mutmut's mutants/: a git that read the gate's pathspecs without regard to case would leave it
    // out of the change, and one that read them as literal paths would leave out every file.
    let tests = "def test_a():\n    assert 1\n\n\ndef test_b():\n    assert 2\n";
    let repo = python_project(&[("pyproject.toml", "[tool.ruff]\n"), ("Mutants/test_case.py", tests)]);
    edit(&repo, "Mutants/test_case.py", "def test_b():\n    assert 2\n", "");
    for name in ["GIT_LITERAL_PATHSPECS", "GIT_ICASE_PATHSPECS"] {
        // Through the binary: what it leaves out of its tools' environment is its own process's.
        let done = repo.gate_command("HEAD").env(name, "1").output().unwrap();
        let out = String::from_utf8(done.stdout).unwrap();
        assert!(out.contains("`Mutants/test_case.py:5` warning test-integrity removed-test: "), "{name}: {out}");
    }
}

#[test]
fn a_python_expected_value_changed_with_the_code_is_found_and_without_it_is_not() {
    let repo = python_project(&[("conftest.py", "import sys\n"), ("tests/data/make.py", "ROWS = 1\n")]);
    edit(&repo, "tests/test_calc.py", "== 3", "== 4");
    // What goes with the tests is not the code under test.
    edit(&repo, "conftest.py", "import sys\n", "import os\n");
    edit(&repo, "tests/data/make.py", "ROWS = 1\n", "ROWS = 2\n");
    assert_eq!(found(&repo), vec![]);
    edit(&repo, "calc.py", "return a + b", "return b + a");
    let changed = "test-integrity expected-value-changed";
    assert_eq!(found_at(&repo), [("tests/test_calc.py".into(), Some(5), changed.into())]);
}

#[test]
fn a_golden_file_changed_with_python_code_is_an_expected_value_changed_and_without_it_is_not() {
    let repo = python_project(&[
        ("tests/golden/sum.txt", "3\n"),
        ("tests/golden/old.txt", "1\n"),
        ("tests/golden/moved.txt", "2\n"),
        ("tests/golden/cut.txt", "1\n2\n"),
        ("docs/golden/notes.txt", "a\n"),
    ]);
    repo.put("tests/golden/sum.txt", b"4\n");
    // Lines removed, none added.
    repo.put("tests/golden/cut.txt", b"1\n");
    repo.git(&["rm", "--quiet", "tests/golden/old.txt"]);
    // Its expected value unchanged; then a new expected value, not a changed one; then a file no test reads.
    repo.git(&["mv", "tests/golden/moved.txt", "tests/golden/kept.txt"]);
    repo.put("tests/golden/new.txt", b"5\n");
    repo.put("docs/golden/notes.txt", b"b\n");
    edit(&repo, "tests/test_calc.py", "def test_add():", "def test_sum():");
    assert_eq!(found(&repo), vec![]);
    edit(&repo, "calc.py", "return a + b", "return b + a");
    let found: Vec<(String, Option<u32>, String, String, String)> = findings(&repo)
        .into_iter()
        .map(|f| (f.path.unwrap_or_default().to_string_lossy().into(), f.line, f.rule, f.severity, f.message))
        .collect();
    let golden = |name: &str, what: &str| {
        let message = format!("a golden file, the expected output of a test, {what}");
        let rule = "test-integrity expected-value-changed".to_string();
        (format!("tests/golden/{name}"), None, rule, "warning".to_string(), message)
    };
    let changed = "changed in a change that also edits the code under test; check that its diff is what the change \
                   means the output to become.";
    let deleted = "deleted in a change that also edits the code under test; check that no test still needs it.";
    assert_eq!(found, [golden("cut.txt", changed), golden("old.txt", deleted), golden("sum.txt", changed)]);
}

/// Commits under mutants/ what mutmut leaves there after a run in [`python_project`]'s tree: its copy of the code
/// and of the tests beside it.
fn commit_mutmut_state(repo: &Repo) {
    let state = [("mutants/calc.py", CALC_PY.as_bytes()), ("mutants/tests/test_calc.py", TEST_CALC_PY.as_bytes())];
    repo.commit(&state, "mutmut's state");
}

#[test]
fn removing_mutmut_s_state_removes_no_test() {
    let repo = python_project(&[]);
    commit_mutmut_state(&repo);
    repo.git(&["rm", "-r", "--quiet", "mutants"]);
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn an_edit_to_mutmut_s_state_is_no_change_to_the_code() {
    let repo = python_project(&[]);
    commit_mutmut_state(&repo);
    edit(&repo, "mutants/calc.py", "return a + b", "return a + b + 1");
    edit(&repo, "tests/test_calc.py", "== 3", "== 4");
    assert_eq!(found(&repo), vec![]);
}

#[test]
fn a_test_moved_out_of_mutmut_s_state_is_a_new_test() {
    let skipped = "import pytest\n\n\n@pytest.mark.skip\ndef test_x():\n    assert 1\n";
    let repo = python_project(&[("mutants/tests/test_skipped.py", skipped)]);
    // Seen without mutants/, the move adds the file, so the skip at its base is on an added line.
    repo.git(&["mv", "mutants/tests/test_skipped.py", "tests/test_skipped.py"]);
    assert_eq!(found_at(&repo), [("tests/test_skipped.py".into(), Some(4), "test-integrity skip".into())]);
}

#[test]
fn a_python_test_file_that_is_not_utf_8_fails_the_gate_as_unreadable() {
    let repo = python_project(&[]);
    repo.put("tests/test_calc.py", b"def test_a():\n    assert '\xff'\n");
    let found: Vec<_> = findings(&repo).into_iter().filter(|f| f.check() == "test-integrity").collect();
    let [unreadable] = found.as_slice() else { panic!("one finding, not {found:?}") };
    let at = (unreadable.path.as_deref(), unreadable.line, unreadable.rule.as_str(), unreadable.severity.as_str());
    assert_eq!(at, (Some("tests/test_calc.py".as_ref()), None, "test-integrity unreadable", "error"));
    let why = &unreadable.message;
    assert!(why.starts_with("could not be read (") && why.ends_with("), so its tests were not checked."), "{why}");
}

#[test]
fn a_golden_file_changed_with_a_python_script_is_an_expected_value_changed_unless_the_script_is_beside_the_tests() {
    let script = "#!/usr/bin/env python3\nprint(3)\n";
    let golden = ("tests/golden/tool.txt".to_string(), None, "test-integrity expected-value-changed".to_string());
    for (how, expected) in [("edited", vec![golden.clone()]), ("deleted", vec![golden]), ("beside the tests", vec![])] {
        let repo = python_project(&[
            ("tests/golden/tool.txt", "3\n"),
            ("bin/tool", script),
            ("bin/old", script),
            ("tests/bin/fake", script),
        ]);
        match how {
            "edited" => edit(&repo, "bin/tool", "print(3)", "print(4)"),
            "deleted" => drop(repo.git(&["rm", "--quiet", "bin/old"])),
            _ => edit(&repo, "tests/bin/fake", "print(3)", "print(4)"),
        }
        repo.put("tests/golden/tool.txt", b"4\n");
        assert_eq!(found_at(&repo), expected, "{how}");
    }
}

#[test]
fn golden_files_then_python_s_findings_then_rust_s_are_listed_and_the_other_checks_by_path() {
    let repo = package(&[
        ("crate/tests/golden/sum.txt", "3\n"),
        ("zed/calc.py", "def add(a, b):\n    return a + b\n"),
        ("zed/test_calc.py", "from calc import add\n\n\ndef test_add():\n    assert add(1, 2) == 3\n"),
    ]);
    repo.put("crate/tests/golden/sum.txt", b"4\n");
    edit(&repo, "crate/src/lib.rs", "    a + b\n", "    let info = a + b + 1;\n    info\n");
    change_negative(&repo);
    edit(&repo, "zed/calc.py", "return a + b", "data = a + b\n    return data");
    edit(&repo, "zed/test_calc.py", "== 3", "== 4");
    let found: Vec<(String, String)> = found_at(&repo).into_iter().map(|(path, _, rule)| (path, rule)).collect();
    let changed = "test-integrity expected-value-changed";
    assert_eq!(found, [
        ("crate/tests/golden/sum.txt".into(), changed.into()),
        ("zed/test_calc.py".into(), changed.into()),
        ("crate/tests/it.rs".into(), changed.into()),
        ("crate/src/lib.rs".into(), "vague-names vague-name".into()),
        ("zed/calc.py".into(), "vague-names vague-name".into()),
    ]);
}

/// A Python file of six tests of two assertions each, `test_0` to `test_5`.
fn six_tests() -> String {
    let test = |k: u32| format!("def test_{k}():\n    assert add({k}, 0) == {k}\n    assert add(0, {k}) == {k}\n\n\n");
    (0..6).map(test).collect()
}

#[test]
fn a_python_file_renamed_into_or_out_of_the_tests_has_its_tests_read_on_both_sides() {
    // Into the tests, with a test and an assertion removed on the way: the test is gone from the base's path.
    let repo = python_project(&[("checks/calc_checks.py", &six_tests())]);
    repo.git(&["mv", "checks/calc_checks.py", "tests/test_more.py"]);
    let fewer = six_tests().replace("def test_0():\n    assert add(0, 0) == 0\n    assert add(0, 0) == 0\n\n\n", "");
    repo.put("tests/test_more.py", fewer.replace("    assert add(0, 5) == 5\n", "").as_bytes());
    assert_eq!(
        found_at(&repo),
        [
            ("checks/calc_checks.py".into(), Some(1), "test-integrity removed-test".into()),
            ("tests/test_more.py".into(), Some(21), "test-integrity fewer-assertions".into()),
        ]
    );
    // Out of them unchanged: no test is gone.
    let repo = python_project(&[("tests/test_more.py", &six_tests())]);
    std::fs::create_dir(repo.repo.join("checks")).unwrap();
    repo.git(&["mv", "tests/test_more.py", "checks/calc_checks.py"]);
    assert_eq!(found_at(&repo), []);
}

const GO_CALC: &str = "package calc\n\nfunc Add(a, b int) int {\n\treturn a + b\n}\n";

const GO_CALC_TEST: &str = "\
package calc

import \"testing\"

func TestAdd(t *testing.T) {
\tif got := Add(1, 2); got != 3 {
\t\tt.Errorf(\"got %d\", got)
\t}
}
";

/// A Go module at the top of the tree with its code in calc.go and its tests in calc_test.go, and `more`, (path,
/// contents), committed with it as the base. `module` is where its go.mod is, "" for none.
fn go_module(module: &str, more: &[(&str, &str)]) -> Repo {
    let mut files = vec![("calc.go", GO_CALC), ("calc_test.go", GO_CALC_TEST)];
    if !module.is_empty() {
        files.push((module, "module example.com/calc\n\ngo 1.22\n"));
    }
    files.extend(more);
    let files: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (*p, t.as_bytes())).collect();
    Repo::with(&files)
}

/// Raises `Add`'s result and the expected value of `TestAdd`, beside a new function over the limit and a vague name.
fn change_go(repo: &Repo) {
    edit(repo, "calc.go", "return a + b\n", "return a + b + 1\n");
    edit(repo, "calc_test.go", "got != 3 {", "got != 4 {");
    edit(repo, "calc_test.go", "\tif got := Add", "\tt.Skip(\"later\")\n\tif got := Add");
    repo.put("big.go", big_go("").as_bytes());
}

#[test]
fn go_of_a_go_module_gets_each_parser_check_s_findings() {
    let repo = go_module("go.mod", &[]);
    change_go(&repo);
    let found: Vec<(Option<String>, Option<u32>, String)> =
        found_by(&repo, Settings::default()).into_iter().map(|(path, line, rule, _)| (path, line, rule)).collect();
    let at = |path: &str, line: u32, rule: &str| (Some(path.to_string()), Some(line), rule.to_string());
    assert_eq!(found, [
        at("calc_test.go", 6, "test-integrity skip"),
        at("calc_test.go", 7, "test-integrity expected-value-changed"),
        at("big.go", 3, "cognitive-complexity over-limit"),
        // big.go's ifs, all of one shape, 9 tokens each: those from the seventh on repeat the first six.
        at("big.go", 22, "duplicated-blocks"),
        at("big.go", 52, "vague-names vague-name"),
    ]);
}

#[test]
fn a_go_golden_file_under_testdata_changed_with_the_code_is_an_expected_value_changed() {
    let repo = go_module("go.mod", &[("testdata/add.golden", "3\n")]);
    repo.put("testdata/add.golden", b"4\n");
    edit(&repo, "calc.go", "return a + b\n", "return a + b + 1\n");
    let golden = (
        Some("testdata/add.golden".to_string()),
        None,
        "test-integrity expected-value-changed".to_string(),
        "warning".to_string(),
    );
    assert_eq!(found_by(&repo, Settings::default()), vec![golden]);
}

#[test]
fn go_outside_a_go_module_or_under_testdata_is_not_read() {
    for (module, more) in [("", vec![]), ("go.mod", vec![("testdata/broken.go", "package broken\nfunc (\n")])] {
        let repo = go_module(module, &more);
        if module.is_empty() {
            change_go(&repo);
        } else {
            edit(&repo, "testdata/broken.go", "func (\n", "func ((\n");
        }
        assert_eq!(found(&repo), vec![], "{module:?}");
    }
}

#[test]
fn go_of_a_module_whose_go_mod_the_change_deletes_is_still_read() {
    let repo = go_module("sub/go.mod", &[("sub/calc.go", GO_CALC), ("sub/calc_test.go", GO_CALC_TEST)]);
    repo.git(&["rm", "--quiet", "sub/go.mod", "sub/calc_test.go"]);
    let rules: Vec<(Option<String>, String)> =
        found(&repo).into_iter().map(|(path, _, rule, _)| (path, rule)).collect();
    assert_eq!(rules, [(Some("sub/calc_test.go".into()), "test-integrity removed-test".into())]);
}

#[test]
fn a_go_test_file_renamed_to_a_goos_or_goarch_name_is_platform_only() {
    let platform_only = |path: &str| vec![(Some(path.to_string()), None, "test-integrity platform-only".to_string())];
    for (from, to, expected) in [
        ("calc_test.go", "calc_windows_test.go", platform_only("calc_windows_test.go")),
        ("calc_test.go", "calc_linux_amd64_test.go", platform_only("calc_linux_amd64_test.go")),
        ("calc_test.go", "calc_arm64_test.go", platform_only("calc_arm64_test.go")),
        ("calc_linux_test.go", "calc_windows_test.go", platform_only("calc_windows_test.go")),
        ("calc_linux_test.go", "calc_linux_amd64_test.go", platform_only("calc_linux_amd64_test.go")),
        ("calc_linux_amd64_test.go", "calc_amd64_test.go", vec![]),
        ("calc_linux_amd64_test.go", "calc_linux_test.go", vec![]),
        ("calc_android_test.go", "calc_linux_test.go", vec![]),
        ("calc_ios_arm64_test.go", "calc_darwin_test.go", vec![]),
        ("calc_linux_test.go", "calc_android_test.go", platform_only("calc_android_test.go")),
        ("calc_test.go", "windows_test.go", vec![]),
        ("calc_test.go", "calc_unix_test.go", vec![]),
        ("calc_test.go", "calc_windows_extra_test.go", vec![]),
        ("calc_windows_test.go", "calc_test.go", vec![]),
        ("calc_windows_test.go", "add_windows_test.go", vec![]),
    ] {
        let repo = go_module("go.mod", &[]);
        if from != "calc_test.go" {
            repo.git(&["mv", "calc_test.go", from]);
            repo.git(&["commit", "--quiet", "-m", "rename"]);
        }
        repo.git(&["mv", from, to]);
        let found: Vec<_> = found(&repo).into_iter().map(|(path, line, rule, _)| (path, line, rule)).collect();
        assert_eq!(found, expected, "{from} to {to}");
    }
}

/// A Go function scoring 16, over the limit, that binds the vague name `info`, after `header`, in package calc.
fn big_go(header: &str) -> String {
    let ifs: String = (0..16).map(|k| format!("\tif x == {k} {{\n\t\tx += {k}\n\t}}\n")).collect();
    format!("{header}package calc\n\nfunc big(x int) int {{\n{ifs}\tinfo := x\n\treturn info\n}}\n")
}

#[test]
fn vendored_generated_or_symlinked_go_is_not_read_but_is_code_under_test_and_testdata_is_neither() {
    let generated = "// Code generated by stringer; DO NOT EDIT.\n\n";
    let rule = "test-integrity expected-value-changed".to_string();
    let changed = vec![(Some("calc_test.go".to_string()), Some(6), rule)];
    for (path, header, expected) in [
        ("vendor/example.com/dep/big.go", "", changed.clone()),
        ("big_string.go", generated, changed.clone()),
        ("_examples/big.go", "", changed.clone()),
        ("link.go", "", changed.clone()),
        ("testdata/big.go", "", vec![]),
    ] {
        let repo = go_module("go.mod", &[]);
        if path == "link.go" {
            repo.put("notes/big.txt", big_go(header).as_bytes());
            std::os::unix::fs::symlink("notes/big.txt", repo.repo.join(path)).unwrap();
        } else {
            repo.put(path, big_go(header).as_bytes());
        }
        edit(&repo, "calc_test.go", "got != 3 {", "got != 4 {");
        let found: Vec<_> = found(&repo).into_iter().map(|(path, line, rule, _)| (path, line, rule)).collect();
        assert_eq!(found, expected, "{path}");
    }
}

/// A Python function of 55 tokens, over the default `min-tokens`; `total` in it stands for its name only.
const TOTAL: &str = "\
def total(prices, wanted):
    sum_ = 0
    for name in wanted:
        if name in prices and prices[name] > 0:
            sum_ += prices[name] * 2
        else:
            print('missing', name)
    sum_ = round(sum_, 2)
    return sum_
";

/// The duplicated-blocks findings, (path, line, the place the message names), on the change from HEAD to the
/// working tree, every file in it tracked, by `settings`.
fn copies(repo: &Repo, settings: Settings) -> Vec<(String, u32, String)> {
    repo.git(&["add", "-A"]);
    let change = Change::from_tree(&repo.repo, "HEAD", settings, None).unwrap();
    let other = |message: &str| message.split('`').nth(1).unwrap().to_string();
    let found = duplicated_blocks(&Sources::read(&change).unwrap()).unwrap();
    found.iter().map(|f| (f.path.clone().unwrap().into_string().unwrap(), f.line.unwrap(), other(&f.message))).collect()
}

#[test]
fn a_copied_function_with_its_names_changed_is_a_finding_to_weigh() {
    let repo = Repo::with(&[("shop/prices.py", TOTAL.as_bytes())]);
    let copy = TOTAL.replace("total", "weight").replace("prices", "masses").replace("'missing'", "'unknown'");
    repo.put("scale/weights.py", format!("import os\n\n\n{copy}").as_bytes());
    assert_eq!(copies(&repo, Settings::default()), [("scale/weights.py".into(), 4, "shop/prices.py:1".into())]);
    let found = found_by(&repo, Settings::default());
    assert_eq!(found, [(Some("scale/weights.py".into()), Some(4), "duplicated-blocks".into(), "warning".into())]);
}

#[test]
fn a_copied_block_shorter_than_min_tokens_is_not_found() {
    let short = "def total(prices):\n    return sum(p * 2 for p in prices.values() if p > 0)\n";
    let repo = Repo::with(&[("a.py", short.as_bytes())]);
    repo.put("b.py", short.replace("total", "weight").as_bytes());
    assert_eq!(copies(&repo, Settings::default()), []);
    let settings = Settings { duplicates_min_tokens: 10, ..Settings::default() };
    assert_eq!(copies(&repo, settings), [("b.py".into(), 1, "a.py:1".into())]);
}

#[test]
fn a_block_the_change_adds_twice_is_found_once() {
    let repo = Repo::with(&[("README.md", b"x\n")]);
    repo.put("a.py", TOTAL.as_bytes());
    repo.put("b.py", TOTAL.replace("total", "weight").as_bytes());
    assert_eq!(copies(&repo, Settings::default()), [("b.py".into(), 1, "a.py:1".into())]);
}

#[test]
fn an_exempt_file_is_neither_judged_nor_matched() {
    let repo = Repo::with(&[("tests/test_prices.py", TOTAL.as_bytes())]);
    repo.put("prices.py", TOTAL.as_bytes());
    repo.put("tests/test_more.py", TOTAL.as_bytes());
    assert_eq!(copies(&repo, Settings::default()), []);
    let settings = Settings { duplicates_exempt: gate::glob::Globs::new(&[] as &[&str]), ..Settings::default() };
    let found: Vec<String> = copies(&repo, settings).into_iter().map(|(path, _, _)| path).collect();
    assert_eq!(found, ["prices.py", "tests/test_more.py"]);
}

#[test]
fn a_python_script_told_by_its_first_line_is_judged() {
    let repo = Repo::with(&[("prices.py", TOTAL.as_bytes())]);
    repo.put("bin/tool", format!("#!/usr/bin/env python3\n{TOTAL}").as_bytes());
    assert_eq!(copies(&repo, Settings::default()), [("bin/tool".into(), 2, "prices.py:1".into())]);
}

#[test]
fn go_outside_every_module_is_neither_judged_nor_matched() {
    let big = big_go("");
    let repo = go_module("mod/go.mod", &[("examples/big.go", &big)]);
    assert_eq!(copies(&repo, Settings::default()), []);
    repo.put("examples/more.go", big.as_bytes());
    assert_eq!(copies(&repo, Settings::default()), []);
    // big.go's ifs repeat its own, the seventh on the first ([`big_go`]); the copy outside the module is not named.
    repo.put("mod/big.go", big.as_bytes());
    assert_eq!(copies(&repo, Settings::default()), [("mod/big.go".into(), 22, "mod/big.go:4".into())]);
}

#[test]
fn a_tracked_symlink_to_a_fifo_is_not_opened() {
    let repo = Repo::with(&[("prices.py", TOTAL.as_bytes())]);
    let fifo = repo.outside.join("pipe");
    assert!(Command::new("mkfifo").arg(&fifo).status().unwrap().success());
    std::os::unix::fs::symlink(&fifo, repo.repo.join("run")).unwrap();
    repo.put("weights.py", TOTAL.replace("total", "weight").as_bytes());
    // Opened for reading with no writer, a FIFO blocks for good, so the check runs apart and is given a minute.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || tx.send(copies(&repo, Settings::default())).unwrap());
    let found = rx.recv_timeout(Duration::from_secs(60)).expect("duplicated-blocks blocked on the FIFO");
    assert_eq!(found, [("weights.py".into(), 1, "prices.py:1".into())]);
}

/// The lines of Rust test code run whenever its tests pass, so counting them would cover a new function's lack of
/// tests with its tests' own lines. Each case adds a 15-line function no test runs, and test code beside it.
#[test]
fn the_coverage_of_new_rust_code_leaves_out_the_lines_of_its_test_code() {
    let lets: String = (1..=12).map(|k| format!("    let x = x + {k};\n")).collect();
    let scale = format!("\npub fn scale(x: i32) -> i32 {{\n{lets}    x\n}}\n");
    let module = "\n#[cfg(test)]\nmod scale_tests {\n    use super::*;\n\n    #[test]\n    fn scales() {\n        \
                  let s = scale(0);\n        assert_ne!(s, -1);\n    }\n}\n";
    let helper = "\n#[cfg(test)]\nfn twice(x: i32) -> i32 {\n    let y = x + x;\n    y\n}\n";
    let file = "use tiny::scale;\n\n#[test]\nfn scales() {\n    let s = scale(0);\n    assert_ne!(s, -1);\n}\n";
    let lib = |test_code: &str| format!("{LIB}{scale}{test_code}");
    let cases = [
        ("an inline module", vec![("crate/src/lib.rs", lib(module))]),
        ("a #[cfg(test)] fn", vec![("crate/src/lib.rs", lib(helper))]),
        ("a file among the tests", vec![("crate/src/lib.rs", lib("")), ("crate/tests/scale.rs", file.into())]),
        // Its lines are left out unread, as test-files.rust places it among the tests.
        (
            "a test file that does not parse",
            vec![("crate/src/lib.rs", lib("")), ("crate/tests/broken.rs", "fn broken( {\n    1\n".into())],
        ),
    ];
    // As cargo-llvm-cov reports it: the function's lines never ran, every other line of the changed files that maps
    // to code did; blank lines, attributes and `use` map to none.
    let untested = LIB.lines().count() as u32 + 2..LIB.lines().count() as u32 + 17;
    let maps_to_code = |line: &str| !(line.is_empty() || ["#[", "use "].iter().any(|s| line.trim_start().starts_with(s)));
    for (case, files) in cases {
        let repo = package(&[("pyproject.toml", "[tool.ruff]\n"), ("gate.toml", "[linters]\nclippy = []\n")]);
        let mut lcov = String::new();
        for (path, text) in &files {
            repo.put(path, text.as_bytes());
            lcov += &format!("SF:{}\n", repo.repo.join(path).display());
            for (n, _) in (1..).zip(text.lines()).filter(|(_, line)| maps_to_code(line)) {
                let hit = !(*path == "crate/src/lib.rs" && untested.contains(&n));
                lcov += &format!("DA:{n},{}\n", u32::from(hit));
            }
            lcov += "end_of_record\n";
        }
        repo.git(&["add", "-A"]);
        repo.put("results.xml", b"<?xml version=\"1.0\"?><testsuites><testsuite name=\"t\"/></testsuites>");
        repo.put("lcov.info", lcov.as_bytes());
        let given = Given {
            tests: vec![repo.repo.join("results.xml")],
            coverage: vec![repo.repo.join("lcov.info")],
            mutation_budget: Some(0),
            base_run_budget: Some(0),
            ..Given::default()
        };
        let report = judge_change(&repo.repo, "HEAD", &given).unwrap();
        let coverage = report.coverage.unwrap();
        assert_eq!((coverage.lines, coverage.covered_lines), (15, 0), "{case}");
    }
}
