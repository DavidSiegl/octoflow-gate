//! The vague-names check on Rust, read with lang_rust: tests/test_gate.py's TestRustVagueNames, the cases that do not
//! depend on gate.py's gate-rust helper.

mod common;

use checks::names::vague_names;
use checks::{ChangedFile, Finding};

const BASE: &str = "fn old(value: u32) -> u32 {\n    value\n}\n";

/// The findings of the check on `files`.
fn found(files: &[ChangedFile]) -> Vec<Finding> {
    vague_names(files, true)
}

/// The name each finding on src/lib.rs, edited from [`BASE`] to `after`, reports, with its line counted from the end
/// of BASE; and that each is a vague-name warning on src/lib.rs.
fn names(after: &str) -> Vec<(u32, String)> {
    let found = found(&[common::edited("src/lib.rs", BASE, after)]);
    let base = u32::try_from(BASE.lines().count()).unwrap();
    for f in &found {
        assert_eq!(
            (f.path.as_deref(), f.rule.as_str(), f.severity.as_str()),
            (Some("src/lib.rs".as_ref()), "vague-names vague-name", "warning"),
            "{f:?}"
        );
    }
    found.iter().map(|f| (f.line.unwrap() - base, f.message.split('`').nth(1).unwrap().to_string())).collect()
}

/// `expected`'s (line, name) pairs as [`names`] gives them.
fn pairs(expected: &[(u32, &str)]) -> Vec<(u32, String)> {
    expected.iter().map(|&(line, name)| (line, name.to_string())).collect()
}

#[test]
fn each_binding_kind_an_added_line_binds_is_found_once_at_its_first_binding() {
    let added = "use std::fmt::Display as ShownValue;
const MAX_VALUE: u32 = 1;
static RAW_DATA: u32 = 2;
struct RunInfo;
enum Status { Up }
union Handle { a: u32 }
trait StateManager { type Info; const LIMIT_VALUE: u32; }
type Data = u32;
fn handle(raw_info: u32, (a, b_status): (u32, u32)) -> u32 {
    let line_value = raw_info;
    for row_data in [a] {}
    match Some(b_status) { Some(inner_handle) => {} None => {} }
    if let Some(maybe_info) = Some(1) {}
    while let Some(next_status) = None::<u32> {}
    let add = |left_value: u32| left_value;
    let line_value = add(line_value);
    line_value
}
";
    assert_eq!(
        names(&(BASE.to_string() + added)),
        pairs(&[
            (1, "ShownValue"),
            (2, "MAX_VALUE"),
            (3, "RAW_DATA"),
            (4, "RunInfo"),
            (5, "Status"),
            (6, "Handle"),
            (7, "StateManager"),
            (7, "Info"),
            (7, "LIMIT_VALUE"),
            (8, "Data"),
            (9, "handle"),
            (9, "raw_info"),
            (9, "b_status"),
            (10, "line_value"),
            (11, "row_data"),
            (12, "inner_handle"),
            (13, "maybe_info"),
            (14, "next_status"),
            (15, "left_value"),
        ])
    );
}

#[test]
fn a_camel_case_name_is_vague_only_by_its_last_word() {
    let after = BASE.to_string() + "struct InfoStore;\nstruct HTTPStatus;\nstruct Valuer;\nfn result() {}\n";
    assert_eq!(names(&after), pairs(&[(2, "HTTPStatus")]));
}

#[test]
fn a_trait_impl_s_fn_takes_its_name_and_parameters_from_the_trait() {
    let added = "struct Door;
impl Opener for Door {
    fn handle(&self, raw_data: u32) -> u32 {
        let shown_value = raw_data;
        fn inner(own_info: u32) {}
        shown_value
    }
}
impl Door {
    fn status(&self, raw_data: u32) {}
}
";
    assert_eq!(
        names(&(BASE.to_string() + added)),
        pairs(&[(4, "shown_value"), (5, "own_info"), (10, "status"), (10, "raw_data")])
    );
}

#[test]
fn a_binding_on_a_line_the_change_left_alone_is_not_found() {
    let after = BASE.replace("    value\n", "    value + 1\n");
    assert_eq!(found(&[common::edited("src/lib.rs", BASE, &after)]), vec![]);
}

#[test]
fn a_file_that_does_not_parse_is_a_finding_at_the_line_it_fails_on() {
    // src/lib.rs comes before src/m.rs, so a check that stopped at it would not read src/m.rs.
    let files = [
        common::edited("src/lib.rs", BASE, &(BASE.to_string() + "fn broken( {\n")),
        common::added("src/m.rs", "struct RunInfo;\n"),
    ];
    let found = found(&files);
    let at: Vec<_> = found.iter().map(|f| (f.path.as_deref().unwrap(), f.line, f.rule.as_str())).collect();
    assert_eq!(
        at,
        [
            ("src/lib.rs".as_ref(), Some(4), "vague-names not-read"),
            ("src/m.rs".as_ref(), Some(1), "vague-names vague-name"),
        ]
    );
    assert!(found[0].message.starts_with("does not parse as Rust ("), "{}", found[0].message);
    assert!(found[0].message.ends_with("), so its names were not read."), "{}", found[0].message);
    assert_eq!(found[0].severity, "warning");
}

#[test]
fn a_file_the_change_adds_no_line_to_is_not_read_nor_one_it_deletes() {
    let unparsable = "fn broken( {\n";
    let files = [
        common::edited("src/lib.rs", unparsable, unparsable),
        common::deleted("src/gone.rs", "struct RunInfo;\n"),
    ];
    assert_eq!(found(&files), vec![]);
}

#[test]
fn a_name_bound_on_several_added_lines_is_found_once_at_its_first() {
    let after = BASE.to_string() + "fn f(a: u32) { let raw_data = a; }\nfn g(raw_data: u32) {}\n";
    assert_eq!(names(&after), pairs(&[(1, "raw_data")]));
}
