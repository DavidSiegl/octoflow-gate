//! The vague-names check on Go, read with lang_go.

mod common;

use checks::names::vague_names;

const BASE: &str = "package calc\n\nfunc old(value int) int {\n\treturn value\n}\n";

/// The (line, name) each finding on calc/calc.go, edited from [`BASE`] to BASE and `after`, reports, its line counted
/// from the end of BASE; and that each is a vague-name warning on calc/calc.go.
fn names(after: &str) -> Vec<(u32, String)> {
    let found = vague_names(&[common::edited("calc/calc.go", BASE, &format!("{BASE}{after}"))], true);
    let base = u32::try_from(BASE.lines().count()).unwrap();
    for f in &found {
        assert_eq!(
            (f.path.as_deref(), f.rule.as_str(), f.severity.as_str()),
            (Some("calc/calc.go".as_ref()), "vague-names vague-name", "warning"),
            "{f:?}"
        );
    }
    found.iter().map(|f| (f.line.unwrap() - base, f.message.split('`').nth(1).unwrap().to_string())).collect()
}

#[test]
fn each_binding_kind_an_added_line_binds_is_found_once_at_its_first_binding() {
    let added = r#"type RunInfo struct{ data int }
type Status = int
type StateManager interface{ Handle() }
const maxValue = 1
var rawData, _ = 2, 3
func handle(rowInfo int, _ string) (userData int, err error) {
	lineValue := rowInfo
	for i, itemStatus := range []int{lineValue} { _ = i; _ = itemStatus }
	if okData := true; okData {}
	switch kindInfo := any(1).(type) { default: _ = kindInfo }
	select { case msgData := <-make(chan int): _ = msgData; default: }
	f := func(argValue int) {}
	f(1)
	lineValue = 2
	return
}
func (m *RunInfo) Value() int { return 0 }
"#;
    let expected = [
        (1, "RunInfo"),
        (2, "Status"),
        (3, "StateManager"),
        (3, "Handle"),
        (4, "maxValue"),
        (5, "rawData"),
        (6, "handle"),
        (6, "rowInfo"),
        (6, "userData"),
        (7, "lineValue"),
        (8, "itemStatus"),
        (9, "okData"),
        (10, "kindInfo"),
        (11, "msgData"),
        (12, "argValue"),
        (17, "Value"),
    ];
    assert_eq!(names(added), expected.map(|(l, n)| (l, n.to_string())));
}

#[test]
fn a_name_bound_on_a_line_the_change_did_not_add_is_not_found() {
    assert_eq!(names("func fresh() {}\n"), vec![]);
}

#[test]
fn an_import_s_own_name_is_a_binding_and_its_path_is_not() {
    let after = "package calc\n\nimport (\n\tpkgData \"example.com/data\"\n\t\"example.com/info\"\n)\n";
    let found = vague_names(&[common::edited("calc/calc.go", "package calc\n", after)], true);
    let lines: Vec<_> = found.iter().map(|f| (f.line, f.message.split('`').nth(1).unwrap())).collect();
    assert_eq!(lines, [(Some(4), "pkgData")]);
}
