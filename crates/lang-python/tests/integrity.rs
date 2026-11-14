//! The test-integrity check on Python tests read by lang_python. What the gate decides over a real change before the
//! check judges it is crates/gate/tests/parser_checks.rs's.

mod common;

use checks::integrity::weakened_tests;
use checks::ChangedFile;
use common::{added, among_tests, changed, deleted, edited, replaced};

const CALC_TESTS: &str = "\
import unittest

from calc import add


class TestAdd(unittest.TestCase):
    def test_small(self):
        self.assertEqual(add(1, 2), 3)
        self.assertEqual(add(0, 0), 0)

    def test_large(self):
        assert add(10, 20) == 30


class TestNegative(unittest.TestCase):
    def test_one(self):
        assert add(-1, -1) == -2

    def test_two(self):
        with self.assertRaises(TypeError):
            add(None, 1)


def test_function():
    assert add(2, 2) == 4
";

const OTHER_TESTS: &str = "def test_a():\n    assert True\n\n\ndef test_b():\n    assert 1\n";

const CALC: &str = "def add(a, b):\n    return a + b\n";

const CALC_PATH: &str = "tests/test_calc.py";
const OTHER_PATH: &str = "tests/test_other.py";

/// A finding as these tests compare it: its path, line and rule, without the check's name.
type Found = (String, Option<u32>, String);

fn at(path: &str, line: impl Into<Option<u32>>, rule: &str) -> Found {
    (path.into(), line.into(), rule.into())
}

/// The check's findings on `files`, and their messages. The change edits the code under test when one of its files is
/// neither a test file nor beside the tests, as gate.toml's default `test-files` tell.
fn found(files: &[ChangedFile]) -> (Vec<Found>, Vec<String>) {
    let beside_tests = |path: &str| among_tests(path) || path.split('/').any(|part| part == "tests");
    let code_changed = files
        .iter()
        .flat_map(|f| [&f.before, &f.after])
        .flatten()
        .any(|side| !beside_tests(&side.path.to_string_lossy()));
    let found = weakened_tests(files, code_changed);
    let place = |f: &checks::Finding| {
        let rule = f.rule.strip_prefix("test-integrity ").unwrap().to_string();
        (f.path.as_ref().unwrap().to_string_lossy().into_owned(), f.line, rule)
    };
    (found.iter().map(place).collect(), found.into_iter().map(|f| f.message).collect())
}

/// tests/test_calc.py edited by replacing `old` with `new`.
fn calc_tests_with(old: &str, new: &str) -> ChangedFile {
    edited(CALC_PATH, CALC_TESTS, &replaced(CALC_TESTS, old, new))
}

/// calc.py with an edit to the code under test.
fn code_edit() -> ChangedFile {
    edited("calc.py", CALC, &CALC.replace("return a + b", "return b + a"))
}

#[test]
fn an_unchanged_file_has_no_findings() {
    assert_eq!(found(&[edited(CALC_PATH, CALC_TESTS, CALC_TESTS)]).0, []);
}

#[test]
fn a_removed_test_is_found_at_its_line_at_the_base() {
    let (place, messages) =
        found(&[calc_tests_with("    def test_large(self):\n        assert add(10, 20) == 30\n\n", "")]);
    assert_eq!(place, [at(CALC_PATH, 11, "removed-test")]);
    assert_eq!(messages, ["`TestAdd.test_large` is gone; this was line 11 at the base."]);
}

#[test]
fn a_removed_class_is_one_finding_with_its_tests() {
    let class =
        &CALC_TESTS[CALC_TESTS.find("class TestNegative").unwrap()..CALC_TESTS.find("def test_function").unwrap()];
    let (place, messages) = found(&[calc_tests_with(class, "")]);
    assert_eq!(place, [at(CALC_PATH, 15, "removed-test")]);
    assert_eq!(messages, ["`TestNegative` and its 2 tests are gone; this was line 15 at the base."]);
}

#[test]
fn a_deleted_test_file_is_one_finding() {
    let (place, messages) = found(&[deleted(OTHER_PATH, OTHER_TESTS)]);
    assert_eq!(place, [at(OTHER_PATH, None, "removed-test")]);
    assert_eq!(messages, ["deleted, with its 2 tests: `test_a`, `test_b`."]);
}

#[test]
fn a_test_renamed_or_moved_with_the_same_body_is_not_removed() {
    assert_eq!(found(&[calc_tests_with("def test_large(self):", "def test_big(self):")]).0, []);
    // Renamed within a renamed file.
    let moved = changed(
        Some((OTHER_PATH, OTHER_TESTS)),
        Some(("tests/test_moved.py", &OTHER_TESTS.replace("test_b", "test_bee"))),
    );
    assert_eq!(found(&[moved]).0, []);
    // Into another file, re-quoted and re-wrapped on the way.
    let rewritten = "def test_a():\n    # as before\n    assert True\n\n\ndef test_b():\n    assert (\n        1)\n";
    assert_eq!(found(&[deleted(OTHER_PATH, OTHER_TESTS), added("tests/test_new.py", OTHER_TESTS)]).0, []);
    assert_eq!(found(&[deleted(OTHER_PATH, OTHER_TESTS), added("tests/test_new.py", rewritten)]).0.len(), 1);
}

#[test]
fn a_test_deeper_than_a_recursive_walk_could_go_is_matched_and_compared() {
    let sum = vec!["1"; 3000].join(" + ");
    let deep = format!("def test_deep():\n    x = 1\n    assert x + {sum} == 3001\n");
    let path = "tests/test_deep.py";
    assert_eq!(found(&[edited(path, &deep, &deep.replace("def test_deep", "def test_deeper"))]).0, []);
    assert_eq!(found(&[code_edit(), edited(path, &deep, &deep.replace("x = 1", "x = 2"))]).0, []);
    let (place, _) = found(&[code_edit(), edited(path, &deep, &deep.replace("== 3001", "== 3002"))]);
    assert_eq!(place, [at(path, 3, "expected-value-changed")]);
}

#[test]
fn an_assertion_with_an_int_too_long_to_print_in_decimal_is_reported_as_any_other() {
    let path = "tests/test_big.py";
    let big = format!("def test_big():\n    assert 1 == 0x{}\n", "f".repeat(4000));
    let (place, messages) = found(&[code_edit(), edited(path, &big, &replaced(&big, "assert 1 ==", "assert 2 =="))]);
    assert_eq!(place, [at(path, 2, "expected-value-changed")]);
    // Quoted as written and cut short, where the gate up to 0.6.0 said "(code at line 2, too long to print)"
    // (docs/port-differences.md, "Quoted code is quoted as written").
    let [message] = messages.as_slice() else { panic!("one message, not {messages:?}") };
    assert!(message.starts_with("`test_big`: `assert 1 == 0xfff"), "{message}");
    assert!(message.contains("fff…` became `assert 2 == 0xfff"), "{message}");
    assert!(message.ends_with("fff…`, in a change that also edits the code under test."), "{message}");
}

#[test]
fn added_skips_are_found_at_their_lines() {
    let calc =
        replaced(CALC_TESTS, "    def test_small(self):\n", "    @unittest.skip('later')\n    def test_small(self):\n");
    let calc = replaced(
        &calc,
        "    def test_one(self):\n        assert",
        "    def test_one(self):\n        self.skipTest('flaky')\n        assert",
    );
    let other = replaced(
        OTHER_TESTS,
        "def test_b():\n",
        "import pytest\n\n\n@pytest.mark.xfail\ndef test_b():\n    pytest.skip('no')\n",
    );
    let (place, messages) = found(&[edited(CALC_PATH, CALC_TESTS, &calc), edited(OTHER_PATH, OTHER_TESTS, &other)]);
    assert_eq!(
        place,
        [at(CALC_PATH, 7, "skip"), at(CALC_PATH, 18, "skip"), at(OTHER_PATH, 8, "skip"), at(OTHER_PATH, 10, "skip")]
    );
    assert_eq!(messages[0], "`unittest.skip` added: it skips a test or expects it to fail.");
    assert_eq!(messages[2], "`pytest.mark.xfail` added: it skips a test or expects it to fail.");
}

#[test]
fn a_type_parameter_named_as_a_skip_is_no_skip_but_its_bound_is_read() {
    let generic = "def test_a[skip]():\n    assert 1\n\n\nclass TestB[xfail, *skipIf, **skipTest]:\n    pass\n\n\n\
                   type Skipped[SkipTest] = list[SkipTest]\n\n\ndef test_c[T: skipif]():\n    assert 1\n";
    let (place, _) = found(&[added(OTHER_PATH, generic)]);
    // The alias's value reads SkipTest, its parameter; the bound skipif is read as ast reads it, a Name.
    assert_eq!(place, [at(OTHER_PATH, 9, "skip"), at(OTHER_PATH, 12, "skip")]);
}

#[test]
fn a_test_moved_with_the_decorators_it_had_adds_no_skip_but_one_added_on_the_way_is_found() {
    let base = "import pytest\n\n\n@pytest.mark.xfail\ndef test_b():\n    assert 1\n";
    let moved = "import pytest\n\n\n@pytest.mark.slow\n@pytest.mark.xfail\ndef test_bee():\n    assert 1\n";
    assert_eq!(found(&[deleted(OTHER_PATH, base), added("tests/test_new.py", moved)]).0, []);
    let skipped = moved.replace("@pytest.mark.slow\n", "@pytest.mark.skip\n");
    let (place, _) = found(&[deleted(OTHER_PATH, base), added("tests/test_new.py", &skipped)]);
    assert_eq!(place, [at("tests/test_new.py", 4, "skip")]);
    // A skip in the body moves with it when the body is the same.
    let base = "import pytest\n\n\ndef test_b():\n    pytest.skip('no')\n    assert 1\n";
    assert_eq!(found(&[deleted(OTHER_PATH, base), added("tests/test_new.py", base)]).0, []);
    // However its quotes, trailing commas and comments are rewritten on the way, as black and an editor do.
    let rewritten = base.replace("'no')", "\"no\",)  # still");
    assert_eq!(found(&[deleted(OTHER_PATH, base), added("tests/test_new.py", &rewritten)]).0, []);
    let swallowing = "def test_b():\n    try:\n        assert 1\n    except Exception:  # why\n        pass\n";
    let rewritten = swallowing.replace("Exception:  # why", "Exception :  # flaky");
    assert_eq!(found(&[deleted(OTHER_PATH, swallowing), added("tests/test_new.py", &rewritten)]).0, []);
}

#[test]
fn a_kept_test_s_mark_moved_to_another_parametrized_case_is_found() {
    let base = "import pytest\n\n\n@pytest.mark.parametrize('n', [\n    pytest.param(1, marks=pytest.mark.xfail),\n    \
                pytest.param(2),\n])\ndef test_b(n):\n    assert n\n";
    let moved = base
        .replace("pytest.param(1, marks=pytest.mark.xfail)", "pytest.param(1)")
        .replace("pytest.param(2)", "pytest.param(2, marks=pytest.mark.xfail)");
    assert_eq!(found(&[edited(OTHER_PATH, base, &moved)]).0, [at(OTHER_PATH, 6, "skip")]);
    // A case added beside it leaves the mark its own.
    let added_case = base.replace("    pytest.param(2),\n", "    pytest.param(2),\n    pytest.param(3),\n");
    assert_eq!(found(&[edited(OTHER_PATH, base, &added_case)]).0, []);
    // On one line, a mark added to a second case is found beside the first's.
    let one_line = "import pytest\n\n\n@pytest.mark.parametrize('n', [pytest.param(1, marks=pytest.mark.xfail), \
                    pytest.param(2)])\ndef test_b(n):\n    assert n\n";
    let both = one_line.replace("pytest.param(2)", "pytest.param(2, marks=pytest.mark.xfail)");
    assert_eq!(found(&[edited(OTHER_PATH, one_line, &both)]).0, [at(OTHER_PATH, 4, "skip")]);
    // So is one on a second case like the first.
    let twice = one_line.replace("pytest.param(2)", "pytest.param(1, marks=pytest.mark.xfail)");
    assert_eq!(found(&[edited(OTHER_PATH, one_line, &twice)]).0, [at(OTHER_PATH, 4, "skip")]);
}

#[test]
fn a_test_moved_with_a_case_s_marks_rewrapped_adds_no_skip() {
    let base = "import pytest\n\n\n@pytest.mark.parametrize('n', [pytest.param(1, marks=[pytest.mark.skip, \
                pytest.mark.xfail])])\ndef test_b(n):\n    assert n\n";
    let wrapped = base.replace(
        "marks=[pytest.mark.skip, pytest.mark.xfail]",
        "marks=[\n        pytest.mark.skip,\n        pytest.mark.xfail,\n    ]",
    );
    assert_eq!(found(&[deleted(OTHER_PATH, base), added("tests/test_new.py", &wrapped)]).0, []);
    // A mark added on its own line among them is found there.
    let more = wrapped.replace("        pytest.mark.xfail,\n", "        pytest.mark.xfail,\n        pytest.mark.skip,\n");
    assert_eq!(found(&[edited("tests/test_new.py", &wrapped, &more)]).0, [at("tests/test_new.py", 7, "skip")]);
}

#[test]
fn a_kept_test_s_skip_with_another_condition_or_a_second_one_stacked_above_is_found() {
    let base = "import sys\n\nimport pytest\n\n\n@pytest.mark.skipif(sys.platform == 'win32', reason='x')\ndef test_b():\n    \
                assert 1\n";
    let always = base.replace("sys.platform == 'win32'", "True");
    assert_eq!(found(&[edited(OTHER_PATH, base, &always)]).0, [at(OTHER_PATH, 6, "skip")]);
    let stacked = base.replace("@pytest", "@pytest.mark.skipif(True, reason='y')\n@pytest");
    assert_eq!(found(&[edited(OTHER_PATH, base, &stacked)]).0, [at(OTHER_PATH, 6, "skip")]);
    // A wrapped skipif whose condition line alone changed, at that line.
    let wrapped = base.replace("skipif(sys", "skipif(\n    sys").replace(", reason", ",\n    reason");
    let always = wrapped.replace("    sys.platform == 'win32',", "    True,");
    assert_eq!(found(&[edited(OTHER_PATH, &wrapped, &always)]).0, [at(OTHER_PATH, 7, "skip")]);
}

#[test]
fn a_skip_is_a_name_python_reads_not_one_it_binds_or_passes_by_keyword() {
    let other =
        format!("{OTHER_TESTS}\n\nfrom unittest import skip as skipIf\n\n\ndef test_c(xfail=1):\n\
                 \x20   assert run(skip=xfail)\n");
    let (place, messages) = found(&[edited(OTHER_PATH, OTHER_TESTS, &other)]);
    assert_eq!(place, [at(OTHER_PATH, 13, "skip")]);
    assert_eq!(messages, ["`xfail` added: it skips a test or expects it to fail."]);
}

#[test]
fn a_skip_caught_or_entered_under_an_alias_is_found() {
    let other = format!(
        "{OTHER_TESTS}\n\ndef test_c():\n    try:\n        run()\n    except SkipTest as exc:\n        raise\n\
         \x20   with skip as how:\n        pass\n"
    );
    let (place, _) = found(&[edited(OTHER_PATH, OTHER_TESTS, &other)]);
    assert_eq!(place, [at(OTHER_PATH, 12, "skip"), at(OTHER_PATH, 14, "skip")]);
}

#[test]
fn skips_already_at_the_base_are_not_found() {
    let base = "import pytest\n\n\n@pytest.mark.skipif(True, reason='x')\ndef test_x():\n    assert 1\n";
    assert_eq!(found(&[edited("tests/test_skipped.py", base, &base.replace("assert 1", "assert 2"))]).0, []);
}

#[test]
fn fewer_assertions_are_found_at_the_test() {
    let calc = replaced(CALC_TESTS, "        self.assertEqual(add(0, 0), 0)\n", "");
    let calc = replaced(
        &calc,
        "        with self.assertRaises(TypeError):\n            add(None, 1)\n",
        "        add(None, 1)\n",
    );
    let (place, messages) = found(&[edited(CALC_PATH, CALC_TESTS, &calc)]);
    assert_eq!(place, [at(CALC_PATH, 7, "fewer-assertions"), at(CALC_PATH, 18, "fewer-assertions")]);
    assert_eq!(messages[0], "`TestAdd.test_small` has 1 assertions, 2 at the base.");
}

#[test]
fn more_assertions_and_new_tests_are_not_found() {
    let calc = calc_tests_with(
        "        assert add(10, 20) == 30\n",
        "        assert add(10, 20) == 30\n        assert add(20, 10) == 30\n",
    );
    let other = edited(
        OTHER_PATH,
        OTHER_TESTS,
        &replaced(OTHER_TESTS, "def test_a():", "def test_new():\n    assert 3\n\n\ndef test_a():"),
    );
    assert_eq!(found(&[calc, other]).0, []);
}

#[test]
fn swallowed_errors_are_found() {
    let calc = replaced(
        CALC_TESTS,
        "        assert add(-1, -1) == -2\n",
        "        try:\n            assert add(-1, -1) == -2\n        except:\n            pass\n",
    );
    let calc = replaced(
        &calc,
        "    assert add(2, 2) == 4\n",
        "    try:\n        assert add(2, 2) == 4\n    except (Exception) as e:  # all of them\n\
         \x20       'ignored'\n        ...\n\
         \x20   try:\n        add(1, 1)\n    except ValueError:\n        pass\n\
         \x20   try:\n        add(1, 1)\n    except Exception:\n        raise\n",
    );
    let (place, messages) = found(&[edited(CALC_PATH, CALC_TESTS, &calc)]);
    assert_eq!(place, [at(CALC_PATH, 19, "swallowed-error"), at(CALC_PATH, 30, "swallowed-error")]);
    assert_eq!(messages[0], "`except:` added in a test: it hides the failure it catches.");
    assert_eq!(messages[1], "`except (Exception) as e:` added in a test: it hides the failure it catches.");
}

#[test]
fn an_expected_value_changed_with_the_code_is_reported_as_the_pair_and_alone_not() {
    let test = || calc_tests_with("assert add(10, 20) == 30", "assert add(10, 20) == 31");
    let (place, messages) = found(&[code_edit(), test()]);
    assert_eq!(place, [at(CALC_PATH, 12, "expected-value-changed")]);
    assert_eq!(
        messages,
        ["`TestAdd.test_large`: `assert add(10, 20) == 30` became `assert add(10, 20) == 31`, in a change that also \
          edits the code under test."]
    );
    assert_eq!(found(&[test()]).0, []);
}

#[test]
fn expected_values_moved_onto_their_neighbours_old_ones_are_each_changed() {
    let path = "tests/test_s.py";
    let base = "def test_s():\n    assert next(c) == 1\n    assert next(c) == 2\n    assert next(c) == 3\n";
    let raised = base.replace("== 3", "== 4").replace("== 2", "== 3").replace("== 1", "== 2");
    let lines = |files: &[ChangedFile]| found(files).0.into_iter().map(|f| f.1).collect::<Vec<_>>();
    assert_eq!(lines(&[code_edit(), edited(path, base, &raised)]), [Some(2), Some(3), Some(4)]);
    let base = "def test_s():\n    assert f(0) == 1\n    assert f(0) == 2\n";
    assert_eq!(lines(&[code_edit(), edited(path, base, &base.replace("== 1", "== 2"))]), [Some(2)]);
}

#[test]
fn a_string_requoted_is_no_changed_value_and_one_of_another_prefix_or_text_is() {
    let base = "def test_s():\n    assert name() == 'a' \"b\"\n    assert f'{x}-{y}' == b'c'\n";
    let path = "tests/test_s.py";
    assert_eq!(found(&[code_edit(), edited(path, base, &base.replace("'a' \"b\"", "\"ab\""))]).0, []);
    assert_eq!(found(&[code_edit(), edited(path, base, &base.replace("f'{x}-{y}'", "F\"{x}-{y}\""))]).0, []);
    for (old, new) in [("'a'", "'A'"), ("b'c'", "'c'"), ("-{y}", "+{y}")] {
        let (place, _) = found(&[code_edit(), edited(path, base, &base.replace(old, new))]);
        assert_eq!(place.len(), 1, "{old} to {new}: {place:?}");
        assert_eq!(place[0].2, "expected-value-changed");
    }
}

#[test]
fn adjacent_f_strings_split_elsewhere_are_no_changed_value_and_ones_of_other_text_are() {
    let base = "def test_s():\n    assert name() == f'{x} is a' f'b, not {y}'\n";
    let path = "tests/test_s.py";
    for same in ["f'{x} is ' f'ab, not {y}'", "f'{x} is ab, not {y}'", "f'{x}' f\" is ab, not \" f'{y}'"] {
        let new = base.replace("f'{x} is a' f'b, not {y}'", same);
        assert_eq!(found(&[code_edit(), edited(path, base, &new)]).0, [], "{same}");
    }
    for other in ["f'{x} is a' f' b, not {y}'", "f'{x} was a' f'b, not {y}'"] {
        let (place, _) = found(&[code_edit(), edited(path, base, &base.replace("f'{x} is a' f'b, not {y}'", other))]);
        assert_eq!(place.len(), 1, "{other}: {place:?}");
        assert_eq!(place[0].2, "expected-value-changed");
    }
}

#[test]
fn a_literal_s_sign_or_what_holds_it_is_part_of_the_literal() {
    let path = "tests/test_s.py";
    let edit = |old: &str, new: &str| {
        let base = format!("def test_s():\n    assert f() == {old}\n");
        found(&[code_edit(), edited(path, &base, &base.replace(old, new))]).0
    };
    let pairs = [
        ("-5", "5"),
        ("5", "-5"),
        ("-5", "-6"),
        ("+5", "5"),
        ("(\"a\",)", "\"a\""),
        ("[1, 2]", "[1, 2, 3]"),
        ("{\"a\": 1}", "{\"a\": -1}"),
        ("(1, (2, 3))", "(1, [2, 3])"),
        ("(1,)", "1"),
    ];
    for (old, new) in pairs {
        assert_eq!(edit(old, new), [at(path, 2, "expected-value-changed")], "{old} to {new}");
    }
    // The same value written in other parentheses or layout, and a value that is no literal, which the shape holds.
    let same = [("(5)", "5"), ("-(5)", "- 5"), ("((1,))", "(1,)"), ("[1, 2]", "[\n        1,\n        2,\n    ]")];
    for (old, new) in same {
        assert_eq!(edit(old, new), [], "{old} to {new}");
    }
    for (old, new) in [("-x", "x"), ("(1, x)", "(1, x, 2)")] {
        assert_eq!(edit(old, new), [], "{old} to {new}");
    }
}

// A test file whose test asserts against a module-level constant: the expected value held outside the assertion.
const GOLDEN_TESTS: &str = "\
import unittest

from calc import add

GOLDEN = \"a\"


class TestGolden(unittest.TestCase):
    def test_add(self):
        self.assertEqual(add(\"\", \"a\"), GOLDEN)
";

const GOLDEN_PATH: &str = "tests/test_golden.py";
const ASSERTION: &str = "        self.assertEqual(add(\"\", \"a\"), GOLDEN)\n";

/// The check's findings once `tests` is tests/test_golden.py's base and the change edits the code and replaces `old`
/// with `new` in that file.
fn found_after_golden_edit(tests: &str, old: &str, new: &str) -> (Vec<Found>, Vec<String>) {
    found(&[code_edit(), edited(GOLDEN_PATH, tests, &replaced(tests, old, new))])
}

/// [`found_after_golden_edit`]'s places when the change makes GOLDEN "b".
fn golden_edited(tests: &str) -> Vec<Found> {
    let old = if tests.contains("GOLDEN: str") { "GOLDEN: str = \"a\"\n\n" } else { "GOLDEN = \"a\"\n\n" };
    found_after_golden_edit(tests, old, &old.replace("\"a\"", "\"b\"")).0
}

fn changed_at_10() -> Vec<Found> {
    vec![at(GOLDEN_PATH, 10, "expected-value-changed")]
}

#[test]
fn an_edited_constant_an_assertion_names_is_an_expected_value_changed() {
    let (place, messages) = found_after_golden_edit(GOLDEN_TESTS, "GOLDEN = \"a\"", "GOLDEN = \"b\"");
    assert_eq!(place, changed_at_10());
    assert_eq!(
        messages,
        ["`GOLDEN` (line 5), which `TestGolden.test_add` names in its assertions, changed, in a change that also edits \
          the code under test."]
    );
    assert_eq!(golden_edited(&GOLDEN_TESTS.replace(", GOLDEN)", ", test_golden.GOLDEN)")), changed_at_10());
    assert_eq!(golden_edited(&GOLDEN_TESTS.replace("GOLDEN = \"a\"", "GOLDEN: str = \"a\"")), changed_at_10());
    assert_eq!(golden_edited(&GOLDEN_TESTS.replace("def test_add(self):", "def test_add[T](self):")), changed_at_10());
}

#[test]
fn an_edited_constant_is_not_found_without_a_code_change_or_when_only_its_layout_changed() {
    let edit = edited(GOLDEN_PATH, GOLDEN_TESTS, &GOLDEN_TESTS.replace("GOLDEN = \"a\"", "GOLDEN = \"b\""));
    assert_eq!(found(&[edit]).0, []);
    assert_eq!(found_after_golden_edit(GOLDEN_TESTS, "GOLDEN = \"a\"", "GOLDEN = (  # the same\n    'a')").0.len(), 1);
    assert_eq!(found_after_golden_edit(GOLDEN_TESTS, "GOLDEN = \"a\"", "GOLDEN = 'a'  # the same").0, []);
    assert_eq!(
        found_after_golden_edit(GOLDEN_TESTS, "from calc import add\n", "from calc import add  # the code\n").0,
        []
    );
}

#[test]
fn a_constant_added_with_or_for_its_test_is_not_found() {
    assert_eq!(found(&[code_edit(), added(GOLDEN_PATH, GOLDEN_TESTS)]).0, []);
    let base = GOLDEN_TESTS.replace(", GOLDEN)", ", \"a\")").replace("GOLDEN = \"a\"\n", "");
    assert_eq!(found(&[code_edit(), edited(GOLDEN_PATH, &base, &GOLDEN_TESTS.replace("\"a\"\n", "\"b\"\n"))]).0, []);
}

#[test]
fn a_constant_computed_by_a_call_is_none_and_one_that_becomes_so_or_is_bound_again_is_no_longer_one() {
    let computed = GOLDEN_TESTS.replace("GOLDEN = \"a\"", "GOLDEN = str(\"a\")");
    assert_eq!(found_after_golden_edit(&computed, "str(\"a\")", "str(\"b\")").0, []);
    assert_eq!(found_after_golden_edit(GOLDEN_TESTS, "GOLDEN = \"a\"", "GOLDEN = str(\"b\")").0, changed_at_10());
    let (place, messages) =
        found_after_golden_edit(GOLDEN_TESTS, "GOLDEN = \"a\"\n", "GOLDEN = \"a\"\nGOLDEN = \"b\"\n");
    assert_eq!(place, [at(GOLDEN_PATH, 11, "expected-value-changed")]);
    assert!(
        messages[0].starts_with(
            "`GOLDEN` (line 5 at the base), which `TestGolden.test_add` names in its assertions, \
             is no longer a constant"
        ),
        "{messages:?}"
    );
}

#[test]
fn two_tests_asserting_against_one_edited_constant_are_one_finding_each() {
    let tests = format!(
        "{GOLDEN_TESTS}\n    def test_again(self):\n        self.assertEqual(add(\"a\", \"\"), GOLDEN)\n\
         \x20       self.assertNotEqual(add(\"\", \"\").upper(), GOLDEN)\n"
    );
    let (place, messages) = found_after_golden_edit(&tests, "GOLDEN = \"a\"", "GOLDEN = \"b\"");
    assert_eq!(place, [at(GOLDEN_PATH, 10, "expected-value-changed"), at(GOLDEN_PATH, 13, "expected-value-changed")]);
    assert!(messages[1].contains("`TestGolden.test_again` names in its assertions"), "{messages:?}");
}

#[test]
fn a_function_s_local_of_the_constant_s_name_leaves_it_a_constant() {
    let tests =
        format!("{GOLDEN_TESTS}\n\ntry:\n    import json\nexcept ImportError:\n    def helper():\n\
                 \x20       GOLDEN = 1\n");
    assert_eq!(golden_edited(&tests), changed_at_10());
}

#[test]
fn a_constant_s_sign_or_type_changed_is_an_expected_value_changed() {
    for (old, new) in [("-5", "5"), ("1", "True"), ("(\"a\",)", "\"a\"")] {
        let tests = GOLDEN_TESTS.replace("GOLDEN = \"a\"", &format!("GOLDEN = {old}"));
        let place = found_after_golden_edit(&tests, &format!("GOLDEN = {old}\n"), &format!("GOLDEN = {new}\n")).0;
        assert_eq!(place, changed_at_10(), "{old} to {new}");
    }
}

#[test]
fn a_constant_named_through_the_module_s_package_or_an_alias_is_an_expected_value_changed() {
    for (spelled, imported) in [
        ("tests.test_golden", "\n"),
        ("tg", "import tests.test_golden as tg\n"),
        ("tg", "from tests import test_golden as tg\n"),
    ] {
        let tests = GOLDEN_TESTS
            .replace(", GOLDEN)", &format!(", {spelled}.GOLDEN)"))
            .replace("from calc import add\n\n", &format!("from calc import add\n{imported}"));
        assert_eq!(golden_edited(&tests), changed_at_10(), "{spelled} {imported}");
    }
}

#[test]
fn a_name_the_test_binds_anywhere_is_not_the_constant() {
    // Shadowing is not resolved: any binding of the name in the test, at any depth, leaves the constant out.
    for inner in [
        "        GOLDEN = \"a\"\n        self.assertEqual(add(\"\", \"a\"), GOLDEN)\n",
        "        try:\n            add(None, 1)\n        except TypeError as GOLDEN:\n\
         \x20           self.assertIn(\"x\", str(GOLDEN))\n",
        "        def check(GOLDEN):\n            assert add('', 'a') == GOLDEN\n\n        check('a')\n",
        "        assert all(GOLDEN for GOLDEN in ['a'])\n",
        "        assert [GOLDEN for GOLDEN in ['a']]\n",
        "        assert (lambda GOLDEN: GOLDEN)('a')\n",
        "        class F:\n            GOLDEN = 1\n\n            def m(self):\n\
         \x20               assert add(\"\", \"a\") == GOLDEN\n\n        F().m()\n",
        "        assert (lambda GOLDEN=GOLDEN: add(\"\", \"a\") == GOLDEN)()\n",
        "        assert all(GOLDEN for GOLDEN in [GOLDEN])\n",
        "        global GOLDEN\n        assert add(\"\", \"a\") == GOLDEN\n",
        "        assert add(\"\", \"a\") == GOLDEN\n        del GOLDEN\n",
        "        from calc import add as GOLDEN\n        assert add(\"\", \"a\") == GOLDEN\n",
        "        x = GOLDEN\n        assert [GOLDEN for GOLDEN in \"b\"] and x\n",
    ] {
        assert_eq!(golden_edited(&GOLDEN_TESTS.replace(ASSERTION, inner)), [], "{inner}");
    }
}

#[test]
fn a_constant_read_inside_nested_generators_or_lambdas_is_found() {
    for inner in [
        "        assert all(GOLDEN == x for x in (y for y in \"a\"))\n",
        "        assert all(x for x in (y == GOLDEN for y in \"a\"))\n",
        "        assert (lambda f=(lambda: 0): add(\"\", \"a\") == GOLDEN)()\n",
    ] {
        assert_eq!(golden_edited(&GOLDEN_TESTS.replace(ASSERTION, inner)), changed_at_10(), "{inner}");
    }
}

#[test]
fn an_attribute_of_a_name_the_test_binds_is_not_the_module_s_constant() {
    let aliased =
        GOLDEN_TESTS.replace("from calc import add\n\n", "from calc import add\nimport tests.test_golden as tg\n");
    for spelled in ["tg", "test_golden", "tests"] {
        let read =
            if spelled == "tests" { "tests.test_golden.GOLDEN".to_string() } else { format!("{spelled}.GOLDEN") };
        let tests = aliased
            .replace("    def test_add(self):\n", &format!("    def test_add(self, {spelled}=None):\n"))
            .replace(", GOLDEN)", &format!(", {read})"));
        assert_eq!(golden_edited(&tests), [], "{spelled}");
    }
    // The module imported inside the test binds the root of module.NAME there.
    for lines in [
        &["import tests.test_golden", "self.assertEqual(add(\"\", \"a\"), tests.test_golden.GOLDEN)"][..],
        &["from tests import test_golden", "self.assertEqual(add(\"\", \"a\"), test_golden.GOLDEN)"],
        &[
            "import tests.test_golden as tg",
            "",
            "def check():",
            "    assert add(\"\", \"a\") == tg.GOLDEN",
            "",
            "check()",
        ],
        &["if tg:", "    import tests.test_golden as tg", "self.assertEqual(add(\"\", \"a\"), tg.GOLDEN)"],
    ] {
        let inner: String = lines.iter().map(|x| format!("        {x}\n")).collect();
        let tests = GOLDEN_TESTS
            .replace("    def test_add(self):\n", "    def test_add(self, tg=None):\n")
            .replace(ASSERTION, &inner);
        assert_eq!(golden_edited(&tests), [], "{lines:?}");
    }
}

#[test]
fn a_test_file_that_does_not_parse_or_cannot_be_read_says_so() {
    let broken = edited(OTHER_PATH, OTHER_TESTS, &OTHER_TESTS.replace("def test_b():", "def test_b(:"));
    let (place, messages) = found(&[broken]);
    assert_eq!(place, [at(OTHER_PATH, 5, "unparsable")]);
    assert_eq!(messages, ["does not parse as Python, so its tests were not checked."]);
    let coded = edited(OTHER_PATH, OTHER_TESTS, &format!("# coding: cp1252\n{OTHER_TESTS}"));
    let (place, messages) = found(&[coded]);
    assert_eq!(place, [at(OTHER_PATH, None, "unreadable")]);
    assert!(
        messages[0].starts_with("could not be read (") && messages[0].ends_with(", so its tests were not checked.")
    );
    // A file with no tests at the base loses none by not parsing.
    assert_eq!(found(&[added("tests/test_new.py", "def test_b(:\n")]).0, []);
}

#[test]
fn a_helper_class_is_no_test_and_only_test_files_are_read() {
    assert_eq!(
        found(&[edited("tests/test_help.py", "class Helper:\n    def make(self):\n        return 1\n", "")]).0,
        []
    );
    let files = [
        deleted("helpers.py", "def test_helper():\n    assert 1\n"),
        added("pkg/sub_test.py", "def test_x():\n    pass\n"),
    ];
    assert_eq!(found(&files).0, []);
}

#[test]
fn a_decorated_or_async_test_and_one_under_a_statement_are_read_as_python_s_runners_find_them() {
    let base = "import pytest\n\n\n@pytest.fixture\nasync def test_a():\n    assert 1\n\n\n\
                if True:\n    def test_hidden():\n        assert 1\n";
    let source = lang_python::read(base.as_bytes(), std::path::Path::new("tests/test_x.py"), true).unwrap();
    let tests: Vec<_> = source.tests.iter().map(|t| (t.name.as_str(), t.line, t.lines.first, t.lines.last)).collect();
    assert_eq!(tests, [("test_a", 5, 4, 6)]);
}

#[test]
fn a_trailing_comma_is_layout_but_for_the_one_that_makes_an_index_a_tuple() {
    let wrapped = "assert add(\n            10,\n            20,\n        ) == 31";
    let (place, _) = found(&[code_edit(), calc_tests_with("assert add(10, 20) == 30", wrapped)]);
    assert_eq!(place, [at(CALC_PATH, 12, "expected-value-changed")]);
    let listed = GOLDEN_TESTS.replace("GOLDEN = \"a\"", "GOLDEN = [\"a\", T[1, 2], T[1]]");
    let golden_became = |new: &str| found_after_golden_edit(&listed, "GOLDEN = [\"a\", T[1, 2], T[1]]", new).0;
    assert_eq!(golden_became("GOLDEN = [\n    \"a\",\n    T[1, 2,],\n    T[1],\n]"), []);
    assert_eq!(golden_became("GOLDEN = [\"a\", T[1, 2], T[1,]]"), changed_at_10());
}

#[test]
fn a_format_spec_is_part_of_an_f_string_s_text_also_one_that_starts_with_an_equals_sign() {
    let base = "def test_s():\n    assert name() == f'{x:>10} {y!r:{w}.3f} {z:=^40}'\n";
    let path = "tests/test_s.py";
    assert_eq!(found(&[code_edit(), edited(path, base, &base.replace('\'', "\""))]).0, []);
    for (old, new) in [(":>10", ":>20"), (".3f", ".4f"), (":=^40", ":=^20"), (":=^40", ":^40")] {
        let (place, _) = found(&[code_edit(), edited(path, base, &base.replace(old, new))]);
        assert_eq!(place, [at(path, 2, "expected-value-changed")], "{old} to {new}");
    }
    // The letters of a spec are no names the assertion reads.
    let spec = "def test_s():\n    assert name() == f'{n:=+5d} {n:>5d}'\n";
    let source = lang_python::read(spec.as_bytes(), std::path::Path::new(path), true).unwrap();
    assert_eq!(source.tests[0].assertions[0].reads, ["name", "n"]);
}
