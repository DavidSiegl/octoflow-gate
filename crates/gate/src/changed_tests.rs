//! The Rust and Go tests a change adds or edits, as the checks that run them pick them out: passes-at-base's Rust and
//! Go halves, and flaky-tests' Go half. Python's are pytest::changed_tests, by pytest's own idea of a test.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};

use checks::ChangedFile;
use model::{Lines, Test, Unreadable};

use crate::diff::removal_points;
use crate::change::Change;

/// A test the change adds or edits, as [`changed_tests`] finds one, in the working tree.
pub struct ChangedTest {
    /// Its file's path in the working tree, from the tree's top.
    pub path: OsString,
    /// Its name, as model::Test's: the path from its file to it in Rust, as `tests::adds`; its function's name in Go.
    pub name: String,
    /// Its `fn` or `func` line, where a finding on it goes.
    pub line: u32,
}

/// The tests of `files`, files of `change`, that `runs` accepts and the change touches: adds lines to, or removes
/// lines at one of the diff's removal points (diff::removal_points) from just above its first line, as an `#[ignore]`
/// taken off its top, to just above its last; a removal after its last line, the `}`, is below the test, as a deleted
/// neighbour's is, and does not count. Only the files the change leaves in the working tree are read, in the change's
/// order, each file's tests by where they start.
///
/// Also gives back each file the change edits whose working tree side could not be read, with why, when it holds
/// tests by its place or its base held test code: the tests in it, if any, cannot be told.
pub fn changed_tests<'a>(
    change: &Change,
    files: &'a [ChangedFile],
    runs: impl Fn(&Test) -> bool,
) -> (Vec<ChangedTest>, Vec<(&'a OsStr, &'a Unreadable)>) {
    let removals = removal_points(&change.diff);
    let (mut tests, mut unread) = (vec![], vec![]);
    for file in files {
        let Some(after) = &file.after else { continue };
        let removed = removals.get(&after.path).cloned().unwrap_or_default();
        if file.added.is_empty() && removed.is_empty() {
            continue;
        }
        let source = match &after.source {
            Ok(source) => source,
            Err(unreadable) => {
                let base_source = file.before.as_ref().and_then(|b| b.source.as_ref().ok());
                if after.among_tests || base_source.is_some_and(|s| !s.test_lines.is_empty()) {
                    unread.push((after.path.as_os_str(), unreadable));
                }
                continue;
            }
        };
        let touched = source.tests.iter().filter(|t| runs(t) && touches(t.lines, &file.added, &removed));
        tests.extend(touched.map(|t| ChangedTest { path: after.path.clone(), name: t.name.clone(), line: t.line }));
    }
    (tests, unread)
}

/// Whether the change touches a test that spans `lines` in the working tree, as [`changed_tests`] says, by adding one
/// of the lines `added` or removing lines at one of `removal_points`.
fn touches(lines: Lines, added: &BTreeSet<u32>, removal_points: &BTreeSet<u32>) -> bool {
    added.iter().any(|&n| lines.holds(n))
        || removal_points.iter().any(|&n| lines.first.saturating_sub(1) <= n && n < lines.last)
}
