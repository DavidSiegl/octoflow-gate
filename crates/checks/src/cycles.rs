//! The dependency-cycles check: a cycle of imports between a project's modules that a change closes. Two modules
//! that import each other, directly or through others, cannot be read, tested or moved apart.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::ffi::{OsStr, OsString};

use model::{Import, ImportGraph};

use crate::Finding;

/// The check's findings, each failing the gate, by path and line: one for each cycle of imports in `now` that `base`
/// lacks, at an import line the change added that closes it, naming the cycle's files in order from that import's
/// and back. Two imports that close cycles of the same files make one finding, at the first.
///
/// `base` and `now` are one language's graphs of the tree at the base and of the working tree, `base`'s paths
/// already those the files have in the working tree where the change renames them. `added` holds the lines the change
/// added to each file of the working tree. A file `exempt` names takes part in no cycle, its imports and the imports
/// of it both left out.
///
/// A cycle is new where it uses an import from one file of another that the base had no import of. Only such an
/// import, on an added line, closes one: a cycle the change only adds a line to, or one the base held already, is
/// not reported, as nothing outside a change's added lines is anywhere else in the gate.
pub fn new_cycles(
    base: &ImportGraph,
    now: &ImportGraph,
    added: &BTreeMap<OsString, BTreeSet<u32>>,
    exempt: &dyn Fn(&OsStr) -> bool,
) -> Vec<Finding> {
    let now_edges = edges(now, exempt);
    let base_edges = edges(base, exempt);
    let had = |from: &OsStr, to: &OsStr| base_edges.get(from).is_some_and(|targets| targets.contains_key(to));
    let mut reported: HashSet<BTreeSet<&OsStr>> = HashSet::new();
    let mut out = vec![];
    for (&from, targets) in &now_edges {
        let Some(lines) = added.get(from) else { continue };
        for (&to, imports) in targets {
            let Some(closing) = imports.iter().find(|i| lines.contains(&i.line)).filter(|_| !had(from, to)) else {
                continue;
            };
            let Some(back) = shortest_path(&now_edges, to, from) else { continue };
            if !reported.insert(back.iter().copied().collect()) {
                continue;
            }
            out.push(finding(from, closing, &back));
        }
    }
    out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    out
}

/// `graph`'s imports between files `exempt` leaves in, by importing file, then imported file, each pair's imports in
/// the order they are written.
fn edges<'g>(
    graph: &'g ImportGraph,
    exempt: &dyn Fn(&OsStr) -> bool,
) -> BTreeMap<&'g OsStr, BTreeMap<&'g OsStr, Vec<&'g Import>>> {
    let mut out: BTreeMap<&OsStr, BTreeMap<&OsStr, Vec<&Import>>> = BTreeMap::new();
    for (from, imports) in graph.iter().filter(|(path, _)| !exempt(path)) {
        let targets = out.entry(from.as_os_str()).or_default();
        for import in imports.iter().filter(|i| !exempt(&i.module) && i.module != *from) {
            targets.entry(import.module.as_os_str()).or_default().push(import);
        }
    }
    out
}

/// The files of a shortest chain of imports from `start` to `end` in `edges`, both included; None when there is
/// none. Of two chains of one length, the one through the files first by path.
fn shortest_path<'g>(
    edges: &BTreeMap<&'g OsStr, BTreeMap<&'g OsStr, Vec<&'g Import>>>,
    start: &'g OsStr,
    end: &'g OsStr,
) -> Option<Vec<&'g OsStr>> {
    let mut came_from: HashMap<&OsStr, &OsStr> = HashMap::new();
    let mut queue = VecDeque::from([start]);
    let mut seen = HashSet::from([start]);
    while let Some(at) = queue.pop_front() {
        if at == end {
            let mut path = vec![end];
            while let Some(&before) = came_from.get(path[path.len() - 1]) {
                path.push(before);
            }
            path.reverse();
            return Some(path);
        }
        for &next in edges.get(at).into_iter().flat_map(|targets| targets.keys()) {
            if seen.insert(next) {
                came_from.insert(next, at);
                queue.push_back(next);
            }
        }
    }
    None
}

/// The finding on `closing`, an import in `from` of the first of `back`, the files leading back to `from`.
fn finding(from: &OsStr, closing: &Import, back: &[&OsStr]) -> Finding {
    let shown = |path: &OsStr| format!("`{}`", path.to_string_lossy());
    let chain: Vec<String> = std::iter::once(from).chain(back.iter().copied()).map(shown).collect();
    let deferred = if closing.deferred {
        " The import is deferred, inside a function, but the modules depend on each other all the same."
    } else {
        ""
    };
    let message = format!(
        "this import closes a dependency cycle, {}: these modules can no longer be read, tested or moved apart.\
         {deferred}",
        chain.join(" → ")
    );
    Finding::of("dependency-cycles", "", true, Some(&from.to_os_string()), Some(closing.line), message)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A graph of `edges`, each (importing file, imported file, line).
    fn graph(edges: &[(&str, &str, u32)]) -> ImportGraph {
        let mut out = ImportGraph::new();
        for &(from, to, line) in edges {
            let import = Import { module: to.into(), line, deferred: false };
            out.entry(from.into()).or_default().push(import);
        }
        out
    }

    /// The added lines `lines`, each (file, line).
    fn added(lines: &[(&str, u32)]) -> BTreeMap<OsString, BTreeSet<u32>> {
        let mut out: BTreeMap<OsString, BTreeSet<u32>> = BTreeMap::new();
        for &(path, line) in lines {
            out.entry(path.into()).or_default().insert(line);
        }
        out
    }

    /// (path, line, message) of each finding of the check, nothing exempt.
    fn found(base: &ImportGraph, now: &ImportGraph, lines: &[(&str, u32)]) -> Vec<(String, u32, String)> {
        let at = |f: Finding| (f.path.unwrap().to_string_lossy().into_owned(), f.line.unwrap(), f.message);
        new_cycles(base, now, &added(lines), &|_| false).into_iter().map(at).collect()
    }

    #[test]
    fn an_added_import_that_closes_a_chain_back_is_found_with_the_chain_in_order() {
        let base = graph(&[("b.py", "c.py", 1), ("c.py", "a.py", 1)]);
        let now = graph(&[("a.py", "b.py", 3), ("b.py", "c.py", 1), ("c.py", "a.py", 1)]);
        let found = found(&base, &now, &[("a.py", 3)]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert_eq!((found[0].0.as_str(), found[0].1), ("a.py", 3));
        let opening = "this import closes a dependency cycle, `a.py` → `b.py` → `c.py` → `a.py`:";
        assert!(found[0].2.starts_with(opening), "{}", found[0].2);
    }

    #[test]
    fn an_import_on_a_line_the_change_did_not_add_closes_nothing() {
        let now = graph(&[("a.py", "b.py", 3), ("b.py", "a.py", 1)]);
        assert_eq!(found(&ImportGraph::new(), &now, &[("a.py", 4)]), []);
    }

    #[test]
    fn a_second_import_between_two_files_the_base_already_joined_is_not_found() {
        let base = graph(&[("a.py", "b.py", 1), ("b.py", "a.py", 1)]);
        let now = graph(&[("a.py", "b.py", 1), ("a.py", "b.py", 2), ("b.py", "a.py", 1)]);
        assert_eq!(found(&base, &now, &[("a.py", 2)]), []);
    }

    #[test]
    fn two_imports_that_close_cycles_of_the_same_files_make_one_finding() {
        let now = graph(&[("a.py", "b.py", 1), ("b.py", "a.py", 1)]);
        let found = found(&ImportGraph::new(), &now, &[("a.py", 1), ("b.py", 1)]);
        assert_eq!(found.iter().map(|f| (f.0.as_str(), f.1)).collect::<Vec<_>>(), [("a.py", 1)]);
    }

    #[test]
    fn a_deferred_import_is_found_and_said_to_be_deferred() {
        let mut now = graph(&[("b.py", "a.py", 1)]);
        now.insert("a.py".into(), vec![Import { module: "b.py".into(), line: 5, deferred: true }]);
        let found = found(&ImportGraph::new(), &now, &[("a.py", 5)]);
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].2.ends_with("The import is deferred, inside a function, but the modules depend on each other \
                                      all the same."), "{}", found[0].2);
    }

    #[test]
    fn an_exempt_file_breaks_the_cycles_through_it() {
        let now = graph(&[("a.py", "tests/t.py", 1), ("tests/t.py", "a.py", 1)]);
        let exempt = |path: &OsStr| path.to_string_lossy().starts_with("tests/");
        assert_eq!(new_cycles(&ImportGraph::new(), &now, &added(&[("a.py", 1)]), &exempt), []);
    }
}
