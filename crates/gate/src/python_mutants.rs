//! Python's half of the surviving-mutants check (mutants.rs): mutmut mutates the functions a change touches in
//! Python code under test, and a function whose mutants its tests do not notice is a finding.
//!
//! mutmut 3 (3.8.0 here) is bounded to the functions the change edits like this. Its own selector, `mutmut run` with
//! mutant names, only picks which mutants run: it first makes a mutant of every function in its source_paths, which for
//! a large module takes minutes. So mutmut runs on a copy of the tree, and in the copy every function and class of the
//! files mutmut mutates that holds no changed function gets [`SKIP`] at the end of the line its header ends on; files
//! it does not mutate are left as they are. mutmut skips such a function whole: no mutants, and none of the call
//! tracing that slows the tests. Appending a comment keeps every line where it was, and mutmut writes its state,
//! mutants/, into the copy, so the working tree is never written to.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::{Duration, Instant};

use lang_python::Unit;
use regex::Regex;
use serde_json::{json, Value};
use toml::Table;

use crate::copies::{copy_tracked, finding, in_scratch_directory};
use crate::glob::fnmatch;
use crate::change::MUTMUT_STATE;
use crate::mutants::{KILL_WAIT, STOP_GRACE, SURVIVOR_EXAMPLES};
use crate::parser_checks::{python_test, regular_files_at_base, PythonChange};
use crate::paths::{normpath, real_path};
use crate::process::{self, run_with, Extras, Finished};
use crate::pytest::{self, truthy, Setup};
use crate::report::Finding;
use crate::text;

/// What the end of a header's line gets for mutmut to skip the def or class whole.
const SKIP: &[u8] = b"  # pragma: no mutate block";

/// How `mutmut results` words a mutant it did not try against a test: one the budget stopped it before, and one of a
/// function no test calls.
const UNTRIED: [&str; 3] = ["not checked", "check was interrupted by user", "no tests"];

/// The seconds listing mutmut's results and showing what the survivors change get in all, each call's stop
/// included.
const REPORT_TIMEOUT: u64 = 60;

/// The seconds reading which tests `--test-contexts` records gets, and pytest's collection of mutmut's test
/// selection.
const TEST_CONTEXTS_TIMEOUT: u64 = 30;
const TEST_COLLECTION_TIMEOUT: u64 = 30;

/// A data file in which no line is recorded for at least one in this many of the tests it records is taken for one a
/// coverage core wrote that records a line only for the first few tests to run it. Under Python 3.14, coverage
/// 7.14's default core, sys.monitoring, does that under `--cov-context=test` without a warning. Tests left out that
/// way could kill mutants that are then reported as survivors. A file recording fewer than this many tests is never
/// taken for an under-recorded one.
const UNDER_RECORDED_SHARE: u64 = 10;

/// The program [`recorded_tests`] runs in the project's Python, as the gate runs mutmut, so that it reads a
/// coverage.py data file through coverage's API rather than its SQLite schema; the file itself says what it takes
/// and prints.
const TEST_CONTEXTS_PROGRAM: &str = include_str!("_test_contexts.py");

/// What the collection that narrows mutmut's tests is doing, as its problems name it.
const COLLECTING: &str = "collecting [tool.mutmut]'s test selection";

/// Why mutmut made no mutant of a function it was given, when the gate cannot tell which cause it was.
const EVERY_CAUSE: &str = "either it has nothing mutmut mutates, the rules' do_not_mutate_patterns or \
                           mutate_only_covered_lines leave its lines out, a `# pragma: no mutate` keeps them out, or \
                           the gate could not read mutmut's names for its mutants";

/// How mutmut is set up for a change, beyond the change itself.
#[derive(Clone, Copy, Debug)]
pub struct Mutmut<'a> {
    /// The pyproject.toml whose `[tool.mutmut]` mutmut runs by, whatever the tree's own says. Absolute. None for no
    /// rules, under which mutmut mutates nothing.
    pub rules: Option<&'a Path>,
    /// `--test-contexts`: a coverage.py data file of the tree's test run with a context per test, as
    /// `pytest --cov-context=test` writes it, by which mutmut's tests are narrowed. Absolute.
    pub test_contexts: Option<&'a Path>,
}

/// Python's findings of the surviving-mutants check on `python`, in no order; each rule is the check's own, without
/// its name, and each severity "warning".
///
/// Mutation-tests with mutmut each top-level function and method of a top-level class (lang_python::Unit) that the
/// change adds lines to, decorators included, or removes lines from, in a Python file outside the tests that it leaves
/// a regular file in the working tree. Lines removed right after a function's last line count too, though they may have
/// been the blank lines below it; comments and blank lines above its first decorator do not, since they change nothing
/// mutmut mutates. Nor does a function whose file's base has one of its name with the same code, its decorators,
/// parameters, defaults, annotations and body, whatever its comments and layout, in a file that holds no mutmut pragma
/// on either side ([`edited`]): an edit to those alone changes nothing mutmut mutates, while one to its signature alone
/// can change what its body does, and one to a pragma which of its lines mutmut mutates. Reports at its def each such
/// function with mutants its tests did not notice (`survived`), with how many of the mutants tried they were and what the first
/// mutants::SURVIVOR_EXAMPLES change, as a line before and after; and each one no test calls (`untested`).
///
/// mutmut runs by the project's Python (change::Change::python) on a copy of the files git tracks, without what the
/// tree tracks under mutmut's state, so it starts without a cache. It runs there by the `[tool.mutmut]` of
/// `mutmut.rules`, whatever the copy's says: its source_paths, only_mutate and do_not_mutate decide which files can
/// be mutated, and a changed function outside them, or one mutmut never mutates, is a `not-mutated` finding saying
/// why; its test selection says which tests run. mutmut may run `seconds`; a run stopped there is a
/// `budget-reached` finding that says how many of the mutants it tried, beside what those showed. Listing the
/// results then takes at most [`REPORT_TIMEOUT`] seconds more.
///
/// With `mutmut.test_contexts`, mutmut runs only the tests of its selection, as pytest collects it in the copy with
/// mutmut's options, that the file records running a line of a mutated function's body ([`recorded_tests`]), which
/// spares it two runs of the whole selection before its first mutant. When there is no such test, no mutmut starts,
/// and each mutated function is an `untested` finding. A file unfit to tell them, or a collection that fails, is a
/// `not-narrowed` finding saying why, and mutmut runs the whole selection.
///
/// mutmut's pytest runs take the configuration pytest finds for the tests they run (pytest::groups), which must be
/// one: tests under more than one are a `not-run` finding naming them. A mutmut that cannot start, fails, or whose
/// results cannot be listed, rules that name no source_paths or no rules at all, a changed file that cannot be read
/// or parsed, and a copy that cannot be made are `not-run` findings too, saying so; a copy that cannot be removed
/// afterwards is a `not-removed` one.
pub fn python_mutants(python: &PythonChange, mutmut: &Mutmut, seconds: u64) -> Vec<Finding> {
    let in_scope = |path: &OsStr| !python_test(&python.change.settings, path);
    let (units, unreadable) = python.changed_units(in_scope, |unit, added, removal_points| {
        added.iter().any(|&n| unit.lines.first <= n && n <= unit.lines.last)
            || removal_points.iter().any(|&n| unit.lines.first <= n + 1 && n <= unit.lines.last)
    });
    let not_read = |(path, why): (OsString, String)| {
        finding(Some(&path), None, "not-run", format!("{why}, so its functions were not mutation-tested."))
    };
    let mut out: Vec<Finding> = unreadable.into_iter().map(not_read).collect();
    let functions = edited(python, units);
    if !functions.is_empty() {
        out.extend(mutation_test(python, &functions, mutmut, seconds));
    }
    out
}

/// Those of `units`, each with its file's path as PythonChange::changed_units gives them, whose code the change
/// edited, in the same order: all but a unit whose file's base has a unit of its name with the same code
/// (lang_python::Unit::code), which only the unit's comments and layout can have changed, as nothing mutmut mutates
/// does. Every unit of a file counts as edited where the file holds a mutmut pragma ([`mutmut_pragma`]) at the base or
/// in the working tree, since a pragma is a comment that decides which lines mutmut mutates, by where it sits; and
/// where its base is no regular file, cannot be read or parsed, or is none.
fn edited(python: &PythonChange, units: Vec<(OsString, Unit)>) -> Vec<Function> {
    // The base path of each file the units are in that has one.
    let mut befores: HashMap<OsString, OsString> = HashMap::new();
    for (path, _) in &units {
        let file = python.files.iter().find(|f| f.after.as_ref().is_some_and(|after| &after.path == path));
        if let Some(before) = file.and_then(|f| f.before.as_ref()) {
            befores.insert(path.clone(), before.path.clone());
        }
    }
    let wanted: Vec<OsString> = befores.values().cloned().collect();
    let sources = regular_files_at_base(python.change, &wanted).unwrap_or_default();
    // The units to compare a file's with: its base's, or None where each of its units counts as edited.
    let comparable = |path: &OsStr| -> Option<Vec<Unit>> {
        let base = sources.get(befores.get(path)?)?;
        let now = std::fs::read(python.change.root.join(path)).ok()?;
        if holds_pragma(base) || holds_pragma(&now) {
            return None;
        }
        lang_python::units(base).ok()
    };
    let mut bases: HashMap<OsString, Option<Vec<Unit>>> = HashMap::new();
    let mut out = vec![];
    for (path, unit) in units {
        let base = bases.entry(path.clone()).or_insert_with(|| comparable(&path));
        let same = |b: &Unit| b.name == unit.name && b.code == unit.code;
        if !base.as_ref().is_some_and(|base| base.iter().any(same)) {
            out.push(Function { path, unit });
        }
    }
    out
}

/// Whether a line of the Python source `source` holds a mutmut pragma ([`mutmut_pragma`]), in a comment or not.
fn holds_pragma(source: &[u8]) -> bool {
    source.split(|&b| b == b'\n').any(|line| mutmut_pragma(&String::from_utf8_lossy(line)).is_some())
}

/// A function the change touched, the unit mutmut mutates.
struct Function {
    /// Its file, from the tree's top.
    path: OsString,
    unit: Unit,
}

impl Function {
    /// A finding of `rule` at the function's def.
    fn finding(&self, rule: &str, message: String) -> Finding {
        finding(Some(&self.path), Some(self.unit.line), rule, message)
    }
}

/// [`python_mutants`]'s findings on `functions`, the change's changed functions: those mutmut does not mutate, then
/// what a run of mutmut of `seconds` found of the others.
fn mutation_test(python: &PythonChange, functions: &[Function], mutmut: &Mutmut, seconds: u64) -> Vec<Finding> {
    let table = match mutmut.rules.map(mutmut_table) {
        Some(Ok(table)) => table,
        Some(Err(problem)) => return vec![not_run(&problem)],
        None => {
            return vec![not_run(
                "there is no pyproject.toml beside the gate's settings, so no [tool.mutmut] names source_paths and \
                 what mutmut mutates is unknown",
            )]
        }
    };
    let (mut out, mut mutated) = (vec![], vec![]);
    for f in functions {
        match outside_mutmut(&f.path, &table).or_else(|| unmutatable(&f.unit)) {
            Some(why) => {
                let message = format!("`{}` was not mutation-tested: {why}.", f.unit.name);
                out.push(f.finding("not-mutated", message));
            }
            None => mutated.push(f),
        }
    }
    if mutated.is_empty() {
        return out;
    }
    let change = python.change;
    let mut recorded = None;
    if let Some(data_file) = mutmut.test_contexts {
        match recorded_tests(&change.python, &change.root, &mutated, data_file) {
            Err(why) => out.push(not_narrowed(&why)),
            Ok(tests) if tests.is_empty() => {
                out.extend(untested_by_contexts(&mutated));
                return out;
            }
            Ok(tests) => recorded = Some(tests),
        }
    }
    out.extend(in_scratch_directory("not-run", "to run mutmut on a copy of the tree in", |tmp| {
        let copy = TreeCopy { python: &change.python, tree: &tmp.join("tree") };
        on_copy(&copy, &change.root, tmp, &mutated, &table, seconds, recorded.as_ref())
    }));
    out
}

/// A `not-run` finding: `problem`, a clause, and that the change's functions were not mutation-tested.
fn not_run(problem: &str) -> Finding {
    finding(None, None, "not-run", format!("{problem}; the change's functions were not mutation-tested."))
}

/// The `not-narrowed` finding: `--test-contexts` was given, but `why`, a clause, kept the gate from narrowing
/// mutmut's test selection to the tests it records.
fn not_narrowed(why: &str) -> Finding {
    let message = format!(
        "mutmut was given its whole test selection, not only the tests `--test-contexts` records reaching the changed \
         functions: {why}."
    );
    finding(None, None, "not-narrowed", message)
}

/// An `untested` finding for each of `functions`, which no test of mutmut's selection reaches as `--test-contexts`
/// records the tests, so that mutmut is not started.
fn untested_by_contexts(functions: &[&Function]) -> Vec<Finding> {
    let message = |f: &Function| {
        format!(
            "no test of [tool.mutmut]'s selection runs `{}`, as `--test-contexts` records them, so mutmut was not \
             started.",
            f.unit.name
        )
    };
    functions.iter().map(|f| f.finding("untested", message(f))).collect()
}

/// The `[tool.mutmut]` table of the pyproject.toml at `rules`, empty when it has none. Errs with the problem, a
/// clause, when it cannot be read, names no source_paths, without which what mutmut mutates cannot be told, or holds
/// an only_mutate, do_not_mutate, do_not_mutate_patterns, also_copy, pytest_add_cli_args,
/// pytest_add_cli_args_test_selection or tests_dir that is not a list of strings: the keys [`strings`] reads.
fn mutmut_table(rules: &Path) -> Result<Table, String> {
    let named = text::path(rules.as_os_str());
    let unread = |why: String| format!("could not read [tool.mutmut] in {named}: {why}");
    let source = std::fs::read_to_string(rules).map_err(|e| unread(e.to_string()))?;
    let whole: Table = source.parse().map_err(|e: toml::de::Error| unread(e.message().to_string()))?;
    let table = match whole.get("tool") {
        None => Table::new(),
        Some(toml::Value::Table(tool)) => match tool.get("mutmut") {
            None => Table::new(),
            Some(toml::Value::Table(table)) => table.clone(),
            // No table, so no source_paths.
            Some(_) => Table::new(),
        },
        Some(_) => return Err(unread("its `tool` is not a table".into())),
    };
    let is_strings = |setting: &toml::Value| {
        setting.as_array().is_some_and(|items| items.iter().all(toml::Value::is_str))
    };
    if !source_paths(&table).is_some_and(|sources| truthy(sources) && is_strings(sources)) {
        return Err(format!(
            "[tool.mutmut] in {named} names no source_paths as a list of paths, so what mutmut mutates is unknown"
        ));
    }
    let listed = [
        "only_mutate",
        "do_not_mutate",
        "do_not_mutate_patterns",
        "also_copy",
        "pytest_add_cli_args",
        "pytest_add_cli_args_test_selection",
        "tests_dir",
    ];
    for key in listed {
        if table.get(key).is_some_and(|setting| !is_strings(setting)) {
            return Err(format!("[tool.mutmut] {key} in {named} is not a list of strings"));
        }
    }
    Ok(table)
}

/// The setting of `table`, a `[tool.mutmut]`, that names what mutmut mutates: its source_paths, else, when it has
/// none or an empty one, paths_to_mutate, the name mutmut 2 gave it.
fn source_paths(table: &Table) -> Option<&toml::Value> {
    table.get("source_paths").filter(|sources| truthy(sources)).or_else(|| table.get("paths_to_mutate"))
}

/// The list of strings `table`, a `[tool.mutmut]` as [`mutmut_table`] gives it, has under `key`; empty without one.
fn strings(table: &Table, key: &str) -> Vec<String> {
    let items = table.get(key).and_then(toml::Value::as_array).map(Vec::as_slice).unwrap_or_default();
    items.iter().filter_map(|item| item.as_str().map(String::from)).collect()
}

/// `table` with `items` as the list of strings under `key`.
fn set_strings(table: &mut Table, key: &str, items: Vec<String>) {
    table.insert(key.to_string(), toml::Value::Array(items.into_iter().map(toml::Value::String).collect()));
}

/// Why mutmut, set up by `table`, a `[tool.mutmut]` with source_paths, mutates nothing in the file at `path`, from
/// the tree's top; None when it may. As mutmut 3.8.0 decides it, by walking source_paths for files named *.py
/// (its walk_source_files), so never a `#!` python script without the name, then its Config.should_mutate's
/// only_mutate and do_not_mutate patterns.
fn outside_mutmut(path: &OsStr, table: &Table) -> Option<String> {
    let sources: Vec<String> = source_paths(table)
        .and_then(toml::Value::as_array)
        .map(|sources| sources.iter().filter_map(|source| source.as_str().map(String::from)).collect())
        .unwrap_or_default();
    let under = |root: &Path| {
        let root = root.as_os_str().as_bytes();
        let below = path.as_bytes().strip_prefix(root).is_some_and(|rest| rest.starts_with(b"/"));
        root == b"." || path.as_bytes() == root || below
    };
    if !sources.iter().any(|source| under(&normpath(Path::new(source)))) {
        return Some(format!("it is outside [tool.mutmut] source_paths ({})", sources.join(", ")));
    }
    if !path.as_bytes().ends_with(b".py") {
        return Some("mutmut mutates only files named *.py".into());
    }
    let name = text::per_byte_lossy(path.as_bytes());
    let only = strings(table, "only_mutate");
    if !only.is_empty() && !only.iter().any(|pattern| fnmatch(&name, pattern)) {
        return Some(format!("[tool.mutmut] only_mutate ({}) leaves it out", only.join(", ")));
    }
    let mut excluded = strings(table, "do_not_mutate");
    excluded.retain(|pattern| fnmatch(&name, pattern));
    (!excluded.is_empty()).then(|| format!("[tool.mutmut] do_not_mutate excludes it ({})", excluded.join(", ")))
}

/// Why mutmut 3.8.0 makes no mutant of `unit` whatever it holds, as its file_mutation.py decides; None when it may.
fn unmutatable(unit: &Unit) -> Option<String> {
    let name = unit.name.rsplit('.').next().unwrap_or_default();
    if ["__new__", "__getattribute__", "__setattr__"].contains(&name) {
        return Some(format!("mutmut never mutates `{name}`"));
    }
    let decorators: Vec<&str> = unit.decorators.iter().map(String::as_str).collect();
    if !matches!(decorators[..], [] | ["staticmethod"] | ["classmethod"]) {
        return Some(
            "mutmut does not mutate a decorated function, unless @staticmethod or @classmethod is its one decorator"
                .into(),
        );
    }
    None
}

/// The pytest node ids of the tests that `data_file`, the absolute path of a coverage.py data file with a context
/// per test, records running a line of the body of any of `functions`, those in `root`'s working tree; a one-line
/// function's own line counts as its body. coverage.py reads the file, in `python`.
///
/// Errs with why the file cannot tell them, a clause: coverage.py cannot read it, it holds no test's context, it
/// records too few tests on each line ([`UNDER_RECORDED_SHARE`]), it did not measure the file of one of
/// `functions`, which it never does for a path that is not UTF-8, or it names none of the tests from `root`
/// ([`rooted_elsewhere`]). The node ids are as pytest names the tests from the rootdir of the run that wrote the file,
/// which must be `root`, as mutmut runs them in its copy of `root`.
fn recorded_tests(
    python: &Path,
    root: &Path,
    functions: &[&Function],
    data_file: &Path,
) -> Result<BTreeSet<String>, String> {
    let named = text::one_line(&text::path(data_file.as_os_str()));
    let span = |f: &Function| {
        let (line, last) = (f.unit.line, f.unit.lines.last);
        json!([real_path(&root.join(&f.path)).to_string_lossy(), if last > line { line + 1 } else { line }, last])
    };
    let spans = Value::Array(functions.iter().map(|f| span(f)).collect()).to_string();
    // -I: the program runs in root, whose own modules, such as a json.py, must not stand in for the ones it imports.
    let argv = [python.as_os_str(), "-I".as_ref(), "-c".as_ref(), TEST_CONTEXTS_PROGRAM.as_ref()];
    let argv = [&argv[..], &[data_file.as_os_str(), spans.as_ref()]].concat();
    let read = process::run(&argv, root, Duration::from_secs(TEST_CONTEXTS_TIMEOUT))
        .map_err(|e| {
            let python = text::path(python.as_os_str());
            format!("coverage.py could not be started with the Python `{python}` to read `{named}`: {e}")
        })?;
    if read.stopped {
        return Err(format!("reading `{named}` took longer than {TEST_CONTEXTS_TIMEOUT} seconds"));
    }
    if read.status != 0 {
        return Err(format!("coverage.py could not read `{named}`: {}", read.said()));
    }
    let found: Recorded = serde_json::from_slice(&read.stdout)
        .map_err(|e| format!("reading `{named}` printed what the gate cannot read: {e}"))?;
    if found.reached.len() != functions.len() {
        return Err(format!(
            "reading `{named}` printed what the gate cannot read: not a list of node ids or null for each changed \
             function"
        ));
    }
    if found.tests == 0 {
        return Err(format!("`{named}` records no test's context, as `pytest --cov-context=test` writes them"));
    }
    if found.busiest.saturating_mul(UNDER_RECORDED_SHARE) < found.tests {
        return Err(format!(
            "`{named}` records {} tests but none of its lines for more than {}, as a coverage core that records each \
             line for its first tests alone writes it; pin the ctrace core, with `[tool.coverage.run] core = \
             \"ctrace\"`",
            found.tests, found.busiest
        ));
    }
    if let Some((f, _)) = functions.iter().zip(&found.reached).find(|(_, tests)| tests.is_none()) {
        return Err(format!("`{named}` did not measure `{}`", text::one_line(&text::path(&f.path))));
    }
    let tests: BTreeSet<String> = found.reached.into_iter().flatten().flatten().collect();
    rooted_elsewhere(root, &tests).map_or(Ok(tests), Err)
}

/// What [`TEST_CONTEXTS_PROGRAM`] prints.
#[derive(serde::Deserialize)]
struct Recorded {
    /// How many tests the file records a context of.
    tests: u64,
    /// The most of them it records running any one line.
    busiest: u64,
    /// For each span given, the node ids of the tests it records running a line of it; None when the file did not
    /// measure the span's path.
    reached: Vec<Option<Vec<String>>>,
}

/// A copy of the tree that mutmut runs in.
struct TreeCopy<'a> {
    /// The program that runs Python, as change::Change::python names it.
    python: &'a Path,
    /// The copy's top.
    tree: &'a Path,
}

impl TreeCopy<'_> {
    /// mutmut with `args`, run at the copy's top for at most `timeout`, and what it printed; `stopped` when the
    /// timeout ended it. Errs when it cannot be started.
    ///
    /// It is stopped softly, with SIGINT and mutants::STOP_GRACE: on SIGINT mutmut stops trying mutants and keeps
    /// the results it has. So it returns within `timeout`, mutants::STOP_GRACE and mutants::KILL_WAIT. Its temporary
    /// files, and those of the tests it runs, go into the copy's .tmp, so that what a test it killed left goes with
    /// the copy. Its mutant names and diffs hold paths and source lines, in which only `\n` ends a line.
    fn mutmut(&self, args: &[&str], timeout: Duration) -> std::io::Result<Finished> {
        let tmp = self.tree.join(".tmp");
        std::fs::create_dir_all(&tmp)?;
        // `mutmut`, its console script, runs this line. `python -m mutmut` would import its __main__ a second time
        // in each forked worker, which mutmut warns against.
        let mut argv: Vec<&OsStr> = vec![self.python.as_os_str(), "-c".as_ref()];
        argv.push("from mutmut.__main__ import cli; cli()".as_ref());
        argv.extend(args.iter().map(OsStr::new));
        let extras = Extras { env: &[("TMPDIR", tmp.as_os_str())], soft_stop: Some(STOP_GRACE), ..Extras::default() };
        run_with(&argv, self.tree, timeout, &extras)
    }

    /// The program that runs Python, as a finding names it when it cannot be started.
    fn python_named(&self) -> String {
        text::path(self.python.as_os_str())
    }

    /// mutmut's state in the copy: its copy of the code with the mutants, and each module's results in
    /// `<path>.meta` beside it.
    fn state(&self) -> PathBuf {
        self.tree.join(MUTMUT_STATE)
    }
}

/// The time a mutmut run may be given that must end by `deadline`, its stop included; None when none is left.
fn time_left(deadline: Instant) -> Option<Duration> {
    deadline.checked_duration_since(Instant::now() + STOP_GRACE + KILL_WAIT).filter(|left| !left.is_zero())
}

/// The findings of mutmut run on `functions` for `seconds` in `copy`, made as [`prepare_copy`] says, with `table`, the
/// rules' `[tool.mutmut]`, in place of its own, and its pytest runs taking the configuration of the tests they run
/// and no configuration or conftest.py from above the copy ([`give_mutmut`]). `recorded`, when given, are the node
/// ids of the tests `--test-contexts` records reaching `functions`: mutmut is then given those of them its selection
/// collects ([`narrowed_table`]), and none collected, it is not started and each function is an `untested` finding.
fn on_copy(
    copy: &TreeCopy,
    root: &Path,
    tmp: &Path,
    functions: &[&Function],
    table: &Table,
    seconds: u64,
    recorded: Option<&BTreeSet<String>>,
) -> Vec<Finding> {
    let config = match prepare_copy(copy, root, tmp, functions, table, recorded) {
        Ok(config) => config,
        Err(problem) => return vec![not_run(&problem)],
    };
    // Rooted at the top, not the configuration's directory, as mutmut's --rootdir=. roots its runs.
    let setup = || Setup {
        python: copy.python.to_path_buf(),
        root: copy.tree.to_path_buf(),
        inifile: config.clone(),
        top: copy.tree.to_path_buf(),
    };
    let (table, mut out) = match recorded.map(|recorded| narrowed_table(&setup(), table, recorded)) {
        None => (table.clone(), vec![]),
        Some(Narrowed::To(narrowed)) => (narrowed, vec![]),
        Some(Narrowed::ToNothing) => return untested_by_contexts(functions),
        Some(Narrowed::Not(why)) => (table.clone(), vec![not_narrowed(&why)]),
    };
    let ran = give_mutmut(copy, &table, &config).and_then(|()| {
        copy.mutmut(&["run"], Duration::from_secs(seconds))
            .map_err(|e| format!("could not start mutmut with the Python `{}`: {e}", copy.python_named()))
    });
    match ran {
        Ok(run) => out.extend(mutmut_findings(copy, functions, &run, seconds, &table)),
        Err(problem) => out.push(not_run(&problem)),
    }
    out
}

/// Makes `copy`, in `tmp`, an empty private directory, of the files git tracks in `root`'s working tree
/// (copies::copy_tracked), but for what it tracks under mutmut's state, and marks it for mutmut to skip all but
/// `functions` ([`mark_for_mutmut`] by `table`, the rules' `[tool.mutmut]`). Returns the pytest configuration of the
/// tests mutmut runs, those `table`'s selection names or, given, those of `recorded` under it ([`test_places`]), as
/// pytest::groups finds it. Errs with the problem, a clause, when the copy cannot be made or marked, the
/// configuration cannot be found, or the tests take more than one.
fn prepare_copy(
    copy: &TreeCopy,
    root: &Path,
    tmp: &Path,
    functions: &[&Function],
    table: &Table,
    recorded: Option<&BTreeSet<String>>,
) -> Result<PathBuf, String> {
    copy_tracked(root, copy.tree).map_err(not_started)?;
    // Before anything reads the copy's pyproject.toml, pytest's configuration among it.
    own_pyproject(root, copy.tree)?;
    // mutmut takes what it finds under its state for its own cache, results included, so it starts without what the
    // tree tracks there.
    let state = copy.state();
    match state.symlink_metadata() {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&state).map_err(not_started)?,
        Ok(_) => std::fs::remove_file(&state).map_err(not_started)?,
        Err(_) => {}
    }
    mark_for_mutmut(copy.tree, table, functions).map_err(not_started)?;
    // One configuration for all of mutmut's pytest runs, as it takes one pytest_add_cli_args.
    let places = test_places(copy.tree, table, recorded);
    let groups = pytest::groups(copy.python, copy.tree, tmp, &places).map_err(not_started)?;
    match &groups[..] {
        [group] => Ok(group.setup.inifile.clone()),
        _ => Err(several_configurations(&groups)),
    }
}

/// Puts `table`, a `[tool.mutmut]`, in place of the one in the pyproject.toml of `copy`, with what makes mutmut's
/// pytest runs take `config`, a configuration file in the copy or beside it, with `-c`, and no conftest.py from above
/// its state, mutants/, with `--confcutdir`. A `config` in the copy goes into mutmut's state by also_copy, and is
/// given from there, so that paths in it, such as pythonpath, name the mutated code. Errs with the problem, a
/// clause, when a path is not UTF-8, the directory for `config` in the state cannot be made, or the table cannot be
/// put in place ([`use_mutmut_table`]).
fn give_mutmut(copy: &TreeCopy, table: &Table, config: &Path) -> Result<(), String> {
    let state = copy.state();
    let mut given = table.clone();
    let mut config = config.to_path_buf();
    if let Ok(in_tree) = config.strip_prefix(copy.tree).map(Path::to_path_buf) {
        let mut also_copy = strings(table, "also_copy");
        also_copy.push(utf8(&in_tree).map_err(not_started)?);
        set_strings(&mut given, "also_copy", also_copy);
        // mutmut copies also_copy's files before the directories it copies by default, tests/ among them, and makes
        // no directory for a file.
        if let Some(dir) = in_tree.parent() {
            std::fs::create_dir_all(state.join(dir)).map_err(not_started)?;
        }
        config = state.join(in_tree);
    }
    // mutmut runs pytest in mutants/ with --rootdir=. and these after its own arguments. Without -c, pytest looks
    // for its configuration in each directory above, up to one that another account may write to when the project
    // has none.
    let mut args = strings(table, "pytest_add_cli_args");
    args.extend(["-c".to_string(), utf8(&config).map_err(not_started)?]);
    args.extend(["--confcutdir".to_string(), utf8(&state).map_err(not_started)?]);
    set_strings(&mut given, "pytest_add_cli_args", args);
    use_mutmut_table(copy.tree, &given)
}

/// The paths in `tree`, a copy of a tree, that pytest under mutmut starts from to find its configuration, for
/// pytest::groups, each normalized below the top, "" for the top itself. They are the arguments of `table`'s test
/// selection, tests_dir's among them, that name a file, a directory or a node id in the copy, else the top alone,
/// where pytest given no path starts from; when `recorded` node ids are given, the files of those of them that lie
/// in one of those paths take their place. An option's value that names a file, as `--ignore`'s given apart from
/// it, cannot be told from a path, and counts as one.
fn test_places(tree: &Path, table: &Table, recorded: Option<&BTreeSet<String>>) -> BTreeSet<OsString> {
    let mut selected = BTreeSet::new();
    let selection = ["pytest_add_cli_args_test_selection", "tests_dir"].map(|key| strings(table, key));
    for arg in selection.iter().flatten().filter(|arg| !arg.starts_with('-')) {
        let path = normpath(Path::new(arg.split("::").next().unwrap_or_default()));
        let below = path.components().all(|part| matches!(part, std::path::Component::Normal(_)));
        if below && tree.join(&path).exists() {
            selected.insert(path.into_os_string());
        } else if path == Path::new(".") {
            selected.insert(OsString::new());
        }
    }
    if selected.is_empty() {
        selected.insert(OsString::new());
    }
    let Some(recorded) = recorded else { return selected };
    let files = recorded.iter().map(|id| pytest::node_parts(OsStr::new(id)).0);
    let mut out: BTreeSet<OsString> =
        files.filter(|file| selected.iter().any(|place| Path::new(file).starts_with(place))).collect();
    if out.is_empty() {
        out.insert(OsString::new());
    }
    out
}

/// Why `recorded`, pytest node ids, cannot name tests as mutmut's runs in a copy of `root`, the tree, name them, from
/// its top, a clause; None when there are none or one of them names a file of `root`. pytest names tests from its
/// rootdir, which, unless `--rootdir` sets it, is the directory of the configuration it finds, so a run started as
/// `pytest tests/` with a tests/pytest.ini records `test_calc.py::test_x`.
fn rooted_elsewhere(root: &Path, recorded: &BTreeSet<String>) -> Option<String> {
    let file = |id: &String| root.join(pytest::node_parts(OsStr::new(id)).0);
    if recorded.is_empty() || recorded.iter().any(|id| file(id).is_file()) {
        return None;
    }
    let first = recorded.first().map(|id| text::one_line(id)).unwrap_or_default();
    Some(format!(
        "none of the tests it records is named from the tree's top, as `{first}` is not: the pytest run that recorded \
         them had its rootdir elsewhere, which `--rootdir` at the top avoids"
    ))
}

/// The problem, a clause, of tests that pytest::groups puts under more than one configuration, `groups`, when mutmut
/// runs all its tests under one.
fn several_configurations(groups: &[pytest::Group]) -> String {
    let named = |group: &pytest::Group| match group.inifile_in_tree() {
        Some(inifile) => format!("`{}`", text::one_line(&text::path(inifile.as_os_str()))),
        None => "the empty one of tests with none".into(),
    };
    let names: Vec<String> = groups.iter().map(named).collect();
    format!(
        "the tests mutmut runs take more than one pytest configuration, {}, where mutmut runs them all under one",
        text::series(&names, None)
    )
}

/// `path` as a `[tool.mutmut]` setting can hold it; errs, saying so, when it is not UTF-8.
fn utf8(path: &Path) -> Result<String, String> {
    let named = || format!("{} is not UTF-8, which a [tool.mutmut] setting must be", text::path(path.as_os_str()));
    path.to_str().map(String::from).ok_or_else(named)
}

/// What [`narrowed_table`] made of a `[tool.mutmut]`.
enum Narrowed {
    /// The table with its test selection narrowed to the recorded tests pytest collects from it.
    To(Table),
    /// pytest collects none of the recorded tests.
    ToNothing,
    /// The collection failed, as this clause says, so the table stays as it is.
    Not(String),
}

/// `table`, a `[tool.mutmut]`, with its test selection narrowed to those of `recorded`, pytest node ids, that pytest
/// collects from it at `setup`, a copy of the tree, by the selection and pytest_add_cli_args mutmut's own runs are
/// given: so pytest itself applies their paths, `--ignore`, `--deselect`, `-m` and the like. The copy holds every
/// tracked file, where mutmut's own holds source_paths, also_copy and tests/, so a selection that reaches outside
/// those is collected wider here than mutmut runs it.
fn narrowed_table(setup: &Setup, table: &Table, recorded: &BTreeSet<String>) -> Narrowed {
    // Without pytest-randomly, which only shuffles the list. mutmut appends tests_dir, a deprecated alias, to the
    // selection.
    let mut args: Vec<String> = ["-p", "no:randomly", "--collect-only"].map(String::from).to_vec();
    for key in ["pytest_add_cli_args_test_selection", "tests_dir", "pytest_add_cli_args"] {
        args.extend(strings(table, key));
    }
    let args: Vec<OsString> = args.into_iter().map(OsString::from).collect();
    // Exit status 5: pytest collected no test.
    let timeout = Duration::from_secs(TEST_COLLECTION_TIMEOUT);
    let collected = match pytest::run(setup, COLLECTING, &args, timeout, &[0, 5], &[]) {
        Ok(collected) => collected,
        Err(problem) => return Narrowed::Not(problem.clause),
    };
    let ids = match pytest::listed_node_ids(&collected, COLLECTING, |_| true) {
        Ok(ids) => ids,
        Err(problem) => return Narrowed::Not(problem),
    };
    let ids: BTreeSet<String> = ids.iter().map(|id| text::lossless(id.as_bytes())).collect();
    let tests: Vec<String> = recorded.iter().filter(|id| ids.contains(id.as_str())).cloned().collect();
    if tests.is_empty() {
        return Narrowed::ToNothing;
    }
    let mut narrowed = table.clone();
    narrowed.remove("tests_dir");
    set_strings(&mut narrowed, "pytest_add_cli_args_test_selection", tests);
    Narrowed::To(narrowed)
}

/// Every file under the directory `dir` that is no directory, a symbolic link among them, which is not followed.
/// Errs when a directory cannot be read.
fn files_under(dir: &Path) -> std::io::Result<Vec<PathBuf>> {
    let mut out = vec![];
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            out.extend(files_under(&entry.path())?);
        } else {
            out.push(entry.path());
        }
    }
    Ok(out)
}

/// Marks for mutmut to skip ([`SKIP`]) each def and class that holds none of `functions`, as
/// lang_python::header_ends_outside chooses them, in the regular files under `tree`, a copy of a tree, that mutmut
/// set up by `table`, a `[tool.mutmut]` with source_paths, mutates ([`outside_mutmut`]); no other file is read. A
/// Python file that cannot be decoded or parsed is left as it is. Errs, a clause, when a directory or one of those
/// files cannot be read or written.
fn mark_for_mutmut(tree: &Path, table: &Table, functions: &[&Function]) -> Result<(), String> {
    let mut kept: BTreeMap<&OsStr, BTreeSet<&str>> = BTreeMap::new();
    for f in functions {
        kept.entry(&f.path).or_default().insert(&f.unit.name);
    }
    let files = files_under(tree).map_err(|e| format!("cannot read the copy of the tree: {e}"))?;
    for path in files {
        let Ok(rel) = path.strip_prefix(tree) else { continue };
        let regular = path.symlink_metadata().is_ok_and(|meta| meta.is_file());
        if !regular || outside_mutmut(rel.as_os_str(), table).is_some() {
            continue;
        }
        let source = std::fs::read(&path).map_err(|e| crate::copies::failed("read", &path, &e))?;
        let mine = kept.get(rel.as_os_str());
        let Ok(ends) = lang_python::header_ends_outside(&source, |name| mine.is_some_and(|mine| mine.contains(name)))
        else {
            continue;
        };
        if !ends.is_empty() {
            std::fs::write(&path, marked(&source, &ends)).map_err(|e| crate::copies::failed("write", &path, &e))?;
        }
    }
    Ok(())
}

/// `source`, a Python file's bytes, with [`SKIP`] appended to each of its lines `ends` numbers, from 1, before the
/// `\r` of one that ends in `\r\n`; no line moves.
fn marked(source: &[u8], ends: &[u32]) -> Vec<u8> {
    let mut lines: Vec<Vec<u8>> = source.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect();
    for &n in ends {
        let Some(line) = usize::try_from(n).ok().and_then(|n| lines.get_mut(n.wrapping_sub(1))) else { continue };
        let at = line.len() - usize::from(line.ends_with(b"\r"));
        line.splice(at..at, SKIP.iter().copied());
    }
    lines.join(&b'\n')
}

/// The problem, a clause, of mutmut that could not be started because of `e`, a failure to set up its run.
fn not_started(e: impl std::fmt::Display) -> String {
    format!("could not start mutmut: {e}")
}

/// Makes the pyproject.toml of `copy`, a copy of the tree `root` (copies::copy_tracked), a file of the copy's own
/// where `root`'s is a symbolic link, which the copy holds as the same link: the file holds what the link names
/// from `root`, since from the copy's place a relative link names another file or none, and written through, a link
/// would change the file it names. Anything else is left as it is. Errs with the problem, a clause, when what the
/// link names cannot be read, a link to no file among it, or the copy's file cannot be written.
fn own_pyproject(root: &Path, copy: &Path) -> Result<(), String> {
    let path = copy.join("pyproject.toml");
    if !path.is_symlink() {
        return Ok(());
    }
    let linked = std::fs::read(root.join("pyproject.toml"))
        .map_err(|e| not_started(format!("`pyproject.toml` is a symbolic link, and what it names cannot be read: {e}")))?;
    std::fs::remove_file(&path).map_err(not_started)?;
    std::fs::write(&path, linked).map_err(not_started)
}

/// Puts `table` in place of the `[tool.mutmut]` of `tree`'s pyproject.toml, a file or missing ([`own_pyproject`]),
/// leaving the rest of the file, the tests' own settings among it, as the tree has it. Errs with the problem, a
/// clause, when the file cannot be read or written, or the result does not read back as `table` ([`with_table`]).
fn use_mutmut_table(tree: &Path, table: &Table) -> Result<(), String> {
    let path = tree.join("pyproject.toml");
    let own = match std::fs::read_to_string(&path) {
        Ok(own) => own,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(not_started(e)),
    };
    let replaced = with_table(&own, table)?;
    std::fs::write(&path, replaced).map_err(not_started)
}

/// `own`, a pyproject.toml's text, with `table` as its `[tool.mutmut]`: the lines under each `[tool.mutmut]`
/// header dropped, and the table's settings written under one at the end. Errs, a clause, when the result does not
/// read back as `table`, as when the file defines the table other than under a `[tool.mutmut]` header.
fn with_table(own: &str, table: &Table) -> Result<String, String> {
    /// A line that opens a table or an array of tables, with the name captured.
    static HEADER: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"^\s*\[\[?\s*([^\[\],=#]+?)\s*\]\]?\s*(?:#.*)?$").expect("a regex")
    });
    let own = own.replace("\r\n", "\n").replace('\r', "\n");
    let (mut kept, mut inside) = (vec![], false);
    for line in own.split('\n') {
        if let Some(header) = HEADER.captures(line) {
            inside = &header[1] == "tool.mutmut";
        }
        if !inside {
            kept.push(line);
        }
    }
    let mut out = format!("{}\n\n[tool.mutmut]\n", kept.join("\n").trim_end_matches('\n'));
    for (name, setting) in table {
        out.push_str(&format!("{} = {setting}\n", toml::Value::String(name.clone())));
    }
    let read_back = out.parse::<Table>().ok();
    let replaced = read_back.as_ref().and_then(|whole| whole.get("tool")?.get("mutmut")?.as_table()) == Some(table);
    if !replaced {
        return Err("the rules' [tool.mutmut] could not be put in place of the tree's in its pyproject.toml".into());
    }
    Ok(out)
}

/// What the run of mutmut in `copy` found of `functions`, as `mutmut results` and `mutmut show` tell it: `run` is
/// how it ended, stopped by the budget of `seconds` or not, and `table` the `[tool.mutmut]` it ran by.
fn mutmut_findings(
    copy: &TreeCopy,
    functions: &[&Function],
    run: &Finished,
    seconds: u64,
    table: &Table,
) -> Vec<Finding> {
    /// The line mutmut 3.8.0 prints, exiting with status 1 before it tries any mutant, when no test calls any
    /// function it mutated, or it made no mutant at all; the mutants it made are then left not checked. Only the
    /// whole line counts: the output of a failing test, which mutmut prints before its own, may hold its words.
    static NO_TESTS: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
        let line = "(?m)^Stopping early, because we could not find any test case for any mutant\\. It seems that the \
                    selected tests do not cover any code that we mutated\\.$";
        regex::bytes::Regex::new(line).expect("a regex")
    });
    let no_tests = run.status != 0 && NO_TESTS.is_match(&run.stdout);
    if run.status != 0 && !run.stopped && !no_tests {
        return vec![not_run(&format!("mutmut exited with status {}: {}", run.status, said_by_mutmut(run)))];
    }
    let deadline = Instant::now() + Duration::from_secs(REPORT_TIMEOUT);
    let (mut statuses, cut_off) = match statuses_after_run(copy, run.stopped, deadline) {
        Ok(listed) => listed,
        Err(problem) => return vec![not_run(&problem)],
    };
    if no_tests {
        statuses.iter_mut().for_each(|(_, status)| *status = "no tests".into());
    }
    let mutants = mutants_by_function(functions, &statuses);
    // A function mutmut was given but listed no mutant of. mutmut saves a module's mutants, none or more, in
    // mutants/<path>.meta once it has made them all, so stopped by the budget it had not reached a module without
    // one, or the stop lost its results (statuses_after_run).
    let saved = |f: &Function| {
        let mut save = f.path.clone();
        save.push(".meta");
        copy.state().join(save).exists()
    };
    let unlisted = (0..functions.len()).filter(|k| !mutants.iter().any(|(owner, _)| owner == k)).map(|k| functions[k]);
    let (unreached, unmade): (Vec<&Function>, Vec<&Function>) =
        unlisted.partition(|f| run.stopped && !saved(f));
    let mut out: Vec<Finding> = unmade
        .iter()
        .map(|f| {
            let why = why_no_mutant(&f.unit, table);
            let name = &f.unit.name;
            f.finding("not-mutated", format!("`{name}` was not mutation-tested: mutmut made no mutant of it; {why}."))
        })
        .collect();
    for (owner, mine) in &mutants {
        out.extend(listed_findings(copy, functions[*owner], mine, deadline));
    }
    if run.stopped {
        out.push(budget_reached(&statuses, seconds, &unreached, &cut_off));
    }
    out
}

/// The mutants of one function with their statuses, each a (mutant, status) pair as [`Statuses`] holds them.
type Listed<'a> = Vec<(&'a str, &'a str)>;

/// Each of `functions` that `statuses` lists a mutant of, as its index in `functions`, with its mutants in the
/// order mutmut lists them, which is the order it made them in; the functions come in the order of their first
/// mutants, and a mutant of none of them is left out.
fn mutants_by_function<'a>(functions: &[&Function], statuses: &'a Statuses) -> Vec<(usize, Listed<'a>)> {
    let by_prefix: HashMap<String, usize> = functions.iter().enumerate().map(|(k, f)| (mutant_prefix(f), k)).collect();
    let mut mutants: Vec<(usize, Listed)> = vec![];
    for (mutant, status) in statuses {
        let Some(&owner) = by_prefix.get(prefix_of(mutant)) else { continue };
        match mutants.iter_mut().find(|(k, _)| *k == owner) {
            Some((_, mine)) => mine.push((mutant, status)),
            None => mutants.push((owner, vec![(mutant, status)])),
        }
    }
    mutants
}

/// The findings for `f` from `mine`, the mutants mutmut listed of it: one for those that survived, with what
/// [`survivors`] has mutmut show of them in `copy` by `deadline`, and one for those no test calls.
fn listed_findings(copy: &TreeCopy, f: &Function, mine: &Listed, deadline: Instant) -> Vec<Finding> {
    let tried = mine.iter().filter(|(_, status)| !UNTRIED.contains(status)).count();
    let of = |wanted: &str| -> Vec<&str> { mine.iter().filter(|(_, s)| *s == wanted).map(|(m, _)| *m).collect() };
    let mut out = vec![];
    let survived = of("survived");
    if !survived.is_empty() {
        out.push(survivors(copy, f, &survived, tried, deadline));
    }
    let untested = of("no tests").len();
    if untested > 0 {
        let name = &f.unit.name;
        let message = format!("no test calls `{name}`, so none of its {untested} mutants can make one fail.");
        out.push(f.finding("untested", message));
    }
    out
}

/// [`mutant_statuses`]'s listing of the mutants a run of mutmut made in `copy`, by `deadline`, with the paths of the
/// modules whose results the run's stop lost, sorted; `stopped` is whether the budget stopped the run. Errs with the
/// problem, a clause, when they could not be listed.
///
/// The stop can land while mutmut rewrites a module's .meta, which `mutmut results` cannot read, so a stopped run's
/// cut-off .meta files are removed from the copy first; their modules then list no mutant, as one the budget kept
/// mutmut from does.
fn statuses_after_run(copy: &TreeCopy, stopped: bool, deadline: Instant) -> Result<(Statuses, Vec<OsString>), String> {
    let cut_off = match stopped {
        true => drop_cut_off_saves(&copy.state())
            .map_err(|e| format!("could not clear the results mutmut's stop cut off: {e}"))?,
        false => vec![],
    };
    match mutant_statuses(copy, deadline) {
        Ok(statuses) => Ok((statuses, cut_off)),
        Err(problem) if cut_off.is_empty() => Err(problem),
        Err(problem) => {
            let named: Vec<String> = cut_off.iter().map(|path| text::path(path)).collect();
            let lost = text::series(&named, Some(5));
            Err(format!("{problem} after the gate removed the cut-off results of {lost}"))
        }
    }
}

/// Removes each of mutmut's saves of a module's results, `<path>.py.meta` under `state`, mutmut's state in a copy,
/// that does not parse as JSON, as one cut off mid-write leaves it, and returns the paths of their modules in the
/// tree, sorted; none when `state` is no directory. A save that cannot be read is left as it is: mutmut reads a
/// missing one as no results, and fails to list on any other, which is then the finding. Errs when `state` cannot be
/// searched or a save cannot be removed.
fn drop_cut_off_saves(state: &Path) -> std::io::Result<Vec<OsString>> {
    if !state.is_dir() {
        return Ok(vec![]);
    }
    let mut out = vec![];
    for path in files_under(state)? {
        let Some(module) = path.as_os_str().as_bytes().strip_suffix(b".meta").filter(|m| m.ends_with(b".py")) else {
            continue;
        };
        let Ok(saved) = std::fs::read(&path) else { continue };
        if serde_json::from_slice::<Value>(&saved).is_err() {
            std::fs::remove_file(&path)?;
            let module = Path::new(OsStr::from_bytes(module));
            out.push(module.strip_prefix(state).unwrap_or(module).as_os_str().to_os_string());
        }
    }
    out.sort();
    Ok(out)
}

/// mutmut's status of each mutant, its word for what became of it, by the mutant's name, in the order mutmut lists
/// them.
type Statuses = Vec<(String, String)>;

/// Each mutant mutmut made in `copy` with its status, as `mutmut results` words it: killed, survived, no tests, not
/// checked and the like. Errs with the problem, a clause, when they could not be listed by `deadline`, which
/// mutmut's stop is kept within: with too little time left for a listing and its stop, none is started.
fn mutant_statuses(copy: &TreeCopy, deadline: Instant) -> Result<Statuses, String> {
    let late = || format!("mutmut did not list its results within {REPORT_TIMEOUT} seconds");
    let listed = copy.mutmut(&["results", "--all", "true"], time_left(deadline).ok_or_else(late)?).map_err(|e| {
        format!("could not start mutmut with the Python `{}` to list its results: {e}", copy.python_named())
    })?;
    if listed.stopped {
        return Err(late());
    }
    if listed.status != 0 {
        let said = said_by_mutmut(&listed);
        return Err(format!("mutmut could not list its results, exiting with status {}: {said}", listed.status));
    }
    Ok(listed_statuses(&text::lossless(&listed.stdout)))
}

/// The mutants `mutmut results` lists in `printed`, each on a line `<name>: <status>`, in the order of their first
/// lines; a name listed again takes its last status.
fn listed_statuses(printed: &str) -> Statuses {
    static LISTED: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\s*(\S+): (.+?)\s*$").expect("a regex"));
    let mut out: Statuses = vec![];
    let mut places: HashMap<&str, usize> = HashMap::new();
    for found in LISTED.captures_iter(printed) {
        let (name, status) = (found.get(1).map_or("", |m| m.as_str()), found[2].to_string());
        match places.get(name) {
            Some(&k) => out[k].1 = status,
            None => {
                places.insert(name, out.len());
                out.push((name.to_string(), status));
            }
        }
    }
    out
}

/// The name mutmut 3 gives the mutants of `f` before their `__mutmut_<n>`: its module, from its path as mutmut's
/// get_mutant_name makes it, a dot, then `x_<function>` or `xǁ<class>ǁ<method>`, which holds no dot. [`prefix_of`]
/// reads it back from a mutant's name.
fn mutant_prefix(f: &Function) -> String {
    let path = text::path(&f.path);
    let module = path.strip_suffix(".py").unwrap_or(&path).replace('/', ".");
    let module = module.strip_prefix("src.").unwrap_or(&module);
    let named = match f.unit.name.rsplit_once('.') {
        Some((class, name)) => format!("xǁ{class}ǁ{name}"),
        None => format!("x_{}", f.unit.name),
    };
    format!("{module}.{named}").replace(".__init__.", ".")
}

/// The part of a mutant's name, as `mutmut results` lists it, that [`mutant_prefix`] gives its function: all before
/// its last `__mutmut_`, since a function named `_mutmut_x` has one in its own name, as `x__mutmut_x`.
fn prefix_of(mutant: &str) -> &str {
    mutant.rsplit_once("__mutmut_").map_or("", |(prefix, _)| prefix)
}

/// The finding that the budget of `seconds` stopped mutmut, having made the mutants of `statuses` and none yet of
/// `unreached`, the functions it was given in modules it had not reached; of those, the ones in `cut_off`, paths of
/// modules whose results the stop lost mid-save, are said to be so.
fn budget_reached(statuses: &Statuses, seconds: u64, unreached: &[&Function], cut_off: &[OsString]) -> Finding {
    let tried = statuses.iter().filter(|(_, status)| !UNTRIED.contains(&status.as_str())).count();
    let named = |functions: Vec<&&Function>| {
        let each: Vec<String> =
            functions.iter().map(|f| format!("`{}` in {}", f.unit.name, text::path(&f.path))).collect();
        text::series(&each, Some(5))
    };
    let (lost, unmade): (Vec<&&Function>, Vec<&&Function>) = unreached.iter().partition(|f| cut_off.contains(&f.path));
    let mut message = match (statuses.len(), lost.is_empty()) {
        (0, true) => "budget reached before mutmut had made its mutants".to_string(),
        (0, false) => "budget reached with none of the mutants mutmut made left to list".to_string(),
        (made, _) => format!("budget reached after {tried} of {made} mutants"),
    };
    message.push_str(&format!(
        ": mutmut was stopped at {seconds} seconds (`--mutation-budget`), so the mutants it did not try may survive too"
    ));
    if !unmade.is_empty() {
        message.push_str(&format!("; mutmut made no mutant of {} before the budget ran out", named(unmade)));
    }
    if !lost.is_empty() {
        let lost = named(lost);
        message.push_str(&format!(
            "; the stop cut off mutmut's save of its results for {lost}, so they count as not reached"
        ));
    }
    message.push('.');
    finding(None, None, "budget-reached", message)
}

/// Why mutmut 3.8.0 made no mutant of `unit`, a function it was given, as a clause: the one cause the gate can tell
/// cheaply, a `# pragma: no mutate block` where its header ends or `table`'s do_not_mutate_patterns matching each
/// statement in it, else every cause there can be.
fn why_no_mutant(unit: &Unit, table: &Table) -> String {
    // The one form of mutmut's pragmas that the gate recognises and names as the cause: `block` in the comment of
    // the line its header ends on, with the body on the lines below (mutmut's pragma_handling.py,
    // _visit_compound_header). A block pragma on a line of its own above the def or atop its body, one on its
    // class, or a start/end pair around it keep it out too, and get EVERY_CAUSE, which names a pragma among the
    // causes.
    if unit.header_comment.as_deref().and_then(mutmut_pragma) == Some("block") {
        return "a `# pragma: no mutate block` where its header ends keeps it out".into();
    }
    // mutmut skips every expression that starts on a line a pattern matches; a statement with a block of its own
    // holds lines of other statements, which this does not look into.
    let patterns = strings(table, "do_not_mutate_patterns");
    if patterns.is_empty() || unit.body.is_empty() || unit.body.iter().any(|statement| statement.compound) {
        return EVERY_CAUSE.into();
    }
    // Each pattern is read when a line first needs it, as mutmut's `re.search` reads it; one that is no regex
    // leaves the cause untold.
    let mut read: Vec<Option<Regex>> = vec![None; patterns.len()];
    for statement in &unit.body {
        let line = usize::try_from(statement.line - unit.lines.first).ok().and_then(|k| unit.source.get(k));
        if line.and_then(|line| first_match(&patterns, &mut read, line)).is_none() {
            return EVERY_CAUSE.into();
        }
    }
    "the rules' do_not_mutate_patterns match each statement in it".into()
}

/// The index of the first of `patterns` that matches `line`; None when none does, or when one tried before a match
/// is no regex. `read[k]` holds pattern k as a regex once a line has needed it, and is filled here.
fn first_match(patterns: &[String], read: &mut [Option<Regex>], line: &str) -> Option<usize> {
    for (k, pattern) in patterns.iter().enumerate() {
        if read[k].is_none() {
            read[k] = Some(Regex::new(pattern).ok()?);
        }
        if read[k].as_ref().is_some_and(|regex| regex.is_match(line)) {
            return Some(k);
        }
    }
    None
}

/// What `comment` says to mutmut 3.8.0, as its _parse_pragma_token reads it: "block", "start", "end" or "bare" for
/// a `# pragma: no mutate` comment of each form; None for any other comment.
fn mutmut_pragma(comment: &str) -> Option<&'static str> {
    if !comment.contains("# pragma:") {
        return None;
    }
    let (_, after) = comment.split_once("no mutate")?;
    let options = text::py_strip(after).trim_start_matches([':', ' ']);
    let word = options.split(',').next().and_then(|first| first.split_whitespace().next());
    Some(["block", "start", "end"].into_iter().find(|form| word == Some(form)).unwrap_or("bare"))
}

/// The finding for the mutants `survived` of `f`, of the `tried` ones mutmut ran against its tests: how many, and
/// what the first mutants::SURVIVOR_EXAMPLES of them change, each as `mutmut show` gives it in `copy` by `deadline`.
fn survivors(copy: &TreeCopy, f: &Function, survived: &[&str], tried: usize, deadline: Instant) -> Finding {
    let shown = survived.iter().take(SURVIVOR_EXAMPLES).filter_map(|mutant| example(copy, &f.unit, mutant, deadline));
    let examples: Vec<String> = shown.collect();
    let said = format!("{} of {tried} mutants of `{}` survived its tests", survived.len(), f.unit.name);
    let message = match examples.is_empty() {
        true => format!("{said}; mutmut did not show in time what they change."),
        false => format!("{said}: {}.", examples.join("; ")),
    };
    f.finding("survived", message)
}

/// What `mutant`, a mutant of `unit`, changes, as "line N, `before` became `after`"; None when mutmut does not show
/// it in `copy` by `deadline`, which mutmut's stop is kept within.
fn example(copy: &TreeCopy, unit: &Unit, mutant: &str, deadline: Instant) -> Option<String> {
    let shown = copy.mutmut(&["show", mutant], time_left(deadline)?).ok()?;
    if shown.stopped || shown.status != 0 {
        return None;
    }
    let (at, old, new) = mutation(&text::lossless(&shown.stdout))?;
    // `at` counts from the first line of the function's code as mutmut prints it, which takes in the comments right
    // above it, so the line is the nearest one that reads as the mutant's removed one.
    let expected = unit.lines.first.saturating_add(at).saturating_sub(1);
    let reads_so = |line: &&String| old.first().is_some_and(|removed| text::py_strip(line) == removed);
    let hits = unit.source.iter().enumerate().filter(|(_, line)| reads_so(line));
    let hits = hits.filter_map(|(k, _)| u32::try_from(k).ok().map(|k| unit.lines.first + k));
    let line = hits.min_by_key(|&n| n.abs_diff(expected)).unwrap_or(expected);
    Some(format!("line {line}, `{}` became `{}`", text::clip(&old.join(" ")), text::clip(&new.join(" "))))
}

/// What the first hunk of `diff`, `mutmut show`'s diff of a function and its mutant, changes: the line of the
/// function's code where it starts, 1 for the first, and the lines it removes and adds there, stripped; None when it
/// removes none.
fn mutation(diff: &str) -> Option<(u32, Vec<String>, Vec<String>)> {
    let mut lines = diff.split('\n').skip_while(|line| !line.starts_with("@@"));
    let header = lines.next()?.strip_prefix("@@ -")?;
    let digits = header.find(|c: char| !c.is_ascii_digit()).unwrap_or(header.len());
    let mut n: u32 = header[..digits].parse().ok()?;
    let (mut at, mut old, mut new) = (None, vec![], vec![]);
    for line in lines.take_while(|line| !line.starts_with("@@")) {
        if let Some(added) = line.strip_prefix('+') {
            new.push(text::py_strip(added).to_string());
            continue;
        }
        if let Some(removed) = line.strip_prefix('-') {
            at = at.or(Some(n).filter(|&n| n > 0));
            old.push(text::py_strip(removed).to_string());
        }
        n = n.saturating_add(1);
    }
    at.map(|at| (at, old, new))
}

/// Finished::said of a mutmut that went wrong, leaving out the Python warnings it writes to standard error, such as
/// that a `[tool.mutmut]` key is deprecated, which come before its own message on standard output.
fn said_by_mutmut(run: &Finished) -> String {
    /// A Python warning as the warnings module writes it to standard error: where, its category and message, then
    /// the source line it is about, indented.
    static PYTHON_WARNING: LazyLock<regex::bytes::Regex> = LazyLock::new(|| {
        regex::bytes::Regex::new(r"(?m-u)^\S.*:\d+: \w*Warning: .*\n(?:[ \t].*(?:\n|$))?").expect("a regex")
    });
    let stderr = PYTHON_WARNING.replace_all(&run.stderr, &b""[..]).into_owned();
    Finished { status: run.status, stdout: run.stdout.clone(), stderr, stopped: run.stopped }.said()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

    /// The units of the Python `source`, as lang_python reads them.
    fn units(source: &str) -> Vec<Unit> {
        lang_python::units(source.as_bytes()).unwrap()
    }

    /// The first function of `source`, in the file `path`.
    fn function(path: &str, source: &str) -> Function {
        Function { path: path.into(), unit: units(source).remove(0) }
    }

    #[test]
    fn a_listing_that_hangs_ends_by_the_deadline_with_its_stop_counted_in() {
        // The Python ignores SIGINT, so it is killed STOP_GRACE after its time is up; given all the time to the
        // deadline, it would end that long past it.
        use std::os::unix::fs::PermissionsExt;
        let tree = ScratchDir::new();
        let python = tree.join("python");
        std::fs::write(&python, "#!/bin/sh\ntrap '' INT\nexec sleep 60\n").unwrap();
        std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
        let copy = TreeCopy { python: &python, tree: &tree };
        let deadline = Instant::now() + STOP_GRACE + KILL_WAIT + Duration::from_secs(2);
        let listed = mutant_statuses(&copy, deadline);
        let late = Instant::now().checked_duration_since(deadline);
        assert_eq!(listed, Err(format!("mutmut did not list its results within {REPORT_TIMEOUT} seconds")));
        assert_eq!(late, None, "the listing ended after its deadline");
        // With no time left for a listing and its stop, none is started: a Python that is not there would be an
        // error of its own.
        let copy = TreeCopy { python: &tree.join("no-python"), tree: &tree };
        assert_eq!(mutant_statuses(&copy, Instant::now() + STOP_GRACE + KILL_WAIT), listed);
    }

    /// The `[tool.mutmut]` whose settings `settings` holds, as TOML.
    fn table(settings: &str) -> Table {
        settings.parse().unwrap()
    }

    #[test]
    fn a_run_that_must_end_by_a_deadline_gets_the_time_before_its_stop() {
        let stop = STOP_GRACE + KILL_WAIT;
        assert_eq!(time_left(Instant::now() + stop - Duration::from_secs(1)), None);
        assert_eq!(time_left(Instant::now()), None);
        let left = time_left(Instant::now() + stop + Duration::from_secs(20)).unwrap();
        assert!(Duration::from_secs(19) < left && left <= Duration::from_secs(20), "{left:?}");
    }

    #[test]
    fn pytest_starts_from_the_paths_the_selection_names_in_the_copy_else_from_the_top() {
        let tree = ScratchDir::new();
        std::fs::create_dir_all(tree.join("tests/unit")).unwrap();
        std::fs::write(tree.join("tests/unit/test_u.py"), "").unwrap();
        let places = |selection: &str, recorded: Option<&[&str]>| -> Vec<String> {
            let recorded: Option<BTreeSet<String>> = recorded.map(|ids| ids.iter().map(|id| id.to_string()).collect());
            let found = test_places(&tree, &table(selection), recorded.as_ref());
            found.iter().map(|path| text::path(path)).collect()
        };
        // An option, its value, and a path outside the copy or missing from it are not among them.
        let selection = r#"pytest_add_cli_args_test_selection = ["-m", "slow", "./tests/", "/abs", "../up", "gone"]"#;
        assert_eq!(places(selection, None), ["tests"]);
        let selection = r#"pytest_add_cli_args_test_selection = ["tests/unit/test_u.py::test_x"]
                           tests_dir = ["."]"#;
        assert_eq!(places(selection, None), ["", "tests/unit/test_u.py"]);
        assert_eq!(places("", None), [""]);
        // Recorded tests stand in for the selection, but for those outside the paths it names.
        assert_eq!(places(selection, Some(&["t/test_a.py::test_x", "t/test_a.py::T::test_y"])), ["t/test_a.py"]);
        let selection = r#"pytest_add_cli_args_test_selection = ["tests/unit"]"#;
        let recorded = ["tests/unit/test_u.py::test_x", "tests/integration/test_i.py::test_y"];
        assert_eq!(places(selection, Some(&recorded)), ["tests/unit/test_u.py"]);
        assert_eq!(places(selection, Some(&recorded[1..])), [""]);
    }

    #[test]
    fn a_pragma_is_read_by_its_first_option() {
        assert_eq!(mutmut_pragma("# pragma: no mutate"), Some("bare"));
        assert_eq!(mutmut_pragma("# pragma: no mutate block"), Some("block"));
        assert_eq!(mutmut_pragma("# pragma: no mutate: block, anything"), Some("block"));
        assert_eq!(mutmut_pragma("# pragma: no mutate start"), Some("start"));
        assert_eq!(mutmut_pragma("# pragma: no mutate end of it"), Some("end"));
        assert_eq!(mutmut_pragma("# pragma: no mutate blocks"), Some("bare"));
        assert_eq!(mutmut_pragma("# pragma: no mutate , block"), Some("bare"));
        assert_eq!(mutmut_pragma("# pragma: no cover"), None);
        assert_eq!(mutmut_pragma("# no mutate block"), None);
    }

    #[test]
    fn a_diff_s_first_hunk_gives_the_line_and_what_it_removes_and_adds() {
        let diff = "--- calc.py\n+++ calc.py\n@@ -1,4 +1,4 @@\n def x_add(a, b):\n     c = 1\n-    return a + b\n\
                    +    return a - b\n+    # more\n rest\n@@ -9,2 +9,2 @@\n-    later\n+    other\n";
        let found = mutation(diff);
        assert_eq!(found, Some((3, vec!["return a + b".to_string()], vec!["return a - b".into(), "# more".into()])));
    }

    #[test]
    fn a_diff_that_removes_no_line_or_has_no_hunk_gives_nothing() {
        assert_eq!(mutation("@@ -1,2 +1,3 @@\n def f():\n+    pass\n"), None);
        assert_eq!(mutation("--- a\n+++ b\n"), None);
        assert_eq!(mutation("@@ no numbers @@\n-x\n"), None);
    }

    #[test]
    fn a_mutant_s_prefix_is_its_module_and_mutmut_s_name_for_the_function() {
        let prefix = |path: &str, source: &str| mutant_prefix(&function(path, source));
        assert_eq!(prefix("calc.py", "def add():\n    pass\n"), "calc.x_add");
        assert_eq!(prefix("src/pkg/mod.py", "def add():\n    pass\n"), "pkg.mod.x_add");
        assert_eq!(prefix("pkg/__init__.py", "def add():\n    pass\n"), "pkg.x_add");
        assert_eq!(prefix("pkg/calc.py", "class C:\n    def m(self):\n        pass\n"), "pkg.calc.xǁCǁm");
    }

    #[test]
    fn a_mutant_s_name_gives_its_prefix_back_up_to_its_last_number() {
        assert_eq!(prefix_of("calc.x_add__mutmut_12"), "calc.x_add");
        assert_eq!(prefix_of("calc.x__mutmut_x__mutmut_1"), "calc.x__mutmut_x");
        assert_eq!(prefix_of("calc.x_add"), "");
    }

    #[test]
    fn a_mutant_listed_twice_keeps_its_place_and_takes_its_last_status() {
        let printed = "    calc.x_add__mutmut_1: survived\n    calc.x_add__mutmut_2: not checked  \nnoise\n\
                       calc.x_add__mutmut_1: killed\n";
        let listed = listed_statuses(printed);
        let expected = [("calc.x_add__mutmut_1", "killed"), ("calc.x_add__mutmut_2", "not checked")];
        assert_eq!(listed, expected.map(|(name, status)| (name.to_string(), status.to_string())));
    }

    #[test]
    fn the_rules_table_takes_the_place_of_the_file_s_own_and_the_rest_stays() {
        let own = "[project]\r\nname = \"p\"\r\n\r\n[tool.mutmut]  # ours\r\nsource_paths = [\"old\"]\r\n\r\n\
                   [tool.pytest.ini_options]\r\naddopts = \"-q\"\r\n\r\n[ tool.mutmut ]\r\ndebug = true\r\n";
        let rules = table("source_paths = [\"src\"]\ndebug = false\n");
        let written = with_table(own, &rules).unwrap();
        let (kept, added) = written.split_once("\n\n[tool.mutmut]\n").unwrap();
        assert_eq!(kept, "[project]\nname = \"p\"\n\n[tool.pytest.ini_options]\naddopts = \"-q\"");
        let whole: Table = written.parse().unwrap();
        assert_eq!(whole["tool"]["mutmut"].as_table(), Some(&rules), "{added}");
    }

    #[test]
    fn a_file_without_the_table_gets_it() {
        let rules = table("source_paths = [\"src\"]\n");
        let whole: Table = with_table("", &rules).unwrap().parse().unwrap();
        assert_eq!(whole["tool"]["mutmut"].as_table(), Some(&rules));
    }

    #[test]
    fn a_table_the_file_sets_by_a_dotted_key_cannot_be_replaced() {
        let rules = table("source_paths = [\"src\"]\n");
        let problem = with_table("tool.mutmut.debug = true\n", &rules).unwrap_err();
        assert_eq!(problem, "the rules' [tool.mutmut] could not be put in place of the tree's in its pyproject.toml");
    }

    #[test]
    fn only_the_files_mutmut_mutates_are_read_and_marked() {
        use std::os::unix::fs::PermissionsExt;
        let tree = ScratchDir::new();
        std::fs::create_dir_all(tree.join("src")).unwrap();
        std::fs::create_dir_all(tree.join("tests")).unwrap();
        let source = "def f():\n    pass\n";
        for file in ["src/a.py", "src/generated.py", "tests/test_a.py", "tests/unreadable.py"] {
            std::fs::write(tree.join(file), source).unwrap();
        }
        // Read, it would fail the run.
        std::fs::set_permissions(tree.join("tests/unreadable.py"), std::fs::Permissions::from_mode(0o000)).unwrap();
        let settings = table("source_paths = [\"src\"]\ndo_not_mutate = [\"src/generated.py\"]\n");
        assert_eq!(mark_for_mutmut(&tree, &settings, &[]), Ok(()));
        let read = |file: &str| std::fs::read_to_string(tree.join(file)).unwrap();
        assert_eq!(read("src/a.py"), "def f():  # pragma: no mutate block\n    pass\n");
        assert_eq!(read("src/generated.py"), source);
        assert_eq!(read("tests/test_a.py"), source);
    }

    #[test]
    fn a_marked_line_keeps_its_ending_and_no_line_moves() {
        let source = b"def f():\r\n    pass\r\ndef g():\n    pass\nclass C: pass";
        let expected = b"def f():  # pragma: no mutate block\r\n    pass\r\ndef g():  # pragma: no mutate block\n    \
                         pass\nclass C: pass  # pragma: no mutate block";
        assert_eq!(marked(source, &[1, 3, 5, 0, 9]), expected);
    }

    #[test]
    fn a_file_outside_what_mutmut_mutates_is_said_to_be_with_the_setting_that_leaves_it_out() {
        let why = |path: &str, settings: &str| outside_mutmut(OsStr::new(path), &table(settings));
        let src = "source_paths = [\"./src/\", \"lib\"]\n";
        assert_eq!(why("src/a.py", src), None);
        assert_eq!(why("lib/deep/a.py", src), None);
        assert_eq!(why("srcs/a.py", src).unwrap(), "it is outside [tool.mutmut] source_paths (./src/, lib)");
        assert_eq!(why("src/a.pyi", src).unwrap(), "mutmut mutates only files named *.py");
        assert_eq!(why("a.py", "source_paths = [\".\"]\n"), None);
        assert_eq!(why("a.py", "source_paths = [\"a.py\"]\n"), None);
        assert_eq!(why("a.py", "source_paths = []\npaths_to_mutate = [\"a.py\"]\n"), None);
        let only = format!("{src}only_mutate = [\"src/b*\", \"lib/*\"]\n");
        assert_eq!(why("src/a.py", &only).unwrap(), "[tool.mutmut] only_mutate (src/b*, lib/*) leaves it out");
        assert_eq!(why("src/b/a.py", &only), None);
        let not = format!("{src}do_not_mutate = [\"*/a.py\", \"lib/*\", \"src/a*\"]\n");
        assert_eq!(why("src/a.py", &not).unwrap(), "[tool.mutmut] do_not_mutate excludes it (*/a.py, src/a*)");
        assert_eq!(why("src/b.py", &not), None);
    }

    #[test]
    fn a_function_mutmut_never_mutates_is_said_to_be() {
        let why = |source: &str| unmutatable(&units(source)[0]);
        let decorated = "mutmut does not mutate a decorated function, unless @staticmethod or @classmethod is its one \
                         decorator";
        assert_eq!(why("def f():\n    pass\n"), None);
        assert_eq!(why("class C:\n    @staticmethod\n    def f():\n        pass\n"), None);
        assert_eq!(why("class C:\n    @classmethod\n    def f(cls):\n        pass\n"), None);
        assert_eq!(why("@cache\ndef f():\n    pass\n").unwrap(), decorated);
        let twice = "class C:\n    @staticmethod\n    @cache\n    def f():\n        pass\n";
        assert_eq!(why(twice).unwrap(), decorated);
        let new = "class C:\n    def __new__(cls):\n        pass\n";
        assert_eq!(why(new).unwrap(), "mutmut never mutates `__new__`");
        assert_eq!(why("def __setattr__():\n    pass\n").unwrap(), "mutmut never mutates `__setattr__`");
    }

    #[test]
    fn the_budget_s_finding_counts_the_tried_mutants() {
        let statuses: Statuses = [("m.x_f__mutmut_1", "killed"), ("m.x_f__mutmut_2", "not checked")]
            .map(|(name, status)| (name.to_string(), status.to_string()))
            .to_vec();
        let stopped = "(`--mutation-budget`), so the mutants it did not try may survive too";
        let found = budget_reached(&statuses, 1, &[], &[]);
        assert_eq!((found.path, found.line, found.rule.as_str()), (None, None, "budget-reached"));
        assert_eq!(
            found.message,
            format!("budget reached after 1 of 2 mutants: mutmut was stopped at 1 seconds {stopped}.")
        );
        let found = budget_reached(&vec![], 3, &[], &[]);
        assert_eq!(
            found.message,
            format!("budget reached before mutmut had made its mutants: mutmut was stopped at 3 seconds {stopped}.")
        );
    }

    #[test]
    fn the_budget_s_finding_names_the_functions_mutmut_did_not_reach_and_those_whose_results_the_stop_lost() {
        let (f, g) = (function("a.py", "def f():\n    pass\n"), function("b.py", "def g():\n    pass\n"));
        let found = budget_reached(&vec![], 2, &[&f, &g], &["b.py".into()]);
        assert_eq!(
            found.message,
            "budget reached with none of the mutants mutmut made left to list: mutmut was stopped at 2 seconds \
             (`--mutation-budget`), so the mutants it did not try may survive too; mutmut made no mutant of `f` in \
             a.py before the budget ran out; the stop cut off mutmut's save of its results for `g` in b.py, so they \
             count as not reached."
        );
    }

    #[test]
    fn a_function_without_mutants_gets_the_one_cause_the_gate_can_tell() {
        let why = |source: &str, settings: &str| why_no_mutant(&units(source)[0], &table(settings));
        let patterns = "the rules' do_not_mutate_patterns match each statement in it";
        assert_eq!(
            why("def f():  # pragma: no mutate block\n    return 1\n", ""),
            "a `# pragma: no mutate block` where its header ends keeps it out"
        );
        assert_eq!(why("def f():  # pragma: no mutate\n    return 1\n", ""), EVERY_CAUSE);
        assert_eq!(why("def f():\n    return 1\n", ""), EVERY_CAUSE);
        let two = "def f():\n    \"\"\"Doc.\"\"\"\n    log(1)\n    return 1\n";
        assert_eq!(why(two, "do_not_mutate_patterns = [\"log\", \"return\"]\n"), patterns);
        assert_eq!(why(two, "do_not_mutate_patterns = [\"log\"]\n"), EVERY_CAUSE);
        let nested = "def f():\n    if x:\n        return 1\n";
        assert_eq!(why(nested, "do_not_mutate_patterns = [\".\"]\n"), EVERY_CAUSE);
    }

    #[test]
    fn a_pattern_that_is_no_regex_leaves_the_cause_untold_only_once_a_line_needs_it() {
        let why = |settings: &str| why_no_mutant(&units("def f():\n    return 1\n")[0], &table(settings));
        assert_eq!(
            why("do_not_mutate_patterns = [\"return\", \"(\"]\n"),
            "the rules' do_not_mutate_patterns match each statement in it"
        );
        assert_eq!(why("do_not_mutate_patterns = [\"(\", \"return\"]\n"), EVERY_CAUSE);
    }

    #[test]
    fn what_mutmut_said_leaves_out_python_s_warnings() {
        let stderr = "/venv/mutmut/config.py:12: DeprecationWarning: tests_dir is deprecated\n  warnings.warn(\n";
        let run = Finished { status: 1, stdout: b"no mutants\n".to_vec(), stderr: stderr.into(), stopped: false };
        assert_eq!(said_by_mutmut(&run), "no mutants");
        let failed = Finished { stderr: format!("{stderr}Boom: it broke\n").into(), ..run };
        assert_eq!(said_by_mutmut(&failed), "Boom: it broke");
    }
}
