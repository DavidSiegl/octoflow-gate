//! The flaky-tests check: the Python tests a change adds or edits, rerun with pytest-randomly under several seeds.
//! A test that passes in one order and fails in another is one the build's single green run does not show.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::time::Duration;

use crate::copies::{copy_tracked, finding, in_scratch_directory, sort_by_place};
use crate::junit::{junit_outcomes, Outcome};
use crate::parser_checks::PythonChange;
use crate::pytest::{self, changed_tests, junit_case, labels, listed_tests, node_parts, Group, Setup, Tests};
use crate::report::Finding;
use crate::text::series;
use crate::Unjudged;

/// The pytest-randomly seeds the changed tests are rerun under, once each, in this order.
const SEEDS: [u32; 5] = [1, 2, 3, 4, 5];

/// How long one pytest run of the check may take: the collection, and each rerun.
const RUN_TIMEOUT: Duration = Duration::from_secs(30);

/// The flaky-tests check's findings on `python`, by path and line, a finding on no path or line first; each rule is
/// the check's own, without its name, and each severity "warning".
///
/// Reruns the Python tests the change touched (pytest::changed_tests) once under each of [`SEEDS`] with
/// pytest-randomly, and reports at its def each test whose outcome, failed, xfailed (failed as its xfail mark
/// expects) or passed, differs between seeds, naming the seeds of each (rule `mixed-results`). A test that failed
/// under every seed that judged it, at least two, is not flaky, but is a finding of its own (`failed-every-run`): the
/// build may fail it too, which the failing-tests section lists, or it may need what the copy below lacks, so that
/// the reruns could not judge it. One that xfails under every seed is not reported.
///
/// The tests run in a temporary copy of the files git tracks (copies::copy_tracked), so that what the project's
/// addopts write, such as a coverage report, does not replace the build's own; a test that needs an untracked file,
/// such as a build product, cannot find it there. A run that skips a test, or does not report it, as when the
/// project's addopts stop at the first failure, neither passed nor failed it. A test that is not mixed, which fewer
/// than two runs passed or failed, is reported as one that too few runs judged (`not-rerun`), whether runs skipped it
/// or the reruns stopped early; when no run told anything, the finding about the run that went wrong stands for them
/// all.
///
/// pytest decides which of those defs are tests, so a helper named `test_*` that it does not collect is not rerun.
/// pytest and pytest-randomly must be importable by the judged project's Python (judge::Change::python). A pytest run
/// that cannot start, takes longer than [`RUN_TIMEOUT`], ends in a usage or collection error or writes no readable
/// report ends the reruns of the tests under its configuration (pytest::groups) with a `not-rerun` finding saying
/// so, after what the runs before it showed, and the tests under other configurations are rerun still; a changed
/// test file that cannot be read or parsed, and a copy that cannot be made, are `not-rerun` findings too; a copy
/// that cannot be removed afterwards is a `not-removed` one, beside the results. Each run has [`RUN_TIMEOUT`] of its
/// own, so each configuration the changed tests fall under adds its runs to the check's time.
pub fn flaky_tests(python: &PythonChange) -> Vec<Finding> {
    let (tests, unreadable) = changed_tests(python);
    let not_rerun = |(path, why): (OsString, String)| {
        finding(Some(&path), None, "not-rerun", format!("{why}, so its tests were not rerun."))
    };
    let mut out: Vec<Finding> = unreadable.into_iter().map(not_rerun).collect();
    if !tests.is_empty() {
        out.extend(in_scratch_directory("not-rerun", "to rerun the changed tests in", |tmp| {
            rerun_findings(python, tmp, &tests)
        }));
    }
    sort_by_place(&mut out);
    out
}

/// [`flaky_tests`]' findings on `tests`, rerun in a copy of the tracked files of `python`'s tree that it makes in
/// `tmp`, an empty directory, beside the runs' reports: once for each pytest configuration the changed test files
/// take, so that a group that cannot run leaves the others' findings.
fn rerun_findings(python: &PythonChange, tmp: &Path, tests: &Tests) -> Vec<Finding> {
    let change = python.change;
    let whole = |message: String| vec![finding(None, None, "not-rerun", message)];
    let tree = tmp.join("tree");
    if let Err(e) = copy_tracked(&change.root, &tree) {
        return whole(format!(
            "could not copy the tracked files to rerun the changed tests in: {e}; the changed tests were not rerun."
        ));
    }
    let files: BTreeSet<OsString> = tests.keys().map(|(path, _)| path.clone()).collect();
    let groups = match pytest::groups(&change.python, &tree, tmp, &files) {
        Ok(groups) => groups,
        Err(e) => {
            return whole(format!(
                "could not write an empty pytest configuration for the reruns: {e}; the changed tests were not rerun."
            ));
        }
    };
    let mut out = vec![];
    for (n, (group, label)) in groups.iter().zip(labels(&groups)).enumerate() {
        let reports = tmp.join(format!("reports-{n}"));
        if let Err(e) = std::fs::create_dir(&reports) {
            let which = label.as_deref().unwrap_or("the changed tests");
            let message = format!("could not make a directory for the reruns' reports: {e}; {which} were not rerun.");
            out.push(finding(None, None, "not-rerun", message));
            continue;
        }
        let found = group_rerun_findings(group, &group.own(tests), &reports, label.as_deref());
        out.extend(found.into_iter().map(|f| group.in_tree(f)));
    }
    out
}

/// [`rerun_findings`]' findings on `tests`, keyed by their paths from `group`'s directory, as pytest names them
/// there, rerun by `group`'s configuration, with the runs' reports in `reports`, an empty directory; their paths are
/// from `group`'s directory too. `label` names the group's tests in a finding about all of them (pytest::labels).
fn group_rerun_findings(group: &Group, tests: &Tests, reports: &Path, label: Option<&str>) -> Vec<Finding> {
    let (ids, problem) = match collect(&group.setup, tests) {
        Ok(ids) => (ids, None),
        Err(problem) => (vec![], Some(problem)),
    };
    let reruns = Reruns::run(&group.setup, &ids, reports);
    let mut out = vec![];
    for id in &ids {
        let (path, name, _) = node_parts(id);
        if let (Some((rule, message)), Some(line)) = (reruns.judge(id), tests.get(&(path.clone(), name))) {
            out.push(finding(Some(&path), Some(*line), rule, message));
        }
    }
    if let Some(problem) = problem.or(reruns.stopped.as_ref().map(|(_, problem)| problem.clone())) {
        let judged = if reruns.seeds.is_empty() {
            format!("{} were not rerun.", label.unwrap_or("the changed tests"))
        } else {
            let of = label.map(|label| format!(" of {label}")).unwrap_or_default();
            format!("only the reruns{of} under {} were judged.", seeds(&reruns.seeds))
        };
        out.push(finding(None, None, "not-rerun", format!("{problem}; {judged}")));
    }
    out
}

/// The pytest node ids of `tests`, keyed by their paths from `setup`'s root, one per parameter set and sorted, as
/// `setup` collects them. Errs with the problem, a clause, when pytest could not collect them.
fn collect(setup: &Setup, tests: &Tests) -> Result<Vec<OsString>, String> {
    let doing = "collecting the changed tests";
    let files: BTreeSet<&OsString> = tests.keys().map(|(path, _)| path).collect();
    // Without pytest-randomly, which only shuffles the list, and cannot hash a node id that is not UTF-8.
    let mut args: Vec<OsString> = ["-p", "no:randomly", "--collect-only"].map(OsString::from).into();
    args.extend(files.into_iter().map(|file| setup.argument(file)));
    // Exit status 5: pytest collected no test from the files, as for a file of helpers named test_*.
    let collected = pytest::run(setup, doing, &args, RUN_TIMEOUT, &[0, 5], &[]).map_err(|problem| problem.clause)?;
    listed_tests(&collected, doing, tests)
}

/// The outcome of each of `ids` that pytest reported when it ran them by `setup`, in the order pytest-randomly's
/// `seed` gives, writing its JUnit report to `report`; a test reported more than once takes the first of failed,
/// xfailed and passed that any of its cases has, else skipped. Errs with the problem, a clause, when that run told
/// nothing.
fn rerun<'a>(
    setup: &Setup,
    ids: &'a [OsString],
    seed: u32,
    report: &Path,
) -> Result<HashMap<&'a OsStr, Outcome>, String> {
    let doing = format!("rerunning them under seed {seed}");
    let mut junit = OsString::from("--junitxml=");
    junit.push(report);
    let mut args: Vec<OsString> = vec!["-p".into(), "randomly".into(), format!("--randomly-seed={seed}").into(), junit];
    args.extend(ids.iter().map(|id| setup.argument(id)));
    let ran = pytest::run(setup, &doing, &args, RUN_TIMEOUT, &[0, 1], &[]).map_err(|problem| problem.clause)?;
    if !report.is_file() {
        return Err(format!("pytest, {doing}, wrote no test report: {}", ran.said()));
    }
    let cases = junit_outcomes(report)
        .map_err(|Unjudged(e)| format!("pytest, {doing}, wrote a report the gate cannot read: {e}"))?;
    let by_case: HashMap<String, &OsStr> = ids.iter().map(|id| (junit_case(id), id.as_os_str())).collect();
    let rank = |outcome: Outcome| match outcome {
        Outcome::Skipped => 0,
        Outcome::Passed => 1,
        Outcome::Xfailed => 2,
        Outcome::Failed => 3,
    };
    let mut out: HashMap<&OsStr, Outcome> = HashMap::new();
    for (case, outcome, _) in cases {
        if let Some(id) = by_case.get(&case) {
            let so_far = out.entry(id).or_insert(Outcome::Skipped);
            if rank(outcome) >= rank(*so_far) {
                *so_far = outcome;
            }
        }
    }
    Ok(out)
}

/// What the reruns of a set of tests, one under each of [`SEEDS`] in turn, told of each test.
struct Reruns<'a> {
    /// Each test's outcome, by its node id and then by the seed of each run that reported it failed, xfailed or
    /// passed.
    outcomes: HashMap<&'a OsStr, BTreeMap<u32, Outcome>>,
    /// The seeds of the runs that told something, in the order they ran.
    seeds: Vec<u32>,
    /// The seed whose run went wrong and ended the reruns, and what went wrong with it, a clause; None when every
    /// run told something.
    stopped: Option<(u32, String)>,
}

impl<'a> Reruns<'a> {
    /// Reruns the tests of node ids `ids` by `setup`, once under each of [`SEEDS`], until a run goes wrong, writing
    /// the runs' reports in `reports`. For no ids, no run.
    fn run(setup: &Setup, ids: &'a [OsString], reports: &Path) -> Reruns<'a> {
        let mut reruns = Reruns { outcomes: HashMap::new(), seeds: vec![], stopped: None };
        for seed in SEEDS.into_iter().take(if ids.is_empty() { 0 } else { SEEDS.len() }) {
            match rerun(setup, ids, seed, &reports.join(format!("seed-{seed}.xml"))) {
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

    /// The rule and message of the check's finding on the test of node id `id`, one of those rerun, by what the
    /// reruns told of it; None when they show nothing to report.
    fn judge(&self, id: &OsStr) -> Option<(&'static str, String)> {
        let (_, name, params) = node_parts(id);
        let name = format!("{name}{params}");
        let none = BTreeMap::new();
        let by_seed = self.outcomes.get(id).unwrap_or(&none);
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
                    "`{name}` failed under {}, every rerun{left_out}, which ran in a copy of the tracked files. If \
                     the build failed it too, the failing tests list it. If the build passed it, it needs something \
                     the copy lacks, such as git's metadata, an untracked or ignored file or a build product, so \
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
