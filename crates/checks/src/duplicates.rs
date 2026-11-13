//! The duplicated-blocks check: a block of code a change adds that the tree already holds elsewhere, or that the
//! change adds twice, names and literals aside. A copy is logic that has to be fixed twice; a change is where it
//! enters, so the check reads only what the change added.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsString;

use model::CodeToken;

use crate::Finding;

/// The check's findings, each to weigh and none failing the gate, by path and line: one for each run of at least
/// `min_tokens` tokens that the change added to a file and that occurs elsewhere in `files`, at the run's first line,
/// naming the other place as `path:line`.
///
/// `files` holds each file of one language in the working tree that the check judges, as (path from the tree's top
/// level, its tokens as its front end gives them); a file left out of it is neither judged nor matched. `added` holds
/// the lines the change added to each file. `min_tokens` is at least 1.
///
/// A window of `min_tokens` tokens is the change's when every one of its tokens lies on a line the change added. It
/// is copied when an equal window lies anywhere else in `files`, in the same file too where the two do not overlap.
/// Copied windows of a file that overlap make one run, reported once, naming the place that matches most of its
/// windows: of two such, one the change did not wholly add before one it did, then the first by path and line. A
/// block the change adds twice is reported once, at the later copy by path and line, naming the earlier.
pub fn copied_blocks(
    files: &[(OsString, Vec<CodeToken>)],
    added: &BTreeMap<OsString, BTreeSet<u32>>,
    min_tokens: usize,
) -> Vec<Finding> {
    let n = min_tokens;
    let shapes: Vec<Vec<u64>> = files.iter().map(|(_, tokens)| tokens.iter().map(|t| t.shape).collect()).collect();
    let ours: Vec<Vec<bool>> = files.iter().map(|(path, tokens)| changes_windows(tokens, added.get(path), n)).collect();
    // Only a window that hashes as one of the change's can match one, so only those are indexed.
    let wanted: HashSet<u64> = window_hashes(&shapes, n)
        .filter(|&(f, i, _)| ours[f][i])
        .map(|(_, _, hash)| hash)
        .collect();
    let mut index: HashMap<u64, Vec<Place>> = HashMap::new();
    for (f, i, hash) in window_hashes(&shapes, n).filter(|(_, _, hash)| wanted.contains(hash)) {
        index.entry(hash).or_default().push(Place { file: f, start: i });
    }
    let line = |place: Place| files[place.file].1[place.start].line;
    let mut out = vec![];
    for (f, (path, _)) in files.iter().enumerate() {
        for run in runs(f, &shapes, &ours, &index, n) {
            let (other, new) = run.best_match(&ours, |p| (&files[p.file].0, line(p)));
            let here = Place { file: f, start: run.start };
            if new && (&files[other.file].0, line(other)) > (path, line(here)) {
                continue;
            }
            let shown = format!("{}:{}", files[other.file].0.to_string_lossy(), line(other));
            let message = format!(
                "these {} tokens repeat `{shown}`, names and literals aside: a copy is logic to fix twice; share it, \
                 or say why the two differ.",
                run.tokens(n)
            );
            out.push(Finding::of("duplicated-blocks", "", false, Some(path), Some(line(here)), message));
        }
    }
    out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    out
}

/// A window's place: the file's index in `files` and the window's first token's index in it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct Place {
    file: usize,
    start: usize,
}

/// Overlapping copied windows of one file.
struct Run {
    /// The first window's start.
    start: usize,
    /// For each copied window, in order: how far its start lies past `start`, and the places elsewhere whose windows
    /// equal it.
    windows: Vec<(usize, Vec<Place>)>,
}

impl Run {
    /// The number of tokens the run covers, its windows being `n` long.
    fn tokens(&self, n: usize) -> usize {
        self.windows.last().map_or(0, |(k, _)| k + n)
    }

    /// The other place that matches most of the run's windows, given by its first window that matches one of them,
    /// and whether each of those windows is the change's ([`changes_windows`]), the place a second copy the change
    /// added rather than code the tree held. Of two places that match as many windows, one the tree held comes
    /// first, then the first by `order`. Places are told apart by where they would start were they aligned with the
    /// run's start.
    fn best_match<K: Ord>(&self, ours: &[Vec<bool>], order: impl Fn(Place) -> K) -> (Place, bool) {
        // aligned start → (windows matched, first place matched, whether every matched window is the change's)
        let mut found: HashMap<(usize, isize), (usize, Place, bool)> = HashMap::new();
        for (k, places) in &self.windows {
            for &p in places {
                let aligned = (p.file, p.start as isize - *k as isize);
                let entry = found.entry(aligned).or_insert((0, p, true));
                entry.0 += 1;
                entry.2 &= ours[p.file][p.start];
            }
        }
        let rank = |&(count, place, new): &(usize, Place, bool)| (std::cmp::Reverse(count), new, order(place));
        let best = found.values().min_by(|a, b| rank(a).cmp(&rank(b)));
        best.map(|&(_, place, new)| (place, new)).expect("a run has a copied window")
    }
}

/// For each window start of `tokens`, `n` long, whether every token of the window lies on a line of `added`.
fn changes_windows(tokens: &[CodeToken], added: Option<&BTreeSet<u32>>, n: usize) -> Vec<bool> {
    let Some(added) = added else { return vec![false; tokens.len().saturating_sub(n - 1)] };
    let mut out = vec![];
    // The number of tokens off added lines among the current window's.
    let mut outside = 0;
    for (i, token) in tokens.iter().enumerate() {
        outside += usize::from(!added.contains(&token.line));
        if i >= n {
            outside -= usize::from(!added.contains(&tokens[i - n].line));
        }
        if i + 1 >= n {
            out.push(outside == 0);
        }
    }
    out
}

/// (file, start, hash) of each window, `n` long, of each of `shapes`, a polynomial hash over the shapes that equal
/// windows share.
fn window_hashes(shapes: &[Vec<u64>], n: usize) -> impl Iterator<Item = (usize, usize, u64)> + '_ {
    const BASE: u64 = 0x0000_0100_0000_01b3;
    // BASE to the power n - 1, to take a window's first shape out of its hash.
    let top = (1..n).fold(1u64, |acc, _| acc.wrapping_mul(BASE));
    shapes.iter().enumerate().flat_map(move |(f, shapes)| {
        let mut hash = 0u64;
        shapes.iter().enumerate().filter_map(move |(i, &shape)| {
            if i >= n {
                hash = hash.wrapping_sub(shapes[i - n].wrapping_mul(top));
            }
            hash = hash.wrapping_mul(BASE).wrapping_add(shape);
            (i + 1 >= n).then(|| (f, i + 1 - n, hash))
        })
    })
}

/// The runs of file `f`: its windows that are the change's ([`changes_windows`]) and match a window elsewhere in
/// `index` of equal shapes, not overlapping it, those that overlap joined.
fn runs(f: usize, shapes: &[Vec<u64>], ours: &[Vec<bool>], index: &HashMap<u64, Vec<Place>>, n: usize) -> Vec<Run> {
    let mut out: Vec<Run> = vec![];
    for (_, i, hash) in window_hashes(&shapes[f..=f], n).filter(|&(_, i, _)| ours[f][i]) {
        let window = &shapes[f][i..i + n];
        let elsewhere = |p: &&Place| p.file != f || p.start.abs_diff(i) >= n;
        let places: Vec<Place> = index
            .get(&hash)
            .into_iter()
            .flatten()
            .filter(elsewhere)
            .filter(|p| shapes[p.file][p.start..p.start + n] == *window)
            .copied()
            .collect();
        if places.is_empty() {
            continue;
        }
        match out.last_mut() {
            Some(run) if i < run.start + run.tokens(n) => run.windows.push((i - run.start, places)),
            _ => out.push(Run { start: i, windows: vec![(0, places)] }),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file's tokens, one per line from line 1, each shape as given.
    fn tokens(shapes: &[u64]) -> Vec<CodeToken> {
        (1..).zip(shapes).map(|(line, &shape)| CodeToken { shape, line }).collect()
    }

    /// The lines `first..=last` of `path`, as the change added them.
    fn added(lines: &[(&str, u32, u32)]) -> BTreeMap<OsString, BTreeSet<u32>> {
        let mut out: BTreeMap<OsString, BTreeSet<u32>> = BTreeMap::new();
        for &(path, first, last) in lines {
            out.entry(path.into()).or_default().extend(first..=last);
        }
        out
    }

    /// (path, line, message) of each finding on `files`, windows 4 tokens long.
    fn found(files: &[(&str, &[u64])], lines: &[(&str, u32, u32)]) -> Vec<(String, u32, String)> {
        let files: Vec<(OsString, Vec<CodeToken>)> = files.iter().map(|&(p, s)| (p.into(), tokens(s))).collect();
        let at = |f: Finding| (f.path.unwrap().to_string_lossy().into_owned(), f.line.unwrap(), f.message);
        copied_blocks(&files, &added(lines), 4).into_iter().map(at).collect()
    }

    #[test]
    fn an_added_block_the_tree_holds_is_found_naming_the_other_place_and_its_length() {
        let found = found(&[("a.rs", &[9, 1, 2, 3, 4, 5, 9]), ("b.rs", &[8, 8, 1, 2, 3, 4, 5])], &[("a.rs", 2, 6)]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].0.as_str(), found[0].1), ("a.rs", 2));
        assert!(found[0].2.starts_with("these 5 tokens repeat `b.rs:3`, names and literals aside"), "{}", found[0].2);
    }

    #[test]
    fn a_block_shorter_than_the_window_is_not_found() {
        assert_eq!(found(&[("a.rs", &[1, 2, 3, 7]), ("b.rs", &[1, 2, 3, 8])], &[("a.rs", 1, 4)]), []);
    }

    #[test]
    fn a_copy_of_lines_the_change_did_not_add_is_not_found() {
        assert_eq!(found(&[("a.rs", &[1, 2, 3, 4]), ("b.rs", &[1, 2, 3, 4])], &[("a.rs", 1, 3)]), []);
    }

    #[test]
    fn a_copy_in_the_same_file_counts_where_the_two_do_not_overlap() {
        let found = found(&[("a.rs", &[1, 2, 3, 4, 1, 2, 3, 4])], &[("a.rs", 5, 8)]);
        assert_eq!(found.iter().map(|f| (f.0.as_str(), f.1)).collect::<Vec<_>>(), [("a.rs", 5)]);
        assert!(found[0].2.contains("`a.rs:1`"), "{}", found[0].2);
    }

    #[test]
    fn a_copy_that_overlaps_the_block_is_not_found() {
        assert_eq!(found(&[("a.rs", &[1, 1, 1, 1, 1, 1])], &[("a.rs", 1, 6)]), []);
    }

    #[test]
    fn a_block_the_change_adds_twice_is_found_once_at_the_later_copy() {
        let found = found(&[("a.rs", &[1, 2, 3, 4]), ("b.rs", &[1, 2, 3, 4])], &[("a.rs", 1, 4), ("b.rs", 1, 4)]);
        assert_eq!(found.iter().map(|f| (f.0.as_str(), f.1)).collect::<Vec<_>>(), [("b.rs", 1)]);
        assert!(found[0].2.contains("`a.rs:1`"), "{}", found[0].2);
    }

    #[test]
    fn of_two_places_that_match_as_much_the_one_the_tree_held_is_named() {
        let files: &[(&str, &[u64])] = &[("a.rs", &[1, 2, 3, 4]), ("b.rs", &[1, 2, 3, 4]), ("c.rs", &[1, 2, 3, 4])];
        let found = found(files, &[("a.rs", 1, 4), ("b.rs", 1, 4)]);
        assert_eq!(found.iter().map(|f| (f.0.as_str(), f.1)).collect::<Vec<_>>(), [("a.rs", 1), ("b.rs", 1)]);
        assert!(found.iter().all(|f| f.2.contains("`c.rs:1`")), "{found:?}");
    }

    #[test]
    fn overlapping_copied_windows_make_one_finding_named_by_the_place_that_matches_most() {
        let files: &[(&str, &[u64])] =
            &[("a.rs", &[1, 2, 3, 4, 5, 6]), ("b.rs", &[1, 2, 3, 4]), ("c.rs", &[1, 2, 3, 4, 5, 6])];
        let found = found(files, &[("a.rs", 1, 6)]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].2.starts_with("these 6 tokens repeat `c.rs:1`"), "{}", found[0].2);
    }
}
