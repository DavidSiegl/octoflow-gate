//! lang_rust::test_modules: the files of a tree that only test code takes in.

use std::collections::BTreeSet;
use std::ffi::OsString;

/// The paths lang_rust::test_modules gives for the tree of `files`, each (path, source), in order.
fn test_modules(files: &[(&str, &str)]) -> BTreeSet<String> {
    let tree: Vec<(OsString, Vec<u8>)> =
        files.iter().map(|&(path, source)| (path.into(), source.as_bytes().to_vec())).collect();
    lang_rust::test_modules(&tree).into_iter().map(|p| p.to_string_lossy().into_owned()).collect()
}

fn paths(paths: &[&str]) -> BTreeSet<String> {
    paths.iter().map(|p| (*p).to_string()).collect()
}

#[test]
fn a_file_a_cfg_test_declaration_takes_in_is_test_code_with_the_files_it_declares() {
    // Wrapped so that no line starts with a cfg, which the release ci/gate.pin names reads as a skip added to this
    // test's body until the pin reaches 0.12.0 (#47).
    let lib = "mod real;\n#[cfg(test)]\nmod fakes;\n#[cfg(all(test, unix))]\n#[path = \"support/helpers.rs\"]\n\
               mod helpers;\n#[cfg(test)]\nmod tests {\n    mod cases;\n}\n";
    let files = [
        ("src/lib.rs", lib),
        ("src/real.rs", "pub fn one() {}\n"),
        ("src/fakes/mod.rs", "mod clock;\n"),
        ("src/fakes/clock.rs", "pub fn now() {}\n"),
        ("src/support/helpers.rs", "pub fn help() {}\n"),
        ("src/tests/cases.rs", "#[test]\nfn case() {}\n"),
    ];
    let expected = paths(&["src/fakes/mod.rs", "src/fakes/clock.rs", "src/support/helpers.rs", "src/tests/cases.rs"]);
    assert_eq!(test_modules(&files), expected);
}

#[test]
fn a_file_declared_by_test_code_and_by_code_alike_is_code_as_is_a_crate_root() {
    let files = [
        ("src/lib.rs", "mod shared;\n"),
        ("src/main.rs", "#[cfg(test)]\nmod shared;\n"),
        ("src/shared.rs", "pub fn s() {}\n"),
        ("tests/it.rs", "#[test]\nfn it() {}\n"),
        ("src/broken.rs", "fn broken( {\n"),
    ];
    assert_eq!(test_modules(&files), BTreeSet::new());
}

#[test]
fn a_file_that_opens_with_cfg_test_is_test_code_with_the_files_it_declares() {
    let files = [
        ("src/lib.rs", "mod only;\n"),
        ("src/only.rs", "#![cfg(test)]\nmod deeper;\n"),
        ("src/only/deeper.rs", "pub fn d() {}\n"),
        ("src/bin/check.rs", "#![cfg(test)]\nfn main() {}\n"),
    ];
    assert_eq!(test_modules(&files), paths(&["src/only.rs", "src/only/deeper.rs", "src/bin/check.rs"]));
}

#[test]
fn a_file_declared_by_test_code_first_and_by_code_after_is_code() {
    // The test declaration of util.rs comes first, by #[path], and so does that of shared.rs, by both children.
    let lib = "#[cfg(test)]\n#[path = \"util.rs\"]\nmod test_util;\nmod util;\n#[cfg(test)]\nmod a;\nmod b;\n";
    let files = [
        ("src/lib.rs", lib),
        ("src/util.rs", "pub fn u() {}\n"),
        ("src/a.rs", "#[path = \"shared.rs\"]\nmod shared;\n"),
        ("src/b.rs", "#[path = \"shared.rs\"]\nmod shared;\n"),
        ("src/shared.rs", "pub fn s() {}\n"),
    ];
    assert_eq!(test_modules(&files), paths(&["src/a.rs"]));
}
