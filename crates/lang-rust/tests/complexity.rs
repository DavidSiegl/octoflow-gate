//! The cognitive-complexity check on Rust read by lang_rust: the score of each construct, by the rules above
//! [`LIMIT`], each expected score added up in the comments, one increment per line; then the check over a change.
//! tests/test_gate.py's TestRustCognitiveComplexity and TestTooComplexRustFunctions.

mod common;

use std::collections::BTreeMap;
use std::ffi::OsStr;

use checks::complexity::{scores, too_complex, LIMIT};
use checks::ChangedFile;
use common::{added, changed, deleted, edited};

/// Each fn of `source`, Rust, that the check judges as one, by its name as the check gives it, with its score: test
/// code left out, as the check never judges it.
fn scores_of(source: &str) -> BTreeMap<String, u32> {
    let file = lang_rust::read(source.as_bytes(), false).expect("the test's Rust parses");
    let scored = file.functions.iter().zip(scores(&file));
    scored.filter(|(f, _)| !f.exempt).map(|(f, n)| (f.name.clone(), n)).collect()
}

/// The score of the one fn of `source`, Rust, that the check judges as one.
fn score(source: &str) -> u32 {
    let scores = scores_of(source);
    assert_eq!(scores.len(), 1, "one judged fn in {scores:?}");
    scores.into_values().next().unwrap_or_default()
}

/// `pairs` as [`scores_of`] gives them.
fn named(pairs: &[(&str, u32)]) -> BTreeMap<String, u32> {
    pairs.iter().map(|&(name, n)| (name.to_string(), n)).collect()
}

#[test]
fn if_else_if_and_else_add_one_and_nest_their_blocks() {
    assert_eq!(score(r#"
fn f(a: i32) -> i32 {
    if a == 1 {             // +1
        1
    } else if a == 2 {      // +1
        if a > 0 { 2 } else { 3 }   // +2, +1
    } else {                // +1
        if a > 0 { 4 } else { 5 }   // +2, +1
    }
}
"#), 9);
}

#[test]
fn what_the_first_block_of_an_if_with_else_ifs_holds_counts() {
    assert_eq!(score(r#"
fn f(a: bool) {
    if a {          // +1
        if a {}     // +2
    } else if a {   // +1
    }
}
"#), 4);
}

#[test]
fn an_if_inside_an_else_block_is_nested_where_an_else_if_is_not() {
    assert_eq!(score(r#"
fn f(a: bool) {
    if a {          // +1
    } else {        // +1
        if a {}     // +2
    }
}
"#), 4);
}

#[test]
fn if_let_and_while_let_count_as_if_and_while() {
    assert_eq!(score(r#"
fn f(mut x: Option<u8>) {
    if let Some(n) = x {            // +1
        while let Some(m) = x {     // +2
            x = None;
        }
    }
}
"#), 3);
}

#[test]
fn loop_while_and_for_add_one_and_their_nesting() {
    assert_eq!(score(r#"
fn f(items: &[u8]) {
    loop {                          // +1
        while true {                // +2
            for i in items {}       // +3
        }
    }
}
"#), 6);
}

#[test]
fn a_condition_and_an_iterator_are_at_their_statements_level() {
    assert_eq!(score(r#"
fn f(a: bool) {
    for i in if a { 0..1 } else { 0..2 } {      // +1, +1 and +1
        while if a { true } else { false } {}   // +2, +2 and +1
        if match a { _ => true } {}             // +2, +2
    }
}
"#), 12);
}

#[test]
fn a_match_adds_one_for_all_its_arms_which_it_nests() {
    // The paper's getWords: a switch is one increment, however many cases it has.
    assert_eq!(score(r#"
fn get_words(n: u8) -> &'static str {
    match n {                           // +1
        1 => "one",
        2 if n > 0 && n < 9 => "a couple",   // the guard's && only: +1
        _ => if n > 9 { "lots" } else { "some" },   // +2, +1
    }
}
"#), 5);
}

#[test]
fn a_let_else_adds_one_and_its_nesting_and_nests_its_else() {
    assert_eq!(score(r#"
fn f(x: Option<u8>) -> u8 {
    let Some(n) = x else {              // +1
        return if x.is_none() { 0 } else { 1 };     // +2, +1
    };
    for _ in 0..n {                     // +1
        let Some(m) = x else { break };     // +2
    }
    n
}
"#), 7);
}

#[test]
fn each_run_of_like_boolean_operators_adds_one() {
    // The paper's `a && b && c || d || e && f`: three runs of like operators.
    assert_eq!(score(r#"
fn f(a: bool, b: bool, c: bool, d: bool, e: bool, g: bool) -> bool {
    a && b && c || d || e && g      // +3
}
"#), 3);
    assert_eq!(score(r#"
fn f(a: bool, b: bool, c: bool) -> bool {
    a || b && c                     // +2
}
"#), 2);
}

#[test]
fn a_parenthesised_operand_continues_the_run_and_a_negation_or_a_call_starts_one() {
    assert_eq!(score(r#"
fn f(a: bool, b: bool, c: bool) -> bool {
    let x = a && (b && c);          // +1
    let y = a && (b || c);          // +2
    let z = a && !(b && c);         // +1, +1
    a || g(b || c)                  // +1, +1
}
"#), 7);
}

#[test]
fn a_let_chain_s_and_is_a_boolean_operator() {
    assert_eq!(score(r#"
fn f(x: Option<u8>, a: bool) {
    if let Some(n) = x && a {}      // +1, +1
}
"#), 2);
}

#[test]
fn a_labelled_break_or_continue_adds_one_and_a_plain_one_nothing() {
    assert_eq!(score(r#"
fn f(items: &[u8]) -> u8 {
    'outer: for i in items {        // +1
        for j in items {            // +2
            if *j == 0 { continue 'outer; }     // +3, +1
            if *j == 1 { break 'outer; }        // +3, +1
            if *j == 2 { continue; }            // +3
        }
    }
    loop { break 7; }               // +1
}
"#), 15);
}

#[test]
fn a_closure_nests_its_body_and_counts_toward_its_fn() {
    assert_eq!(score(r#"
fn f(items: &[u8]) -> usize {
    let g = |x: u8| if x > 1 { 1 } else { 0 };      // +2, +1
    let h = async move || { for i in items {} };    // +2
    items.len()
}
"#), 5);
}

#[test]
fn a_fn_the_body_holds_nests_its_body_and_counts_toward_its_holder() {
    let scores = scores_of(r#"
fn outer(a: bool) {
    fn inner(a: bool) {
        if a {}             // +2
    }
    struct Local;
    impl Local {
        fn method(&self, a: bool) {
            if a {}         // +2
        }
    }
}
"#);
    assert_eq!(
        scores,
        named(&[
            ("outer", 4),
        ])
    );
}

#[test]
fn a_held_fn_that_calls_itself_adds_one() {
    assert_eq!(score(r#"
fn outer() {
    fn inner(n: u8) { inner(n) }                    // +1
    fn holder() { fn deeper() { holder() } }
    fn caller() { inner(1) }
    struct Local;
    impl Local {
        fn method(&self) { self.method() }          // +1
        fn other(&self) { Self::other(self) }       // +1
        fn free(&self) { free() }
    }
    fn by_another() { outer() }
}
"#), 4);  // and outer itself, which by_another calls: +1
}

#[test]
fn recursion_adds_one_to_each_fn_on_a_cycle_of_calls_in_its_file() {
    let scores = scores_of(r#"
fn countdown(n: u8) { if n > 0 { countdown(n - 1) } }    // +1, +1
fn ping(n: u8) { pong(n) }                               // +1
fn pong(n: u8) { ping(n) }                               // +1
fn caller() { ping(1); ping(2) }
fn first(n: u8) { first(n); if n > 0 {} }                // +1, +1
fn solo() { twin() }
mod q {
    pub fn twin() { solo() }
}
struct Pair<T>(T);
impl Pair<Vec<u8>> {
    fn make() { Pair::make() }                           // +1
}
struct S;
impl S {
    fn by_self(&self) { self.by_self() }                 // +1
    fn by_type() { S::by_type() }                        // +1
    fn by_alias() { Self::by_alias() }                   // +1
    fn by_free() { by_free() }
}
trait T {
    fn default(&self) { self.default() }                 // +1
    fn by_trait() { T::by_trait() }                      // +1
}
impl T for S {
    fn default(&self) { self.by_self() }
}
impl std::fmt::Display for S {
    fn fmt(&self) { self.fmt() }                         // +1
}
impl std::fmt::Debug for S {
    fn fmt(&self) {}
}
impl<'a> T for &'a mut S {
    fn by_trait() { Self::by_trait() }                   // +1
}
mod n {
    pub struct S2;
    impl self::S2 {
        pub fn by_path() { S2::by_path() }               // +1
    }
}
trait A { fn run(&self); fn step(&self); }
trait B { fn run(&self); fn step(&self); }
impl A for S {
    fn run(&self) { self.step() }
    fn step(&self) {}
}
impl B for S {
    fn run(&self) {}
    fn step(&self) { self.run() }
}
impl S {
    fn inherent(&self) { self.by_impl() }                // +1
}
impl T for S {
    fn by_impl(&self) { self.inherent() }                // +1
}
impl Clone for S {
    fn clone(&self) -> S { S::clone(self) }              // +1
}
mod m {
    pub fn countdown() { crate::countdown(1) }
    pub fn ping() { super::pong(1); crate::m::ping() }
}
"#);
    assert_eq!(
        scores,
        named(&[
            ("countdown", 2),
            ("ping", 1),
            ("pong", 1),
            ("caller", 0),
            ("first", 2),
            ("solo", 0),
            ("q::twin", 0),
            ("Pair<Vec<u8>>::make", 1),
            ("S::by_self", 1),
            ("S::by_type", 1),
            ("S::by_alias", 1),
            ("S::by_free", 0),
            ("T::default", 1),
            ("T::by_trait", 1),
            ("<S as T>::default", 0),
            ("<S as std::fmt::Display>::fmt", 1),
            ("<S as std::fmt::Debug>::fmt", 0),
            ("<&'a mut S as T>::by_trait", 1),
            ("n::self::S2::by_path", 1),
            ("<S as A>::run", 0),
            ("<S as A>::step", 0),
            ("<S as B>::run", 0),
            ("<S as B>::step", 0),
            ("S::inherent", 1),
            ("<S as T>::by_impl", 1),
            ("<S as Clone>::clone", 1),
            ("m::countdown", 0),
            ("m::ping", 0),
        ])
    );
}

#[test]
fn question_marks_blocks_and_a_macro_s_tokens_add_nothing_but_its_expression_arguments_count() {
    assert_eq!(score(r#"
fn f(x: Result<u8, ()>) -> Result<u8, ()> {
    let v = unsafe { async { x? } };
    let w = { x?.checked_add(1).ok_or(())? };
    println!("{}", if v.is_ok() { 1 } else { 2 });     // +1, +1
    tokio::select! { _ = g() => if true {} }
    Ok(w)
}
"#), 2);
}

#[test]
fn a_fn_is_judged_by_its_path_of_modules_impls_and_traits_but_not_in_a_test_module() {
    // concat! keeps the cfg off the start of a line, where the gate reads it as a skip added to this test's body
    // (TODO.md, "A line of a string literal in a test's body...").
    let scores = scores_of(concat!(r#"
fn free() {}
trait Shape { fn declared(&self); fn defaulted(&self) {} }
impl<'a, T: Clone> std::fmt::Debug for Wrapper<'a, T> {
    fn fmt(&self) {}
}
mod inner {
    pub(crate) fn spread() {}
    #[cfg(test)]
    mod tests {
        fn helper() { if true {} }
        #[test]
        fn checks() {}
    }
}
"#, "#[cfg(all(test, unix))]\n", r#"fn only_in_tests() {}
#[cfg(test)] mod one_line { fn on_its_first_and_last_line() {} }
"#));
    assert_eq!(
        scores,
        named(&[
            ("free", 0),
            ("Shape::defaulted", 0),
            ("<Wrapper<'a,T> as std::fmt::Debug>::fmt", 0),
            ("inner::spread", 0),
            ("only_in_tests", 0),
        ])
    );
}


/// A Rust fn of `n` ifs one after the other, so of cognitive complexity `n`.
fn flat(name: &str, n: u32) -> String {
    let body: String = (0..n).map(|k| format!("    if x == {k} {{\n        x += {k};\n    }}\n")).collect();
    format!("fn {name}(mut x: u32) -> u32 {{\n{body}    x\n}}\n")
}

/// The file the check-level tests change: grow just under the limit, big over it and small well under it.
fn base() -> String {
    format!("{}\n{}\n{}", flat("grow", 14), flat("big", 20), flat("small", 3))
}

const CALC: &str = "engine/src/calc.rs";

/// (path, line, message) of each of the check's findings on `files`, with the gate's default exemptions: the files
/// gate.toml's default `test-files.rust` places among the tests.
fn found(files: &[ChangedFile]) -> Vec<(String, Option<u32>, String)> {
    let exempt = |path: &OsStr| common::among_tests(&path.to_string_lossy());
    let findings = too_complex(files, &exempt);
    findings.into_iter().map(|f| (f.path.unwrap_or_default().to_string_lossy().into(), f.line, f.message)).collect()
}

/// The line `text`'s fn `name` starts on.
fn fn_line(text: &str, name: &str) -> Option<u32> {
    let start = format!("fn {name}(");
    let at = text.lines().position(|l| l.starts_with(&start)).expect("the fn is there");
    u32::try_from(at + 1).ok()
}

/// A finding as [`found`] gives it.
fn finding(path: &str, line: u32, message: &str) -> (String, Option<u32>, String) {
    (path.into(), Some(line), message.into())
}

/// A new fn over the limit in a file of its own, which each check below finds beside what it expects, so that a check
/// that finds nothing at all cannot pass, and the finding on it.
fn fresh() -> (ChangedFile, (String, Option<u32>, String)) {
    let path = "engine/src/fresh.rs";
    let why = "`fresh`: cognitive complexity 16, a new function; the limit is 15.";
    (added(path, &flat("fresh", 16)), finding(path, 1, why))
}

/// [`base`] with an if added inside its kth if, which only grow's and big's first 14 ifs share: grow's then: 2 more,
/// at nesting 1.
fn nested_under(k: u32) -> String {
    let at = format!("    if x == {k} {{\n        x += {k};\n    }}\n    x\n}}\n\nfn big");
    let into = format!("    if x == {k} {{\n        x += {k};\n        if x > 1 {{}}\n    }}\n    x\n}}\n\nfn big");
    base().replacen(&at, &into, 1)
}

#[test]
fn a_new_fn_over_the_limit_is_found_and_one_at_it_is_not() {
    assert_eq!(LIMIT, 15);
    let now = format!("{}\n{}\n{}", base(), flat("new_big", 16), flat("new_ok", 15));
    let why = "`new_big`: cognitive complexity 16, a new function; the limit is 15.";
    let line = fn_line(&now, "new_big").unwrap_or_default();
    assert_eq!(found(&[edited(CALC, &base(), &now)]), vec![finding(CALC, line, why)]);
}

#[test]
fn a_fn_the_change_raises_over_the_limit_is_found_at_its_line() {
    let now = nested_under(13); // grow's last if: 14 becomes 16
    let why = "`grow`: cognitive complexity 16, 14 at the base; the limit is 15.";
    assert_eq!(found(&[edited(CALC, &base(), &now)]), vec![finding(CALC, 1, why)]);
}

#[test]
fn a_fn_over_the_limit_that_the_change_makes_worse_is_found() {
    // big: 20 becomes 22.
    let at = "    if x == 19 {\n        x += 19;\n";
    let now = base().replace(at, &format!("{at}        if x > 1 {{}}\n"));
    let why = "`big`: cognitive complexity 22, 20 at the base; the limit is 15.";
    let line = fn_line(&now, "big").unwrap_or_default();
    assert_eq!(found(&[edited(CALC, &base(), &now)]), vec![finding(CALC, line, why)]);
}

#[test]
fn a_fn_over_the_limit_that_the_change_leaves_no_worse_is_not_found() {
    let now = base().replace("        x += 19;\n", "        x += 190;\n");
    let (fresh, expected) = fresh();
    assert_eq!(found(&[edited(CALC, &base(), &now), fresh]), vec![expected]);
}

#[test]
fn a_fn_moved_unchanged_to_another_file_keeps_its_base() {
    let calc = format!("{}\n{}", flat("grow", 14), flat("small", 3));
    let big = format!("/// Moved.\n{}", flat("big", 20));
    let (fresh, expected) = fresh();
    let files = [edited(CALC, &base(), &calc), added("engine/src/big.rs", &big), fresh];
    assert_eq!(found(&files), vec![expected]);
}

#[test]
fn a_fn_moved_unchanged_out_of_a_file_the_change_deletes_keeps_its_base() {
    // With as many lines again of other fns, which git does not take for calc.rs renamed.
    let others: String = (0..20).map(|k| flat(&format!("other{k}"), 1)).collect();
    let big = flat("big", 20) + &others;
    let (fresh, expected) = fresh();
    let files = [deleted(CALC, &base()), added("engine/src/big.rs", &big), fresh];
    assert_eq!(found(&files), vec![expected]);
}

#[test]
fn a_fn_left_in_a_file_the_change_adds_no_line_to_keeps_its_base_before_a_copy_can() {
    // calc.rs, renamed and only cut, is not judged, but its big is still itself, so the copy is new.
    let kept = format!("{}\n{}", flat("big", 20), flat("small", 3));
    let renamed = changed(Some((CALC, &base())), Some(("engine/src/kept.rs", &kept)));
    assert!(renamed.added.is_empty());
    let copy = "engine/src/copy.rs";
    let why = "`big`: cognitive complexity 20, a new function; the limit is 15.";
    assert_eq!(found(&[renamed, added(copy, &flat("big", 20))]), vec![finding(copy, 1, why)]);
}

#[test]
fn the_tests_are_not_judged() {
    let big = flat("big_test", 20);
    let calc = format!("{}#[cfg(test)]\nmod tests {{\n{big}}}\n", base());
    let (fresh, expected) = fresh();
    let files = [
        added("engine/tests/cli.rs", &big),
        added("engine/src/tests.rs", &big),
        added("engine/tests/broken.rs", "fn broken( {\n"),
        edited(CALC, &base(), &calc),
        fresh,
    ];
    assert_eq!(found(&files), vec![expected]);
}

#[test]
fn the_impls_of_two_traits_for_one_type_name_their_fns_apart() {
    let impls = |a, b| {
        let (a, b) = (flat("from", a), flat("from", b));
        format!("struct X;\nimpl From<u8> for X {{\n{a}}}\nimpl From<u16> for X {{\n{b}}}\n")
    };
    let path = "engine/src/x.rs";
    let why = "`<X as From<u8>>::from`: cognitive complexity 18, 16 at the base; the limit is 15.";
    assert_eq!(found(&[edited(path, &impls(16, 20), &impls(18, 20))]), vec![finding(path, 3, why)]);
}

#[test]
fn a_rust_file_that_does_not_parse_is_not_scored_at_the_line_the_front_end_names() {
    let now = base() + "fn broken( {\n";
    let exempt = |_: &OsStr| false;
    let findings = too_complex(&[edited(CALC, &base(), &now)], &exempt);
    let [finding] = findings.as_slice() else { panic!("one finding, not {findings:?}") };
    // The brace left open is at the line the fn starts on.
    let line = u32::try_from(base().lines().count() + 1).ok();
    let at = (finding.path.as_deref(), finding.line, finding.severity.as_str());
    assert_eq!(at, (Some(OsStr::new(CALC)), line, "error"));
    assert!(finding.message.starts_with("does not parse as Rust ("), "{}", finding.message);
    assert!(finding.message.ends_with("), so its functions were not scored."), "{}", finding.message);
}

#[test]
fn a_base_that_does_not_parse_has_no_functions_to_lend() {
    let why = "`big`: cognitive complexity 20, a new function; the limit is 15.";
    assert_eq!(found(&[edited(CALC, "fn broken( {\n", &flat("big", 20))]), vec![finding(CALC, 1, why)]);
}

#[test]
fn a_fn_whose_body_is_new_is_new_though_another_s_body_is_gone() {
    let calc = format!("{}\n{}", flat("grow", 14), flat("small", 3));
    let other = flat("other", 20).replace("x += 19;", "x -= 19;");
    let path = "engine/src/other.rs";
    let why = "`other`: cognitive complexity 20, a new function; the limit is 15.";
    assert_eq!(found(&[edited(CALC, &base(), &calc), added(path, &other)]), vec![finding(path, 1, why)]);
}
