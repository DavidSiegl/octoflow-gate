//! The dependency-cycles check on Python, its graphs read with lang_python::imports.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use checks::cycles::new_cycles;
use model::Import;

/// A tree of `files`, each (path, source), as lang_python::imports takes it.
fn tree(files: &[(&str, &str)]) -> Vec<(OsString, Vec<u8>)> {
    files.iter().map(|&(path, source)| (path.into(), source.as_bytes().to_vec())).collect()
}

/// (imported file, line, deferred) of each import `path` makes in `files`.
fn imports_of(files: &[(&str, &str)], path: &str) -> Vec<(String, u32, bool)> {
    let graph = lang_python::imports(&tree(files));
    let shown = |i: &Import| (i.module.to_string_lossy().into_owned(), i.line, i.deferred);
    graph[&OsString::from(path)].iter().map(shown).collect()
}

/// (path, line, cycle) of each finding on the change from `base` to `now`, `added` each file's added lines, tests
/// exempt; the cycle as the message names it, its files joined by `→`.
fn found(base: &[(&str, &str)], now: &[(&str, &str)], added: &[(&str, &[u32])]) -> Vec<(String, u32, String)> {
    let added: BTreeMap<OsString, BTreeSet<u32>> =
        added.iter().map(|&(path, lines)| (path.into(), lines.iter().copied().collect())).collect();
    let exempt = |path: &std::ffi::OsStr| path.to_string_lossy().starts_with("tests/");
    let found =
        new_cycles(&lang_python::imports(&tree(base)), &lang_python::imports(&tree(now)), &added, &exempt);
    let cycle = |message: &str| message.split(", ").nth(1).unwrap().split(':').next().unwrap().replace('`', "");
    let at = |f: checks::Finding| (f.path.unwrap().to_string_lossy().into_owned(), f.line.unwrap(), cycle(&f.message));
    found.into_iter().map(at).collect()
}

#[test]
fn imports_name_the_files_of_the_tree_they_load_and_leave_out_the_rest() {
    let files = [
        (
            "app/main.py",
            "import os\nimport app.util\nimport app.pkg.deep as d\nfrom app import util\n\
             from app.pkg import deep, NAME\nfrom . import util\nfrom .pkg import deep\nfrom .. import top\n",
        ),
        ("app/util.py", ""),
        ("app/pkg/__init__.py", "NAME = 1\n"),
        ("app/pkg/deep.py", ""),
        ("top.py", ""),
    ];
    let pkg_or_deep = |line| ("app/pkg/deep.py".to_string(), line, false);
    assert_eq!(
        imports_of(&files, "app/main.py"),
        [
            ("app/util.py".into(), 2, false),
            pkg_or_deep(3),
            ("app/util.py".into(), 4, false),
            pkg_or_deep(5),
            ("app/pkg/__init__.py".into(), 5, false),
            ("app/util.py".into(), 6, false),
            pkg_or_deep(7),
            ("top.py".into(), 8, false),
        ]
    );
}

#[test]
fn a_module_under_src_is_found_from_there() {
    let files = [("src/lib/a.py", "from lib import b\n"), ("src/lib/b.py", "")];
    assert_eq!(imports_of(&files, "src/lib/a.py"), [("src/lib/b.py".into(), 1, false)]);
}

#[test]
fn an_import_in_a_function_is_deferred_and_one_only_a_type_checker_runs_is_none() {
    let files = [
        (
            "a.py",
            "from typing import TYPE_CHECKING\nif TYPE_CHECKING:\n    import b\nelse:\n    import c\n\
             def f():\n    import b\n",
        ),
        ("b.py", ""),
        ("c.py", ""),
    ];
    assert_eq!(imports_of(&files, "a.py"), [("c.py".into(), 5, false), ("b.py".into(), 7, true)]);
}

#[test]
fn a_third_party_import_named_as_a_local_module_elsewhere_is_none() {
    // `json` is a module of the tree only inside `app`, which an absolute `import json` never reaches.
    let files = [("main.py", "import json\nimport yaml\n"), ("app/json.py", "")];
    assert_eq!(imports_of(&files, "main.py"), []);
}

#[test]
fn a_change_closing_a_cycle_of_two_or_of_three_modules_is_found_at_the_closing_import() {
    let base =
        [("a.py", "import b\n"), ("b.py", "x = 1\n"), ("c.py", "import d\n"), ("d.py", "import e\n"), ("e.py", "")];
    let now = [
        ("a.py", "import b\n"),
        ("b.py", "x = 1\nimport a\n"),
        ("c.py", "import d\n"),
        ("d.py", "import e\n"),
        ("e.py", "from c import thing\n"),
    ];
    assert_eq!(
        found(&base, &now, &[("b.py", &[2]), ("e.py", &[1])]),
        [
            ("b.py".into(), 2, "b.py → a.py → b.py".into()),
            ("e.py".into(), 1, "e.py → c.py → d.py → e.py".into())
        ]
    );
}

#[test]
fn an_import_added_to_a_cycle_the_base_has_is_not_found() {
    let base = [("a.py", "import b\n"), ("b.py", "import a\n")];
    let now = [("a.py", "import b\nfrom b import x\n"), ("b.py", "import a\n")];
    assert_eq!(found(&base, &now, &[("a.py", &[2])]), []);
}

#[test]
fn a_deferred_import_that_closes_a_cycle_is_found() {
    let base = [("a.py", "import b\n"), ("b.py", "")];
    let now = [("a.py", "import b\n"), ("b.py", "def f():\n    import a\n")];
    assert_eq!(found(&base, &now, &[("b.py", &[1, 2])]), [("b.py".into(), 2, "b.py → a.py → b.py".into())]);
}

#[test]
fn a_cycle_through_a_test_file_is_not_found() {
    let now = [("a.py", "import tests.helper\n"), ("tests/helper.py", "import a\n")];
    assert_eq!(found(&[], &now, &[("a.py", &[1]), ("tests/helper.py", &[1])]), []);
}
