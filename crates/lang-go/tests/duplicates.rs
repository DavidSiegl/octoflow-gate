//! The duplicated-blocks check on Go, its tokens read with lang_go::code_tokens.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;

use checks::duplicates::copied_blocks;

/// (path, line, other place) of each finding on `files`, each (path, source), the lines `added` gives added to each,
/// blocks of at least 20 tokens.
fn found(files: &[(&str, &str)], added: &[(&str, std::ops::RangeInclusive<u32>)]) -> Vec<(String, u32, String)> {
    let files: Vec<(OsString, Vec<model::CodeToken>)> =
        files.iter().map(|&(path, source)| (path.into(), lang_go::code_tokens(source.as_bytes()))).collect();
    let added: BTreeMap<OsString, BTreeSet<u32>> =
        added.iter().map(|(path, lines)| (path.into(), lines.clone().collect())).collect();
    let other = |message: &str| message.split('`').nth(1).unwrap().to_string();
    let at = |f: checks::Finding| (f.path.unwrap().to_string_lossy().into_owned(), f.line.unwrap(), other(&f.message));
    copied_blocks(&files, &added, 20).into_iter().map(at).collect()
}

const TOTAL: &str = "\
package shop

import \"strings\"

func total(prices map[string]int, wanted []string) int {
\tsum := 0
\tfor _, name := range wanted {
\t\tif price, ok := prices[strings.ToLower(name)]; ok {
\t\t\tsum += price * 2
\t\t}
\t}
\treturn sum
}
";

/// TOTAL with other names, other literals, other comments and other imports.
const RENAMED: &str = "\
package scale

import (
\t\"fmt\"
\t\"strings\"
)

// weight counts the weight of the items asked for.
func weight(masses map[string]int, asked []string) int {
\tkg := 7 // running total
\tfor _, item := range asked {
\t\tif mass, ok := masses[strings.ToUpper(item)]; ok {
\t\t\tkg += mass * 0x10
\t\t}
\t}
\treturn kg
}
";

#[test]
fn a_copy_with_its_names_literals_comments_and_imports_changed_is_found() {
    let found = found(&[("shop/a.go", TOTAL), ("scale/b.go", RENAMED)], &[("scale/b.go", 1..=17)]);
    assert_eq!(found, [("scale/b.go".into(), 9, "shop/a.go:5".into())]);
}

#[test]
fn a_composite_or_function_literal_is_read_as_code() {
    let table = "package p\n\nvar t = []int{1, 2, 3}\nvar f = func(a, b int) int { return a + b }\n";
    let tokens = lang_go::code_tokens(table.as_bytes());
    assert!(tokens.len() > 20, "{tokens:?}");
}

#[test]
fn a_file_that_does_not_parse_has_no_tokens() {
    assert_eq!(lang_go::code_tokens(b"package p\nfunc f( {\n"), []);
}
