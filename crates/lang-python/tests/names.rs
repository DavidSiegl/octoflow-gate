//! The vague-names check on Python, read with lang_python: tests/test_gate.py's TestVagueNames.

mod common;

use checks::names::vague_names;
use checks::{ChangedFile, Finding};
use common::{added, edited};

const BASE: &str = "data = 1\n\n\ndef old(value):\n    return value\n";

/// The findings of the check on `files`.
fn found(files: &[ChangedFile]) -> Vec<Finding> {
    vague_names(files, false)
}

/// The name each finding on calc.py, edited from [`BASE`] to BASE and then `added`, reports, with its line counted
/// from the end of BASE; and that each is a vague-name warning on calc.py.
fn names(added: &str) -> Vec<(u32, String)> {
    let found = found(&[edited("calc.py", BASE, &format!("{BASE}{added}"))]);
    let base = u32::try_from(BASE.lines().count()).unwrap();
    for f in &found {
        assert_eq!(
            (f.path.as_deref(), f.rule.as_str(), f.severity.as_str()),
            (Some("calc.py".as_ref()), "vague-names vague-name", "warning"),
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
fn each_vague_name_an_added_line_binds_is_found_once_at_its_first_binding() {
    let added = "

class InfoManager:
    def __init__(self, raw_info, *, metadata=None, **user_data):
        self.handle = raw_info
        self.result = metadata


def load(path):
    result = []
    for line_value in open(path):
        with line_value as status:
            data = status
            data = data
            result.append(data)
    return result
";
    assert_eq!(
        names(added),
        pairs(&[(4, "raw_info"), (4, "user_data"), (5, "handle"), (11, "line_value"), (12, "status"), (13, "data")])
    );
    let found = found(&[edited("calc.py", BASE, &format!("{BASE}{added}"))]);
    assert_eq!(found[0].message, "`raw_info` could name anything; name it for what it holds.");
}

#[test]
fn an_attribute_of_cls_binds_a_name_and_one_of_anything_else_does_not() {
    let added = "\n\nclass C:\n    @classmethod\n    def make(cls, other):\n        cls.manager = 1\n        other.info = 2\n        other.data[0] = 3\n";
    assert_eq!(names(added), pairs(&[(6, "manager")]));
}

#[test]
fn imports_excepts_matches_and_type_parameters_bind_names() {
    let added = "

import json as raw_data
from os import path as status


def parse[T_value](text: str) -> None:
    try:
        match json.loads(text):
            case {\"k\": handle, **rest_info}:
                pass
            case [*all_status]:
                pass
            case Point(x=x_value) | [x_value] as whole_data:
                pass
    except ValueError as info:
        raise
";
    let found: Vec<String> = names(added).into_iter().map(|(_, name)| name).collect();
    // A mapping pattern's **rest is placed at the pattern's start, before the captures in it, and an `as` at its
    // pattern's.
    assert_eq!(
        found,
        ["raw_data", "status", "T_value", "rest_info", "handle", "all_status", "whole_data", "x_value", "info"]
    );
}

#[test]
fn walruses_loops_comprehensions_and_annotated_names_bind_and_what_is_only_read_does_not() {
    let added = "

def f(xs, data_manager):
    if (first_info := xs[0]):
        pass
    total_value: int
    ys = [raw_data for raw_data in xs if status(raw_data)]
    for (a, [b_handle, *rest_status]) in xs:
        del data_manager
    global info
    key = lambda item_value=value: handle(item_value)
    return xs.data
";
    let found: Vec<String> = names(added).into_iter().map(|(_, name)| name).collect();
    assert_eq!(
        found,
        ["data_manager", "first_info", "total_value", "raw_data", "b_handle", "rest_status", "item_value"]
    );
}

#[test]
fn an_override_s_name_and_parameters_are_its_base_s() {
    let added = "

from typing import override
import typing


class Handler(Base):
    @override
    def handle(self, raw_data):
        status = raw_data
        return status

    @typing.override
    async def manager(self, *raw_info, **more_info):
        pass

    @overrides(1)
    def info(self):
        pass
";
    assert_eq!(names(added), pairs(&[(10, "status"), (18, "info")]));
}

#[test]
fn a_name_on_a_line_the_change_left_alone_is_not_found() {
    assert_eq!(found(&[edited("calc.py", BASE, &BASE.replace("return value", "return value + 1"))]), []);
}

#[test]
fn a_class_or_function_named_by_a_vague_word_is_found_at_its_keyword() {
    let found = found(&[added("tests/test_calc.py", "class Manager:\n    pass\n\n\n@decorated\nasync def handle():\n    pass\n")]);
    assert_eq!(found.iter().map(|f| f.line).collect::<Vec<_>>(), [Some(1), Some(6)]);
}

#[test]
fn every_file_s_findings_are_listed_by_path_then_line_an_unparsable_one_as_not_read() {
    let files = [added("c.py", "status = 1\n"), added("b_broken.py", "def broken(:\n"), added("a.py", "\n\nraw_data = 1\n")];
    let found = found(&files);
    let place: Vec<_> = found.iter().map(|f| (f.path.clone().unwrap(), f.line, f.rule.as_str())).collect();
    assert_eq!(
        place,
        [
            ("a.py".into(), Some(3), "vague-names vague-name"),
            ("b_broken.py".into(), Some(1), "vague-names not-read"),
            ("c.py".into(), Some(1), "vague-names vague-name")
        ]
    );
    assert_eq!(found[1].message, "does not parse as Python, so its names were not read.");
}
