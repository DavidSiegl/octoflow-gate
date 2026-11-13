//! The duplicated-blocks check on Rust, its tokens read with lang_rust::code_tokens.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use checks::duplicates::copied_blocks;

/// (path, line, other place) of each finding on `files`, each (path, source), the lines `added` gives added to each,
/// blocks of at least 20 tokens.
fn found(files: &[(&str, &str)], added: &[(&str, std::ops::RangeInclusive<u32>)]) -> Vec<(String, u32, String)> {
    let files: Vec<(OsString, Vec<model::CodeToken>)> =
        files.iter().map(|&(path, source)| (path.into(), lang_rust::code_tokens(source.as_bytes()))).collect();
    let added: BTreeMap<OsString, BTreeSet<u32>> =
        added.iter().map(|(path, lines)| (path.into(), lines.clone().collect())).collect();
    let other = |message: &str| message.split('`').nth(1).unwrap().to_string();
    let at = |f: checks::Finding| (f.path.unwrap().to_string_lossy().into_owned(), f.line.unwrap(), other(&f.message));
    copied_blocks(&files, &added, 20).into_iter().map(at).collect()
}

const TOTAL: &str = "\
use std::collections::HashMap;

pub fn total(prices: &HashMap<String, u32>, wanted: &[&str]) -> u32 {
    let mut sum = 0;
    for name in wanted {
        if let Some(price) = prices.get(*name) {
            sum += price * 2;
        }
    }
    sum
}
";

/// TOTAL with other names, other literals, other comments and another import.
const RENAMED: &str = "\
use std::collections::BTreeMap as HashMap;

// Counts the weight of the items asked for.
pub fn weight(masses: &HashMap<String, u32>, asked: &[&str]) -> u32 {
    let mut kg = 0; // running total
    for item in asked {
        if let Some(mass) = masses.get(*item) {
            kg += mass * 7;
        }
    }
    kg
}
";

#[test]
fn a_copy_with_its_names_literals_comments_and_imports_changed_is_found() {
    let found = found(&[("src/a.rs", TOTAL), ("src/b.rs", RENAMED)], &[("src/b.rs", 1..=12)]);
    assert_eq!(found, [("src/b.rs".into(), 4, "src/a.rs:3".into())]);
}

#[test]
fn a_copy_in_test_code_is_not_read() {
    let tested = format!("#[cfg(test)]\nmod tests {{\n{TOTAL}}}\n");
    let marked_fn = TOTAL.replace("pub fn total", "#[cfg(all(test, unix))]\npub fn total");
    let whole_file = format!("#![cfg(test)]\n{TOTAL}");
    for copy in [tested, marked_fn, whole_file] {
        assert_eq!(found(&[("src/a.rs", TOTAL), ("src/b.rs", &copy)], &[("src/b.rs", 1..=20)]), [], "{copy}");
    }
}

#[test]
fn a_file_that_does_not_parse_has_no_tokens() {
    assert_eq!(lang_rust::code_tokens(b"fn f( {"), []);
    assert_eq!(lang_rust::code_tokens(b"fn f() {}\xff"), []);
}
