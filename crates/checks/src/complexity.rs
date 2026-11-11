//! The cognitive-complexity check: functions a change edits whose score, by G. Ann Campbell's "Cognitive Complexity"
//! as [`model::Flow`] reads it, is over [`LIMIT`] and worse than at the base. The rules each language's constructs
//! map onto are in docs/quality-gate.md, "Cognitive complexity".

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::{OsStr, OsString};

use model::{Flow, Function, SourceFile};

use crate::{ChangedFile, Finding};

/// The check's limit: a function the change edits fails the gate above it, as with SonarQube's default for S3776.
pub const LIMIT: u32 = 15;

const CHECK: &str = "cognitive-complexity";

/// The check's findings on `files`, one language's files of a change, in path and line order: each function the
/// change adds a line to, attributes or decorators included, in a file `exempt` does not exempt, that is not test code
/// ([`Function::exempt`]), and whose score is over [`LIMIT`] and higher than its base's, as [`base_scores`] pairs
/// them, or which has no base; at its line, with both scores. A function over the limit that the change leaves no
/// worse is not a finding: the gate judges the change, not the codebase.
///
/// `exempt` says whether the project exempts the file at a path from the check (gate.toml's `complexity.exempt`). A
/// file the change adds lines to that its front end could not read is a not-scored finding, which fails the gate as
/// any other does.
pub fn too_complex(files: &[ChangedFile], exempt: &dyn Fn(&OsStr) -> bool) -> Vec<Finding> {
    let mut out = vec![];
    let mut judged = vec![];
    for (k, file) in files.iter().enumerate() {
        let Some(after) = file.after.as_ref().filter(|a| !file.added.is_empty() && !exempt(&a.path)) else {
            continue;
        };
        match &after.source {
            Ok(_) => judged.push(k),
            Err(unreadable) => out.push(Finding::of(
                CHECK,
                "not-scored",
                true,
                Some(&after.path),
                unreadable.line,
                format!("{}, so its functions were not scored.", unreadable.why),
            )),
        }
    }
    let scored: Vec<Vec<Scored>> = files.iter().map(|f| f.now().map(scored_functions).unwrap_or_default()).collect();
    let over_limit = |k: usize| edited_over_limit(&files[k], &scored[k]);
    if judged.iter().all(|&k| over_limit(k).is_empty()) {
        return out;
    }
    let bases = base_scores(files, &scored, &judged, &over_limit, exempt);
    for &k in &judged {
        let path = files[k].after.as_ref().map(|a| &a.path);
        let finding = |i: usize| over_limit_finding(path, &scored[k][i], bases.get(&(k, i)));
        out.extend(over_limit(k).into_iter().filter_map(finding));
    }
    out.sort_by(|a, b| (&a.path, a.line).cmp(&(&b.path, b.line)));
    out
}

/// The indexes in `scored`, the working-tree functions of `file`, of those over [`LIMIT`] that are not test code and
/// that the change adds a line to.
fn edited_over_limit(file: &ChangedFile, scored: &[Scored]) -> Vec<usize> {
    let edited = |f: &Function| file.added.range(f.lines.first..=f.lines.last).next().is_some();
    let over = scored.iter().enumerate().filter(|(_, s)| !s.function.exempt && s.score > LIMIT && edited(s.function));
    over.map(|(i, _)| i).collect()
}

/// The over-limit finding on `scored`, a function over [`LIMIT`] of the file at `path`, whose base scored `was`, None
/// for a new function; None when it is no worse than its base.
fn over_limit_finding(path: Option<&OsString>, scored: &Scored, was: Option<&u32>) -> Option<Finding> {
    let base = match was {
        Some(&was) if scored.score <= was => return None,
        Some(was) => format!("{was} at the base"),
        None => "a new function".to_string(),
    };
    let (name, score) = (&scored.function.name, scored.score);
    let message = format!("`{name}`: cognitive complexity {score}, {base}; the limit is {LIMIT}.");
    Some(Finding::of(CHECK, "over-limit", true, path, Some(scored.function.line), message))
}

/// A function of a [`SourceFile`] with its score.
struct Scored<'a> {
    function: &'a Function,
    score: u32,
}

/// The cognitive complexity of each of `source`'s functions, in their order: the points of its flow, and one more
/// when it recurses, calling itself or lying on a cycle of calls through others of the file's functions that are not
/// test code ([`Function::calls`], [`Function::exempt`]).
pub fn scores(source: &SourceFile) -> Vec<u32> {
    scored_functions(source).iter().map(|s| s.score).collect()
}

/// Each of `source`'s functions, scored as [`scores`] says.
fn scored_functions(source: &SourceFile) -> Vec<Scored<'_>> {
    // A call into test code is not followed: the tests' functions take no part in a module's recursion.
    let edges: Vec<Vec<usize>> = source
        .functions
        .iter()
        .map(|f| f.calls.iter().copied().filter(|&j| !source.functions[j].exempt).collect())
        .collect();
    let recursive = recursing(&edges);
    let scored = source.functions.iter().enumerate();
    scored.map(|(i, f)| Scored { function: f, score: score(&f.flow) + u32::from(recursive.contains(&i)) }).collect()
}

/// The points of `flow`, a function's body, by [`Flow`]'s rules, recursion of the function itself left out.
fn score(flow: &[Flow]) -> u32 {
    // A stack rather than recursion, so that no body is too deep to score.
    let mut points = 0;
    let mut todo: Vec<(&Flow, u32)> = flow.iter().map(|f| (f, 0)).collect();
    while let Some((node, level)) = todo.pop() {
        match node {
            Flow::Increment { nesting, level: at_level, nested } => {
                points += 1 + if *nesting { level } else { 0 };
                todo.extend(at_level.iter().map(|f| (f, level)));
                todo.extend(nested.iter().map(|f| (f, level + 1)));
            }
            Flow::Nesting { recursive, body } => {
                points += u32::from(*recursive);
                todo.extend(body.iter().map(|f| (f, level + 1)));
            }
            Flow::Booleans { ands, operands } => {
                let changes = ands.windows(2).filter(|pair| pair[0] != pair[1]).count();
                points += 1 + u32::try_from(changes).unwrap_or(u32::MAX);
                todo.extend(operands.iter().map(|f| (f, level)));
            }
        }
    }
    points
}

/// The vertices of the graph whose vertices are `edges`' indexes, with an edge from v to each of `edges[v]`, that lie
/// on a cycle: in a strongly connected component of more than one, or with an edge to themselves.
fn recursing(edges: &[Vec<usize>]) -> HashSet<usize> {
    let mut out = HashSet::new();
    for component in strong_components(edges) {
        for &v in &component {
            if component.len() > 1 || edges[v].contains(&v) {
                out.insert(v);
            }
        }
    }
    out
}

/// The strongly connected components of the graph `edges` gives, as [`recursing`] reads it, each vertex in exactly
/// one, by Kosaraju's algorithm; iterative, so that no graph is too deep for it.
fn strong_components(edges: &[Vec<usize>]) -> Vec<Vec<usize>> {
    let mut reverse = vec![vec![]; edges.len()];
    for (v, targets) in edges.iter().enumerate() {
        for &w in targets {
            reverse[w].push(v);
        }
    }
    // Kosaraju's second pass: each vertex not yet in a component, latest finished first, gathers those that reach it.
    let mut seen = vec![false; edges.len()];
    let mut components = vec![];
    for v in finishing_order(edges).into_iter().rev() {
        if seen[v] {
            continue;
        }
        seen[v] = true;
        let mut component = vec![v];
        let mut todo = vec![v];
        while let Some(u) = todo.pop() {
            for &w in &reverse[u] {
                if !seen[w] {
                    seen[w] = true;
                    component.push(w);
                    todo.push(w);
                }
            }
        }
        components.push(component);
    }
    components
}

/// Each vertex of the graph `edges` gives, as [`recursing`] reads it, in the order a depth-first search from each
/// unvisited vertex in turn, lowest first, finishes it: Kosaraju's first pass.
fn finishing_order(edges: &[Vec<usize>]) -> Vec<usize> {
    let mut order = vec![];
    let mut seen = vec![false; edges.len()];
    for root in 0..edges.len() {
        if seen[root] {
            continue;
        }
        seen[root] = true;
        // The path from root to the vertex being searched, each with the index in its edges of the next to try.
        let mut walk = vec![(root, 0)];
        while let Some((v, next)) = walk.last_mut() {
            let v = *v;
            match edges[v][*next..].iter().position(|&w| !seen[w]) {
                Some(k) => {
                    let w = edges[v][*next + k];
                    *next += k + 1;
                    seen[w] = true;
                    walk.push((w, 0));
                }
                None => {
                    walk.pop();
                    order.push(v);
                }
            }
        }
    }
    order
}

/// The order in which [`base_scores`] pairs a function with a base function of its body, each as (of its name, in its
/// file's base): a function unchanged where it was, moved, renamed in place, renamed and moved.
const BODY_STEPS: [(bool, bool); 4] = [(true, true), (true, false), (false, true), (false, false)];

/// The base score of each function the check judges that has a base, by (index in `files`, index in its file's
/// functions); a function with no key is new. `scored` is each file's working-tree functions, `judged` the files whose
/// functions the check judges, and `over_limit` the functions of a judged file over the limit that the change edits.
///
/// A function's base is a function of the base, each the base of one function at most, so that a copy never takes the
/// score of what it copies. First a base function with its body, since an unchanged function is that function whatever
/// its name: of its name in its file's base, under the file's old name when the change renamed it, then of its name
/// elsewhere, as for a function moved; then of any name in its file's base, then elsewhere, as for one renamed. A base
/// function lends to a function of another file only when its body is gone from its file, which now holds it fewer
/// times than the base did: one still there, under any name, is the base of itself, and the other a copy. Failing a
/// body, a function over the limit takes the highest-scoring base function of its name left in its file's base.
///
/// A judged function takes no base from test code: a function exempt itself ([`Function::exempt`]), or one of a file
/// `exempt` exempts, since the tests were never held to the limit. Every other function of the change, exempt and in
/// files the check does not judge included, pairs by body too, in each step before the judged ones, so that one moved
/// unchanged among the tests is still itself and lends its base to no copy, whatever the copy's name.
///
/// The cost of bodies first: a copy of a function whose original the change edits takes the original's old score, as
/// if the original were the new function under its name (TODO.md, "A copy of a function whose original the change
/// edits keeps the old score and passes.").
fn base_scores(
    files: &[ChangedFile],
    scored: &[Vec<Scored>],
    judged: &[usize],
    over_limit: &dyn Fn(usize) -> Vec<usize>,
    exempt: &dyn Fn(&OsStr) -> bool,
) -> HashMap<(usize, usize), u32> {
    let mut pool = BasePool::new(files, scored, exempt);
    let is_judged = |k: usize, i: usize| judged.contains(&k) && !scored[k][i].function.exempt;
    // Every function of the working tree by (file, function) index, the ones not judged first.
    let mut everyone: Vec<(usize, usize)> =
        scored.iter().enumerate().flat_map(|(k, functions)| (0..functions.len()).map(move |i| (k, i))).collect();
    everyone.sort_by_key(|&(k, i)| is_judged(k, i));
    for (same_name, same_file) in BODY_STEPS {
        for &(k, i) in &everyone {
            pool.lend_by_body((k, i), &scored[k][i], same_name, same_file, is_judged(k, i));
        }
    }
    for &k in judged {
        for i in over_limit(k) {
            pool.lend_by_name((k, i), &scored[k][i]);
        }
    }
    pool.lent.into_iter().filter(|((k, i), _)| is_judged(*k, *i)).collect()
}

/// The functions of the change's files at the base, each of which lends its score to one function of the working tree
/// at most, for [`base_scores`].
struct BasePool<'a> {
    /// The base functions that have lent no score yet, by the index in the change's files of the file they are in.
    left: BTreeMap<usize, Vec<Scored<'a>>>,
    /// (file, body) of each body that the working tree's file holds fewer times than its base did: moved out of it,
    /// edited or deleted, so that its base functions with that body may lend to functions of other files. The count
    /// does not ask where the working tree's copies came from (TODO.md: "A body moved into a file ...").
    gone: HashSet<(usize, &'a str)>,
    /// Whether the file at each index exempts its base functions, by gate.toml's `complexity.exempt`.
    exempt_file: Vec<bool>,
    /// The score lent to each function of the working tree, by (file, function) index.
    lent: HashMap<(usize, usize), u32>,
}

impl<'a> BasePool<'a> {
    /// The pool of `files`' base functions, `scored` being each file's working-tree functions and `exempt` saying
    /// whether gate.toml exempts the file at a path.
    fn new(files: &'a [ChangedFile], scored: &[Vec<Scored>], exempt: &dyn Fn(&OsStr) -> bool) -> BasePool<'a> {
        let mut left = BTreeMap::new();
        let mut gone = HashSet::new();
        for (k, file) in files.iter().enumerate() {
            let Some(base) = file.base() else { continue };
            let functions = scored_functions(base);
            let mut counts: HashMap<&str, usize> = HashMap::new();
            for s in &functions {
                *counts.entry(s.function.body.as_str()).or_default() += 1;
            }
            for (body, n) in counts {
                if scored[k].iter().filter(|s| s.function.body == body).count() < n {
                    gone.insert((k, body));
                }
            }
            left.insert(k, functions);
        }
        let exempt_file = files.iter().map(|f| f.before.as_ref().is_some_and(|b| exempt(&b.path))).collect();
        BasePool { left, gone, exempt_file, lent: HashMap::new() }
    }

    /// Lend to `scored`, the working tree's function at `at`, unless it has a score lent, the score of a base function
    /// left with its body: and its name for `same_name`, and in its own file's base for `same_file`, else in another
    /// file's whose body is gone from it. A `judged` function takes none from test code.
    fn lend_by_body(&mut self, at: (usize, usize), scored: &Scored, same_name: bool, same_file: bool, judged: bool) {
        if self.lent.contains_key(&at) {
            return;
        }
        let twin = self.left.iter().filter(|(&k, _)| (k == at.0) == same_file).find_map(|(&k, left)| {
            left.iter()
                .position(|base| {
                    base.function.body == scored.function.body
                        && (!same_name || base.function.name == scored.function.name)
                        && (same_file || self.gone.contains(&(k, base.function.body.as_str())))
                        && !(judged && (base.function.exempt || self.exempt_file[k]))
                })
                .map(|i| (k, i))
        });
        if let Some((k, i)) = twin {
            self.lend(at, k, i);
        }
    }

    /// Lend to `scored`, a judged function at `at`, unless it has a score lent, the highest score of the base
    /// functions of its name left in its own file's base that are not test code.
    fn lend_by_name(&mut self, at: (usize, usize), scored: &Scored) {
        if self.lent.contains_key(&at) || self.exempt_file[at.0] {
            return;
        }
        let Some(left) = self.left.get(&at.0) else { return };
        let mut best: Option<usize> = None;
        for (i, base) in left.iter().enumerate() {
            if !base.function.exempt
                && base.function.name == scored.function.name
                && best.is_none_or(|b| base.score > left[b].score)
            {
                best = Some(i);
            }
        }
        if let Some(i) = best {
            self.lend(at, at.0, i);
        }
    }

    fn lend(&mut self, at: (usize, usize), k: usize, i: usize) {
        if let Some(left) = self.left.get_mut(&k) {
            let base = left.remove(i);
            self.lent.insert(at, base.score);
        }
    }
}
