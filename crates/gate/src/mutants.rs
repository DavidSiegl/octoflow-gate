//! The surviving-mutants check: a function the change touches whose mutants its tests do not notice is a finding.
//! Here is the check and its Rust half, in which cargo-mutants mutates the functions the change edits in Rust code
//! under test; the Python half, which runs mutmut, is python_mutants.rs, and the Go half, which runs gremlins,
//! go_mutants.rs.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::time::{Duration, Instant};

use checks::Finding;
use model::Function;
use serde_json::Value;

use crate::clippy::workspaces;
use crate::copies::{copy_tracked, finding, in_scratch_directory, sort_by_place, Budget};
use crate::diff::removal_points;
use crate::go_mutants::{edited_functions, go_mutants};
use crate::parser_checks::{project_dir, RustChange, Sources};
use crate::process::{run_with, Extras, Finished};
use crate::python_mutants::{python_mutants, Mutmut};
use crate::text;

/// How each outcome cargo-mutants 27 writes in mutants.out/outcomes.json (a scenario's "summary") counts: a missed
/// mutant survived; a caught one, and one whose tests outlived cargo-mutants' `--timeout`, did not; an unviable one,
/// which does not build, was not tried. "Success" and "Failure" are the unmutated baseline's.
const SURVIVED: &str = "MissedMutant";
const TRIED: [&str; 3] = ["MissedMutant", "CaughtMutant", "Timeout"];

/// The seconds cargo-mutants gives one mutant's tests before it counts them a timeout (its `--timeout`).
const TEST_TIMEOUT: u64 = 60;

/// How long cargo-mutants has, after the SIGINT that stops a run at the deadline, to write what it found and exit
/// before it is killed, and mutmut as long.
pub(crate) const STOP_GRACE: Duration = Duration::from_secs(10);

/// What a run is given beyond [`STOP_GRACE`] after the deadline: the most process::run_with waits for what a killed
/// process printed (its KILL_WAIT).
pub(crate) const KILL_WAIT: Duration = Duration::from_secs(5);

/// How many of a function's surviving mutants its finding shows, in the order the mutation tool made them.
pub(crate) const SURVIVOR_EXAMPLES: usize = 3;

/// The surviving-mutants check's findings on `sources`, by path and line, a finding on no path or line first; each
/// rule is the check's own, without its name, and each severity "warning".
///
/// Mutation-tests the Python functions the change touches with mutmut, set up as `mutmut` says
/// (python_mutants::python_mutants), then the Rust functions it edits with cargo-mutants ([`rust_mutants`]),
/// then the Go functions it edits with gremlins (go_mutants::go_mutants), all within `budget` seconds. Each language
/// in turn has an equal share of what the ones before it left, among itself and the languages after it that have
/// something to mutate: with all three, mutmut has a third of the budget, cargo-mutants half of what is left, and
/// gremlins the rest. Each one's findings name its share as the seconds it was stopped at.
pub fn surviving_mutants(sources: &Sources, mutmut: &Mutmut, budget: u64) -> Vec<Finding> {
    let budget = Budget::starting_now(budget);
    let (rust, mut out) = edited_rust(&sources.rust);
    let (go, unread) = edited_functions(&sources.go);
    out.extend(unread);
    let share = |later: &[bool]| budget.share(1 + later.iter().map(|&has| u32::from(has)).sum::<u32>());
    let python = share(&[!rust.is_empty(), !go.is_empty()]);
    out.extend(python_mutants(&sources.python, mutmut, python.seconds as f64));
    out.extend(rust_mutants(&sources.rust, &rust, &share(&[!go.is_empty()])));
    out.extend(go_mutants(&sources.go, &go, &share(&[])));
    sort_by_place(&mut out);
    out
}

/// A function the change edits, which the mutation tool of its language mutates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Edited {
    /// Its file, from the tree's top.
    pub(crate) path: OsString,
    /// Its name, as model::Function's, a Go method's as `Stack.Push`.
    pub(crate) name: String,
    /// The lines from its `fn` or `func` to its closing brace, its doc comment and attributes left out.
    pub(crate) line: u32,
    pub(crate) last: u32,
}

impl Edited {
    /// `f`, of the file at `path`, from the tree's top, as an edited function.
    pub(crate) fn of(path: &OsStr, f: &Function) -> Edited {
        Edited { path: path.to_os_string(), name: f.name.clone(), line: f.line, last: f.lines.last }
    }
}

/// Whether a change edits the function `f`: it adds one of `added`, the lines it adds to `f`'s file, from `f`'s `fn` or
/// `func` line to its closing brace, or removes lines from between two of those, at one of `removed`, as
/// diff::removal_points gives them. A deleted guard clause needs a test as much as an added one.
pub(crate) fn edits(f: &Function, added: &BTreeSet<u32>, removed: &BTreeSet<u32>) -> bool {
    added.range(f.line..=f.lines.last).next().is_some() || removed.iter().any(|&n| f.line <= n && n < f.lines.last)
}

/// The Rust functions the change that `rust` reads edits, as [`edits`] says, in the change's order of their files and
/// then by line, and a `not-run` finding for each file whose functions cannot be told, as one that does not parse.
///
/// A function counts when it is a `fn` with a body, outside test code (model::Function's `exempt`), of a `.rs` file
/// that is Rust code under test ([`RustChange::is_code`]) and a regular file of the working tree: not a symlink, a
/// submodule or a special file, whose reading could block or never end.
fn edited_rust(rust: &RustChange) -> (Vec<Edited>, Vec<Finding>) {
    let removals = removal_points(&rust.change.diff);
    let none = BTreeSet::new();
    let regular =
        |p: &OsStr| std::fs::symlink_metadata(rust.change.root.join(p)).is_ok_and(|m| m.file_type().is_file());
    let (mut edited, mut unread) = (vec![], vec![]);
    for file in &rust.files {
        let Some(after) = &file.after else { continue };
        let path = after.path.as_os_str();
        let removed = removals.get(path).unwrap_or(&none);
        let rs = path.as_bytes().ends_with(b".rs");
        if (file.added.is_empty() && removed.is_empty()) || !rs || !rust.is_code(path) || !regular(path) {
            continue;
        }
        match &after.source {
            Ok(source) => edited.extend(
                (source.functions.iter())
                    .filter(|f| !f.exempt && edits(f, &file.added, removed))
                    .map(|f| Edited::of(path, f)),
            ),
            Err(unreadable) => {
                let message = format!("{}, so its functions were not mutation-tested.", unreadable.why);
                unread.push(finding(Some(path), unreadable.line, "not-run", message));
            }
        }
    }
    (edited, unread)
}

/// Rust's findings of the surviving-mutants check on `functions`, [`edited_rust`]'s of `rust`, in no order.
///
/// Runs cargo-mutants, until `budget` runs out, on each Cargo workspace that holds one of `functions`, in a copy of
/// the files git tracks, as [`on_copy`] says. Each function with mutants that survived is a `survived` finding at the
/// line its fn starts, with how many of the mutants tried of it they were and the first [`SURVIVOR_EXAMPLES`] of
/// them. A function with nothing to mutate gives nothing. The budget running out is a `budget-reached` finding; a
/// file in no workspace cargo can tell, or whose workspace's directory is not above it, is a `not-mutated` one on it;
/// a run that cannot be made or read is a `not-run` finding saying why, and a copy that cannot be removed afterwards a
/// `not-removed` one.
fn rust_mutants(rust: &RustChange, functions: &[Edited], budget: &Budget) -> Vec<Finding> {
    if functions.is_empty() {
        return vec![];
    }
    in_scratch_directory("not-run", "to run cargo-mutants on a copy of the tree in", |dir| {
        on_copy(rust, dir, functions, budget)
    })
}

/// The findings on `functions`, [`edited_rust`]'s, of cargo-mutants run on each Cargo workspace that holds one of
/// them ([`cargo_roots`]), in path order of the workspaces, in a copy of the files git tracks made in `dir`, an empty
/// directory, beside each run's output. cargo-mutants mutates only `functions` ([`run_workspace`]), and its tests run
/// with `--test-tool nextest`, each mutant's within [`TEST_TIMEOUT`].
///
/// A run the deadline stops is a `budget-reached` finding beside what the mutants it had tried showed, and each
/// workspace after it a `not-run` one; a copy that cannot be made is the one `not-run` finding.
fn on_copy(rust: &RustChange, dir: &Path, functions: &[Edited], budget: &Budget) -> Vec<Finding> {
    let tree = dir.join("tree");
    if let Err(e) = copy_tracked(&rust.change.root, &tree) {
        return vec![not_run(&format!("could not copy the tree for cargo-mutants: {e}"))];
    }
    let mut out = vec![];
    let mut by_path: BTreeMap<&OsStr, Vec<&Edited>> = BTreeMap::new();
    for f in functions {
        by_path.entry(&f.path).or_default().push(f);
    }
    let mut roots: BTreeMap<OsString, Vec<&Edited>> = BTreeMap::new();
    let mut root_of = cargo_roots(&tree, by_path.keys().copied());
    for (path, mine) in by_path {
        let why = match root_of.remove(path).expect("cargo_roots answers each path it is given") {
            Ok(root) if Path::new(path).starts_with(&root) => {
                roots.entry(root).or_default().extend(mine);
                continue;
            }
            Ok(root) => format!("its workspace at `{}` is not above it", dir_name(&root)),
            Err(why) => why,
        };
        out.push(finding(Some(path), None, "not-mutated", format!("was not mutation-tested: {why}.")));
    }
    let roots: Vec<_> = roots.into_iter().collect();
    for (n, (root, mine)) in roots.iter().enumerate() {
        let run = Run { tree: &tree, scratch: &dir.join(format!("run-{n}")) };
        let (found, stopped) = run_workspace(&run, root, mine, budget);
        out.extend(found);
        if stopped {
            out.extend(roots[n + 1..].iter().map(|(later, _)| {
                not_run(&format!(
                    "the budget ran out before cargo-mutants reached the workspace at `{}`",
                    dir_name(later)
                ))
            }));
            break;
        }
    }
    out
}

/// For each of `paths`, Rust files by their repository paths, the directory, from `tree`'s top, empty for the top
/// itself, that cargo-mutants runs in to mutate it: that of the root manifest of the workspace, as cargo tells it
/// (clippy::workspaces), of the package whose Cargo.toml is nearest above it; or why it has none, a clause, when no
/// Cargo.toml lies above it, or cargo cannot tell its workspace. Cargo is asked once a package.
fn cargo_roots<'p>(
    tree: &Path,
    paths: impl Iterator<Item = &'p OsStr>,
) -> HashMap<&'p OsStr, Result<OsString, String>> {
    let mut by_package: HashMap<&Path, Result<OsString, String>> = HashMap::new();
    let mut out = HashMap::new();
    for path in paths {
        let root = match project_dir(path, "Cargo.toml", |manifest| tree.join(manifest).is_file()) {
            Some(package) => by_package.entry(package).or_insert_with(|| package_root(tree, package)).clone(),
            None => Err("no Cargo.toml lies above it".to_string()),
        };
        out.insert(path, root);
    }
    out
}

/// The directory, from `tree`'s top, of the root manifest of the workspace of the package in `package`, a directory
/// from `tree`'s top, as cargo tells it. Errs with why, a clause, when cargo cannot tell it.
fn package_root(tree: &Path, package: &Path) -> Result<OsString, String> {
    let nearest = package.join("Cargo.toml");
    let (manifests, problems) = workspaces("cargo-mutants", tree, &[nearest.clone().into_os_string()]);
    match (manifests.first(), problems.first()) {
        (Some(manifest), _) => Ok(manifest.parent().unwrap_or(Path::new("")).as_os_str().to_os_string()),
        (None, Some(problem)) => Err(problem.strip_suffix('.').unwrap_or(problem).to_string()),
        (None, None) => Err(format!("cargo cannot tell the workspace of `{}`", text::path(nearest.as_os_str()))),
    }
}

/// The directory `dir`, from the tree's top, as findings name it, as a Cargo workspace's or a Go module's: `.` for the
/// top.
pub(crate) fn dir_name(dir: &OsStr) -> String {
    if dir.is_empty() {
        ".".to_string()
    } else {
        text::path(dir)
    }
}

/// cargo-mutants' `--re` for the mutants of `f`, a function in the workspace at `root`, a directory from the tree's
/// top, empty for the top, that `f`'s path lies under: each mutant's name, as `--list` shows it, opens with its file's
/// path from the workspace's directory, its line and its column, as `src/lib.rs:2:5: replace add -> u32 with 0`, and
/// the lines of `f` hold every mutant of it.
fn mutants_of(root: &OsStr, f: &Edited) -> String {
    let path = f.path.as_bytes();
    let path = if root.is_empty() { path } else { &path[root.len() + 1..] };
    let lines: Vec<String> = (f.line..=f.last).map(|n| n.to_string()).collect();
    format!("^{}:(?:{}):", regex::escape(&String::from_utf8_lossy(path)), lines.join("|"))
}

/// Where a run of cargo-mutants on one workspace mutates and writes.
struct Run<'a> {
    /// The copy of the tracked files, which `--in-place` mutates.
    tree: &'a Path,
    /// A directory the run makes for its mutants.out, outside the copy.
    scratch: &'a Path,
}

/// The findings of one run of cargo-mutants in the workspace at `root`, a directory from the tree's top, on the
/// mutants of `functions` alone ([`mutants_of`]), as `run` says, within `budget`; and whether the deadline stopped it,
/// or was too near to start it.
fn run_workspace(run: &Run, root: &OsStr, functions: &[&Edited], budget: &Budget) -> (Vec<Finding>, bool) {
    let left = budget.deadline.checked_duration_since(Instant::now() + STOP_GRACE + KILL_WAIT);
    let Some(left) = left.filter(|left| !left.is_zero()) else {
        let message = format!(
            "budget reached before cargo-mutants started on the workspace at `{}`: the {} seconds of \
             `--mutation-budget` left too little to run and stop it, so its mutants were not tried.",
            dir_name(root),
            budget.seconds
        );
        return (vec![finding(None, None, "budget-reached", message)], true);
    };
    if let Err(e) = std::fs::create_dir_all(run.scratch) {
        return (vec![not_run(&format!("could not make the directory for cargo-mutants' output: {e}"))], false);
    }
    // --in-place mutates the copy itself, which cargo-mutants would otherwise copy once more; mutants.out goes to
    // scratch, outside it.
    let dir = run.tree.join(root);
    let timeout = TEST_TIMEOUT.to_string();
    let mut argv: Vec<&OsStr> = ["cargo", "mutants", "--in-place", "--no-shuffle", "--dir"].map(OsStr::new).to_vec();
    argv.push(dir.as_os_str());
    let wanted: Vec<OsString> = functions.iter().map(|f| mutants_of(root, f).into()).collect();
    argv.extend(wanted.iter().flat_map(|re| [OsStr::new("--re"), re.as_os_str()]));
    argv.extend(["--test-tool", "nextest", "--timeout", &timeout, "--output"].map(OsStr::new));
    argv.extend([run.scratch.as_os_str(), OsStr::new("--colors"), OsStr::new("never")]);
    let env = [("CARGO_TERM_COLOR", OsStr::new("never"))];
    let extras = Extras { env: &env, soft_stop: Some(STOP_GRACE), ..Extras::default() };
    match run_with(&argv, run.tree, left, &extras) {
        Ok(done) => (run_findings(&done, &run.scratch.join("mutants.out"), root, budget.seconds), done.stopped),
        Err(e) => (vec![not_run(&format!("could not start cargo-mutants: {e}"))], false),
    }
}

/// The findings of a finished run of cargo-mutants, `done`, on the workspace at `root`, whose mutants.out is
/// `output`, under a budget of `budget` seconds, as findings name it: a `survived` finding for each function with
/// missed mutants ([`survivors`]), and a `budget-reached` one when the deadline stopped it; none when it found nothing
/// to mutate. A `not-run` finding alone when its outcomes cannot be read, as they may not be when it was stopped, or
/// its unmutated baseline failed.
fn run_findings(done: &Finished, output: &Path, root: &OsStr, budget: u64) -> Vec<Finding> {
    // cargo-mutants writes no outcomes when no mutant matches the `--re` filters, and exits 0.
    if done.status == 0 && !done.stopped && output.join("outcomes.json").symlink_metadata().is_err() {
        return vec![];
    }
    let outcomes = match (outcomes(output), done.stopped) {
        (Ok(outcomes), _) => outcomes,
        (Err(_), true) => vec![],
        (Err(problem), false) => {
            return vec![not_run(&format!(
                "cargo-mutants, exiting with status {}, wrote no outcomes the gate can read ({problem}): {}",
                done.status,
                done.said()
            ))]
        }
    };
    let place = format!("the workspace at `{}`", dir_name(root));
    let baseline = outcomes.iter().find(|o| o.scenario == "Baseline").and_then(|o| o.summary.as_str());
    if let Some(summary) = baseline.filter(|&s| s != "Success") {
        return vec![not_run(&format!(
            "the unmutated baseline of {place} did not pass ({summary}, as cargo-mutants words it; its log is \
             mutants.out/log/baseline.log), so it tried no mutant"
        ))];
    }
    let prefix = if root.is_empty() { vec![] } else { [root.as_bytes(), b"/"].concat() };
    let mut out = match survivors(&prefix, &outcomes) {
        Ok(out) => out,
        Err(e) => return vec![not_run(&format!("cargo-mutants wrote a mutant's outcome the gate cannot read ({e})"))],
    };
    if done.stopped {
        let tried = outcomes.iter().filter(|o| o.summary.as_str().is_some_and(|s| TRIED.contains(&s))).count();
        let when = if outcomes.is_empty() {
            format!("before cargo-mutants had tested the unmutated baseline of {place}")
        } else {
            format!("after {tried} of the mutants cargo-mutants made of {place}")
        };
        let message = format!(
            "budget reached {when}: it was stopped at {budget} seconds (`--mutation-budget`), so the mutants it did \
             not try may survive too."
        );
        out.push(finding(None, None, "budget-reached", message));
    }
    out
}

/// One outcome cargo-mutants wrote, as it writes it: its scenario, "Baseline" or `{"Mutant": {...}}`, and its
/// summary, a string, as [`SURVIVED`] and [`TRIED`] name them.
#[derive(Debug, PartialEq)]
struct Outcome {
    scenario: Value,
    summary: Value,
}

/// The outcomes cargo-mutants wrote to outcomes.json in `output`, its mutants.out; cargo-mutants rewrites the file
/// after each mutant, so a run it was stopped in leaves those it finished. Errs with the problem, a clause, when it
/// cannot be read, or an outcome has no scenario or no summary that is a string.
fn outcomes(output: &Path) -> Result<Vec<Outcome>, String> {
    let path = output.join("outcomes.json");
    let named = text::path(path.as_os_str());
    let bytes = std::fs::read(&path).map_err(|e| format!("{named}: {e}"))?;
    let read: Value = serde_json::from_slice(&bytes).map_err(|e| format!("{named}: {e}"))?;
    let Some(Value::Array(outcomes)) = read.get("outcomes") else {
        return Err(format!("{named} holds no list of outcomes"));
    };
    outcomes
        .iter()
        .map(|o| match (o.get("scenario"), o.get("summary")) {
            (Some(scenario), Some(summary @ Value::String(_))) => {
                Ok(Outcome { scenario: scenario.clone(), summary: summary.clone() })
            }
            _ => Err(format!("{named} holds an outcome without a scenario or a summary")),
        })
        .collect()
}

/// A `survived` finding for each function of which `outcomes` hold a missed mutant, at the line its fn's span
/// starts, in the file cargo-mutants names with `prefix`, its workspace's directory and a slash, before it, in the
/// order cargo-mutants first tried a mutant of each: how many of its tried mutants survived, and what the first
/// [`SURVIVOR_EXAMPLES`] of them change, in the order cargo-mutants tried them, by the description in its name, as
/// `line 10: replace > with == in unchecked`. Errs, a clause, when a mutant tried has no file, span or name.
fn survivors(prefix: &[u8], outcomes: &[Outcome]) -> Result<Vec<Finding>, String> {
    // A function by its path, name and fn line; each with its mutants tried, and whether each survived.
    type Function = (OsString, String, u32);
    let mut functions: Vec<(Function, Vec<(bool, Mutant)>)> = vec![];
    for o in outcomes {
        let summary = o.summary.as_str().unwrap_or_default();
        let Some(mutant) = o.scenario.get("Mutant").filter(|m| truthy(m) && TRIED.contains(&summary)) else {
            continue;
        };
        let mutant = Mutant::read(mutant)?;
        let path = OsString::from_vec([prefix, mutant.file.as_bytes()].concat());
        let key = (path, mutant.function.clone(), mutant.fn_line);
        let tried = (summary == SURVIVED, mutant);
        match functions.iter_mut().find(|(k, _)| *k == key) {
            Some((_, mine)) => mine.push(tried),
            None => functions.push((key, vec![tried])),
        }
    }
    let mut out = vec![];
    for ((path, name, line), tried) in functions {
        let survived: Vec<&Mutant> = tried.iter().filter(|(s, _)| *s).map(|(_, m)| m).collect();
        if survived.is_empty() {
            continue;
        }
        let examples: Vec<String> = survived
            .iter()
            .take(SURVIVOR_EXAMPLES)
            .map(|m| format!("line {}: {}", m.line, m.name.split_once(": ").map_or(m.name.as_str(), |(_, d)| d)))
            .collect();
        let message = format!(
            "{} of {} mutants of `{name}` survived its tests: {}.",
            survived.len(),
            tried.len(),
            examples.join("; ")
        );
        out.push(finding(Some(&path), Some(line), "survived", message));
    }
    Ok(out)
}

/// Whether a JSON value counts as true where Python's truth would: not null, false, 0, "", [] or {}.
fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64() != Some(0.0),
        Value::String(s) => !s.is_empty(),
        Value::Array(a) => !a.is_empty(),
        Value::Object(o) => !o.is_empty(),
    }
}

/// A mutant cargo-mutants tried, as [`survivors`] reads it from its scenario's "Mutant".
struct Mutant {
    /// Its file, from its workspace's directory.
    file: String,
    /// Its line, where its span starts.
    line: u32,
    /// Its name, its place, a colon and a space, then what it changes.
    name: String,
    /// Its function's name, `?` when cargo-mutants names none, and the line its fn starts at, the mutant's own when
    /// cargo-mutants gives the function no span.
    function: String,
    fn_line: u32,
}

impl Mutant {
    /// The mutant `m` describes; errs, naming what is missing, when it has no file, span or name.
    fn read(m: &Value) -> Result<Mutant, String> {
        let start_line = |span: Option<&Value>| {
            span.and_then(|s| s.get("start")?.get("line")?.as_u64()).and_then(|n| u32::try_from(n).ok())
        };
        let string = |key: &str| m.get(key).and_then(Value::as_str).map(String::from).ok_or(format!("no `{key}`"));
        let line = start_line(m.get("span")).ok_or("no `span` with a start line")?;
        let function = m.get("function").filter(|f| truthy(f));
        let name = function.and_then(|f| f.get("function_name")?.as_str()).unwrap_or("?");
        let fn_line = function.and_then(|f| f.get("span")).map_or(Ok(line), |span| {
            start_line(Some(span)).ok_or("no function `span` with a start line")
        })?;
        Ok(Mutant { file: string("file")?, line, name: string("name")?, function: name.to_string(), fn_line })
    }
}

/// A `not-run` finding: `problem`, a clause, and that the change's Rust was not mutation-tested.
fn not_run(problem: &str) -> Finding {
    finding(None, None, "not-run", format!("{problem}; the change's Rust was not mutation-tested."))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::scratch::ScratchDir;

    /// One outcome as cargo-mutants 27 writes it to outcomes.json, of a mutant at `line` of src/lib.rs of `function`,
    /// whose fn starts at `fn_line`, named `name` after its place.
    fn mutant(name: &str, summary: &str, function: &str, fn_line: u32, line: u32) -> Value {
        json!({
            "scenario": {"Mutant": {
                "name": format!("src/lib.rs:{line}:7: {name}"),
                "file": "src/lib.rs",
                "span": {"start": {"line": line}},
                "function": {"function_name": function, "span": {"start": {"line": fn_line}}},
            }},
            "summary": summary,
        })
    }

    /// [`mutant`] of `unchecked`, whose fn starts at line 9, at line 10.
    fn unchecked(name: &str, summary: &str) -> Value {
        mutant(name, summary, "unchecked", 9, 10)
    }

    fn baseline(summary: &str) -> Value {
        json!({"scenario": "Baseline", "summary": summary})
    }

    /// `values` as [`outcomes`] reads them.
    fn read(values: &[Value]) -> Vec<Outcome> {
        let dir = ScratchDir::new();
        write_outcomes(&dir, values);
        outcomes(&dir).unwrap()
    }

    fn write_outcomes(dir: &Path, values: &[Value]) {
        std::fs::write(dir.join("outcomes.json"), json!({"outcomes": values}).to_string()).unwrap();
    }

    fn survived(path: &str, line: u32, message: &str) -> Finding {
        finding(Some(OsStr::new(path)), Some(line), "survived", message.into())
    }

    #[test]
    fn missed_survives_caught_and_timeout_are_tried_and_unviable_is_not() {
        let outcomes = read(&[
            baseline("Success"),
            unchecked("replace > with == in unchecked", "MissedMutant"),
            unchecked("replace > with < in unchecked", "CaughtMutant"),
            unchecked("replace unchecked -> bool with true", "Timeout"),
            unchecked("replace unchecked -> bool with false", "Unviable"),
            mutant("replace * with + in checked", "CaughtMutant", "checked", 5, 6),
        ]);
        assert_eq!(survivors(b"engine/", &outcomes), Ok(vec![survived(
            "engine/src/lib.rs",
            9,
            "1 of 3 mutants of `unchecked` survived its tests: line 10: replace > with == in unchecked."
        )]));
    }

    #[test]
    fn a_finding_shows_the_first_examples_only() {
        let outcomes: Vec<Value> =
            "<=>!".chars().map(|op| unchecked(&format!("replace > with {op} in unchecked"), "MissedMutant")).collect();
        let found = survivors(b"", &read(&outcomes)).unwrap();
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].message.matches("replace").count(), SURVIVOR_EXAMPLES);
        assert!(found[0].message.starts_with("4 of 4 mutants of `unchecked` survived its tests: "));
    }

    #[test]
    fn a_survivor_is_named_by_what_cargo_mutants_knows_of_its_function() {
        let mut bare = mutant("replace a: b with c in nameless", "MissedMutant", "unchecked", 9, 12);
        bare["scenario"]["Mutant"]["function"] = json!({});
        let mut spanless = mutant("replace x with y in spanless", "MissedMutant", "spanless", 9, 20);
        spanless["scenario"]["Mutant"]["function"].as_object_mut().unwrap().remove("span");
        let two: Vec<Value> =
            "<=".chars().map(|op| unchecked(&format!("replace > with {op} in unchecked"), "MissedMutant")).collect();
        let found = survivors(b"", &read(&[bare, spanless, two[0].clone(), two[1].clone()])).unwrap();
        let found: Vec<(Option<u32>, &str)> = found.iter().map(|f| (f.line, f.message.as_str())).collect();
        assert_eq!(found, vec![
            (Some(12), "1 of 1 mutants of `?` survived its tests: line 12: replace a: b with c in nameless."),
            (Some(20), "1 of 1 mutants of `spanless` survived its tests: line 20: replace x with y in spanless."),
            (Some(9), "2 of 2 mutants of `unchecked` survived its tests: line 10: replace > with < in unchecked; \
                       line 10: replace > with = in unchecked."),
        ]);
    }

    #[test]
    fn unreadable_outcomes_are_a_problem() {
        let dir = ScratchDir::new();
        assert!(outcomes(&dir).unwrap_err().contains("outcomes.json: No such file"));
        write_outcomes(&dir, &[json!({"scenario": "Baseline"})]);
        let problem = outcomes(&dir).unwrap_err();
        assert!(problem.ends_with("outcomes.json holds an outcome without a scenario or a summary"), "{problem}");
        std::fs::write(dir.join("outcomes.json"), "{}").unwrap();
        assert!(outcomes(&dir).unwrap_err().ends_with("outcomes.json holds no list of outcomes"));
        write_outcomes(&dir, &[baseline("Success")]);
        assert_eq!(outcomes(&dir), Ok(vec![Outcome { scenario: json!("Baseline"), summary: json!("Success") }]));
    }

    /// A function of `path` from the line of its `fn` to `last`.
    fn edited(path: &str, line: u32, last: u32) -> Edited {
        Edited { path: path.into(), name: "f".into(), line, last }
    }

    #[test]
    fn cargo_mutants_selects_a_function_s_mutants_by_its_file_from_the_workspace_and_its_lines() {
        let re = mutants_of(OsStr::new("engine"), &edited("engine/src/a+b.rs", 9, 11));
        assert_eq!(re, r"^src/a\+b\.rs:(?:9|10|11):");
        let re = regex::Regex::new(&re).unwrap();
        for (name, selected) in [
            ("src/a+b.rs:10:5: replace f -> u32 with 0", true),
            ("src/a+b.rs:1:5: replace g -> u32 with 0", false),
            ("src/a+b.rs:110:5: replace h -> u32 with 0", false),
            ("src/x/a+b.rs:10:5: replace f -> u32 with 0", false),
        ] {
            assert_eq!(re.is_match(name), selected, "{name}");
        }
        assert_eq!(mutants_of(OsStr::new(""), &edited("src/lib.rs", 2, 2)), r"^src/lib\.rs:(?:2):");
        // Only the path's start is the workspace's directory, which may recur below it.
        let nested = edited("core/lib/core/src/x.rs", 4, 4);
        assert_eq!(mutants_of(OsStr::new("core"), &nested), r"^lib/core/src/x\.rs:(?:4):");
    }

    #[test]
    fn the_workspace_is_the_one_cargo_tells() {
        let dir = ScratchDir::new();
        let package = "\nversion = '0.1.0'\nedition = '2021'\n";
        for (path, text) in [
            ("Cargo.toml", "[workspace]\nmembers = ['engine']\nexclude = ['tools/x']\n".to_string()),
            ("engine/Cargo.toml", format!("[package]\nname = 'engine'{package}")),
            ("engine/src/lib.rs", String::new()),
            ("tools/x/Cargo.toml", format!("[package]\nname = 'x'{package}")),
            ("tools/x/src/main.rs", "fn main() {}\n".to_string()),
        ] {
            std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.join(path), text).unwrap();
        }
        let root = |path: &str| cargo_roots(&dir, [OsStr::new(path)].into_iter()).remove(OsStr::new(path)).unwrap();
        assert_eq!(root("engine/src/lib.rs"), Ok("".into()));
        std::fs::remove_file(dir.join("Cargo.toml")).unwrap();
        assert_eq!(root("engine/src/lib.rs"), Ok("engine".into()));
        assert_eq!(root("tools/x/src/main.rs"), Ok("tools/x".into()));
        assert_eq!(root("src/lib.rs"), Err("no Cargo.toml lies above it".into()));
    }

    /// A run that exited with `exit_code`, stopped by the deadline or not, having said "it said so".
    fn done(exit_code: i32, stopped: bool) -> Finished {
        Finished { status: exit_code, stdout: vec![], stderr: b"it said so\n".to_vec(), stopped }
    }

    /// [`run_findings`] of `done` in the workspace at engine, under a budget of 30 seconds, having written
    /// `outcomes`, when given, to its mutants.out.
    fn engine_findings(done: &Finished, outcomes: Option<&[Value]>) -> Vec<Finding> {
        let dir = ScratchDir::new();
        if let Some(outcomes) = outcomes {
            write_outcomes(&dir, outcomes);
        }
        let found = run_findings(done, &dir, OsStr::new("engine"), 30);
        found
    }

    #[test]
    fn a_run_that_finds_nothing_to_mutate_gives_nothing_and_one_that_fails_without_outcomes_is_not_run() {
        assert_eq!(engine_findings(&done(0, false), None), vec![]);
        let found = engine_findings(&done(1, false), None);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].rule.as_str(), &found[0].path), ("not-run", &None));
        let message = &found[0].message;
        assert!(message.starts_with("cargo-mutants, exiting with status 1, wrote no outcomes the gate can read ("));
        assert!(message.contains("outcomes.json: No such file"), "{message}");
        assert!(message.ends_with("): it said so; the change's Rust was not mutation-tested."), "{message}");
    }

    #[test]
    fn the_survivors_of_a_run_that_ends_are_found_under_the_workspace() {
        let outcomes = [baseline("Success"), unchecked("replace > with == in unchecked", "MissedMutant")];
        assert_eq!(engine_findings(&done(0, false), Some(&outcomes)), vec![survived(
            "engine/src/lib.rs",
            9,
            "1 of 1 mutants of `unchecked` survived its tests: line 10: replace > with == in unchecked."
        )]);
    }

    #[test]
    fn a_run_stopped_before_its_baseline_is_budget_reached() {
        assert_eq!(engine_findings(&done(-2, true), None), vec![finding(None, None, "budget-reached",
            "budget reached before cargo-mutants had tested the unmutated baseline of the workspace at `engine`: it \
             was stopped at 30 seconds (`--mutation-budget`), so the mutants it did not try may survive too.".into())]);
    }

    #[test]
    fn a_run_stopped_after_some_mutants_counts_those_it_tried() {
        let outcomes = [
            baseline("Success"),
            unchecked("replace > with < in unchecked", "CaughtMutant"),
            unchecked("replace > with == in unchecked", "MissedMutant"),
            unchecked("replace unchecked -> bool with true", "Timeout"),
            unchecked("replace unchecked -> bool with false", "Unviable"),
        ];
        let found = engine_findings(&done(-2, true), Some(&outcomes));
        let rules: Vec<&str> = found.iter().map(|f| f.rule.as_str()).collect();
        assert_eq!(rules, ["survived", "budget-reached"]);
        assert_eq!(found[1].message, "budget reached after 3 of the mutants cargo-mutants made of the workspace at \
            `engine`: it was stopped at 30 seconds (`--mutation-budget`), so the mutants it did not try may survive \
            too.");
    }

    #[test]
    fn a_baseline_that_fails_is_not_run() {
        assert_eq!(engine_findings(&done(4, false), Some(&[baseline("Failure")])), vec![not_run(
            "the unmutated baseline of the workspace at `engine` did not pass (Failure, as cargo-mutants words it; its \
             log is mutants.out/log/baseline.log), so it tried no mutant"
        )]);
    }

    #[test]
    fn a_mutant_s_outcome_the_gate_cannot_read_is_not_run() {
        let nameless = json!({"scenario": {"Mutant": {"name": "no file"}}, "summary": "MissedMutant"});
        let outcomes = [baseline("Success"), nameless];
        assert_eq!(engine_findings(&done(2, false), Some(&outcomes)), vec![not_run(
            "cargo-mutants wrote a mutant's outcome the gate cannot read (no `span` with a start line)"
        )]);
    }

    /// [`run_workspace`] in the copy `tree`, writing under `scratch`, on a function of `path` in the workspace at
    /// `root`, with `seconds` left of a budget of 30.
    fn workspace_run(tree: &Path, scratch: &Path, root: &str, path: &str, seconds: u64) -> (Vec<Finding>, bool) {
        let run = Run { tree, scratch };
        let f = edited(path, 1, 3);
        let budget = Budget { seconds: 30, deadline: Instant::now() + Duration::from_secs(seconds) };
        run_workspace(&run, OsStr::new(root), &[&f], &budget)
    }

    #[test]
    fn a_run_past_its_deadline_starts_nothing() {
        // The tree is missing, so a cargo-mutants started there would be a could-not-start finding.
        let dir = ScratchDir::new();
        let (tree, scratch) = (dir.join("missing"), dir.join("run"));
        let reached = |at: &str| {
            let message = format!("budget reached before cargo-mutants started on the workspace at `{at}`: the 30 \
                                   seconds of `--mutation-budget` left too little to run and stop it, so its mutants \
                                   were not tried.");
            (vec![finding(None, None, "budget-reached", message)], true)
        };
        // 15 seconds are the stop's grace and the wait for what a killed run printed.
        assert_eq!(workspace_run(&tree, &scratch, "engine", "engine/src/a.rs", 15), reached("engine"));
        assert_eq!(workspace_run(&tree, &scratch, "", "src/a.rs", 0), reached("."));
        assert!(!scratch.exists());
    }

    #[test]
    fn an_output_directory_that_cannot_be_made_or_a_cargo_mutants_that_cannot_start_is_not_run() {
        let dir = ScratchDir::new();
        std::fs::write(dir.join("a-file"), "").unwrap();
        let (found, stopped) = workspace_run(&dir, &dir.join("a-file/run"), "engine", "engine/src/a.rs", 600);
        assert!(!stopped);
        assert_eq!(found, vec![not_run("could not make the directory for cargo-mutants' output: Not a directory (os \
                                        error 20)")]);
        let found = workspace_run(&dir.join("missing"), &dir.join("run"), "engine", "engine/src/a.rs", 600);
        assert_eq!(found, (vec![not_run("could not start cargo-mutants: No such file or directory (os error 2)")],
                           false));
        assert_eq!(not_run("it failed"), finding(None, None, "not-run",
                                                 "it failed; the change's Rust was not mutation-tested.".into()));
    }
}
