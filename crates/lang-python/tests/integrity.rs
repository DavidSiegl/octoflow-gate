//! The test-integrity check on Python tests read by lang_python: tests/test_gate.py's TestWeakenedTests, but for what
//! the gate decides over a real change (crates/gate/tests/parser_checks.rs).

mod common;

use checks::integrity::weakened_tests;
use checks::ChangedFile;
use common::{added, among_tests, changed, deleted, edited};

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

/// `text` with its one `old` replaced by `new`.
fn replaced(text: &str, old: &str, new: &str) -> String {
    assert_eq!(text.matches(old).count(), 1, "{old:?}");
    text.replacen(old, new, 1)
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
    let (place, messages) = found(&[calc_tests_with("    def test_large(self):\n        assert add(10, 20) == 30\n\n", "")]);
    assert_eq!(place, [at(CALC_PATH, 11, "removed-test")]);
    assert_eq!(messages, ["`TestAdd.test_large` is gone; this was line 11 at the base."]);
}

#[test]
fn a_removed_class_is_one_finding_with_its_tests() {
    let class = &CALC_TESTS[CALC_TESTS.find("class TestNegative").unwrap()..CALC_TESTS.find("def test_function").unwrap()];
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
    let moved = changed(Some((OTHER_PATH, OTHER_TESTS)), Some(("tests/test_moved.py", &OTHER_TESTS.replace("test_b", "test_bee"))));
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
fn added_skips_are_found_at_their_lines() {
    let calc = replaced(CALC_TESTS, "    def test_small(self):\n", "    @unittest.skip('later')\n    def test_small(self):\n");
    let calc = replaced(&calc, "    def test_one(self):\n        assert", "    def test_one(self):\n        self.skipTest('flaky')\n        assert");
    let other = replaced(OTHER_TESTS, "def test_b():\n", "import pytest\n\n\n@pytest.mark.xfail\ndef test_b():\n    pytest.skip('no')\n");
    let (place, messages) = found(&[edited(CALC_PATH, CALC_TESTS, &calc), edited(OTHER_PATH, OTHER_TESTS, &other)]);
    assert_eq!(
        place,
        [at(CALC_PATH, 7, "skip"), at(CALC_PATH, 18, "skip"), at(OTHER_PATH, 8, "skip"), at(OTHER_PATH, 10, "skip")]
    );
    assert_eq!(messages[0], "`unittest.skip` added: it skips a test or expects it to fail.");
    assert_eq!(messages[2], "`pytest.mark.xfail` added: it skips a test or expects it to fail.");
}

#[test]
fn a_skip_is_a_name_python_reads_not_one_it_binds_or_passes_by_keyword() {
    let other = format!("{OTHER_TESTS}\n\nfrom unittest import skip as skipIf\n\n\ndef test_c(xfail=1):\n    assert run(skip=xfail)\n");
    let (place, messages) = found(&[edited(OTHER_PATH, OTHER_TESTS, &other)]);
    assert_eq!(place, [at(OTHER_PATH, 13, "skip")]);
    assert_eq!(messages, ["`xfail` added: it skips a test or expects it to fail."]);
}

#[test]
fn skips_already_at_the_base_are_not_found() {
    let base = "import pytest\n\n\n@pytest.mark.skipif(True, reason='x')\ndef test_x():\n    assert 1\n";
    assert_eq!(found(&[edited("tests/test_skipped.py", base, &base.replace("assert 1", "assert 2"))]).0, []);
}

#[test]
fn fewer_assertions_are_found_at_the_test() {
    let calc = replaced(CALC_TESTS, "        self.assertEqual(add(0, 0), 0)\n", "");
    let calc = replaced(&calc, "        with self.assertRaises(TypeError):\n            add(None, 1)\n", "        add(None, 1)\n");
    let (place, messages) = found(&[edited(CALC_PATH, CALC_TESTS, &calc)]);
    assert_eq!(place, [at(CALC_PATH, 7, "fewer-assertions"), at(CALC_PATH, 18, "fewer-assertions")]);
    assert_eq!(messages[0], "`TestAdd.test_small` has 1 assertions, 2 at the base.");
}

#[test]
fn more_assertions_and_new_tests_are_not_found() {
    let calc = calc_tests_with("        assert add(10, 20) == 30\n", "        assert add(10, 20) == 30\n        assert add(20, 10) == 30\n");
    let other = edited(OTHER_PATH, OTHER_TESTS, &replaced(OTHER_TESTS, "def test_a():", "def test_new():\n    assert 3\n\n\ndef test_a():"));
    assert_eq!(found(&[calc, other]).0, []);
}

#[test]
fn swallowed_errors_are_found() {
    let calc = replaced(CALC_TESTS, "        assert add(-1, -1) == -2\n", "        try:\n            assert add(-1, -1) == -2\n        except:\n            pass\n");
    let calc = replaced(
        &calc,
        "    assert add(2, 2) == 4\n",
        "    try:\n        assert add(2, 2) == 4\n    except (Exception) as e:  # all of them\n        'ignored'\n        ...\n\
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
fn a_string_requoted_is_no_changed_value_and_one_of_another_prefix_or_text_is() {
    let base = "def test_s():\n    assert name() == 'a' \"b\"\n    assert f'{x}-{y}' == b'c'\n";
    let path = "tests/test_s.py";
    assert_eq!(found(&[code_edit(), edited(path, base, &base.replace("'a' \"b\"", "\"ab\""))]).0, []);
    for (old, new) in [("'a'", "'A'"), ("b'c'", "'c'"), ("-{y}", "+{y}")] {
        let (place, _) = found(&[code_edit(), edited(path, base, &base.replace(old, new))]);
        assert_eq!(place.len(), 1, "{old} to {new}: {place:?}");
        assert_eq!(place[0].2, "expected-value-changed");
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
    assert_eq!(found_after_golden_edit(GOLDEN_TESTS, "from calc import add\n", "from calc import add  # the code\n").0, []);
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
    let (place, messages) = found_after_golden_edit(GOLDEN_TESTS, "GOLDEN = \"a\"\n", "GOLDEN = \"a\"\nGOLDEN = \"b\"\n");
    assert_eq!(place, [at(GOLDEN_PATH, 11, "expected-value-changed")]);
    assert!(
        messages[0].starts_with(
            "`GOLDEN` (line 5 at the base), which `TestGolden.test_add` names in its assertions, is no longer a constant"
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
    let tests = format!("{GOLDEN_TESTS}\n\ntry:\n    import json\nexcept ImportError:\n    def helper():\n        GOLDEN = 1\n");
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
        "        try:\n            add(None, 1)\n        except TypeError as GOLDEN:\n            self.assertIn(\"x\", str(GOLDEN))\n",
        "        def check(GOLDEN):\n            assert add('', 'a') == GOLDEN\n\n        check('a')\n",
        "        assert all(GOLDEN for GOLDEN in ['a'])\n",
        "        assert [GOLDEN for GOLDEN in ['a']]\n",
        "        assert (lambda GOLDEN: GOLDEN)('a')\n",
        "        class F:\n            GOLDEN = 1\n\n            def m(self):\n                assert add(\"\", \"a\") == GOLDEN\n\n        F().m()\n",
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
    let aliased = GOLDEN_TESTS.replace("from calc import add\n\n", "from calc import add\nimport tests.test_golden as tg\n");
    for spelled in ["tg", "test_golden", "tests"] {
        let read = if spelled == "tests" { "tests.test_golden.GOLDEN".to_string() } else { format!("{spelled}.GOLDEN") };
        let tests = aliased
            .replace("    def test_add(self):\n", &format!("    def test_add(self, {spelled}=None):\n"))
            .replace(", GOLDEN)", &format!(", {read})"));
        assert_eq!(golden_edited(&tests), [], "{spelled}");
    }
    // The module imported inside the test binds the root of module.NAME there.
    for lines in [
        &["import tests.test_golden", "self.assertEqual(add(\"\", \"a\"), tests.test_golden.GOLDEN)"][..],
        &["from tests import test_golden", "self.assertEqual(add(\"\", \"a\"), test_golden.GOLDEN)"],
        &["import tests.test_golden as tg", "", "def check():", "    assert add(\"\", \"a\") == tg.GOLDEN", "", "check()"],
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
    assert!(messages[0].starts_with("could not be read (") && messages[0].ends_with(", so its tests were not checked."));
    // A file with no tests at the base loses none by not parsing.
    assert_eq!(found(&[added("tests/test_new.py", "def test_b(:\n")]).0, []);
}

#[test]
fn a_helper_class_is_no_test_and_only_test_files_are_read() {
    assert_eq!(found(&[edited("tests/test_help.py", "class Helper:\n    def make(self):\n        return 1\n", "")]).0, []);
    let files = [deleted("helpers.py", "def test_helper():\n    assert 1\n"), added("pkg/sub_test.py", "def test_x():\n    pass\n")];
    assert_eq!(found(&files).0, []);
}

#[test]
fn a_decorated_or_async_test_and_one_under_a_statement_are_read_as_python_s_runners_find_them() {
    let base = "import pytest\n\n\n@pytest.fixture\nasync def test_a():\n    assert 1\n\n\nif True:\n    def test_hidden():\n        assert 1\n";
    let source = lang_python::read(base.as_bytes(), std::path::Path::new("tests/test_x.py"), true).unwrap();
    let tests: Vec<_> = source.tests.iter().map(|t| (t.name.as_str(), t.line, t.lines.first, t.lines.last)).collect();
    assert_eq!(tests, [("test_a", 5, 4, 6)]);
}
