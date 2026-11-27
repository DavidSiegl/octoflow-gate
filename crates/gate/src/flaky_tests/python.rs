//! flaky-tests' Python half: the changed tests rerun with pytest under pytest-randomly's seeds.

use std::collections::{BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::path::Path;

use crate::copies::{finding, OnCopy, ScratchCopy};
use crate::junit::{junit_outcomes, Outcome};
use crate::pytest::{self, changed_tests, junit_case, labels, listed_tests, node_parts, Group, Setup, Tests};
use crate::report::Finding;
use crate::sources::PythonChange;
use crate::Unjudged;

use super::{stopped_not_rerun, Reruns, RUN_TIMEOUT};

/// The check's findings on `python`, in no order: those on its changed test files that cannot be read, and the run on
/// the copy that gives the others, None when there is no test to rerun.
///
/// Reruns the Python tests the change touched (pytest::changed_tests) once under each of [`SEEDS`](super::SEEDS) with
/// pytest-randomly, and reports at its def each test whose outcome, failed, xfailed (failed as its xfail mark expects)
/// or passed, differs between seeds, naming the seeds of each (rule `mixed-results`). A test that failed under every
/// seed that judged it, at least two, is not flaky, but is a finding of its own (`failed-every-run`): the build may
/// fail it too, which the failing-tests section lists, or it may need what the copy below lacks, so that the reruns
/// could not judge it. One that xfails under every seed is not reported.
///
/// A run that skips a test, or does not report it, as when the project's addopts stop at the first failure, neither
/// passed nor failed it. A test that is not mixed, which fewer than two runs passed or failed, is reported as one that
/// too few runs judged (`not-rerun`), whether runs skipped it or the reruns stopped early; when no run told anything,
/// the finding about the run that went wrong stands for them all.
///
/// pytest decides which of those defs are tests, so a helper named `test_*` that it does not collect is not rerun.
/// pytest and pytest-randomly must be importable by the judged project's Python (change::Change::python). A pytest run
/// that cannot start, takes longer than [`RUN_TIMEOUT`], ends in a usage or collection error or writes no readable
/// report ends the reruns of the tests under its configuration (pytest::groups) with a `not-rerun` finding saying
/// so, after what the runs before it showed, and the tests under other configurations are rerun still; a changed
/// test file that cannot be read or parsed is a `not-rerun` finding too. Each run has [`RUN_TIMEOUT`] of its own, so
/// each configuration the changed tests fall under adds its runs to the check's time.
pub(super) fn python_findings<'a>(python: &'a PythonChange) -> (Vec<Finding>, Option<OnCopy<'a>>) {
    let (tests, unreadable) = changed_tests(python);
    let not_rerun = |(path, why): (OsString, String)| {
        finding(Some(&path), None, "not-rerun", format!("{why}, so its tests were not rerun."))
    };
    let out = unreadable.into_iter().map(not_rerun).collect();
    let run = (!tests.is_empty()).then(|| -> OnCopy { Box::new(move |copy| rerun_findings(python, copy, &tests)) });
    (out, run)
}

/// [`python_findings`]' findings on `tests`, rerun in `copy`, with the runs' reports in its scratch directory: once
/// for each pytest configuration the changed test files take, so that a group that cannot run leaves the others'
/// findings.
fn rerun_findings(python: &PythonChange, copy: &ScratchCopy, tests: &Tests) -> Vec<Finding> {
    let change = python.change;
    let whole = |message: String| vec![finding(None, None, "not-rerun", message)];
    let (tree, tmp) = (&copy.tree, &copy.scratch);
    let files: BTreeSet<OsString> = tests.keys().map(|(path, _)| path.clone()).collect();
    let groups = match pytest::groups(&change.python, tree, tmp, &files) {
        Ok(groups) => groups,
        Err(e) => {
            return whole(format!(
                "could not write an empty pytest configuration for the reruns: {e}; the changed tests were not rerun."
            ));
        }
    };
    let mut out = vec![];
    for (n, (group, label)) in groups.iter().zip(labels(&groups)).enumerate() {
        let reports = tmp.join(format!("pytest-reports-{n}"));
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
    let reruns = if ids.is_empty() {
        Reruns::default()
    } else {
        Reruns::run(|seed| rerun(&group.setup, &ids, seed, &reports.join(format!("seed-{seed}.xml"))))
    };
    let mut out = vec![];
    for id in &ids {
        let (path, name, params) = node_parts(id);
        let judged = reruns.judge(&id.as_os_str(), &format!("{name}{params}"));
        if let (Some((rule, message)), Some(line)) = (judged, tests.get(&(path.clone(), name))) {
            out.push(finding(Some(&path), Some(*line), rule, message));
        }
    }
    if let Some(problem) = problem.or(reruns.stopped.as_ref().map(|(_, problem)| problem.clone())) {
        out.push(stopped_not_rerun(&problem, label, &reruns.seeds));
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
