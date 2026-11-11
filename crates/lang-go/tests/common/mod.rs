//! What the tests of the checks on Go share: a change to Go files, made in memory and read with lang_go as the gate
//! reads a git change, so that a test of a check says only what the files were and became.

#![allow(dead_code, reason = "each test crate that includes this module uses only part of it")]

use std::collections::BTreeSet;

use checks::{ChangedFile, Side};

/// A file of a change: `before`, its path and text at the base, None where the change adds it; `after`, its path and
/// text in the working tree, None where the change deletes it. Each side is read as the gate reads it: among the
/// tests whole when it is a `_test.go` file. Its added and removed lines are those of a shortest line
/// diff between the two texts, as git's would be wherever the shortest is one.
pub fn changed(before: Option<(&str, &str)>, after: Option<(&str, &str)>) -> ChangedFile {
    let side = |(path, text): (&str, &str)| Side {
        path: path.into(),
        source: lang_go::read(text.as_bytes(), lang_go::among_tests(path.as_bytes())),
        among_tests: lang_go::among_tests(path.as_bytes()),
    };
    let (removed, added) = diff(&lines(before), &lines(after));
    ChangedFile { before: before.map(side), after: after.map(side), added, removed }
}

/// A file the change edits in place, from `before` to `after`.
pub fn edited(path: &str, before: &str, after: &str) -> ChangedFile {
    changed(Some((path, before)), Some((path, after)))
}

/// A file the change adds.
pub fn added(path: &str, text: &str) -> ChangedFile {
    changed(None, Some((path, text)))
}

/// A file the change deletes.
pub fn deleted(path: &str, text: &str) -> ChangedFile {
    changed(Some((path, text)), None)
}

/// The lines of a side's text, none for no side.
fn lines<'a>(side: Option<(&str, &'a str)>) -> Vec<&'a str> {
    side.map(|(_, text)| text.lines().collect()).unwrap_or_default()
}

/// The lines, from 1, that a shortest diff from `a` to `b` removes from `a` and adds to `b`: those outside a longest
/// common subsequence of the two.
fn diff(a: &[&str], b: &[&str]) -> (BTreeSet<u32>, BTreeSet<u32>) {
    // common[i][j]: the length of a longest common subsequence of a[i..] and b[j..].
    let mut common = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            common[i][j] = if a[i] == b[j] { common[i + 1][j + 1] + 1 } else { common[i + 1][j].max(common[i][j + 1]) };
        }
    }
    let number = |k: usize| u32::try_from(k + 1).expect("a test's file has fewer than 2^32 lines");
    let (mut removed, mut added) = (BTreeSet::new(), BTreeSet::new());
    let (mut i, mut j) = (0, 0);
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            (i, j) = (i + 1, j + 1);
        } else if j == b.len() || (i < a.len() && common[i + 1][j] >= common[i][j + 1]) {
            removed.insert(number(i));
            i += 1;
        } else {
            added.insert(number(j));
            j += 1;
        }
    }
    (removed, added)
}
