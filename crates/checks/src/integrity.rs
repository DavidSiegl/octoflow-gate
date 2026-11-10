//! The test-integrity check: tests a change removed, skipped, stripped of assertions or wrapped in a swallowed error,
//! and the expected values it changed together with the code under test. gate.py's weakened_tests, for the languages
//! whose front ends give the model; docs/quality-gate.md, "Test integrity", says what each finding means.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;

use model::{first_line, Assertion, SourceFile, Test, WeakeningKind};

use crate::{ChangedFile, Finding};

const CHECK: &str = "test-integrity";

/// The check's findings on `files`, one language's files of a change, by path and then line:
///
/// - removed-test: each test of the base gone from the working tree, matched by name within its file, two of one name
///   in turn, else by body anywhere, as a test renamed or moved; an inline module or test class whose tests are all
///   gone is one finding, and a file deleted, or left with no source of the language, one;
/// - fewer-assertions: a test kept with fewer assertions than at the base;
/// - expected-value-changed, when `code_changed`: an assertion of a kept test whose literals changed while the rest of
///   it stayed, the assertions of the two sides aligned as Python's difflib aligns sequences; and a constant of the
///   file's base ([`model::Constant`]) that a kept test's assertions name on both sides and that the change edited or
///   made no constant, at the first assertion now naming it;
/// - skip, platform-only and swallowed-error: each [`model::Weakening`] on a line the change added;
/// - unreadable: a test file, one the settings place among the tests or with test code on a side, that its front end
///   could not read as a whole on a side ([`model::Unreadable`] with no line, as for one that is not UTF-8), which is
///   all the check says of it: which tests the change removed or weakened cannot be told;
/// - unparsable: a file its front end could not parse in the working tree whose base held tests, which is all the
///   check says of it; a base that does not parse has no tests to lose.
///
/// `code_changed` says whether the change also edits the code under test, which [`edits_code`] tells per file and the
/// caller decides over the whole change. Only the test files take part: those with test code on a side, and those an
/// unreadable finding is about. A removed test, fewer assertions, a changed expected value and a platform-only cfg are
/// what a spec can ask for, so the gate weighs them; the rest fail it.
pub fn weakened_tests(files: &[ChangedFile], code_changed: bool) -> Vec<Finding> {
    let mut out = vec![];
    // The tests of each file the check reads, at the base and in the working tree, by its index in files.
    let mut before: Vec<Keyed> = vec![];
    let mut after: Vec<Keyed> = vec![];
    for (k, file) in files.iter().enumerate() {
        let (base, now) = (file.base(), file.now());
        let held = |source: Option<&SourceFile>| source.is_some_and(|s| !s.test_lines.is_empty());
        if let Some(unreadable) = unreadable_test_file(file, held(base) || held(now)) {
            out.push(unreadable);
            continue;
        }
        if !held(base) && !held(now) {
            continue;
        }
        if let Some(Err(unreadable)) = file.after.as_ref().map(|a| &a.source) {
            if base.is_some_and(|b| !b.tests.is_empty()) {
                let path = file.after.as_ref().map(|a| &a.path);
                let message = format!("{}, so its tests were not checked.", unreadable.why);
                out.push(Finding::of(CHECK, "unparsable", true, path, unreadable.line, message));
            }
            continue;
        }
        before.extend(keyed(k, base));
        after.extend(keyed(k, now));
        out.extend(added_weakenings(file));
    }
    out.extend(removed_tests(files, &before, &after));
    out.extend(kept_tests(files, &before, &after, code_changed));
    out.sort_by(|a, b| (&a.path, a.line.unwrap_or(0)).cmp(&(&b.path, b.line.unwrap_or(0))));
    out
}

/// The findings on the tests the change kept, those of `after`, the tests of `files` in the working tree, with a test
/// of their key in `before`, the tests at the base: what [`kept_test`] finds of each and, when `code_changed`, what
/// [`changed_constants`] does.
fn kept_tests(files: &[ChangedFile], before: &[Keyed], after: &[Keyed], code_changed: bool) -> Vec<Finding> {
    let at_base: HashMap<Key, &Keyed> = before.iter().map(|t| (t.key(), t)).collect();
    let mut out = vec![];
    for t in after {
        let Some(was) = at_base.get(&t.key()) else { continue };
        let file = &files[t.file];
        let path = file.after.as_ref().map(|a| &a.path);
        out.extend(kept_test(path, was.test, t.test, code_changed));
        if let (true, Some(base), Some(now)) = (code_changed, file.base(), file.now()) {
            out.extend(changed_constants(path, (was.test, base), (t.test, now)));
        }
    }
    out
}

/// The unreadable finding on `file` when a side of it is one its front end could not read as a whole and it is a test
/// file: among the tests by its path on a side, or, as `test_code` says, holding test code on a side it could read.
/// On the working tree's path where it has one, else the base's. None for any other file.
fn unreadable_test_file(file: &ChangedFile, test_code: bool) -> Option<Finding> {
    let sides = [&file.after, &file.before];
    let placed = sides.iter().filter_map(|s| s.as_ref()).any(|s| s.among_tests);
    if !(placed || test_code) {
        return None;
    }
    let why = sides.iter().filter_map(|s| s.as_ref()?.source.as_ref().err()).find(|e| e.line.is_none())?;
    let path = sides.iter().find_map(|s| s.as_ref().map(|s| &s.path));
    let message = format!("{}, so its tests were not checked.", why.why);
    Some(Finding::of(CHECK, "unreadable", true, path, None, message))
}

/// Whether `file` edits the code under test rather than its tests: the change adds, deletes or renames it, or adds a
/// line outside the working tree's [`SourceFile::test_lines`], or removes one outside the base's. Where its front end
/// could not read a side, every line of that side counts as code.
///
/// What else makes a file code, such as Rust's lying in a Cargo package, and which files the project places among its
/// tests whole, is the caller's to tell.
pub fn edits_code(file: &ChangedFile) -> bool {
    let (Some(before), Some(after)) = (&file.before, &file.after) else { return true };
    let outside = |source: Option<&SourceFile>, line: u32| {
        source.is_none_or(|s| !s.test_lines.iter().any(|l| l.holds(line)))
    };
    before.path != after.path
        || file.added.iter().any(|&n| outside(file.now(), n))
        || file.removed.iter().any(|&n| outside(file.base(), n))
}

/// A test on one side of a change, with what tells it from the others there.
struct Keyed<'a> {
    /// The index in the change's files of the file it is in.
    file: usize,
    test: &'a Test,
    /// How many tests of its file and side come before it under the same name, as two fns of one name under cfgs
    /// that never hold together can be, or a macro's: so each of them is a test of its own, the first matched with the
    /// first.
    namesake: usize,
}

/// What matches a test with its old self: its file, its name and its [`Keyed::namesake`].
type Key<'a> = (usize, &'a str, usize);

impl Keyed<'_> {
    fn key(&self) -> Key<'_> {
        (self.file, self.test.name.as_str(), self.namesake)
    }
}

/// The tests of `source`, the file at index `file` on one side, keyed; none for no source.
fn keyed(file: usize, source: Option<&SourceFile>) -> Vec<Keyed<'_>> {
    let mut seen: HashMap<&str, usize> = HashMap::new();
    let tests = source.map(|s| s.tests.as_slice()).unwrap_or_default();
    tests
        .iter()
        .map(|test| {
            let n = seen.entry(test.name.as_str()).or_default();
            *n += 1;
            Keyed { file, test, namesake: *n - 1 }
        })
        .collect()
}

/// The removed-test findings: `before` and `after` are the tests of `files` at the base and in the working tree, as
/// [`weakened_tests`] reads them. A test whose body reappears in a test of a new key, in any file, is not removed:
/// each such new test accounts for one.
fn removed_tests(files: &[ChangedFile], before: &[Keyed], after: &[Keyed]) -> Vec<Finding> {
    let now: Vec<Key> = after.iter().map(Keyed::key).collect();
    let was: Vec<Key> = before.iter().map(Keyed::key).collect();
    let mut bodies: HashMap<&str, usize> = HashMap::new();
    for t in after.iter().filter(|t| !was.contains(&t.key())) {
        *bodies.entry(t.test.body.as_str()).or_default() += 1;
    }
    let mut removed: BTreeMap<usize, Vec<&Keyed>> = BTreeMap::new();
    for t in before.iter().filter(|t| !now.contains(&t.key())) {
        match bodies.get_mut(t.test.body.as_str()) {
            Some(n) if *n > 0 => *n -= 1,
            _ => removed.entry(t.file).or_default().push(t),
        }
    }
    let mut out = vec![];
    for (k, gone) in removed {
        let file = &files[k];
        let Some(base) = file.base() else { continue };
        let path = file.before.as_ref().map(|b| &b.path);
        if file.now().is_none() {
            let message = format!("deleted, with its {} tests: {}.", gone.len(), names(&gone));
            out.push(Finding::of(CHECK, "removed-test", false, path, None, message));
        } else {
            out.extend(removed_from(path, base, &gone));
        }
    }
    out
}

/// The removed-test findings on `removed`, tests of the file at `path` that the change keeps, whose base is `base`:
/// one for each test group whose tests are all removed, the outermost such group of each test, and one for each test
/// outside such a group.
fn removed_from(path: Option<&OsString>, base: &SourceFile, removed: &[&Keyed]) -> Vec<Finding> {
    let gone = |t: &Test| removed.iter().any(|r| std::ptr::eq(r.test, t));
    // The groups that hold a removed test and only removed ones, outermost first.
    let mut emptied: Vec<_> = base
        .test_groups
        .iter()
        .filter(|g| {
            removed.iter().any(|r| g.lines.holds(r.test.line))
                && base.tests.iter().filter(|t| g.lines.holds(t.line)).all(gone)
        })
        .collect();
    emptied.sort_by_key(|g| g.lines.first);
    let mut out = vec![];
    let mut reported = vec![];
    for t in removed {
        match emptied.iter().find(|g| g.lines.holds(t.test.line)) {
            None => {
                let message = format!("`{}` is gone; this was line {} at the base.", t.test.name, t.test.line);
                out.push(Finding::of(CHECK, "removed-test", false, path, Some(t.test.line), message));
            }
            Some(whole) if !reported.contains(&whole.line) => {
                reported.push(whole.line);
                let n = removed.iter().filter(|r| whole.lines.holds(r.test.line)).count();
                let message =
                    format!("`{}` and its {n} tests are gone; this was line {} at the base.", whole.name, whole.line);
                out.push(Finding::of(CHECK, "removed-test", false, path, Some(whole.line), message));
            }
            Some(_) => {}
        }
    }
    out
}

/// The first five of `tests`' names, quoted, and how many more there are.
fn names(tests: &[&Keyed]) -> String {
    const SHOWN: usize = 5;
    let shown: Vec<String> = tests.iter().take(SHOWN).map(|t| format!("`{}`", t.test.name)).collect();
    let more = tests.len().saturating_sub(SHOWN);
    shown.join(", ") + &if more > 0 { format!(" and {more} more") } else { String::new() }
}

/// The findings on a test the change kept, `was` at the base and `now` in the working tree, in the file at `path`:
/// fewer assertions than at the base, and, when `code_changed`, each assertion whose literals changed while its shape
/// stayed.
fn kept_test(path: Option<&OsString>, was: &Test, now: &Test, code_changed: bool) -> Vec<Finding> {
    let mut out = vec![];
    let (n, m) = (now.assertions.len(), was.assertions.len());
    if n < m {
        let message = format!("`{}` has {n} assertions, {m} at the base.", now.name);
        out.push(Finding::of(CHECK, "fewer-assertions", false, path, Some(now.line), message));
    }
    if code_changed {
        for (a, b) in changed_literals(&was.assertions, &now.assertions) {
            let message = format!(
                "`{}`: `{}` became `{}`, in a change that also edits the code under test.",
                now.name,
                first_line(&a.text),
                first_line(&b.text)
            );
            out.push(Finding::of(CHECK, "expected-value-changed", false, path, Some(b.line), message));
        }
    }
    out
}

/// The expected-value-changed findings on a test the change kept in the file at `path`, for the constants its
/// assertions name: `was` is the test and its file at the base, `now` those in the working tree. One for each constant
/// of the base that the test's assertions read on both sides ([`Assertion::reads`]) and that is now no constant of the
/// file, or one whose expression differs; at the first assertion now reading it.
fn changed_constants(path: Option<&OsString>, was: (&Test, &SourceFile), now: (&Test, &SourceFile)) -> Vec<Finding> {
    let constant = |file: &'_ SourceFile, name: &str| file.constants.iter().find(|c| c.name == name).cloned();
    let read_at_base = |name: &String| was.0.assertions.iter().any(|a| a.reads.contains(name));
    let mut out = vec![];
    let mut seen: Vec<&String> = vec![];
    for (assertion, name) in now.0.assertions.iter().flat_map(|a| a.reads.iter().map(move |name| (a, name))) {
        if seen.contains(&name) {
            continue;
        }
        seen.push(name);
        let Some(base) = constant(was.1, name).filter(|_| read_at_base(name)) else { continue };
        let (place, what) = match constant(now.1, name) {
            None => {
                let what = "is no longer a constant of the file that this check can read";
                (format!("line {} at the base", base.line), what)
            }
            Some(defined) if defined.expression != base.expression => (format!("line {}", defined.line), "changed"),
            Some(_) => continue,
        };
        let message = format!(
            "`{name}` ({place}), which `{}` names in its assertions, {what}, in a change that also edits the code \
             under test.",
            now.0.name
        );
        out.push(Finding::of(CHECK, "expected-value-changed", false, path, Some(assertion.line), message));
    }
    out
}

/// The findings on the [`model::Weakening`]s of `file` in the working tree that lie on lines the change added.
fn added_weakenings(file: &ChangedFile) -> Vec<Finding> {
    let (Some(now), Some(side)) = (file.now(), &file.after) else { return vec![] };
    let added = now.weakenings.iter().filter(|w| file.added.contains(&w.line));
    added
        .map(|w| {
            let fails = w.kind != WeakeningKind::PlatformOnly;
            Finding::of(CHECK, w.kind.rule(), fails, Some(&side.path), Some(w.line), w.message.clone())
        })
        .collect()
}

/// The (before, after) pairs of a test's assertions `was` and `now` that kept their shape but not their literals, the
/// two aligned on their shapes by [`matching_pairs`].
fn changed_literals<'a>(was: &'a [Assertion], now: &'a [Assertion]) -> Vec<(&'a Assertion, &'a Assertion)> {
    let a: Vec<&str> = was.iter().map(|x| x.shape.as_str()).collect();
    let b: Vec<&str> = now.iter().map(|x| x.shape.as_str()).collect();
    matching_pairs(&a, &b).into_iter().map(|(i, j)| (&was[i], &now[j])).filter(|(x, y)| x.literals != y.literals).collect()
}

/// The pairs (i, j), ascending, of the elements `a[i] == b[j]` that Python's
/// `difflib.SequenceMatcher(None, a, b, autojunk=False).get_matching_blocks()` matches: the longest common run, the
/// earliest in `a` of those, then the earliest in `b`, and the same again on each side of it.
fn matching_pairs<T: PartialEq>(a: &[T], b: &[T]) -> Vec<(usize, usize)> {
    let mut out = vec![];
    let mut todo = vec![(0, a.len(), 0, b.len())];
    while let Some((alo, ahi, blo, bhi)) = todo.pop() {
        let (i, j, k) = longest_match(&a[alo..ahi], &b[blo..bhi]);
        let (i, j) = (alo + i, blo + j);
        if k == 0 {
            continue;
        }
        out.extend((0..k).map(|d| (i + d, j + d)));
        todo.push((alo, i, blo, j));
        todo.push((i + k, ahi, j + k, bhi));
    }
    out.sort_unstable();
    out
}

/// The longest run of elements that `a` and `b` share, as (start in a, start in b, length), the earliest in `a` of
/// the longest, then the earliest in `b`, as `difflib.SequenceMatcher.find_longest_match` breaks ties without junk;
/// length 0 when they share none.
fn longest_match<T: PartialEq>(a: &[T], b: &[T]) -> (usize, usize, usize) {
    let (mut i, mut j, mut k) = (0, 0, 0);
    // The length of the shared run ending at each element of b, after the previous element of a; shifted by one.
    let mut ending = vec![0; b.len() + 1];
    for (x, ax) in a.iter().enumerate() {
        let mut next = vec![0; b.len() + 1];
        for (y, _) in b.iter().enumerate().filter(|(_, by)| ax == *by) {
            let n = ending[y] + 1;
            next[y + 1] = n;
            if n > k {
                (i, j, k) = (x + 1 - n, y + 1 - n, n);
            }
        }
        ending = next;
    }
    (i, j, k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matching_pairs_are_difflib_s_matching_blocks() {
        // Each expected value is Python's difflib.SequenceMatcher(None, a, b, autojunk=False).get_matching_blocks()
        // on the two strings' characters, as (i, j, n) blocks.
        for (a, b, blocks) in [
            ("", "", vec![]),
            ("a", "", vec![]),
            ("", "a", vec![]),
            ("abc", "abc", vec![(0, 0, 3)]),
            ("abxcd", "abcd", vec![(0, 0, 2), (3, 2, 2)]),
            ("abab", "baba", vec![(0, 1, 3)]),
            ("aaa", "aa", vec![(0, 0, 2)]),
            ("abcab", "ab", vec![(0, 0, 2)]),
            ("xabyab", "abzab", vec![(1, 0, 2), (4, 3, 2)]),
            ("abcd", "dcba", vec![(0, 3, 1)]),
            ("qabxcdq", "abycdq", vec![(1, 0, 2), (4, 3, 3)]),
        ] {
            let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
            let expected: Vec<(usize, usize)> =
                blocks.into_iter().flat_map(|(i, j, n)| (0..n).map(move |d| (i + d, j + d))).collect();
            assert_eq!(matching_pairs(&a, &b), expected, "{a:?} {b:?}");
        }
    }
}
