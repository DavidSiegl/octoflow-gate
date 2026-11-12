//! The dependency-cycles check on Rust, its graphs read with lang_rust::imports.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use checks::cycles::new_cycles;
use model::Import;

/// A tree of `files`, each (path, source), as lang_rust::imports takes it.
fn tree(files: &[(&str, &str)]) -> Vec<(OsString, Vec<u8>)> {
    files.iter().map(|&(path, source)| (path.into(), source.as_bytes().to_vec())).collect()
}

/// (imported file, line) of each import `path` makes in `files`.
fn imports_of(files: &[(&str, &str)], path: &str) -> Vec<(String, u32)> {
    let graph = lang_rust::imports(&tree(files));
    let shown = |i: &Import| {
        assert!(!i.deferred);
        (i.module.to_string_lossy().into_owned(), i.line)
    };
    graph[&OsString::from(path)].iter().map(shown).collect()
}

/// (path, line, cycle) of each finding on the change from `base` to `now`, `added` each file's added lines, files
/// under `tests/` exempt; the cycle as the message names it, its files joined by `→`.
fn found(base: &[(&str, &str)], now: &[(&str, &str)], added: &[(&str, &[u32])]) -> Vec<(String, u32, String)> {
    let added: BTreeMap<OsString, BTreeSet<u32>> =
        added.iter().map(|&(path, lines)| (path.into(), lines.iter().copied().collect())).collect();
    let exempt = |path: &std::ffi::OsStr| path.to_string_lossy().starts_with("tests/");
    let found = new_cycles(&lang_rust::imports(&tree(base)), &lang_rust::imports(&tree(now)), &added, &exempt);
    let cycle = |message: &str| message.split(", ").nth(1).unwrap().split(':').next().unwrap().replace('`', "");
    let at = |f: checks::Finding| (f.path.unwrap().to_string_lossy().into_owned(), f.line.unwrap(), cycle(&f.message));
    found.into_iter().map(at).collect()
}

const LIB: &str = "mod a;\nmod b;\nmod c;\nmod nested;\n";

#[test]
fn uses_and_paths_in_code_name_the_files_of_the_modules_they_reach() {
    let files = [
        ("src/lib.rs", LIB),
        (
            "src/a.rs",
            "use crate::b::{self, Thing as T};\nuse crate::{\n    c::*,\n    nested::inner::Deep,\n};\n\
             use std::fmt;\nuse other_crate::x;\nfn f() -> crate::c::Kind {\n    super::b::g()\n}\n",
        ),
        ("src/b.rs", ""),
        ("src/c.rs", ""),
        ("src/nested/mod.rs", "pub mod inner;\n"),
        ("src/nested/inner.rs", "pub struct Deep;\n"),
    ];
    assert_eq!(
        imports_of(&files, "src/a.rs"),
        [
            ("src/b.rs".into(), 1),
            ("src/c.rs".into(), 3),
            ("src/nested/inner.rs".into(), 4),
            ("src/c.rs".into(), 8),
            ("src/b.rs".into(), 9),
        ]
    );
}

#[test]
fn a_module_below_a_file_named_for_it_and_an_inline_one_resolve_to_their_files() {
    let files = [
        ("src/main.rs", "mod outer;\nmod side;\n"),
        ("src/outer.rs", "mod child;\npub mod inline {\n    pub fn f() {}\n}\n"),
        ("src/outer/child.rs", "use crate::side::S;\n"),
        ("src/side.rs", "use crate::outer::inline::f;\n#[path = \"elsewhere.rs\"]\nmod moved;\n"),
        ("src/elsewhere.rs", "use crate::outer::child;\n"),
    ];
    assert_eq!(imports_of(&files, "src/outer/child.rs"), [("src/side.rs".into(), 1)]);
    assert_eq!(imports_of(&files, "src/side.rs"), [("src/outer.rs".into(), 1)]);
    // `moved` is side's own module, but outer::child is not.
    assert_eq!(imports_of(&files, "src/elsewhere.rs"), [("src/outer/child.rs".into(), 1)]);
}

#[test]
fn an_import_between_a_module_and_one_it_holds_is_none() {
    let files = [
        ("src/lib.rs", "mod a;\nuse a::X;\nuse crate::a::Y;\n"),
        ("src/a.rs", "mod b;\nuse super::Z;\nuse crate::a::b::W;\n"),
        ("src/a/b.rs", "use super::super::Z;\nuse crate::a::Y;\n"),
    ];
    assert_eq!(imports_of(&files, "src/lib.rs"), []);
    assert_eq!(imports_of(&files, "src/a.rs"), []);
    assert_eq!(imports_of(&files, "src/a/b.rs"), []);
}

#[test]
fn test_code_imports_nothing() {
    let files = [
        ("src/lib.rs", "mod a;\nmod b;\n#[cfg(test)]\nmod checks;\n"),
        ("src/a.rs", "#[cfg(test)]\nmod tests {\n    use crate::b::X;\n}\n#[cfg(test)]\nuse crate::b::Y;\n"),
        ("src/b.rs", "#![cfg(test)]\nuse crate::a::X;\n"),
        ("src/checks.rs", "use crate::a::X;\n"),
    ];
    assert_eq!(imports_of(&files, "src/a.rs"), []);
    assert_eq!(imports_of(&files, "src/b.rs"), []);
    assert_eq!(imports_of(&files, "src/checks.rs"), []);
}

#[test]
fn a_file_two_crate_roots_reach_is_a_module_of_each() {
    let files = [
        ("tests/one.rs", "mod common;\nmod helpers;\n"),
        ("tests/two.rs", "mod common;\n"),
        ("tests/common/mod.rs", "use crate::helpers::H;\n"),
        ("tests/helpers.rs", ""),
    ];
    assert_eq!(imports_of(&files, "tests/common/mod.rs"), [("tests/helpers.rs".into(), 1)]);
}

#[test]
fn a_file_that_does_not_parse_imports_nothing() {
    let files = [("src/lib.rs", "mod a;\nmod b;\n"), ("src/a.rs", "use crate::b::X;\nfn (\n"), ("src/b.rs", "")];
    assert_eq!(imports_of(&files, "src/a.rs"), []);
}

#[test]
fn a_change_closing_a_cycle_of_two_or_of_three_modules_is_found_at_the_closing_import() {
    let lib = ("src/lib.rs", "mod a;\nmod b;\nmod c;\nmod d;\nmod e;\n");
    let base = [
        lib,
        ("src/a.rs", "use crate::b::X;\n"),
        ("src/b.rs", "pub struct X;\n"),
        ("src/c.rs", "use crate::d::X;\n"),
        ("src/d.rs", "use crate::e::X;\n"),
        ("src/e.rs", ""),
    ];
    let now = [
        lib,
        ("src/a.rs", "use crate::b::X;\n"),
        ("src/b.rs", "pub struct X;\nuse super::a;\n"),
        ("src/c.rs", "use crate::d::X;\n"),
        ("src/d.rs", "use crate::e::X;\n"),
        ("src/e.rs", "fn f() {\n    crate::c::g();\n}\n"),
    ];
    assert_eq!(
        found(&base, &now, &[("src/b.rs", &[2]), ("src/e.rs", &[1, 2, 3])]),
        [
            ("src/b.rs".into(), 2, "src/b.rs → src/a.rs → src/b.rs".into()),
            ("src/e.rs".into(), 2, "src/e.rs → src/c.rs → src/d.rs → src/e.rs".into())
        ]
    );
}

#[test]
fn a_cycle_closed_by_editing_the_path_line_of_a_multi_line_use_is_found() {
    let lib = ("src/lib.rs", "mod a;\nmod b;\nmod old;\n");
    let rest = [("src/b.rs", "use crate::a::Y;\n"), ("src/old.rs", "")];
    let base = [lib, ("src/a.rs", "use crate::old::{\n    X,\n};\n"), rest[0], rest[1]];
    let now = [lib, ("src/a.rs", "use crate::b::{\n    X,\n};\n"), rest[0], rest[1]];
    assert_eq!(
        found(&base, &now, &[("src/a.rs", &[1])]),
        [("src/a.rs".into(), 1, "src/a.rs → src/b.rs → src/a.rs".into())]
    );
}

#[test]
fn an_import_added_to_a_cycle_the_base_has_is_not_found() {
    let lib = ("src/lib.rs", "mod a;\nmod b;\n");
    let base = [lib, ("src/a.rs", "use crate::b::X;\n"), ("src/b.rs", "use crate::a::Y;\n")];
    let now = [lib, ("src/a.rs", "use crate::b::X;\nuse crate::b::Z;\n"), ("src/b.rs", "use crate::a::Y;\n")];
    assert_eq!(found(&base, &now, &[("src/a.rs", &[2])]), []);
}

#[test]
fn a_cycle_through_an_exempt_file_is_not_found() {
    let now = [
        ("src/lib.rs", "mod a;\n#[path = \"../tests/helper.rs\"]\nmod helper;\n"),
        ("src/a.rs", "use crate::helper::H;\n"),
        ("tests/helper.rs", "use crate::a::A;\n"),
    ];
    assert_eq!(imports_of(&now, "tests/helper.rs"), [("src/a.rs".into(), 1)]);
    assert_eq!(found(&[], &now, &[("src/a.rs", &[1]), ("tests/helper.rs", &[1])]), []);
}
