//! The test-integrity check on Go tests read by lang_go, over a small Go module `calc` with its code in calc.go and its
//! tests in calc_test.go: removed tests and subtests, skips, build constraints, fewer assertions, changed expected
//! values and swallowed errors.

mod common;

use checks::integrity::{edits_code, weakened_tests};
use checks::{ChangedFile, Finding};
use common::{added, deleted, edited};

const CALC: &str = "\
package calc

func Add(a, b int) int {
	return a + b
}
";

const CALC_TEST: &str = r#"package calc

import (
	"testing"

	"github.com/stretchr/testify/require"
)

func TestAdd(t *testing.T) {
	if got := Add(1, 2); got != 3 {
		t.Errorf("Add(1, 2) = %d, want 3", got)
	}
	t.Run("negative", func(t *testing.T) {
		require.Equal(t, -2, Add(-1, -1))
	})
	for _, c := range []struct{ a, b, want int }{{1, 1, 2}} {
		t.Run(c.name(), func(t *testing.T) {
			if Add(c.a, c.b) != c.want {
				t.Fatal("wrong")
			}
		})
	}
}

func BenchmarkAdd(b *testing.B) {
	for range b.N {
		Add(1, 2)
	}
}

func FuzzAdd(f *testing.F) {
	f.Fuzz(func(t *testing.T, a int) {
		if Add(a, 0) != a {
			t.Fail()
		}
	})
}
"#;

const CALC_PATH: &str = "calc/calc.go";
const TEST_PATH: &str = "calc/calc_test.go";

/// `text` with its first `old` replaced by `new`; panics where `old` is not in it.
fn edit(text: &str, old: &str, new: &str) -> String {
    assert!(text.contains(old), "{old:?} is not in the text");
    text.replacen(old, new, 1)
}

/// Whether the change of `files` edits the code under test, as the gate tells it: a file that is not a `_test.go`
/// file that [`edits_code`] says it edits.
fn code_changed(files: &[ChangedFile]) -> bool {
    let code = |side: &Option<checks::Side>| side.as_ref().is_some_and(|s| !s.among_tests);
    files.iter().any(|f| (code(&f.before) || code(&f.after)) && edits_code(f))
}

/// The check's findings on the change of `files`, its code_changed as the gate tells it.
fn check(files: &[ChangedFile]) -> Vec<Finding> {
    weakened_tests(files, code_changed(files))
}

/// (line, rule) of each finding, the rule without the check's name, each on the test file.
fn wheres(found: &[Finding]) -> Vec<(Option<u32>, String)> {
    found
        .iter()
        .map(|f| {
            assert_eq!(f.path.as_deref(), Some(TEST_PATH.as_ref()), "{f:?}");
            (f.line, f.rule.strip_prefix("test-integrity ").expect("a test-integrity rule").to_string())
        })
        .collect()
}

fn at(line: u32, rule: &str) -> (Option<u32>, String) {
    (Some(line), rule.into())
}

#[test]
fn the_tests_are_the_go_test_functions_and_the_subtests_named_by_a_literal() {
    let file = lang_go::read(CALC_TEST.as_bytes(), true).unwrap();
    let tests: Vec<_> = file.tests.iter().map(|t| (t.name.as_str(), t.line, t.assertions.len())).collect();
    assert_eq!(tests, [("TestAdd", 9, 2), ("TestAdd/negative", 13, 1), ("BenchmarkAdd", 25, 0), ("FuzzAdd", 31, 1)]);
    let first = &file.tests[0].assertions[0];
    assert_eq!(first.literals, ["1", "2", "3", "\"Add(1, 2) = %d, want 3\""]);
    assert_eq!(model::first_line(&first.text), "if got := Add(1, 2); got != 3 {");
}

#[test]
fn what_is_not_named_or_typed_as_a_test_is_none() {
    let source = r#"package calc

import tst "testing"

func Testlower(t *tst.T) {}
func TestWrongType(t *testing.T) {}
func TestTwo(t *tst.T, n int) {}
func TestMain(m *tst.M) {}
func (s suite) TestMethod(t *tst.T) {}
func Test(t *tst.T) {
	t.Run(name, func(t *tst.T) { t.Error("x") })
}
"#;
    let file = lang_go::read(source.as_bytes(), true).unwrap();
    let tests: Vec<_> = file.tests.iter().map(|t| (t.name.as_str(), t.assertions.len())).collect();
    assert_eq!(tests, [("Test", 1)]);
    assert!(
        lang_go::read(CALC_TEST.as_bytes(), false).unwrap().tests.is_empty(),
        "a file not among the tests has none"
    );
}

#[test]
fn an_edit_beside_the_tests_has_no_findings() {
    let files = [edited(CALC_PATH, CALC, &edit(CALC, "a + b", "b + a"))];
    assert!(code_changed(&files));
    assert_eq!(check(&files), vec![]);
}

#[test]
fn a_removed_test_and_a_removed_subtest_are_found_at_their_lines_at_the_base() {
    let test = edit(
        CALC_TEST,
        "\tt.Run(\"negative\", func(t *testing.T) {\n\t\trequire.Equal(t, -2, Add(-1, -1))\n\t})\n",
        "",
    );
    let test = edit(&test, "func BenchmarkAdd(b *testing.B) {\n\tfor range b.N {\n\t\tAdd(1, 2)\n\t}\n}\n\n", "");
    let found = check(&[edited(TEST_PATH, CALC_TEST, &test)]);
    assert_eq!(wheres(&found), [at(13, "removed-test"), at(25, "removed-test")]);
    assert_eq!(found[0].message, "`TestAdd/negative` is gone; this was line 13 at the base.");
}

#[test]
fn a_test_renamed_with_its_body_is_not_removed_and_a_deleted_test_file_is_one_finding() {
    let renamed = edit(CALC_TEST, "func BenchmarkAdd(b", "func BenchmarkSum(b");
    assert_eq!(check(&[edited(TEST_PATH, CALC_TEST, &renamed)]), vec![]);
    let found = check(&[deleted(TEST_PATH, CALC_TEST)]);
    assert_eq!(found.len(), 1);
    assert_eq!(
        found[0].message,
        "deleted, with its 4 tests: `TestAdd`, `TestAdd/negative`, `BenchmarkAdd`, `FuzzAdd`."
    );
}

#[test]
fn fewer_assertions_are_found_at_the_test() {
    let test = edit(CALC_TEST, "\t\tif Add(a, 0) != a {\n\t\t\tt.Fail()\n\t\t}\n", "\t\tAdd(a, 0)\n");
    assert_eq!(wheres(&check(&[edited(TEST_PATH, CALC_TEST, &test)])), [at(31, "fewer-assertions")]);
}

#[test]
fn added_skips_and_build_constraints_are_found_at_their_lines() {
    let test = edit(CALC_TEST, "package calc\n", "//go:build integration\n\npackage calc\n");
    let test = edit(
        &test,
        "\tt.Run(\"negative\", func(t *testing.T) {\n",
        "\tt.Run(\"negative\", func(t *testing.T) {\n\t\tt.Skip(\"flaky\")\n",
    );
    let test = edit(&test, "\tfor range b.N {\n", "\tb.SkipNow()\n\tfor range b.N {\n");
    let found = check(&[edited(TEST_PATH, CALC_TEST, &test)]);
    assert_eq!(wheres(&found), [at(1, "skip"), at(16, "skip"), at(29, "skip")]);
    assert_eq!(found[0].message, "`//go:build integration` added: it turns the file's tests off.");
    assert_eq!(found[1].message, "`t.Skip(\"flaky\")` added: it skips a test.");
    assert!(found.iter().all(|f| f.severity == "error"), "{found:?}");
}

#[test]
fn a_build_constraint_naming_only_platforms_is_weighed_not_a_skip() {
    let test = edit(CALC_TEST, "package calc\n", "//go:build linux || darwin\n\npackage calc\n");
    let found = check(&[edited(TEST_PATH, CALC_TEST, &test)]);
    assert_eq!(wheres(&found), [at(1, "platform-only")]);
    assert_eq!(
        found[0].message,
        "`//go:build linux || darwin` added: it turns the file's tests off on the other platforms."
    );
    assert_eq!(found[0].severity, "warning");
}

#[test]
fn a_build_constraint_in_a_file_of_code_is_none() {
    let calc = edit(CALC, "package calc\n", "//go:build linux\n\npackage calc\n");
    let found = check(&[edited(CALC_PATH, CALC, &calc)]);
    assert_eq!(found, vec![]);
}

#[test]
fn an_expected_value_changed_with_the_code_is_found_and_without_it_is_not() {
    let test = edit(
        CALC_TEST,
        "got != 3 {\n\t\tt.Errorf(\"Add(1, 2) = %d, want 3\"",
        "got != 4 {\n\t\tt.Errorf(\"Add(1, 2) = %d, want 4\"",
    );
    let test = edit(&test, "require.Equal(t, -2,", "require.Equal(t, -3,");
    assert_eq!(check(&[edited(TEST_PATH, CALC_TEST, &test)]), vec![]);
    let calc = edit(CALC, "a + b", "a + b + 1");
    let found = check(&[edited(CALC_PATH, CALC, &calc), edited(TEST_PATH, CALC_TEST, &test)]);
    assert_eq!(wheres(&found), [at(10, "expected-value-changed"), at(14, "expected-value-changed")]);
    assert_eq!(
        found[0].message,
        "`TestAdd`: `if got := Add(1, 2); got != 3 {` became `if got := Add(1, 2); got != 4 {`, in a change that also \
         edits the code under test."
    );
    assert_eq!(
        found[1].message,
        "`TestAdd/negative`: `require.Equal(t, -2, Add(-1, -1))` became `require.Equal(t, -3, Add(-1, -1))`, in a \
         change that also edits the code under test."
    );
}

#[test]
fn errors_thrown_away_in_test_code_are_swallowed_errors() {
    let helper = "
func mustAdd(t testing.TB) int {
	err := check()
	_ = err
	if closeErr := shut(); closeErr != nil {
	}
	if err != nil { /* fine */ }
	_ = value
	return 0
}
";
    let test = format!("{CALC_TEST}{helper}");
    let found = check(&[edited(TEST_PATH, CALC_TEST, &test)]);
    assert_eq!(wheres(&found), [at(41, "swallowed-error"), at(42, "swallowed-error"), at(44, "swallowed-error")]);
    assert_eq!(found[0].message, "`_ = err` added in `mustAdd`: it throws away the error that would fail the test.");
    assert_eq!(
        found[1].message,
        "`if closeErr := shut(); closeErr != nil {` added in `mustAdd`: its empty block throws away the error that \
         would fail the test."
    );
}

#[test]
fn a_test_file_that_no_longer_parses_is_unparsable() {
    let found = check(&[edited(TEST_PATH, CALC_TEST, &edit(CALC_TEST, "func FuzzAdd(f", "func FuzzAdd(f f("))]);
    assert_eq!(wheres(&found).into_iter().map(|(_, rule)| rule).collect::<Vec<_>>(), ["unparsable"]);
}

#[test]
fn a_new_test_file_s_tests_are_no_finding() {
    assert_eq!(check(&[added(TEST_PATH, CALC_TEST)]), vec![]);
}
