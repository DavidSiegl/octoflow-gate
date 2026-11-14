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
    assert_eq!(tests, [("TestAdd", 9, 3), ("TestAdd/negative", 13, 1), ("BenchmarkAdd", 25, 0), ("FuzzAdd", 31, 1)]);
    let first = &file.tests[0].assertions[0];
    assert_eq!(first.literals, ["1", "2", "3", "\"Add(1, 2) = %d, want 3\""]);
    assert_eq!(model::first_line(&first.text), "if got := Add(1, 2); got != 3 {");
    // `got` and `t` are the test's own names.
    assert_eq!(first.reads, ["Add"]);
    // The case of its table is an assertion too.
    let case = &file.tests[0].assertions[1];
    assert_eq!((case.line, case.text.as_str(), case.literals.len()), (16, "{1, 1, 2}", 3));
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
fn testify_s_assert_and_require_are_assertions_under_the_names_they_are_imported_by() {
    let source = r#"package calc

import (
	"testing"

	"github.com/stretchr/testify/assert"
	must "github.com/stretchr/testify/require"
)

func TestAdd(t *testing.T) {
	assert.Equal(t, 3, Add(1, 2))
	must.Equal(t, 4, Add(2, 2))
	a := assert.New(t)
	require.Equal(t, 5, Add(2, 3))
	_ = a
}
"#;
    let file = lang_go::read(source.as_bytes(), true).unwrap();
    let texts: Vec<&str> = file.tests[0].assertions.iter().map(|a| a.text.as_str()).collect();
    assert_eq!(texts, ["assert.Equal(t, 3, Add(1, 2))", "must.Equal(t, 4, Add(2, 2))"]);
    let fewer = edit(source, "\tassert.Equal(t, 3, Add(1, 2))\n", "");
    assert_eq!(wheres(&check(&[edited(TEST_PATH, source, &fewer)])), [at(10, "fewer-assertions")]);
}

#[test]
fn the_methods_of_a_name_bound_to_testify_s_new_are_assertions() {
    let source = r#"package calc

import (
	"testing"

	"github.com/stretchr/testify/assert"
	must "github.com/stretchr/testify/require"
)

func TestAdd(t *testing.T) {
	a := assert.New(t)
	var r = must.New(t)
	a.Equal(3, Add(1, 2))
	t.Run("negative", func(t *testing.T) {
		r.Equal(-2, Add(-1, -1))
	})
	b := a
	b.Equal(4, Add(2, 2))
}
"#;
    let file = lang_go::read(source.as_bytes(), true).unwrap();
    let texts: Vec<&str> = file.tests.iter().flat_map(|t| &t.assertions).map(|a| a.text.as_str()).collect();
    assert_eq!(texts, ["a.Equal(3, Add(1, 2))", "r.Equal(-2, Add(-1, -1))"]);
    let fewer = edit(source, "\ta.Equal(3, Add(1, 2))\n", "");
    assert_eq!(wheres(&check(&[edited(TEST_PATH, source, &fewer)])), [at(10, "fewer-assertions")]);
}

/// Tests that assert through helpers of the file: `check` fails its test itself, `checkAll` through `check2`, `log`
/// fails nothing, and `t.Run`'s literal passes its own `t` on.
const HELPER_TEST: &str = r#"package calc

import "testing"

func TestAdd(t *testing.T) {
	check(t, Add(1, 2), 3)
	checkAll(t, Add(2, 2), 4)
	log(t, Add(3, 3))
	t.Run("negative", func(t *testing.T) {
		check(t, Add(-1, -1), -2)
	})
	other(Add(1, 1), 2)
}

func check(t *testing.T, got, want int) {
	t.Helper()
	if got != want {
		t.Errorf("got %d, want %d", got, want)
	}
}

func checkAll(tb testing.TB, got, want int) {
	check2(tb, got, want)
}

func check2(tb testing.TB, got, want int) {
	if got != want {
		tb.Fatal("wrong")
	}
}

func log(t *testing.T, got int) {
	t.Log(got)
}

func other(got, want int) bool {
	return got == want
}
"#;

#[test]
fn a_call_of_a_helper_of_the_file_that_fails_the_test_it_is_passed_is_an_assertion() {
    let file = lang_go::read(HELPER_TEST.as_bytes(), true).unwrap();
    let asserted: Vec<(&str, Vec<&str>)> = (file.tests.iter())
        .map(|t| (t.name.as_str(), t.assertions.iter().map(|a| a.text.as_str()).collect()))
        .collect();
    assert_eq!(asserted, [
        ("TestAdd", vec!["check(t, Add(1, 2), 3)", "checkAll(t, Add(2, 2), 4)"]),
        ("TestAdd/negative", vec!["check(t, Add(-1, -1), -2)"]),
    ]);
    let fewer = edit(HELPER_TEST, "\tcheck(t, Add(1, 2), 3)\n", "");
    assert_eq!(wheres(&check(&[edited(TEST_PATH, HELPER_TEST, &fewer)])), [at(5, "fewer-assertions")]);
    let changed = edit(HELPER_TEST, "Add(1, 2), 3)", "Add(1, 2), 4)");
    let calc = edit(CALC, "a + b", "a + b + 1");
    let found = check(&[edited(CALC_PATH, CALC, &calc), edited(TEST_PATH, HELPER_TEST, &changed)]);
    assert_eq!(wheres(&found), [at(6, "expected-value-changed")]);
}

/// Examples: two with their output to check, and one with none, which `go test` compiles but does not run. The
/// trailing comment of `ExampleAdd` is a comment group of its own, as go/parser groups comments.
const EXAMPLE_TEST: &str = r#"package calc

import "fmt"

func ExampleAdd() {
	fmt.Println(Add(1, 2)) // prints the sum
	// Output: 3
}

func Example_unordered() {
	fmt.Println(Add(1, 1))
	fmt.Println(Add(2, 2))
	// Unordered output:
	// 4
	// 2
}

func ExampleSub() {
	fmt.Println(Add(1, 2))
}
"#;

#[test]
fn an_example_with_its_output_is_a_test_whose_assertion_is_that_output() {
    let file = lang_go::read(EXAMPLE_TEST.as_bytes(), true).unwrap();
    let names: Vec<&str> = file.tests.iter().map(|t| t.name.as_str()).collect();
    assert_eq!(names, ["ExampleAdd", "Example_unordered"]);
    let asserted: Vec<(u32, &str, &[String])> = (file.tests.iter().flat_map(|t| &t.assertions))
        .map(|a| (a.line, a.text.as_str(), a.literals.as_slice()))
        .collect();
    assert_eq!(asserted, [
        (7, "// Output: 3", &["3".to_string()][..]),
        (13, "// Unordered output:\n\t// 4\n\t// 2", &["4\n2".to_string()][..]),
    ]);
    let removed = edit(EXAMPLE_TEST, "func ExampleAdd() {\n\tfmt.Println(Add(1, 2)) // prints the sum\n\t// Output: 3\n}\n\n", "");
    assert_eq!(wheres(&check(&[edited(TEST_PATH, EXAMPLE_TEST, &removed)])), [at(5, "removed-test")]);
    let changed = edit(EXAMPLE_TEST, "// Output: 3", "// Output: 4");
    assert_eq!(check(&[edited(TEST_PATH, EXAMPLE_TEST, &changed)]), vec![], "an edit of the tests alone");
    let calc = edit(CALC, "a + b", "a + b + 1");
    let found = check(&[edited(CALC_PATH, CALC, &calc), edited(TEST_PATH, EXAMPLE_TEST, &changed)]);
    assert_eq!(wheres(&found), [at(7, "expected-value-changed")]);
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
fn a_test_moved_with_its_skip_adds_none() {
    let skipped = "package calc\n\nimport \"testing\"\n\nfunc TestNetwork(t *testing.T) {\n\tt.Skip(\"needs network\")\n\t\
                   if Add(1, 2) != 3 {\n\t\tt.Fatal(\"no\")\n\t}\n}\n";
    let moved = [deleted(TEST_PATH, skipped), added("calc/network_test.go", skipped)];
    assert_eq!(check(&moved), vec![]);
    // However the comments in what weakens it are edited on the way.
    let skipping = skipped.replace("t.Skip(\"needs network\")", "t.Skip( /* offline */ \"needs network\")");
    let moved = [deleted(TEST_PATH, skipped), added("calc/network_test.go", &skipping)];
    assert_eq!(check(&moved), vec![]);
    // A wrapped skip whose argument line alone changed is found at that line.
    let wrapped = edit(skipped, "t.Skip(\"needs network\")", "t.Skip(\n\t\t\"needs network\",\n\t)");
    let found = check(&[edited(TEST_PATH, &wrapped, &edit(&wrapped, "\"needs network\"", "\"always\""))]);
    assert_eq!(found.iter().map(|f| (f.line, f.rule.as_str())).collect::<Vec<_>>(), [(Some(7), "test-integrity skip")]);
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
        "`//go:build linux || darwin` added: it turns the file's tests off on the other platforms and toolchains."
    );
    assert_eq!(found[0].severity, "warning");
    let test = edit(CALC_TEST, "package calc\n", "//go:build go1.21 && cgo\n\npackage calc\n");
    assert_eq!(wheres(&check(&[edited(TEST_PATH, CALC_TEST, &test)])), [at(1, "platform-only")]);
}

#[test]
fn a_legacy_plus_build_line_is_read_where_the_file_has_no_go_build_line() {
    let with = |header: &str| edit(CALC_TEST, "package calc\n", &format!("{header}\n\npackage calc\n"));
    let found = |header: &str| check(&[edited(TEST_PATH, CALC_TEST, &with(header))]);
    let ignored = found("// +build ignore");
    assert_eq!(wheres(&ignored), [at(1, "skip")]);
    assert_eq!(ignored[0].message, "`// +build ignore` added: it turns the file's tests off.");
    assert_eq!(wheres(&found("// +build linux")), [at(1, "platform-only")]);
    assert_eq!(wheres(&found("// +build linux,amd64 darwin,!cgo")), [at(1, "platform-only")]);
    assert_eq!(wheres(&found("// +build linux\n// +build windows")), [at(1, "skip"), at(2, "skip")]);
    assert_eq!(wheres(&found("//go:build linux\n// +build ignore")), [at(1, "platform-only")]);
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

/// A test whose expected values stand outside its assertions: in a case of its table, and in a constant of the file.
const TABLE_TEST: &str = r#"
const want = 3

func TestTable(t *testing.T) {
	for _, c := range []struct{ in, want int }{
		{in: 2, want: 4},
		{in: 3, want: 6},
	} {
		if got := Add(c.in, c.in); got != c.want {
			t.Errorf("Add(%d, %d) = %d", c.in, c.in, got)
		}
	}
	if Add(1, 2) != want {
		t.Error("not want")
	}
}
"#;

#[test]
fn a_case_of_a_table_and_a_constant_edited_with_the_code_are_changed_expected_values() {
    let base = format!("{CALC_TEST}{TABLE_TEST}");
    let calc = edit(CALC, "a + b", "a + b + 1");
    let changed = |test: &str| check(&[edited(CALC_PATH, CALC, &calc), edited(TEST_PATH, &base, test)]);
    let test = edit(&base, "{in: 2, want: 4}", "{in: 2, want: 5}");
    assert_eq!(check(&[edited(TEST_PATH, &base, &test)]), vec![], "an edit of the tests alone");
    let found = changed(&test);
    assert_eq!(wheres(&found), [at(43, "expected-value-changed")]);
    assert_eq!(
        found[0].message,
        "`TestTable`: `{in: 2, want: 4}` became `{in: 2, want: 5}`, in a change that also edits the code under test."
    );
    let test = edit(&base, "const want = 3", "const want = 4");
    let found = changed(&test);
    assert_eq!(wheres(&found), [at(50, "expected-value-changed")]);
    assert_eq!(
        found[0].message,
        "`want` (line 39), which `TestTable` names in its assertions, changed, in a change that also edits the code \
         under test."
    );
}

#[test]
fn a_case_added_to_or_removed_from_a_table_with_the_code_leaves_its_neighbours_unchanged() {
    let base = format!("{CALC_TEST}{TABLE_TEST}");
    let calc = edit(CALC, "a + b", "a + b + 1");
    let changed = |test: &str| check(&[edited(CALC_PATH, CALC, &calc), edited(TEST_PATH, &base, test)]);
    let test = edit(&base, "{in: 3, want: 6},", "{in: 3, want: 6},\n\t\t{in: 4, want: 9},");
    assert_eq!(changed(&test), vec![]);
    let test = edit(&base, "\t\t{in: 3, want: 6},\n", "");
    assert_eq!(wheres(&changed(&test)), [at(41, "fewer-assertions")]);
    let test = edit(&edit(&base, "{in: 3, want: 6}", "{in: 3, want: 7}"), "{in: 2, want: 4}", "{in: 3, want: 6}");
    let found = changed(&test);
    assert_eq!(wheres(&found), [at(43, "expected-value-changed"), at(44, "expected-value-changed")], "each moved on");
}

/// A test with a table of cases, `cases`, ranged over by name and keyed by a constant, beside literals that are no
/// table: an input, and expected values inside testify's assertions.
const LITERALS_TEST: &str = r#"
const keyA = "a"

func TestLiterals(t *testing.T) {
	cases := map[string]struct{ in, want int }{
		keyA: {
			in:   2,
			want: 4,
		},
	}
	for name, c := range cases {
		if Add(c.in, len(name)) != c.want {
			t.Error(name)
		}
	}
	in := []struct{ a int }{{a: 1}}
	require.Equal(t, []struct{ a int }{{a: 2}, {a: 3}}, Sum(in))
	require.Equal(t, map[string]int{keyA: 1}, Count(in))
}
"#;

#[test]
fn only_a_literal_ranged_over_is_a_table_and_its_cases_are_quoted_whole() {
    let base = format!("{CALC_TEST}{LITERALS_TEST}");
    let calc = edit(CALC, "a + b", "a + b + 1");
    let changed = |test: &str| check(&[edited(CALC_PATH, CALC, &calc), edited(TEST_PATH, &base, test)]);
    let found = changed(&edit(&base, "want: 4,", "want: 5,"));
    assert_eq!(wheres(&found), [at(43, "expected-value-changed")]);
    assert_eq!(
        found[0].message,
        "`TestLiterals`: `keyA: {in:   2, want: 4,}` became `keyA: {in:   2, want: 5,}`, in a change that also edits \
         the code under test."
    );
    assert_eq!(changed(&edit(&base, "{a: 1}", "{a: 7}")), vec![], "an input");
    assert_eq!(wheres(&changed(&edit(&base, "{a: 3}", "{a: 4}"))), [at(54, "expected-value-changed")]);
    assert_eq!(changed(&edit(&base, ", {a: 3}}", "}")), vec![], "an expected value one item shorter");
}

#[test]
fn a_constant_keying_a_map_is_read() {
    let base = format!("{CALC_TEST}{LITERALS_TEST}");
    let calc = edit(CALC, "a + b", "a + b + 1");
    let changed = |test: &str| {
        check(&[edited(CALC_PATH, CALC, &calc), edited(TEST_PATH, test, &edit(test, "\"a\"", "\"b\""))])
    };
    assert_eq!(wheres(&changed(&base)), [at(43, "expected-value-changed")], "a table's key");
    let unkeyed = edit(&base, "\t\tkeyA: {", "\t\t\"x\": {");
    assert_eq!(wheres(&changed(&unkeyed)), [at(55, "expected-value-changed")], "an assertion's map");
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
    // The weakening is the `if` up to its block's `{`: below that line, an edit to a comment in the empty block, or to
    // an else after it, the path that did not fail, adds none.
    let commented = edit(&test, "\tif err != nil { /* fine */ }\n", "\tif err != nil {\n\t\t// fine\n\t}\n");
    assert_eq!(check(&[edited(TEST_PATH, &commented, &edit(&commented, "// fine", "// ok"))]), vec![]);
    let empty = edit(&test, "\tif err != nil { /* fine */ }\n", "\tif err != nil {\n\t}\n");
    let with_else = edit(&empty, "\tif err != nil {\n\t}\n", "\tif err != nil {\n\t} else {\n\t\tuse(1)\n\t}\n");
    assert_eq!(check(&[edited(TEST_PATH, &empty, &with_else)]), vec![]);
    assert_eq!(check(&[edited(TEST_PATH, &with_else, &edit(&with_else, "} else {", "} else if ok {"))]), vec![]);
    assert_eq!(check(&[edited(TEST_PATH, &with_else, &edit(&with_else, "use(1)", "use(2)"))]), vec![]);
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
