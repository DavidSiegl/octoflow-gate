//! The cognitive-complexity check on Go read by lang_go: the score of each construct, each expected score added up in
//! the comments, one increment per line; then the check over a change to a small Go module.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsStr;

use checks::complexity::{scores, too_complex, LIMIT};
use checks::ChangedFile;
use common::{added, edited};

/// Each function of `source`, Go, that the check judges, by its name as the check gives it, with its score.
fn scores_of(source: &str) -> BTreeMap<String, u32> {
    let file = lang_go::read(source.as_bytes(), false).expect("the test's Go parses");
    file.functions.iter().zip(scores(&file)).map(|(f, n)| (f.name.clone(), n)).collect()
}

/// The score of the one function of `source`, Go.
fn score(source: &str) -> u32 {
    let scores = scores_of(source);
    assert_eq!(scores.len(), 1, "one function in {scores:?}");
    scores.into_values().next().unwrap_or_default()
}

#[test]
fn if_else_if_and_else_add_one_and_nest_their_blocks() {
    assert_eq!(
        score(
            r#"package p
func f(a int) int {
	if a == 1 {                         // +1
		return 1
	} else if b := a * 2; b == 4 {      // +1
		if a > 0 { return 2 } else { return 3 }   // +2, +1
	} else {                            // +1
		if a > 0 { return 4 }           // +2
	}
	return 0
}
"#
        ),
        8
    );
}

#[test]
fn every_form_of_for_adds_one_and_its_nesting() {
    assert_eq!(
        score(
            r#"package p
func f(items []int) {
	for {                               // +1
		for i := 0; i < 3; i++ {        // +2
			for i < 2 {                 // +3
			}
		}
	}
	for _, x := range items {           // +1
		_ = x
	}
}
"#
        ),
        7
    );
}

#[test]
fn a_switch_a_type_switch_and_a_select_add_one_for_all_their_cases_which_they_nest() {
    assert_eq!(
        score(
            r#"package p
func f(x any, ch chan int) {
	switch y := 2; y {                  // +1
	case 1, 2:
		if y > 1 {}                     // +2
	default:
	}
	switch v := x.(type) {              // +1
	case int:
		_ = v
	case string:
	}
	select {                            // +1
	case n := <-ch:
		if n > 0 {}                     // +2
	default:
	}
	switch {                            // +1
	case x == nil:
	}
}
"#
        ),
        8
    );
}

#[test]
fn a_labelled_break_or_continue_and_a_goto_add_one_and_a_plain_jump_nothing() {
    assert_eq!(
        score(
            r#"package p
func f(items []int) {
outer:
	for _, x := range items {           // +1
		for range items {               // +2
			if x > 1 {                  // +3
				continue outer          // +1
			}
			if x > 2 {                  // +3
				break outer             // +1
			}
			break
		}
	}
	goto end                            // +1
end:
}
"#
        ),
        12
    );
}

#[test]
fn each_run_of_like_boolean_operators_adds_one_through_parentheses() {
    assert_eq!(
        score(
            r#"package p
func f(a, b, c, d bool) bool {
	return a && b && c ||               // +1, +1
		(d || a) &&                     // +1
		!(b && c)                       // +1
}
"#
        ),
        4
    );
}

#[test]
fn a_func_literal_nests_its_body_and_counts_toward_its_function() {
    assert_eq!(
        score(
            r#"package p
func f(items []int) func() {
	if len(items) > 0 {                 // +1
		return func() {
			for range items {}          // +3
		}
	}
	return func() {
		if true {}                      // +2
	}
}
"#
        ),
        6
    );
}

#[test]
fn a_function_calling_itself_or_on_a_cycle_adds_one_and_another_package_s_function_nothing() {
    let scores = scores_of(
        r#"package p
func ping(n int) { ping(n); pong(n) }  // +1
func pong(n int) { ping(n) }           // +1
func generic[T any](v T) { generic[T](v) }  // +1
func other(n int) { strconv.Itoa(n) }
func (s *Stack) Push(v int) { s.Push(v) }  // +1
func (s Stack) Len() int { var o Stack; return o.Len() }
func Len() int { return 0 }
"#,
    );
    let expected =
        [("ping", 1), ("pong", 1), ("generic", 1), ("other", 0), ("Stack.Push", 1), ("Stack.Len", 0), ("Len", 0)];
    assert_eq!(scores, expected.iter().map(|&(n, s)| (n.to_string(), s)).collect());
}

#[test]
fn a_method_is_named_by_its_receiver_s_type_and_a_function_without_a_body_is_left_out() {
    let file = lang_go::read(
        b"package p\n\n// Push adds v.\n// It grows s.\nfunc (s *Stack[T]) Push(v T) {\n}\n\nfunc asm(x int) int\n",
        false,
    )
    .unwrap();
    let functions: Vec<_> =
        file.functions.iter().map(|f| (f.name.as_str(), f.line, f.lines.first, f.lines.last)).collect();
    assert_eq!(functions, [("Stack.Push", 5, 3, 6)]);
}

#[test]
fn the_functions_of_a_test_file_are_exempt() {
    let file = lang_go::read(b"package p\nfunc helper() {}\n", true).unwrap();
    assert!(file.functions.iter().all(|f| f.exempt));
}

/// A Go function of `n` ifs one after the other, so of cognitive complexity `n`.
fn flat(name: &str, n: u32) -> String {
    let body: String = (0..n).map(|k| format!("\tif x == {k} {{\n\t\tx += {k}\n\t}}\n")).collect();
    format!("func {name}(x int) int {{\n{body}\treturn x\n}}\n")
}

/// The file the check-level tests change: grow just under the limit, and small well under it.
fn base() -> String {
    format!("package calc\n\n{}\n{}", flat("grow", 14), flat("small", 3))
}

const CALC: &str = "calc/calc.go";

/// (path, line, message) of each of the check's findings on `files`, nothing exempt by path: Go's tests are exempt by
/// their functions.
fn found(files: &[ChangedFile]) -> Vec<(String, Option<u32>, String)> {
    let findings = too_complex(files, &|_: &OsStr| false);
    findings.into_iter().map(|f| (f.path.unwrap_or_default().to_string_lossy().into(), f.line, f.message)).collect()
}

#[test]
fn a_new_function_over_the_limit_is_found_and_one_at_it_is_not() {
    assert_eq!(LIMIT, 15);
    let now = format!("{}\n{}\n{}", base(), flat("newBig", 16), flat("newOK", 15));
    let line = now.lines().position(|l| l.starts_with("func newBig(")).unwrap() as u32 + 1;
    let why = "`newBig`: cognitive complexity 16, a new function; the limit is 15.";
    assert_eq!(found(&[edited(CALC, &base(), &now)]), vec![(CALC.into(), Some(line), why.into())]);
}

#[test]
fn a_function_the_change_raises_over_the_limit_is_found_at_its_line() {
    let now =
        base().replacen("\tif x == 13 {\n\t\tx += 13\n", "\tif x == 13 {\n\t\tx += 13\n\t\tif x > 1 {\n\t\t}\n", 1);
    let why = "`grow`: cognitive complexity 16, 14 at the base; the limit is 15.";
    assert_eq!(found(&[edited(CALC, &base(), &now)]), vec![(CALC.into(), Some(3), why.into())]);
}

#[test]
fn a_function_over_the_limit_in_a_test_file_is_not_found() {
    let test = format!("package calc\n\n{}", flat("helper", 20));
    assert_eq!(found(&[added("calc/calc_test.go", &test)]), vec![]);
}

#[test]
fn a_file_that_does_not_parse_as_go_is_not_scored() {
    let found = found(&[edited(CALC, &base(), &format!("{}\nfunc broken( {{\n", base()))]);
    assert_eq!(found.len(), 1, "{found:?}");
    assert!(found[0].2.starts_with("does not parse as Go ("), "{found:?}");
}

#[test]
fn a_method_whose_type_is_renamed_keeps_its_base_score() {
    // big is over the limit at the base, and the change renames its receiver's type alone.
    let method = |ty: &str| flat("big", 16).replacen("func big(", &format!("func (s *{ty}) big("), 1);
    let before = format!("package calc\n\ntype Stack struct{{}}\n\n{}", method("Stack"));
    let after = format!("package calc\n\ntype Queue struct{{}}\n\n{}", method("Queue"));
    assert_eq!(found(&[edited(CALC, &before, &after)]), vec![]);
}

#[test]
fn a_function_made_a_method_keeps_its_base_score() {
    let before = format!("package calc\n\ntype Stack struct{{}}\n\n{}", flat("big", 16));
    let after = before.replacen("func big(", "func (s *Stack) big(", 1);
    assert_eq!(found(&[edited(CALC, &before, &after)]), vec![]);
}
