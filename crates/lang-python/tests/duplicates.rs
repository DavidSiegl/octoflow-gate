//! The duplicated-blocks check on Python, its tokens read with lang_python::code_tokens.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use checks::duplicates::copied_blocks;

/// (path, line, other place) of each finding on `files`, each (path, source), the lines `added` gives added to each,
/// blocks of at least 20 tokens.
fn found(files: &[(&str, &str)], added: &[(&str, std::ops::RangeInclusive<u32>)]) -> Vec<(String, u32, String)> {
    let files: Vec<(OsString, Vec<model::CodeToken>)> =
        files.iter().map(|&(path, source)| (path.into(), lang_python::code_tokens(source.as_bytes()))).collect();
    let added: BTreeMap<OsString, BTreeSet<u32>> =
        added.iter().map(|(path, lines)| (path.into(), lines.clone().collect())).collect();
    let other = |message: &str| message.split('`').nth(1).unwrap().to_string();
    let at = |f: checks::Finding| (f.path.unwrap().to_string_lossy().into_owned(), f.line.unwrap(), other(&f.message));
    copied_blocks(&files, &added, 20).into_iter().map(at).collect()
}

const TOTAL: &str = "\
import collections

def total(prices, wanted):
    sum_ = 0
    for name in wanted:
        if name in prices:
            sum_ += prices[name] * 2
    print(\"total\", sum_)
    return sum_
";

/// TOTAL with other names, other literals of each kind (an f-string for a string), other comments and other imports.
const RENAMED: &str = "\
from os import path
import sys

def weight(masses, asked):
    # Counts the weight of the items asked for.
    kg = 3
    for item in asked:
        if item in masses:
            kg += masses[item] * 7
    print(f\"weight of {asked}\", kg)
    return kg
";

#[test]
fn a_copy_with_its_names_literals_comments_and_imports_changed_is_found() {
    let found = found(&[("a.py", TOTAL), ("b.py", RENAMED)], &[("b.py", 1..=11)]);
    assert_eq!(found, [("b.py".into(), 4, "a.py:3".into())]);
}

#[test]
fn a_file_that_does_not_parse_has_no_tokens() {
    assert_eq!(lang_python::code_tokens(b"def f(:\n"), []);
    assert_eq!(lang_python::code_tokens(b"# -*- coding: klingon -*-\nx = 1\n"), []);
}
