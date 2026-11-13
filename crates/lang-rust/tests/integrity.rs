//! The test-integrity check on Rust tests read by lang_rust, over a Cargo package `crate` with unit tests in an
//! inline module and integration tests under tests/, and the platform-only cfgs and attribute reading that
//! skips.rs's own tests leave out.

mod common;

use checks::integrity::{edits_code, weakened_tests};
use checks::{ChangedFile, Finding};
use common::{added, among_tests, changed, deleted, edited};
use model::WeakeningKind;

const LIB: &str = "\
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adds_small() {
        assert_eq!(add(1, 2), 3);
        assert_eq!(add(0, 0), 0);
    }

    #[test]
    fn adds_large() {
        assert_eq!(add(10, 20), 30);
    }
}
";

const IT: &str = "\
use tiny::add;

#[test]
fn negative() {
    assert_eq!(add(-1, -1), -2);
}

#[tokio::test]
async fn zero() {
    assert!(add(0, 0) == 0);
}
";

const LIB_PATH: &str = "crate/src/lib.rs";
const IT_PATH: &str = "crate/tests/it.rs";

/// `text` with its first `old` replaced by `new`, as the Python tests' `edit` does; panics where `old` is not in it.
fn edit(text: &str, old: &str, new: &str) -> String {
    assert!(text.contains(old), "{old:?} is not in the text");
    text.replacen(old, new, 1)
}

/// Whether the change of `files`, every one in the Cargo package `crate`, edits the code under test, as the gate
/// tells it: a file the default settings do not place among the tests that [`edits_code`] says it edits.
fn code_changed(files: &[ChangedFile]) -> bool {
    let code = |side: &Option<checks::Side>| side.as_ref().is_some_and(|s| !among_tests(s.path.to_str().unwrap()));
    files.iter().any(|f| (code(&f.before) || code(&f.after)) && edits_code(f))
}

/// The check's findings on the change of `files`, its code_changed as the gate tells it.
fn check(files: &[ChangedFile]) -> Vec<Finding> {
    weakened_tests(files, code_changed(files))
}

/// (path, line, rule) of each finding, the rule without the check's name.
fn wheres(found: &[Finding]) -> Vec<(String, Option<u32>, String)> {
    found
        .iter()
        .map(|f| {
            let path = f.path.as_ref().map(|p| p.to_str().unwrap().to_string()).unwrap_or_default();
            let rule = f.rule.strip_prefix("test-integrity ").expect("a test-integrity rule");
            (path, f.line, rule.to_string())
        })
        .collect()
}

/// (path, line, rule) for an expected finding.
fn at(path: &str, line: Option<u32>, rule: &str) -> (String, Option<u32>, String) {
    (path.into(), line, rule.into())
}

#[test]
fn an_unchanged_tree_and_an_edit_beside_the_tests_have_no_findings() {
    assert_eq!(check(&[]), vec![]);
    let files = [edited(LIB_PATH, LIB, &edit(LIB, "    a + b\n", "    b + a\n"))];
    assert!(code_changed(&files));
    assert_eq!(check(&files), vec![]);
}

#[test]
fn a_removed_test_is_found_at_its_line_at_the_base() {
    let it = edit(IT, "#[test]\nfn negative() {\n    assert_eq!(add(-1, -1), -2);\n}\n\n", "");
    let lib = edit(LIB, "    #[test]\n    fn adds_large() {\n        assert_eq!(add(10, 20), 30);\n    }\n", "");
    let found = check(&[edited(LIB_PATH, LIB, &lib), edited(IT_PATH, IT, &it)]);
    assert_eq!(wheres(&found), vec![at(LIB_PATH, Some(16), "removed-test"), at(IT_PATH, Some(4), "removed-test")]);
    assert_eq!(found[0].message, "`tests::adds_large` is gone; this was line 16 at the base.");
}

#[test]
fn a_test_moved_into_another_file_under_another_name_is_not_removed() {
    let lib = edit(LIB, "    #[test]\n    fn adds_large() {\n        assert_eq!(add(10, 20), 30);\n    }\n", "");
    let it = edit(IT, "use tiny::add;\n", "use tiny::add;\n\n#[test]\nfn large() {\n        assert_eq!(add(10,20),30);\n}\n");
    assert_eq!(check(&[edited(LIB_PATH, LIB, &lib), edited(IT_PATH, IT, &it)]), vec![]);
}

#[test]
fn a_test_moved_with_the_skips_it_had_adds_none_but_a_skip_added_on_the_way_is_found() {
    let large = "    #[test]\n    fn adds_large() {\n        assert_eq!(add(10, 20), 30);\n    }\n";
    let moved = |attributes: &str| {
        edit(IT, "use tiny::add;\n", &format!("use tiny::add;\n\n#[test]\n{attributes}fn large() {{\n    assert_eq!(add(10, 20), 30);\n}}\n"))
    };
    let had = |attributes: &str| edit(LIB, large, &format!("    #[test]\n{attributes}    fn adds_large() {{\n        assert_eq!(add(10, 20), 30);\n    }}\n"));
    let lib = edit(LIB, large, "");
    let check_move = |base: &str, it: &str| check(&[edited(LIB_PATH, base, &lib), edited(IT_PATH, IT, it)]);
    for attributes in ["#[cfg(unix)]\n", "#[ignore]\n", "#[cfg(unix)]\n#[ignore]\n"] {
        let indented: String = attributes.lines().map(|a| format!("    {a}\n")).collect();
        assert_eq!(check_move(&had(&indented), &moved(attributes)), vec![], "{attributes}");
    }
    let found = check_move(&had("    #[cfg(unix)]\n"), &moved("#[cfg(unix)]\n#[ignore]\n"));
    assert_eq!(wheres(&found), vec![at(IT_PATH, Some(5), "skip")]);
    assert_eq!(found[0].severity, "error");
    assert_eq!(found[0].message, "`#[ignore]` added: it skips a test or turns it off.");
    // The same attribute's first line, wrapped as rustfmt wraps it, with another platform after it.
    let wrapped = |os: &str| format!("#[cfg(all(\n    unix,\n    target_os = \"{os}\"\n))]\n");
    let indented: String = wrapped("linux").lines().map(|a| format!("    {a}\n")).collect();
    let found = check_move(&had(&indented), &moved(&wrapped("macos")));
    assert_eq!(wheres(&found), vec![at(IT_PATH, Some(4), "platform-only")]);
}

#[test]
fn a_kept_test_carries_its_skips_but_not_one_whose_condition_changed() {
    let with = |attributes: &str| edit(LIB, "    #[test]\n    fn adds_large", &format!("    #[test]\n{attributes}    fn adds_large"));
    let base = with("    #[ignore]\n    #[cfg(unix)]\n");
    let reordered = with("    #[cfg(unix)]\n    #[ignore]\n");
    assert_eq!(check(&[edited(LIB_PATH, &base, &reordered)]), vec![]);
    let base = with("    #[cfg_attr(miri, ignore)]\n");
    let found = check(&[edited(LIB_PATH, &base, &with("    #[cfg_attr(unix, ignore)]\n"))]);
    assert_eq!(wheres(&found), vec![at(LIB_PATH, Some(16), "skip")]);
    // A wrapped cfg whose inner line alone changed, at that line.
    let base = with("    #[cfg_attr(\n        miri,\n        ignore\n    )]\n");
    let found = check(&[edited(LIB_PATH, &base, &edit(&base, "        miri,", "        unix,"))]);
    assert_eq!(wheres(&found), vec![at(LIB_PATH, Some(17), "skip")]);
}

#[test]
fn a_discard_is_its_whole_statement_a_swallowed_closure_s_body_included() {
    let with = |body: &str| edit(LIB, "        assert_eq!(add(10, 20), 30);\n", body);
    let found = |base: &str, now: &str| wheres(&check(&[edited(LIB_PATH, base, now)]));
    // rustfmt's wraps of a long `let` and of a method chain, each made a discard on a line the value is not on.
    let kept = with("        let r =\n            std::panic::catch_unwind(|| assert_eq!(add(1, 1), 2));\n        r.unwrap();\n");
    assert_eq!(found(&kept, &edit(&kept, "let r =", "let _ =")), vec![at(LIB_PATH, Some(17), "swallowed-error")]);
    let kept = with("        std::panic::catch_unwind(|| {\n            assert_eq!(add(1, 1), 2);\n        })\n        .unwrap();\n");
    assert_eq!(found(&kept, &edit(&kept, ".unwrap();", ".ok();")), vec![at(LIB_PATH, Some(20), "swallowed-error")]);
    // An assertion added inside a swallowed closure can never fail.
    let base = with("        let _ = std::panic::catch_unwind(|| {\n            assert_eq!(add(1, 1), 2);\n        });\n");
    let more = edit(&base, "        });", "            assert_eq!(add(2, 2), 4);\n        });");
    assert_eq!(found(&base, &more), vec![at(LIB_PATH, Some(19), "swallowed-error")]);
}

#[test]
fn an_attribute_misread_in_a_string_reaches_no_further_than_its_line() {
    let base = edit(LIB, "        assert_eq!(add(10, 20), 30);\n", "        #[cfg(unix)] let s = \"#[cfg(windows\";\n        assert_eq!(add(10, 20), 30);\n");
    let more = edit(&base, "        assert_eq!(add(10, 20), 30);\n", "        assert_eq!(add(10, 20), 30);\n        assert_eq!(add(1, 2), 3);\n");
    assert_eq!(wheres(&check(&[edited(LIB_PATH, &base, &more)])), vec![]);
}

#[test]
fn an_inline_module_whose_tests_all_went_is_one_finding() {
    let lib = &LIB[..LIB.find("\n#[cfg(test)]").unwrap()];
    let found = check(&[edited(LIB_PATH, LIB, lib)]);
    assert_eq!(wheres(&found), vec![at(LIB_PATH, Some(6), "removed-test")]);
    assert_eq!(found[0].message, "`tests` and its 2 tests are gone; this was line 6 at the base.");
}

#[test]
fn two_tests_of_one_name_are_two_tests() {
    // Under cfgs that never hold together, as two platforms' versions of one test. Wrapped so that no line starts
    // with a cfg, which the gate would read as a skip added to this test's body (#47).
    let twins = "#[cfg(unix)]\n#[test]\nfn twin() {\n    assert!(add(1, 1) == 2);\n}\n\n#[cfg(not(unix))]\n\
                 #[test]\nfn twin() {\n    assert!(add(2, 2) == 4);\n    assert!(add(0, 2) == 2);\n}\n";
    let base = format!("use tiny::add;\n\n{twins}");
    let path = "crate/tests/twins.rs";
    let fewer = edit(&base, "    assert!(add(0, 2) == 2);\n", "");
    assert_eq!(wheres(&check(&[edited(path, &base, &fewer)])), vec![at(path, Some(11), "fewer-assertions")]);
    let one = format!("use tiny::add;\n\n{}", &twins[..twins.find("\n#[cfg(not(unix))]").unwrap()]);
    assert_eq!(wheres(&check(&[edited(path, &base, &one)])), vec![at(path, Some(11), "removed-test")]);
}

#[test]
fn a_deleted_test_file_is_one_finding() {
    let found = check(&[deleted(IT_PATH, IT)]);
    assert_eq!(wheres(&found), vec![at(IT_PATH, None, "removed-test")]);
    assert_eq!(found[0].message, "deleted, with its 2 tests: `negative`, `zero`.");
}

#[test]
fn added_skips_are_found_at_their_lines() {
    let it = edit(IT, "use tiny::add;\n", "#![cfg(feature = \"slow\")]\nuse tiny::add;\n");
    let it = edit(&it, "#[test]\nfn negative", "#[test]\n#[ignore]\nfn negative");
    let it = edit(&it, "#[tokio::test]\n", "#[tokio::test] #[cfg_attr(miri, ignore)]\n");
    let lib = edit(LIB, "#[cfg(test)]\n", "#[cfg(test)]\n#[cfg(miri)]\n");
    let lib = edit(&lib, "        assert_eq!(add(10, 20), 30);\n", "        #[cfg(any())]\n        assert_eq!(add(10, 20), 30);\n");
    let found = check(&[edited(LIB_PATH, LIB, &lib), edited(IT_PATH, IT, &it)]);
    assert_eq!(
        wheres(&found),
        vec![
            at(LIB_PATH, Some(6), "skip"),
            at(LIB_PATH, Some(18), "skip"),
            at(IT_PATH, Some(1), "skip"),
            at(IT_PATH, Some(5), "skip"),
            at(IT_PATH, Some(10), "skip"),
        ]
    );
    assert_eq!(found[4].message, "`#[cfg_attr(miri, ignore)]` added: it skips a test or turns it off.");
}

#[test]
fn a_platform_cfg_on_a_test_its_module_or_its_file_is_weighed_not_a_skip() {
    let it = edit(IT, "use tiny::add;\n", "#![cfg(unix)]\nuse tiny::add;\n");
    let it = edit(&it, "#[test]\nfn negative", "#[test]\n#[cfg(target_os = \"linux\")]\nfn negative");
    let it = edit(
        &it,
        "#[tokio::test]\n",
        "#[tokio::test]\n#[cfg(all(test, any(target_os = \"linux\", target_os = \"macos\")))]\n",
    );
    // As rustfmt wraps a long one.
    let lib = edit(LIB, "#[cfg(test)]\n", "#[cfg(test)]\n#[cfg(any(\n    target_os = \"linux\",\n    target_os = \"macos\"\n))]\n");
    let lib = edit(&lib, "    #[test]\n    fn adds_small", "    #[test]\n    #[cfg(unix)]\n    fn adds_small");
    let found = check(&[edited(LIB_PATH, LIB, &lib), edited(IT_PATH, IT, &it)]);
    assert_eq!(
        wheres(&found),
        vec![
            at(LIB_PATH, Some(6), "platform-only"),
            at(LIB_PATH, Some(14), "platform-only"),
            at(IT_PATH, Some(1), "platform-only"),
            at(IT_PATH, Some(5), "platform-only"),
            at(IT_PATH, Some(11), "platform-only"),
        ]
    );
    assert_eq!(found[3].message, "`#[cfg(target_os = \"linux\")]` added: it turns a test off on the other platforms.");
}

#[test]
fn a_platform_cfg_with_another_condition_or_inside_a_test_is_a_skip() {
    let it = edit(
        IT,
        "#[test]\nfn negative",
        "#[test]\n#[cfg(feature = \"slow\")]\n#[cfg(all(unix, feature = \"x\"))]\nfn negative",
    );
    let it = edit(&it, "#[tokio::test]\n", "#[tokio::test]\n#[cfg(all(unix, not(test)))]\n");
    // A skip after a wrapped platform cfg's closing bracket, at its own line, the platform cfg weighed at its.
    let lib = edit(LIB, "#[cfg(test)]\n", "#[cfg(test)]\n#[cfg(any(\n    unix,\n))] #[cfg(miri)]\n");
    let lib = edit(&lib, "    #[test]\n    fn adds_small", "    #[test] #[cfg(unix)] #[ignore]\n    fn adds_small");
    let lib =
        edit(&lib, "    #[test]\n    fn adds_large", "    #[test]\n    #[cfg(all(test, feature = \"slow\"))]\n    fn adds_large");
    let lib = edit(&lib, "        assert_eq!(add(10, 20), 30);\n", "        #[cfg(unix)]\n        assert_eq!(add(10, 20), 30);\n");
    let found = check(&[edited(LIB_PATH, LIB, &lib), edited(IT_PATH, IT, &it)]);
    assert_eq!(
        wheres(&found),
        vec![
            at(LIB_PATH, Some(6), "platform-only"),
            at(LIB_PATH, Some(8), "skip"),
            at(LIB_PATH, Some(12), "skip"),
            at(LIB_PATH, Some(19), "skip"),
            at(LIB_PATH, Some(21), "skip"),
            at(IT_PATH, Some(4), "skip"),
            at(IT_PATH, Some(5), "skip"),
            at(IT_PATH, Some(11), "skip"),
        ]
    );
    assert_eq!(found[1].message, "`#[cfg(miri)]` added: it skips a test or turns it off.");
    assert_eq!(found[2].message, "`#[ignore]` added: it skips a test or turns it off.");
}

#[test]
fn a_new_test_module_and_attributes_that_skip_nothing_are_not_skips() {
    let lib = edit(LIB, "}\n\n#[cfg(test)]", "}\n\n#[cfg(unix)]\npub fn unix() {}\n\n#[cfg(test)]");
    let it = edit(
        IT,
        "#[test]\nfn negative() {",
        "#[test]\n#[cfg_attr(feature = \"ignore-me\", allow(dead_code))] fn negative() { let _ignore = 1;",
    );
    let more = "#[cfg(test)]\nmod tests {\n    #[test]\n    #[should_panic(expected = \"x\")]\n    fn panics() {\n        \
                panic!(\"x\");\n    }\n}\n";
    let files = [edited(LIB_PATH, LIB, &lib), edited(IT_PATH, IT, &it), added("crate/src/more.rs", more)];
    assert_eq!(check(&files), vec![]);
}

#[test]
fn a_helper_removed_from_a_test_module_is_no_removed_test() {
    let base = edit(LIB, "    #[test]\n    fn adds_small", "    fn one() -> i32 {\n        1\n    }\n\n    #[test]\n    fn adds_small");
    assert_eq!(check(&[edited(LIB_PATH, &base, LIB)]), vec![]);
}

#[test]
fn attributes_in_or_on_a_module_that_holds_no_test_are_not_skips() {
    // Modules before and after the one holding the tests; the attributes are kept off the start of a line here, as
    // the gate would read them as skips added to this test (#47).
    let base = edit(LIB, "\n#[cfg(test)]", "\nmod before {\n    pub fn x() {}\n}\n\n#[cfg(test)]");
    let base = format!("{base}\nmod after {{\n    pub fn y() {{}}\n}}\n");
    let inner = "    #![cfg(feature = \"slow\")]\n";
    let after = edit(&base, "mod before {\n", &format!("mod before {{\n{inner}"));
    let after = edit(&after, "\nmod after {\n", &format!("\n#[cfg(feature = \"slow\")]\nmod after {{\n{inner}"));
    assert_eq!(check(&[edited(LIB_PATH, &base, &after)]), vec![]);
}

#[test]
fn swallowed_errors_are_found() {
    let it = edit(
        IT,
        "    assert_eq!(add(-1, -1), -2);\n",
        "    let _ = std::panic::catch_unwind(|| assert_eq!(add(-1, -1), -2));\n",
    );
    let it = edit(&it, "#[tokio::test]\n", "#[tokio::test]\n#[should_panic]\n");
    let it =
        edit(&it, "    assert!(add(0, 0) == 0);\n", "    assert!(add(0, 0) == 0);\n    _ = std::panic::catch_unwind(|| add(1, 1));\n");
    let lib = edit(
        LIB,
        "        assert_eq!(add(10, 20), 30);\n",
        "        let _ = check();\n        _ = std::fs::remove_file(\"x\");\n        let _ = (|| assert_eq!(add(10, 20), 30))();\n",
    );
    let found = check(&[edited(LIB_PATH, LIB, &lib), edited(IT_PATH, IT, &it)]);
    assert_eq!(
        wheres(&found),
        vec![
            at(LIB_PATH, Some(19), "swallowed-error"),
            at(IT_PATH, Some(5), "swallowed-error"),
            at(IT_PATH, Some(9), "swallowed-error"),
            at(IT_PATH, Some(12), "swallowed-error"),
        ]
    );
    assert_eq!(
        found[1].message,
        "`let _ = std::panic::catch_unwind(|| assert_eq!(add(-1, -1), -2));` added in `negative`: it throws away the \
         panic or the `Err` that would fail the test."
    );
    assert!(found[2].message.contains("`#[should_panic]` without `expected` added on `zero`"), "{}", found[2].message);
}

#[test]
fn fewer_assertions_are_found_at_the_test() {
    let found = check(&[edited(LIB_PATH, LIB, &edit(LIB, "        assert_eq!(add(0, 0), 0);\n", ""))]);
    assert_eq!(wheres(&found), vec![at(LIB_PATH, Some(10), "fewer-assertions")]);
    assert_eq!(found[0].message, "`tests::adds_small` has 1 assertions, 2 at the base.");
}

#[test]
fn an_expected_value_changed_with_the_code_is_reported_as_the_pair() {
    let lib = edit(LIB, "    a + b\n", "    a + b + 1\n");
    let lib = edit(&lib, "assert_eq!(add(10, 20), 30)", "assert_eq!(add(10, 20), 31)");
    let found = check(&[edited(LIB_PATH, LIB, &lib)]);
    assert_eq!(wheres(&found), vec![at(LIB_PATH, Some(17), "expected-value-changed")]);
    assert_eq!(
        found[0].message,
        "`tests::adds_large`: `assert_eq!(add(10, 20), 30)` became `assert_eq!(add(10, 20), 31)`, in a change that also \
         edits the code under test."
    );
}

// A lib whose tests assert against constants: the expected values held outside the assertions, one at the top level
// and one in the test module.
const HELD: &str = "\
pub fn add(a: i32, b: i32) -> i32 {
    a + b
}

pub static SMALL: i32 = 3;

#[cfg(test)]
mod tests {
    use super::*;

    const EXPECTED: i32 = 30;

    #[test]
    fn adds_small() {
        assert_eq!(add(1, 2), super::SMALL);
    }

    #[test]
    fn adds_large() {
        assert_eq!(add(10, 20), EXPECTED);
    }
}
";

#[test]
fn an_edited_const_or_static_an_assertion_reads_is_an_expected_value_changed() {
    let code = edit(HELD, "    a + b\n", "    a + b + 1\n");
    let lib = edit(&code, "const EXPECTED: i32 = 30;", "const EXPECTED: i32 = 31;");
    let lib = edit(&lib, "static SMALL: i32 = 3;", "static SMALL: i32 = 4;");
    let found = check(&[edited(LIB_PATH, HELD, &lib)]);
    let expected = vec![at(LIB_PATH, Some(15), "expected-value-changed"), at(LIB_PATH, Some(20), "expected-value-changed")];
    assert_eq!(wheres(&found), expected);
    assert_eq!(
        found[1].message,
        "`EXPECTED` (line 11), which `tests::adds_large` names in its assertions, changed, in a change that also edits \
         the code under test."
    );
    // Its layout, or the constant edited without the code, is no finding.
    let relaid = edit(&code, "const EXPECTED: i32 = 30;", "const EXPECTED: i32 =\n        30; // the sum");
    assert_eq!(check(&[edited(LIB_PATH, HELD, &relaid)]), vec![]);
    let alone = edit(HELD, "const EXPECTED: i32 = 30;", "const EXPECTED: i32 = 31;");
    assert_eq!(check(&[edited(LIB_PATH, HELD, &alone)]), vec![]);
}

#[test]
fn a_const_that_calls_is_bound_twice_or_is_shadowed_by_the_test_is_not_followed() {
    let code = edit(HELD, "    a + b\n", "    a + b + 1\n");
    let calls = |expr: &str| edit(&code, "const EXPECTED: i32 = 30;", &format!("const EXPECTED: i32 = {expr};"));
    let base = edit(HELD, "const EXPECTED: i32 = 30;", "const EXPECTED: i32 = add(10, 20);");
    assert_eq!(check(&[edited(LIB_PATH, &base, &calls("add(10, 21)"))]), vec![]);
    // A constructor, by its capital, calls nothing.
    let wrapped = |n: u32| edit(HELD, "const EXPECTED: i32 = 30;", &format!("const EXPECTED: Option<i32> = Some({n});"));
    let some = edit(&wrapped(31), "    a + b\n", "    a + b + 1\n");
    assert_eq!(wheres(&check(&[edited(LIB_PATH, &wrapped(30), &some)])), vec![at(LIB_PATH, Some(20), "expected-value-changed")]);
    let twice = |text: &str| edit(text, "pub static SMALL", "mod other {\n    pub const EXPECTED: i32 = 0;\n}\n\npub static SMALL");
    let edited_twice = edit(&twice(&code), "const EXPECTED: i32 = 30;", "const EXPECTED: i32 = 31;");
    assert_eq!(check(&[edited(LIB_PATH, &twice(HELD), &edited_twice)]), vec![]);
    let shadowed = |text: &str| edit(text, "fn adds_large() {\n", "fn adds_large() {\n        const EXPECTED: i32 = 30;\n");
    let edited_shadowed = edit(&shadowed(&code), "const EXPECTED: i32 = 30;", "const EXPECTED: i32 = 31;");
    assert_eq!(check(&[edited(LIB_PATH, &shadowed(HELD), &edited_shadowed)]), vec![]);
}

#[test]
fn a_const_that_is_one_no_longer_is_reported_at_its_line_at_the_base() {
    let lib = edit(HELD, "    a + b\n", "    a + b + 1\n");
    let lib = edit(&lib, "    const EXPECTED: i32 = 30;\n", "");
    let lib = edit(&lib, "    use super::*;\n", "    use super::*;\n    use crate::consts::EXPECTED;\n");
    let found = check(&[edited(LIB_PATH, HELD, &lib)]);
    assert_eq!(wheres(&found), vec![at(LIB_PATH, Some(20), "expected-value-changed")]);
    assert!(found[0].message.starts_with("`EXPECTED` (line 11 at the base), which `tests::adds_large`"), "{}", found[0].message);
}

#[test]
fn an_expected_value_changed_with_only_the_inline_tests_is_not_found() {
    // lib.rs holds both the tests and the code; the change edits only its test module, at both ends.
    let lib = edit(LIB, "assert_eq!(add(10, 20), 30)", "assert_eq!(add(10, 20), 31)");
    let lib = edit(&lib, "mod tests {\n", "mod tests {\n    // the cases\n");
    let lib = edit(&lib, "    use super::*;\n", "");
    let files = [edited(LIB_PATH, LIB, &lib)];
    assert!(!edits_code(&files[0]));
    assert_eq!(check(&files), vec![]);
}

#[test]
fn an_expected_value_changed_with_rust_code_added_elsewhere_is_found() {
    let it = edited(IT_PATH, IT, &edit(IT, "assert_eq!(add(-1, -1), -2)", "assert_eq!(add(-1, -1), -3)"));
    let three = edit(LIB, "    }\n}\n", "    }\n}\n\npub fn three() -> i32 {\n    3\n}\n");
    for code in [added("crate/build.rs", "fn main() {}\n"), edited(LIB_PATH, LIB, &three)] {
        let found = check(&[code, it.clone()]);
        assert_eq!(wheres(&found), vec![at(IT_PATH, Some(5), "expected-value-changed")]);
    }
}

#[test]
fn an_expected_value_changed_with_rust_code_renamed_deleted_or_a_line_of_it_removed_is_found() {
    let util = "pub fn one() -> i32 {\n    1\n}\n";
    let lib = format!("{LIB}\npub fn two() -> i32 {{\n    2\n}}\n");
    let it = edited(IT_PATH, IT, &edit(IT, "assert_eq!(add(-1, -1), -2)", "assert_eq!(add(-1, -1), -3)"));
    for code in [
        changed(Some(("crate/src/util.rs", util)), Some(("crate/src/helpers.rs", util))),
        deleted("crate/src/util.rs", util),
        edited(LIB_PATH, &lib, &edit(&lib, "\npub fn two() -> i32 {\n    2\n}\n", "")),
    ] {
        let found = check(&[code, it.clone()]);
        assert_eq!(wheres(&found), vec![at(IT_PATH, Some(5), "expected-value-changed")]);
    }
}

#[test]
fn a_test_file_the_front_end_cannot_parse_says_so() {
    let found = check(&[edited(IT_PATH, IT, &edit(IT, "fn negative() {", "fn negative( {"))]);
    assert_eq!(wheres(&found), vec![at(IT_PATH, Some(4), "unparsable")]);
    assert!(found[0].message.starts_with("does not parse as Rust ("), "{}", found[0].message);
    assert!(found[0].message.ends_with(", so its tests were not checked."), "{}", found[0].message);
}

#[test]
fn a_file_under_tests_that_is_not_rust_on_purpose_is_no_finding() {
    let broken = "// A fixture of a parser's error.\nfn broken( {\n";
    let files = [
        edited("crate/tests/fixtures/broken.rs", broken, &edit(broken, "fn broken(", "fn still_broken(")),
        added("crate/tests/fixtures/new.rs", broken),
    ];
    assert_eq!(check(&files), vec![]);
}

#[test]
fn the_check_fails_the_gate_for_a_skip_and_weighs_a_removed_test() {
    let it = edit(IT, "#[test]\nfn negative", "#[test]\n#[ignore]\nfn negative");
    let lib = edit(LIB, "    #[test]\n    fn adds_large() {\n        assert_eq!(add(10, 20), 30);\n    }\n", "");
    let found = check(&[edited(LIB_PATH, LIB, &lib), edited(IT_PATH, IT, &it)]);
    let graded: Vec<(&str, &str)> = found.iter().map(|f| (f.rule.as_str(), f.severity.as_str())).collect();
    assert_eq!(graded, vec![("test-integrity removed-test", "warning"), ("test-integrity skip", "error")]);
}

#[test]
fn every_finding_but_a_weighed_one_fails_the_gate() {
    for (rule, severity) in [
        ("removed-test", "warning"),
        ("fewer-assertions", "warning"),
        ("expected-value-changed", "warning"),
        ("platform-only", "warning"),
        ("skip", "error"),
        ("swallowed-error", "error"),
        ("unparsable", "error"),
    ] {
        let it = match rule {
            "removed-test" => edit(IT, "#[test]\nfn negative() {\n    assert_eq!(add(-1, -1), -2);\n}\n\n", ""),
            "fewer-assertions" => edit(IT, "    assert!(add(0, 0) == 0);\n", ""),
            "expected-value-changed" => edit(IT, "assert_eq!(add(-1, -1), -2)", "assert_eq!(add(-1, -1), -3)"),
            "platform-only" => edit(IT, "#[test]\nfn negative", "#[test]\n#[cfg(unix)]\nfn negative"),
            "skip" => edit(IT, "#[test]\nfn negative", "#[test]\n#[ignore]\nfn negative"),
            "swallowed-error" => edit(IT, "#[tokio::test]\n", "#[tokio::test]\n#[should_panic]\n"),
            _ => edit(IT, "fn negative() {", "fn negative( {"),
        };
        let found = weakened_tests(&[edited(IT_PATH, IT, &it)], true);
        let graded: Vec<(&str, &str)> = found.iter().map(|f| (f.rule.as_str(), f.severity.as_str())).collect();
        assert_eq!(graded, vec![(format!("test-integrity {rule}").as_str(), severity)], "{rule}");
    }
}

/// The kinds of the weakenings lang_rust reads in a file of tests holding `attribute` as written on its own line
/// before a test, or as the file's first line for an inner one; None where the file does not parse.
fn weakenings(attribute: &str) -> Option<Vec<WeakeningKind>> {
    let text = if attribute.starts_with("#!") {
        format!("{attribute}\n#[test]\nfn t() {{}}\n")
    } else if attribute.ends_with('}') {
        format!("#[test]\n{attribute}\n")
    } else {
        format!("#[test]\n{attribute}\nfn t() {{}}\n")
    };
    let source = lang_rust::read(text.as_bytes(), true).ok()?;
    Some(source.weakenings.iter().map(|w| w.kind).collect())
}

#[test]
fn platforms_and_their_combinations_are_platform_only() {
    for attribute in [
        "#[cfg(unix)]",
        "#![cfg(windows)]",
        "#[cfg( target_os = \"linux\" )]",
        "#[cfg(target_family = \"wasm\")] fn f() {}",
        "#[cfg(not(target_os = \"macos\"))]",
        "#[cfg(any(target_os = \"linux\", all(unix, not(target_os = \"macos\"))))]",
        "#[cfg(all(test, unix))]",
        "#[cfg(all(unix, test, not(windows)))]",
        "#[cfg(target_family = \"unix\")]",
        "#[cfg(any( target_os = \"linux\", target_os = \"macos\" ))]",
        "#[cfg(all(unix,))]",
    ] {
        assert_eq!(weakenings(attribute), Some(vec![WeakeningKind::PlatformOnly]), "{attribute}");
    }
}

#[test]
fn any_other_attribute_is_not_platform_only() {
    // Those that do not parse, such as `#[cfg(unix)`, skips.rs's own tests judge.
    for attribute in [
        "#[cfg(test)]",
        "#[cfg(miri)]",
        "#[cfg(feature = \"slow\")]",
        "#[cfg(any())]",
        "#[cfg(all())]",
        "#[cfg(not(test))]",
        "#[cfg(any(test, unix))]",
        "#[cfg(all(unix, feature = \"x\"))]",
        "#[cfg(not(unix, windows))]",
        "#[cfg(target_arch = \"x86\")]",
        "#[cfg(unix = \"yes\")]",
        "#[cfg(target_os)]",
        "#[cfg(all(test))]",
        "#[ignore]",
        "#[cfg_attr(unix, ignore)]",
        "#[cfg(all(unix, all(test, windows)))]",
        "#[cfg(all(unix, windows))]",
        "#[cfg(target_os = \"nonexistent\")]",
        "#[cfg(target_family = \"beos\")]",
        "#[cfg(not(any(unix, not(unix))))]",
        "#[cfg(not(target_os = \"linux\", unix))]",
    ] {
        let kinds = weakenings(attribute).unwrap_or_else(|| panic!("{attribute} does not parse"));
        assert!(!kinds.contains(&WeakeningKind::PlatformOnly), "{attribute}");
    }
}

#[test]
fn a_predicate_nested_deeper_than_the_stack_is_not_platform_only() {
    let attribute = format!("#[cfg({}unix{})]", "not(".repeat(5000), ")".repeat(5000));
    let kinds = weakenings(&attribute).expect("the file parses");
    assert!(!kinds.contains(&WeakeningKind::PlatformOnly));
}

/// The messages of the weakenings lang_rust reads in `attributes`, the outer attributes of an inline module of a file
/// of tests.
fn module_skips(attributes: &str) -> Vec<String> {
    let text = format!("{attributes}\nmod tests {{\n    #[test]\n    fn t() {{}}\n}}\n");
    let source = lang_rust::read(text.as_bytes(), true).unwrap_or_else(|e| panic!("{text}: {e:?}"));
    source.weakenings.into_iter().map(|w| w.message).collect()
}

#[test]
fn brackets_in_a_string_literal_do_not_hide_a_skip_after_it_on_the_line() {
    for line in [
        "#[doc = \"[\"] #[cfg(miri)]",
        "#[cfg_attr(any(), foo = '\"')] #[cfg(miri)]",
        "#[doc = r\"\\\"] #[cfg(miri)]",
        "#[cfg_attr(any(), foo = '[')] #[cfg(miri)]",
        "#[doc = r#\"a \"[\"#] #[cfg(miri)]",
    ] {
        assert_eq!(module_skips(line), vec!["`#[cfg(miri)]` added: it skips a test or turns it off."], "{line}");
    }
    assert_eq!(module_skips("#[doc = \"\\\"][\"] #[ignore]"), vec!["`#[ignore]` added: it skips a test or turns it off."]);
}

#[test]
fn an_attribute_inside_a_string_is_none() {
    assert_eq!(module_skips("#[doc = \"#[ignore]\"]"), Vec::<String>::new());
}

#[test]
fn a_module_s_and_a_statement_s_attributes_are_found_wherever_they_are_written_on_their_lines() {
    let found = |after: &str| wheres(&check(&[edited(LIB_PATH, LIB, after)]));
    // On the line of the `mod` it turns off, before the `mod`.
    assert_eq!(found(&edit(LIB, "mod tests {", "#[cfg(miri)] mod tests {")), vec![at(LIB_PATH, Some(6), "skip")]);
    // After code on a test's line, and wrapped with its `ignore` on a later line.
    let large = "        assert_eq!(add(10, 20), 30);\n";
    let after = edit(LIB, large, "        let x = 30; #[cfg(miri)] assert_eq!(add(10, 20), x);\n");
    assert_eq!(found(&after), vec![at(LIB_PATH, Some(17), "skip")]);
    let after = edit(LIB, large, &format!("        #[cfg_attr(\n            miri,\n            ignore\n        )]\n{large}"));
    assert_eq!(found(&after), vec![at(LIB_PATH, Some(17), "skip")]);
}

#[test]
fn an_attribute_in_a_string_literal_skips_nothing() {
    let large = "        assert_eq!(add(10, 20), 30);\n";
    // A line of a multi-line string in a test's body that starts as an attribute.
    let after = edit(LIB, large, &format!("        let fixture = \"\n#[ignore]\n#[cfg(miri)]\nfn f() {{}}\n\";\n{large}"));
    assert_eq!(check(&[edited(LIB_PATH, LIB, &after)]), vec![]);
    // A raw string ending in `\` before a test's existing skip whose body holds a `]` in a string.
    let base = edit(LIB, "    #[test]\n    fn adds_large", "    #[test]\n    #[ignore]\n    fn adds_large");
    let base = edit(&base, large, &format!("        let s = \"a]b\";\n{large}"));
    let after = edit(&base, "#[cfg(test)]\n", "#[doc = r\"C:\\\"]\n#[cfg(test)]\n");
    assert_eq!(check(&[edited(LIB_PATH, &base, &after)]), vec![]);
}
