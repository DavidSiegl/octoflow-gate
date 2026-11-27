//! The flaky-tests check: the Python and Go tests a change adds or edits, rerun in several orders, with
//! pytest-randomly's seeds (python.rs) and `go test -shuffle`'s (go.rs). A test that passes in one order and fails in
//! another is one the build's single green run does not show. The reruns' bookkeeping and the findings both languages
//! give alike are here.

mod go;
mod python;

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsStr;
use std::hash::Hash;
use std::time::Duration;

use model::Unreadable;

use crate::copies::{finding, in_working_tree_copy, sort_by_place};
use crate::junit::Outcome;
use crate::report::Finding;
use crate::sources::Sources;
use crate::text::series;

use go::go_findings;
use python::python_findings;

/// The seeds the changed tests are rerun under, once each, in this order: pytest-randomly's, and `go test -shuffle`'s.
const SEEDS: [u32; 5] = [1, 2, 3, 4, 5];

/// How long one pytest or go test run of the check may take: pytest's collection, and each rerun.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// The flaky-tests check's findings on the Python and Go tests of `sources`, by path and line, a finding on no path or
/// line first; each rule is the check's own, without its name, and each severity "warning". Python's are as
/// [`python_findings`] says, Go's as [`go_findings`] does.
///
/// The tests run in one temporary copy of the working tree (copies::in_working_tree_copy), Python's first, made only
/// when either has tests to rerun, so that what the project's addopts write, such as a coverage report, does not
/// replace the build's own; a test that needs a file git ignores, such as a build product, cannot find it there. A copy
/// that cannot be made is the one `not-rerun` finding beside those on unreadable test files, and a copy that cannot be
/// removed afterwards a `not-removed` one, beside the results.
pub fn flaky_tests(sources: &Sources) -> Vec<Finding> {
    let (mut out, python) = python_findings(&sources.python);
    let (unread, go) = go_findings(&sources.go);
    out.extend(unread);
    let runs = python.into_iter().chain(go).collect();
    out.extend(in_working_tree_copy(sources.python.change, "not-rerun", "the changed tests were not rerun", runs));
    sort_by_place(&mut out);
    out
}

/// The `not-rerun` finding that `problem`, a clause, stopped the reruns of `which`, the tests as findings name them
/// (pytest::labels; "the changed tests" when None), after `ran`, the seeds of the reruns that told something.
fn stopped_not_rerun(problem: &str, which: Option<&str>, ran: &[u32]) -> Finding {
    let judged = if ran.is_empty() {
        format!("{} were not rerun.", which.unwrap_or("the changed tests"))
    } else {
        let of = which.map(|which| format!(" of {which}")).unwrap_or_default();
        format!("only the reruns{of} under {} were judged.", seeds(ran))
    };
    finding(None, None, "not-rerun", format!("{problem}; {judged}"))
}

/// The `not-rerun` finding on `path`, a changed test file, which `unreadable` says could not be read or parsed.
fn unread_not_rerun((path, unreadable): (&OsStr, &Unreadable)) -> Finding {
    let message = format!("{}, so its tests were not rerun.", unreadable.why);
    finding(Some(path), unreadable.line, "not-rerun", message)
}

/// What the reruns of a set of tests, one under each of [`SEEDS`] in turn, told of each test, a test known by a key
/// `K`: its pytest node id, or its Go test name.
#[derive(Default)]
struct Reruns<K> {
    /// Each test's outcome, by its key and then by the seed of each run that reported it failed, xfailed or passed.
    outcomes: HashMap<K, BTreeMap<u32, Outcome>>,
    /// The seeds of the runs that told something, in the order they ran.
    seeds: Vec<u32>,
    /// The seed whose run went wrong and ended the reruns, and what went wrong with it, a clause; None when every
    /// run told something.
    stopped: Option<(u32, String)>,
}

impl<K: Eq + Hash> Reruns<K> {
    /// What `rerun(seed)` tells for each of [`SEEDS`] in turn, until it errs: the outcome of each test it reran, by
    /// its key, or the problem, a clause, that left the run telling nothing.
    fn run(mut rerun: impl FnMut(u32) -> Result<HashMap<K, Outcome>, String>) -> Reruns<K> {
        let mut reruns = Reruns { outcomes: HashMap::new(), seeds: vec![], stopped: None };
        for seed in SEEDS {
            match rerun(seed) {
                Ok(reported) => {
                    reruns.seeds.push(seed);
                    for (id, outcome) in reported.into_iter().filter(|(_, outcome)| *outcome != Outcome::Skipped) {
                        reruns.outcomes.entry(id).or_default().insert(seed, outcome);
                    }
                }
                Err(problem) => {
                    reruns.stopped = Some((seed, problem));
                    break;
                }
            }
        }
        reruns
    }

    /// The rule and message of the check's finding on the test of key `key`, one of those rerun, by what the reruns
    /// told of it, the message calling it `name`, as "test_reads[1]"; None when they show nothing to report.
    fn judge(&self, key: &K, name: &str) -> Option<(&'static str, String)> {
        let none = BTreeMap::new();
        let by_seed = self.outcomes.get(key).unwrap_or(&none);
        let under = |outcome: Outcome| -> Vec<u32> {
            self.seeds.iter().copied().filter(|seed| by_seed.get(seed) == Some(&outcome)).collect()
        };
        // What it did under which seeds, for each outcome it had.
        let verbs = [
            (Outcome::Failed, "failed"),
            (Outcome::Xfailed, "failed as its xfail mark expects"),
            (Outcome::Passed, "passed"),
        ];
        let did: Vec<String> = verbs
            .into_iter()
            .map(|(outcome, verb)| (verb, under(outcome)))
            .filter(|(_, under)| !under.is_empty())
            .map(|(verb, under)| format!("{verb} under {}", seeds(&under)))
            .collect();
        let unjudged: Vec<u32> = self.seeds.iter().copied().filter(|seed| !by_seed.contains_key(seed)).collect();
        if did.len() > 1 {
            let skipped = match unjudged.as_slice() {
                [] => String::new(),
                unjudged => format!(", and was skipped or not run under {}", seeds(unjudged)),
            };
            return Some((
                "mixed-results",
                format!(
                    "`{name}` {}{skipped}: it depends on the order the tests run in, or on state or randomness that \
                     one run does not share with another.",
                    series(&did, None)
                ),
            ));
        }
        if !self.seeds.is_empty() && by_seed.len() < 2 {
            // Without a run that told something, the finding about the run already says no test was rerun.
            let did = match did.first() {
                Some(did) => format!("{did} only"),
                None => "neither passed nor failed under any seed".into(),
            };
            let reports = match unjudged.as_slice() {
                [] => String::new(),
                unjudged => format!("; the reports of {} have it skipped or not at all", seeds(unjudged)),
            };
            let stopped = self.stopped.as_ref().map_or(String::new(), |(seed, _)| {
                format!("; the reruns stopped when the run under seed {seed} went wrong")
            });
            let message = format!("`{name}` {did}{reports}{stopped}, so too few runs tell whether it is flaky.");
            return Some(("not-rerun", message));
        }
        if by_seed.len() >= 2 && by_seed.values().all(|outcome| *outcome == Outcome::Failed) {
            // Not flaky, but the gate cannot tell a test the build fails too from one that needs what the copy lacks.
            let judged: Vec<u32> = by_seed.keys().copied().collect();
            let left_out = match unjudged.as_slice() {
                [] => String::new(),
                unjudged => format!(" that did not skip it or leave it out ({} did)", seeds(unjudged)),
            };
            return Some((
                "failed-every-run",
                format!(
                    "`{name}` failed under {}, every rerun{left_out}, which ran in a copy of the working tree. If \
                     the build failed it too, the failing tests list it. If the build passed it, it needs something \
                     the copy lacks, such as git's metadata, a file git ignores or a build product, so \
                     whether it is flaky could not be judged here.",
                    seeds(&judged)
                ),
            ));
        }
        None
    }
}

/// "seed 3", "seeds 2 and 4" or "seeds 1, 3 and 5".
fn seeds(seeds: &[u32]) -> String {
    let named: Vec<String> = seeds.iter().map(u32::to_string).collect();
    format!("seed{} {}", if seeds.len() > 1 { "s" } else { "" }, series(&named, None))
}