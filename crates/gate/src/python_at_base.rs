//! Python's half of the passes-at-base check (passes_at_base.rs): the Python tests a change adds or edits, run with
//! pytest against the code the change started from.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Instant;

use regex::bytes::Regex;

use crate::copies::{copy_base_with_tests, failed, finding, in_scratch_directory, Budget};
use crate::judge::Change;
use crate::junit::{junit_outcomes, Outcome};
use crate::parser_checks::{python_test, PythonChange};
use crate::passes_at_base::budget_reached;
use crate::paths::{real_path, rel_path};
use crate::process::Finished;
use crate::pytest::{self, changed_tests, junit_case, labels, listed_tests, node_parts, Group, Setup, Tests};
use crate::report::Finding;
use crate::text;

/// The module name of the pytest plugin each run at the base loads with `-p`, written as `<name>.py` to a directory
/// on the run's path; the file itself says what it reports, and how.
const IMPORTS_PLUGIN: &str = "_octoflow_gate_base_imports";
const IMPORTS_PLUGIN_SOURCE: &str = include_str!("_octoflow_gate_base_imports.py");

/// What the collections at the base are doing, as their problems name it.
const COLLECTING: &str = "collecting the changed tests at the base";

/// Python's findings of the passes-at-base check on `python`, in no order; each rule is the check's own, without its
/// name, and each severity "warning".
///
/// When the change edits Python code under test (PythonChange::edits_code), runs the Python tests it touched
/// (pytest::changed_tests) against the base's code, and reports at its def each one that passed there (rule
/// `passes-at-base`): it does not test the change. A change to tests alone has no "without the change" to run them
/// in, and one to docs alone none that a test could see.
///
/// The tests run with pytest, by the judged project's Python (judge::Change::python), in a copy of the base
/// (copies::copy_base_with_tests) that holds every file of the tests the change touches ([`beside_tests`]), conftest.py
/// and data included, as the working tree has it. pytest first collects the changed test files there, then runs
/// those of the changed tests it collected. A test passes there when pytest passed each of its cases that it did not
/// skip, all its parameter sets included. One that fails or errors there tests the change, which is what the check
/// expects, and gives nothing; so does one that pytest does not collect there ([`collect`]).
///
/// The base's code is what the copy's import roots, its top and its src/, and its own configuration, such as
/// pytest's pythonpath, import, ahead of what is installed into the project's Python, an editable install of the
/// project included. A run that still imports the project's code from the working tree
/// ([`imported_from_working_tree`]) judges none of its tests and is a `not-run` finding naming the package.
///
/// The collections and runs together, one of each for every configuration the changed tests fall under
/// (pytest::groups), may take `seconds`, counted from when the copy is made; a stop there is a `budget-reached`
/// finding saying when, and none of that configuration's outcomes is reported; the configurations after it are not
/// run, each a `not-run` finding saying so. A pytest that cannot start, stops before it collects, fails to load a
/// conftest.py through no file the change edits, stops with another usage error, or writes no readable report, a
/// changed test file that cannot be read or parsed, submodules the copy leaves empty, and a copy that cannot be made
/// are `not-run` findings saying so; a copy that cannot be removed afterwards is a `not-removed` one.
pub fn python_at_base(python: &PythonChange, seconds: u64) -> Vec<Finding> {
    if !python.edits_code() {
        return vec![];
    }
    let (tests, unreadable) = changed_tests(python);
    let not_run = |(path, why): (OsString, String)| {
        finding(Some(&path), None, "not-run", format!("{why}, so its tests were not run at the base."))
    };
    let mut out: Vec<Finding> = unreadable.into_iter().map(not_run).collect();
    if !tests.is_empty() {
        out.extend(in_scratch_directory("not-run", "to run the changed tests at the base in", |tmp| {
            base_run_findings(python.change, tmp, &tests, seconds)
        }));
    }
    out
}

/// Whether a repository path belongs with Python's tests rather than with the code they test, as the copy the tests
/// run in at the base takes it from the working tree: a Python test file by gate.toml's `test-files.python`, or a
/// file that goes with the tests by its `test-files.support`, such as a conftest.py or a fixture's data.
fn beside_tests(change: &Change, path: &OsStr) -> bool {
    python_test(&change.settings, path) || change.settings.test_support.matches(path)
}

/// The copy of the base that one check's pytest runs share, as [`base_run_findings`] sets it up.
struct BaseCopy<'a> {
    change: &'a Change,
    /// The copy's top, every group's top.
    tree: &'a Path,
    /// The variables each pytest run takes beside process::tool_env's.
    env: &'a [(&'a str, &'a OsStr)],
    /// The time all the collections and runs have, from when the copy was made.
    budget: Budget,
}

/// [`python_at_base`]'s findings on `tests`, collected and then run, within `seconds` for all of it, in a copy of
/// the base that it makes in `tmp`, an empty directory, beside the runs' reports, their TMPDIR and the plugin
/// [`IMPORTS_PLUGIN`] names: once for each pytest configuration the changed test files take there, one after another
/// while the budget lasts.
///
/// The runs import the base's code from the copy's import roots, its top and its src/ when it has one, which are put
/// on PYTHONPATH ahead of what was there: so they come before site-packages, and before what an editable install
/// puts there, which would import the working tree's code.
fn base_run_findings(change: &Change, tmp: &Path, tests: &Tests, seconds: u64) -> Vec<Finding> {
    let whole = |message: String| vec![finding(None, None, "not-run", message)];
    let (tree, scratch, plugins) = (tmp.join("tree"), tmp.join("tmp"), tmp.join("plugins"));
    let copied = copy_base_with_tests(change, &tree, &tmp.join("base.tar"), |path| beside_tests(change, path))
        .and_then(|empty| write_beside_the_copy(&scratch, &plugins).map(|()| empty));
    let empty = match copied {
        Ok(empty) => empty,
        Err(e) => {
            return whole(format!(
                "could not copy the base to run the changed tests in: {e}; the changed tests were not run there."
            ));
        }
    };
    let files: BTreeSet<OsString> = tests.keys().map(|(path, _)| path.clone()).collect();
    let groups = match pytest::groups(&change.python, &tree, tmp, &files) {
        Ok(groups) => groups,
        Err(e) => {
            return whole(format!(
                "could not write an empty pytest configuration for the run at the base: {e}; the changed tests were \
                 not run there."
            ));
        }
    };
    let src = tree.join("src");
    let mut roots = vec![tree.as_os_str()];
    roots.extend(src.is_dir().then_some(src.as_os_str()));
    roots.push(plugins.as_os_str());
    let inherited = std::env::var_os("PYTHONPATH").filter(|inherited| !inherited.is_empty());
    roots.extend(inherited.as_deref());
    let python_path = roots.join(OsStr::new(":"));
    // No bytecode: a test that imports through a tracked link out of the tree would write it there.
    let env = [
        ("TMPDIR", scratch.as_os_str()),
        ("PYTHONDONTWRITEBYTECODE", OsStr::new("1")),
        ("PYTHONPATH", python_path.as_os_str()),
    ];
    let base_copy = BaseCopy { change, tree: &tree, env: &env, budget: Budget::starting_now(seconds) };
    let mut out = vec![];
    if !empty.is_empty() {
        let (noun, pronoun) = if empty.len() == 1 { ("a submodule", "it") } else { ("submodules", "them") };
        let named: Vec<String> = empty.iter().map(|(path, why)| format!("`{path}`, {why}")).collect();
        let message = format!(
            "the copy of the base leaves {noun} empty, so a changed test that imports from {pronoun}, if any does, \
             was not judged: {}.",
            named.join("; ")
        );
        out.push(finding(None, None, "not-run", message));
    }
    for (n, (group, label)) in groups.iter().zip(labels(&groups)).enumerate() {
        let reports = tmp.join(format!("base-{n}"));
        let (found, stopped) = group_findings(&base_copy, group, &group.own(tests), &reports, label.as_deref());
        out.extend(found.into_iter().map(|f| group.in_tree(f)));
        if stopped {
            out.extend(groups[n + 1..].iter().map(|later| {
                let message = format!(
                    "the run at the base used its {seconds} seconds (`--base-run-budget`) before it reached {}, so \
                     they were not run there.",
                    later.label()
                );
                finding(None, None, "not-run", message)
            }));
            break;
        }
    }
    out
}

/// Makes what the runs need beside the copy: `scratch`, their TMPDIR, and `plugins`, a directory that holds the
/// plugin [`IMPORTS_PLUGIN`] names; neither exists yet. Errs, a clause, when one cannot be made or written.
fn write_beside_the_copy(scratch: &Path, plugins: &Path) -> Result<(), String> {
    for dir in [scratch, plugins] {
        std::fs::create_dir(dir).map_err(|e| failed("make", dir, &e))?;
    }
    let plugin = plugins.join(format!("{IMPORTS_PLUGIN}.py"));
    let written = std::fs::File::create_new(&plugin).and_then(|mut f| f.write_all(IMPORTS_PLUGIN_SOURCE.as_bytes()));
    written.map_err(|e| failed("write", &plugin, &e))
}

/// [`base_run_findings`]' findings on `tests`, keyed by their paths from `group`'s directory, as pytest names them
/// there, collected and run in `base_copy` by `group`'s configuration, writing the run's reports to paths that
/// start with `reports`; their paths are from `group`'s directory too. And whether `base_copy`'s budget stopped it,
/// which leaves no time for another group. `label` names the group's tests in a finding about all of them
/// (pytest::labels). When the run imported the project's code from the working tree
/// ([`imported_from_working_tree`]), no test is judged, and one `not-run` finding names it.
fn group_findings(
    base_copy: &BaseCopy,
    group: &Group,
    tests: &Tests,
    reports: &Path,
    label: Option<&str>,
) -> (Vec<Finding>, bool) {
    let which = label.unwrap_or("the changed tests");
    let reached = |when: String| (vec![budget_reached(&base_copy.budget, "pytest", &when)], true);
    let not_run = |message: String| (vec![finding(None, None, "not-run", message)], false);
    let ids = match collect(base_copy, &group.setup, tests) {
        Ok(ids) => ids,
        Err(Uncollected::Stopped) => return reached(format!("while it collected {which}")),
        Err(Uncollected::Problem(problem)) => return not_run(format!("{problem}; {which} were not run there.")),
    };
    if ids.is_empty() {
        return (vec![], false);
    }
    let left = base_copy.budget.deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return reached(format!("once it had collected {which}, before it ran them"));
    }
    let doing = "running the changed tests at the base";
    let with_suffix = |suffix: &str| {
        let mut path = reports.as_os_str().to_os_string();
        path.push(suffix);
        path
    };
    let (report, imports) = (with_suffix(".xml"), with_suffix("-imports.json"));
    let option = |name: &str, set_to: &OsStr| {
        let mut option = OsString::from(name);
        option.push(set_to);
        option
    };
    let mut args: Vec<OsString> = ["-p", "no:randomly", "-p", IMPORTS_PLUGIN].map(OsString::from).into();
    args.push(option("--gate-imports-root=", base_copy.change.root.as_os_str()));
    args.push(option("--gate-imports-report=", &imports));
    args.push(option("--junitxml=", &report));
    args.extend(ids.iter().map(|id| group.setup.argument(id)));
    let ran = match pytest::run(&group.setup, doing, &args, left, &[0, 1], base_copy.env) {
        Ok(ran) => ran,
        Err(pytest::Problem { stopped: Some(printed), .. }) => {
            let of = label.map(|label| format!(" of {label}")).unwrap_or_default();
            return reached(format!(
                "after {} of the {} test cases{of} it was given, before it reported them",
                cases_run(&printed.stdout),
                ids.len()
            ));
        }
        Err(problem) => return not_run(format!("{}; {which} were not judged.", problem.clause)),
    };
    let read = junit_outcomes(Path::new(&report))
        .map_err(|unjudged| unjudged.0)
        .and_then(|cases| Ok((cases, imported_from_working_tree(base_copy, Path::new(&imports))?)));
    let (cases, outside) = match read {
        Ok(read) => read,
        Err(e) => {
            return not_run(format!("pytest, {doing}, wrote no report the gate can read ({e}): {}", ran.said()));
        }
    };
    if !outside.is_empty() {
        let named: Vec<String> = outside.iter().map(|(name, path)| format!("`{name}` (`{path}`)")).collect();
        return not_run(format!(
            "{which} imported {} from the working tree, not from the copy of the base, as from a project installed \
             into the gate's interpreter, so they were not judged.",
            text::series(&named, Some(5))
        ));
    }
    let passed = passed_tests(&cases, &ids).into_iter().filter_map(|(path, name)| {
        let line = tests.get(&(path.clone(), name.clone()))?;
        let message = format!("`{name}` passes without the change, so it does not test it.");
        Some(finding(Some(&path), Some(*line), "passes-at-base", message))
    });
    (passed.collect(), false)
}

/// How many test cases a pytest run that was stopped had run, by the progress lines of `stdout`, what it printed
/// until then at verbosity -1: a character for each case, then how far along that is.
fn cases_run(stdout: &[u8]) -> usize {
    static PROGRESS: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^([.FEsxX]+) *(?:\[ *\d+%\])?$").expect("a valid pattern"));
    PROGRESS.captures_iter(stdout).map(|line| line[1].len()).sum()
}

/// The tests, as (path, qualified name) and sorted, of which `cases`, a run's outcomes as junit::junit_outcomes
/// gives them, passed every case of `ids`, the node ids the run was given, that it did not skip; a test whose cases
/// were all skipped is not one.
fn passed_tests(cases: &[(String, Outcome, String)], ids: &[OsString]) -> Vec<(OsString, String)> {
    let by_case: BTreeMap<String, &OsString> = ids.iter().map(|id| (junit_case(id), id)).collect();
    // Whether every case of a test that was not skipped passed.
    let mut passed: BTreeMap<(OsString, String), bool> = BTreeMap::new();
    for (case, outcome, _) in cases {
        if let Some(id) = by_case.get(case).filter(|_| *outcome != Outcome::Skipped) {
            let (path, name, _) = node_parts(id);
            *passed.entry((path, name)).or_insert(true) &= *outcome == Outcome::Passed;
        }
    }
    passed.into_iter().filter(|(_, passed)| *passed).map(|(test, _)| test).collect()
}

/// The top-level modules under which a run at the base imported a module from the working tree's files of the
/// project rather than from the copy, whichever of the changed tests, their conftest.py files or their helpers
/// imported it, as the reports the plugin [`IMPORTS_PLUGIN`] names wrote at `imports`, and beside it for
/// pytest-xdist's workers, tell them: each by its name and the path, from the working tree's top, of the file of the
/// first such module in name order, itself or one below it, sorted. The project's files are those git tracks at the
/// base, which the copy holds, and those the change adds; others under the top, such as a virtual environment's, are
/// not the project's.
///
/// Errs, naming the report, when one cannot be read or is no JSON object. A report that names a file whose path is
/// not UTF-8 is one: Python writes such a path as lone surrogates, which serde_json does not read
/// (docs/port-differences.md).
fn imported_from_working_tree(base_copy: &BaseCopy, imports: &Path) -> Result<Vec<(String, String)>, String> {
    let folder = imports.parent().unwrap_or(Path::new(""));
    let mut prefix = imports.file_name().unwrap_or_default().to_os_string();
    prefix.push("-");
    let listed = std::fs::read_dir(folder).and_then(|entries| entries.collect::<std::io::Result<Vec<_>>>());
    let listed = listed.map_err(|e| format!("{}: {e}", text::path(folder.as_os_str())))?;
    let mut reports: Vec<_> = listed.into_iter().map(|entry| entry.path()).collect();
    reports.retain(|path| path.file_name().is_some_and(|name| name.as_bytes().starts_with(prefix.as_bytes())));
    reports.sort();
    reports.insert(0, imports.to_path_buf());
    // Each module a report names, and its file; a later report's entry replaces an earlier one's.
    let mut located: BTreeMap<String, serde_json::Value> = BTreeMap::new();
    for report in reports {
        let unreadable = |why: String| format!("{}: {why}", text::path(report.as_os_str()));
        let read = std::fs::read(&report).map_err(|e| unreadable(e.to_string()))?;
        match serde_json::from_slice(&read).map_err(|e| unreadable(e.to_string()))? {
            serde_json::Value::Object(part) => located.extend(part),
            _ => return Err(unreadable("not a JSON object".into())),
        }
    }
    let added: BTreeSet<&OsStr> = base_copy.change.files.iter().filter_map(|f| f.after.as_deref()).collect();
    let mut out: BTreeMap<String, String> = BTreeMap::new();
    for (name, path) in &located {
        let Some(path) = path.as_str() else { continue };
        if base_copy.tree.join(path).symlink_metadata().is_ok() || added.contains(OsStr::new(path)) {
            let top_level = name.split('.').next().unwrap_or(name);
            out.entry(top_level.to_string()).or_insert_with(|| path.to_string());
        }
    }
    Ok(out.into_iter().collect())
}

/// Why [`collect`] gave no node ids.
enum Uncollected {
    /// The budget's deadline stopped the collection.
    Stopped,
    /// It went wrong otherwise: what did, as a clause a finding can open with.
    Problem(String),
}

/// The node ids, one per parameter set, of those of `tests`, keyed by their paths from `setup`'s root, that pytest
/// collects by `setup` in `base_copy`, with its env, by its budget's deadline.
///
/// A test file that cannot be imported at the base gives no ids for the tests it covers and no problem: that is the
/// expected outcome for a test of what the change adds. So does a test pytest does not collect, such as a `test_*`
/// method of a class not named `Test*`, and each test under a conftest.py that fails to load there, whatever it
/// raises, through a file the change edits: the conftest.py itself, a helper beside the tests that it imports, which
/// the copy holds as the working tree has it, or source the copy holds as the base has it. Without the change's edit
/// of that file, the conftest.py fails, so the failure is the change's. One that fails through no file the change
/// edits is a problem: the base itself fails there. So is a pytest that stops before it collects, as for a plugin
/// that cannot be imported.
fn collect(base_copy: &BaseCopy, setup: &Setup, tests: &Tests) -> Result<Vec<OsString>, Uncollected> {
    /// pytest's summary of a collection that got as far as collecting.
    static COLLECTED: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m)^(?:no tests|\d+ tests?) collected\b").expect("a valid pattern"));
    let mut files: Vec<OsString> = tests.keys().map(|(path, _)| path.clone()).collect();
    files.dedup();
    // pytest loads the conftest.py files above every path it is given before it collects any, so one that fails stops
    // them all: the files under it are left out, and the others collected again, until none fails. Each round leaves
    // out at least the file that led pytest to the conftest.py.
    while !files.is_empty() {
        let mut args: Vec<OsString> =
            ["-p", "no:randomly", "--collect-only", "--continue-on-collection-errors"].map(OsString::from).into();
        args.extend(files.iter().map(|file| setup.argument(file)));
        let left = base_copy.budget.deadline.saturating_duration_since(Instant::now());
        // Exit status 1: some file could not be collected, which --continue-on-collection-errors lets the others
        // outlive; 4, a usage error, which a conftest.py that fails to load gives; 5: no test was collected.
        let collected = match pytest::run(setup, COLLECTING, &args, left, &[0, 1, 4, 5], base_copy.env) {
            Ok(collected) => collected,
            Err(pytest::Problem { stopped: Some(_), .. }) => return Err(Uncollected::Stopped),
            Err(problem) => return Err(Uncollected::Problem(problem.clause)),
        };
        // pytest exits 1 too when it stops before collecting, as for a plugin a conftest.py names that cannot be
        // imported; then it prints a traceback and no summary of what it collected.
        if collected.status == 1 && !COLLECTED.is_match(&collected.stdout) {
            let said = collected.said();
            let problem = format!("pytest stopped before it collected the changed tests at the base: {said}");
            return Err(Uncollected::Problem(problem));
        }
        if collected.status != 4 {
            return listed_tests(&collected, COLLECTING, tests).map_err(Uncollected::Problem);
        }
        files = outside_failed_conftest(base_copy.change, setup, &collected, files).map_err(Uncollected::Problem)?;
    }
    Ok(vec![])
}

/// Those of `files`, test files by their paths from `setup`'s root, that are not under the conftest.py that stopped
/// `collected`, their collection by [`collect`] that exited with status 4, when it failed through a file `change`
/// edits ([`failing_conftest`]); at least one of `files` is under it. Errs with the problem, a clause, otherwise: the
/// conftest.py failed through no file the change edits, so the base itself fails there, it lies above none of
/// `files`, or `collected` names none.
fn outside_failed_conftest(
    change: &Change,
    setup: &Setup,
    collected: &Finished,
    files: Vec<OsString>,
) -> Result<Vec<OsString>, String> {
    let Some(failed) = failing_conftest(collected, &setup.top) else {
        return Err(format!("pytest, {COLLECTING}, exited with status 4: {}", collected.said()));
    };
    let named = text::path(&failed.conftest);
    if !failed.through.iter().any(|path| change.changed.contains_key(path)) {
        return Err(format!(
            "pytest, {COLLECTING}, could not load `{named}`, which fails through no file the change edits, so the \
             base fails there: {}",
            failed.error
        ));
    }
    // One at the top, whose directory is "", covers every file.
    let mut under = Path::new(&failed.conftest).parent().unwrap_or(Path::new("")).as_os_str().as_bytes().to_vec();
    let before = files.len();
    let left: Vec<OsString> = if under.is_empty() {
        vec![]
    } else {
        under.push(b'/');
        files.into_iter().filter(|path| !setup.argument(path).as_bytes().starts_with(&under)).collect()
    };
    if left.len() == before {
        return Err(format!(
            "pytest, {COLLECTING}, could not load `{named}`, above none of the changed tests: {}",
            failed.error
        ));
    }
    Ok(left)
}

/// A conftest.py that failed as pytest loaded it, as [`failing_conftest`] reads it from what pytest printed. Its
/// paths are from the copy's top, each with its directory resolved, but not the file itself: a conftest.py that is a
/// link is named by the link.
struct FailedConftest {
    conftest: OsString,
    /// The exception, as pytest's first line marked "E"; the line that names the conftest.py when there is none. Cut
    /// as text::clip cuts.
    error: String,
    /// The files the traceback passes through, the conftest.py among them.
    through: BTreeSet<OsString>,
}

/// The conftest.py that stopped `collected`, a pytest run in `top` that exited with status 4, as it loaded, whatever
/// it raised: as pytest 9 words that (`print_conftest_import_error` in its `_pytest/config/__init__.py`),
/// "ImportError while loading conftest '<path>'." for any exception, then the traceback, then the exception on lines
/// marked "E". None when `collected` names no such file.
fn failing_conftest(collected: &Finished, top: &Path) -> Option<FailedConftest> {
    // Bytes, not characters: a path pytest prints need not be UTF-8.
    static LOADING: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?m-u)^ImportError while loading conftest '(.*)'\.$").expect("a valid pattern")
    });
    static RAISED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m-u)^E\s+(\S.*)$").expect("a valid pattern"));
    static FRAME: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m-u)^(\S.*?):\d+: in ").expect("a valid pattern"));
    let said = [collected.stdout.as_slice(), collected.stderr.as_slice()].concat();
    let found = LOADING.captures(&said)?;
    let (line, after) = (found.get(0)?, found.get(0)?.end());
    let raised = RAISED.captures_at(&said, after);
    let raised_at = raised.as_ref().and_then(|raised| raised.get(0)).map_or(said.len(), |all| all.start());
    let traceback = &said[after..raised_at];
    let conftest = path_from(top, OsStr::from_bytes(&found[1]));
    let mut through: BTreeSet<OsString> =
        FRAME.captures_iter(traceback).map(|frame| path_from(top, OsStr::from_bytes(&frame[1]))).collect();
    through.insert(conftest.clone());
    let error = match &raised {
        Some(raised) => text::lossless(&raised[1]),
        None => text::lossless(line.as_bytes()),
    };
    Some(FailedConftest { conftest, error: text::clip(text::py_strip(&error)), through })
}

/// `path`, absolute or from `top`, as a repository path from `top`: its directory resolved, as `top` is, and its
/// last part kept as it is.
fn path_from(top: &Path, path: &OsStr) -> OsString {
    let path = Path::new(path);
    let folder = real_path(&top.join(path.parent().unwrap_or(Path::new(""))));
    let named = folder.join(path.file_name().unwrap_or_default());
    OsString::from_vec(rel_path(&named, &real_path(top)).into_os_string().into_vec())
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::diff::ChangedFile;
    use crate::scratch::ScratchDir;
    use crate::settings::Settings;

    /// Writes `contents` to `path`, with the directories above it.
    fn write(path: &Path, contents: &str) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// A pytest run that exited with status 4, having printed `stdout`.
    fn printed(stdout: &str) -> Finished {
        Finished { status: 4, stdout: stdout.into(), stderr: vec![], stopped: false }
    }

    #[test]
    fn the_cases_a_stopped_run_had_run_are_counted_from_its_progress_lines() {
        assert_eq!(cases_run(b""), 0);
        assert_eq!(cases_run(b"."), 1);
        assert_eq!(cases_run(b"..F                [ 60%]\nERROR tests/t.py - boom\nsE [100%]\nxX"), 7);
    }

    #[test]
    fn a_test_passed_when_each_of_its_cases_that_was_not_skipped_did() {
        let named = ["t.py::test_a[1]", "t.py::test_a[2]", "t.py::test_b[1]", "t.py::test_b[2]", "t.py::test_c"];
        let mut ids: Vec<OsString> = named.map(OsString::from).into();
        ids.push("t.py::TestX::test_d".into());
        let case = |id: &str, outcome: Outcome| (junit_case(OsStr::new(id)), outcome, String::new());
        let cases = [
            case("t.py::test_a[1]", Outcome::Passed),
            case("t.py::test_a[2]", Outcome::Skipped),
            case("t.py::test_b[1]", Outcome::Passed),
            case("t.py::test_b[2]", Outcome::Failed),
            case("t.py::test_c", Outcome::Skipped),
            case("t.py::TestX::test_d", Outcome::Passed),
            // Not one of the ids the run was given.
            case("t.py::test_e", Outcome::Passed),
        ];
        let passed = passed_tests(&cases, &ids);
        assert_eq!(passed, [("t.py".into(), "TestX.test_d".to_string()), ("t.py".into(), "test_a".to_string())]);
        // An expected failure is no pass.
        assert_eq!(passed_tests(&[case("t.py::test_c", Outcome::Xfailed)], &ids), []);
    }

    #[test]
    fn where_the_run_at_the_base_imported_from_is_read_from_its_reports() {
        let dir = ScratchDir::new();
        let tree = dir.join("copy");
        write(&tree.join("lib/calc.py"), "");
        let change = Change {
            root: dir.join("repo"),
            base: "b".into(),
            diff: vec![],
            changed: [].into(),
            files: vec![ChangedFile { before: None, after: Some("new/sub.py".into()) }],
            mutmut_state: vec![],
            settings: Settings::default(),
            python: PathBuf::from("python3"),
        };
        let base_copy = BaseCopy { change: &change, tree: &tree, env: &[], budget: Budget::starting_now(1) };
        let report = dir.join("reports/base-0-imports.json");
        let worker = dir.join("reports/base-0-imports.json-gw0");
        let named = |name: &str, path: &str| (name.to_string(), path.to_string());
        // Tracked at the base, as the copy shows, or added by the change: the project's, named by its top-level
        // module; a virtual environment's under the top is not. A pytest-xdist worker's report beside the
        // controller's counts too.
        write(&report, r#"{"calc": "lib/calc.py", "new.sub": "new/sub.py", "pytest": ".venv/lib/pytest/__init__.py"}"#);
        write(&worker, r#"{"w": "lib/calc.py"}"#);
        assert_eq!(
            imported_from_working_tree(&base_copy, &report).unwrap(),
            [named("calc", "lib/calc.py"), named("new", "new/sub.py"), named("w", "lib/calc.py")]
        );
        // A report that cannot be read is named, the controller's or a worker's.
        for bad in [&report, &worker] {
            for text in ["[]", "{"] {
                write(&report, "{}");
                write(&worker, "{}");
                write(bad, text);
                let why = imported_from_working_tree(&base_copy, &report).unwrap_err();
                assert!(why.starts_with(&format!("{}: ", bad.display())), "{why}");
            }
        }
        std::fs::remove_file(&report).unwrap();
        let why = imported_from_working_tree(&base_copy, &report).unwrap_err();
        assert!(why.starts_with(&format!("{}: ", report.display())), "{why}");
    }

    #[test]
    fn a_conftest_that_failed_is_read_from_what_pytest_printed() {
        let top = ScratchDir::new();
        write(&top.join("tests/conftest.py"), "");
        let through = |failed: &FailedConftest| -> Vec<String> {
            failed.through.iter().map(|path| path.to_str().unwrap().to_string()).collect()
        };
        // pytest names the conftest.py by its absolute path, and the files of the traceback from where it runs.
        let said = format!(
            "ImportError while loading conftest '{}/tests/conftest.py'.\ntests/conftest.py:1: in <module>\n    \
             import helpers\ntests/helpers.py:3: in <module>\n    TWO = calc.TWO\n\
             E   AttributeError: module 'calc' has no attribute 'TWO'\nE   more\nother.py:1: in later\n",
            top.display()
        );
        let failed = failing_conftest(&printed(&said), &top).unwrap();
        assert_eq!(failed.conftest, "tests/conftest.py");
        assert_eq!(failed.error, "AttributeError: module 'calc' has no attribute 'TWO'");
        assert_eq!(through(&failed), ["tests/conftest.py", "tests/helpers.py"]);
        // Without a line marked "E", the line that names the conftest.py says what there is to say.
        let failed = failing_conftest(&printed("ImportError while loading conftest 'conftest.py'.\n"), &top).unwrap();
        assert_eq!(failed.conftest, "conftest.py");
        assert_eq!(failed.error, "ImportError while loading conftest 'conftest.py'.");
        assert_eq!(through(&failed), ["conftest.py"]);
        assert!(failing_conftest(&printed("ERROR: usage: no such option\n"), &top).is_none());
    }
}
