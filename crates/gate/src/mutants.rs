//! The surviving-mutants check for Rust: cargo-mutants mutates what the change adds to Rust code under test, and a
//! function whose mutants its tests do not notice is a finding. gate.py's `surviving_mutants`, its Rust half; the
//! Python half, which runs mutmut, comes with gate-standalone/16.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use checks::Finding;
use serde_json::Value;

use crate::clippy::workspaces;
use crate::copies::{copy_tracked, finding, in_scratch_directory, sort_by_place, Budget};
use crate::parser_checks::RustChange;
use crate::process::{run_with, Extras, Finished};
use crate::text;

/// How each outcome cargo-mutants 27 writes in mutants.out/outcomes.json (a scenario's "summary") counts: a missed
/// mutant survived; a caught one, and one whose tests outlived cargo-mutants' `--timeout`, did not; an unviable one,
/// which does not build, was not tried. "Success" and "Failure" are the unmutated baseline's.
const SURVIVED: &str = "MissedMutant";
const TRIED: [&str; 3] = ["MissedMutant", "CaughtMutant", "Timeout"];

/// The seconds cargo-mutants gives one mutant's tests before it counts them a timeout (its `--timeout`).
const TEST_TIMEOUT: u64 = 60;

/// How long cargo-mutants has, after the SIGINT that stops a run at the deadline, to write what it found and exit
/// before it is killed; gate.py's MUTMUT_STOP_GRACE.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// What a run is given beyond [`STOP_GRACE`] after the deadline: the most process::run_with waits for what a killed
/// process printed (its KILL_WAIT); gate.py's TOOL_KILL_WAIT.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// How many of a function's surviving mutants its finding shows, in the order cargo-mutants tried them.
const SURVIVOR_EXAMPLES: usize = 3;

/// The surviving-mutants check's findings on `rust`, by path and line, a finding on no path or line first; each rule
/// is the check's own, without its name, and each severity "warning".
///
/// Runs cargo-mutants within `budget` seconds on each Cargo workspace that holds a Rust file the change adds lines to
/// outside the tests ([`changed_code`]), in a copy of the files git tracks, as [`on_copy`] says. Each function with
/// mutants that survived is a `survived` finding at the line its fn starts, with how many of the mutants tried of it
/// they were and the first [`SURVIVOR_EXAMPLES`] of them. A change whose added lines hold nothing to mutate, such as
/// a use or a const, gives nothing. The budget running out is a `budget-reached` finding; a file in no workspace
/// cargo can tell is a `not-mutated` one on it; a run that cannot be made or read is a `not-run` finding saying why,
/// and a copy that cannot be removed afterwards a `not-removed` one.
pub fn surviving_mutants(rust: &RustChange, budget: u64) -> Vec<Finding> {
    let budget = Budget::starting_now(budget);
    let paths = changed_code(rust);
    if paths.is_empty() {
        return vec![];
    }
    let mut out = in_scratch_directory("not-run", "to run cargo-mutants on a copy of the tree in", |dir| {
        on_copy(rust, dir, &paths, &budget)
    });
    sort_by_place(&mut out);
    out
}

/// The Rust files, in path order, that the change adds lines to and that are Rust code under test
/// ([`RustChange::is_code`]): regular files of the working tree named *.rs; not a symlink, a submodule or a special
/// file, whose reading could block or never end.
fn changed_code(rust: &RustChange) -> Vec<OsString> {
    let link_or_special = |p: &OsStr| {
        std::fs::symlink_metadata(rust.change.root.join(p)).is_ok_and(|m| !m.file_type().is_file())
    };
    let changed = rust.change.changed.iter().filter(|(_, added)| !added.is_empty());
    let rs = changed.map(|(p, _)| p).filter(|p| p.as_bytes().ends_with(b".rs"));
    rs.filter(|p| !link_or_special(p) && rust.is_code(p)).cloned().collect()
}

/// The findings on `paths`, [`changed_code`]'s, of cargo-mutants run on each Cargo workspace that holds one of them
/// ([`cargo_root`]), in path order of the workspaces, in a copy of the files git tracks made in `dir`, an empty
/// directory, beside each run's diff and output. cargo-mutants picks what to mutate itself: `--in-diff` with the
/// change's diff of the workspace's files ([`diff_under`]) mutates only what the added lines hold, and its tests run
/// with `--test-tool nextest`, each mutant's within [`TEST_TIMEOUT`].
///
/// A run the deadline stops is a `budget-reached` finding beside what the mutants it had tried showed, and each
/// workspace after it a `not-run` one; a copy that cannot be made is the one `not-run` finding.
fn on_copy(rust: &RustChange, dir: &Path, paths: &[OsString], budget: &Budget) -> Vec<Finding> {
    let tree = dir.join("tree");
    if let Err(e) = copy_tracked(&rust.change.root, &tree) {
        return vec![not_run(&format!("could not copy the tree for cargo-mutants: {e}"))];
    }
    let mut out = vec![];
    let mut roots: BTreeMap<OsString, Vec<OsString>> = BTreeMap::new();
    for path in paths {
        match cargo_root(&tree, path) {
            Ok(root) => roots.entry(root).or_default().push(path.clone()),
            Err(why) => out.push(finding(Some(path), None, "not-mutated", format!("was not mutation-tested: {why}."))),
        }
    }
    let roots: Vec<_> = roots.into_iter().collect();
    for (n, (root, mine)) in roots.iter().enumerate() {
        let run = Run { diff: &rust.change.diff, tree: &tree, scratch: &dir.join(format!("run-{n}")) };
        let (found, stopped) = run_workspace(&run, root, mine, budget);
        out.extend(found);
        if stopped {
            out.extend(roots[n + 1..].iter().map(|(later, _)| {
                not_run(&format!(
                    "the budget ran out before cargo-mutants reached the workspace at `{}`",
                    workspace_name(later)
                ))
            }));
            break;
        }
    }
    out
}

/// The directory, from `tree`'s top, empty for the top itself, that cargo-mutants runs in to mutate the Rust file at
/// `path`, a repository path: that of the root manifest of the workspace, as cargo tells it (clippy::workspaces), of
/// the package whose Cargo.toml is nearest above it. Errs with why, a clause, when no Cargo.toml lies above it, or
/// cargo cannot tell its workspace.
fn cargo_root(tree: &Path, path: &OsStr) -> Result<OsString, String> {
    let dirs: Vec<&[u8]> = path.as_bytes().split(|&b| b == b'/').collect();
    let dirs = &dirs[..dirs.len() - 1];
    let nearest = (0..=dirs.len())
        .rev()
        .map(|k| dirs[..k].iter().map(|d| OsStr::from_bytes(d)).collect::<PathBuf>().join("Cargo.toml"))
        .find(|manifest| tree.join(manifest).is_file())
        .ok_or_else(|| "no Cargo.toml lies above it".to_string())?;
    let (manifests, problems) = workspaces("cargo-mutants", tree, &[nearest.clone().into_os_string()]);
    match (manifests.first(), problems.first()) {
        (Some(manifest), _) => Ok(manifest.parent().unwrap_or(Path::new("")).as_os_str().to_os_string()),
        (None, Some(problem)) => Err(problem.strip_suffix('.').unwrap_or(problem).to_string()),
        (None, None) => Err(format!("cargo cannot tell the workspace of `{}`", text::path(nearest.as_os_str()))),
    }
}

/// The workspace directory `root`, from the tree's top, as findings name it: `.` for the top.
fn workspace_name(root: &OsStr) -> String {
    if root.is_empty() {
        ".".to_string()
    } else {
        text::path(root)
    }
}

/// The sections of `diff`, the change's `git diff`, of the files at `paths`, each under `root`, a directory from the
/// repository's top, empty for the top, with their paths made relative to `root`, as cargo-mutants' `--in-diff` reads
/// them from the directory it runs in.
fn diff_under(diff: &[u8], root: &OsStr, paths: &[OsString]) -> Vec<u8> {
    let prefix = if root.is_empty() { vec![] } else { [root.as_bytes(), b"/"].concat() };
    let wanted: Vec<Vec<u8>> = paths.iter().map(|p| [b" b/", p.as_bytes()].concat()).collect();
    let starts = (0..diff.len()).filter(|&k| (k == 0 || diff[k - 1] == b'\n') && diff[k..].starts_with(b"diff --git "));
    let starts: Vec<usize> = starts.chain([diff.len()]).collect();
    let mut out = vec![];
    for pair in starts.windows(2) {
        let section = &diff[pair[0]..pair[1]];
        let first = section.split(|&b| b == b'\n').next().unwrap_or_default();
        if !wanted.iter().any(|w| first.ends_with(w)) {
            continue;
        }
        // Only the header, before the first hunk, names the paths; a hunk's lines are the file's, kept as they are.
        let body = (0..section.len())
            .find(|&k| (k == 0 || section[k - 1] == b'\n') && section[k..].starts_with(b"@@"))
            .unwrap_or(section.len());
        let head = replace(&section[..body], &[b"a/", &prefix[..]].concat(), b"a/");
        out.extend(replace(&head, &[b"b/", &prefix[..]].concat(), b"b/"));
        out.extend(&section[body..]);
    }
    out
}

/// `bytes` with each occurrence of `from`, left to right and not overlapping, replaced with `to`.
fn replace(bytes: &[u8], from: &[u8], to: &[u8]) -> Vec<u8> {
    let mut out = vec![];
    let mut k = 0;
    while k < bytes.len() {
        if bytes[k..].starts_with(from) {
            out.extend(to);
            k += from.len();
        } else {
            out.push(bytes[k]);
            k += 1;
        }
    }
    out
}

/// What a run of cargo-mutants on one workspace reads and where it writes.
struct Run<'a> {
    /// The change's diff, of which the run's [`diff_under`] takes the workspace's part.
    diff: &'a [u8],
    /// The copy of the tracked files, which `--in-place` mutates.
    tree: &'a Path,
    /// A directory the run makes for the diff it reads and its mutants.out, outside the copy.
    scratch: &'a Path,
}

/// The findings of one run of cargo-mutants in the workspace at `root` on what the change adds to `paths`, as `run`
/// says, within `budget`; and whether the deadline stopped it, or was too near to start it.
fn run_workspace(run: &Run, root: &OsStr, paths: &[OsString], budget: &Budget) -> (Vec<Finding>, bool) {
    let diff = diff_under(run.diff, root, paths);
    if diff.is_empty() {
        return (vec![], false);
    }
    let left = budget.deadline.checked_duration_since(Instant::now() + STOP_GRACE + KILL_WAIT);
    let Some(left) = left.filter(|left| !left.is_zero()) else {
        let message = format!(
            "budget reached before cargo-mutants started on the workspace at `{}`: the {} seconds of \
             `--mutation-budget` left too little to run and stop it, so its mutants were not tried.",
            workspace_name(root),
            budget.seconds
        );
        return (vec![finding(None, None, "budget-reached", message)], true);
    };
    let diff_file = run.scratch.join("change.diff");
    if let Err(e) = std::fs::create_dir_all(run.scratch).and_then(|()| std::fs::write(&diff_file, &diff)) {
        return (vec![not_run(&format!("could not write the change's diff for cargo-mutants: {e}"))], false);
    }
    // --in-place mutates the copy itself, which cargo-mutants would otherwise copy once more; mutants.out goes to
    // scratch, outside it.
    let dir = run.tree.join(root);
    let timeout = TEST_TIMEOUT.to_string();
    let mut argv: Vec<&OsStr> = ["cargo", "mutants", "--in-place", "--no-shuffle", "--dir"].map(OsStr::new).to_vec();
    argv.extend([dir.as_os_str(), OsStr::new("--in-diff"), diff_file.as_os_str()]);
    argv.extend(["--test-tool", "nextest", "--timeout", &timeout, "--output"].map(OsStr::new));
    argv.extend([run.scratch.as_os_str(), OsStr::new("--colors"), OsStr::new("never")]);
    let extras = Extras { env: &[("CARGO_TERM_COLOR", OsStr::new("never"))], soft_stop: Some(STOP_GRACE) };
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
    // cargo-mutants writes no outcomes when it finds nothing to mutate in the diff, and exits 0.
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
    let place = format!("the workspace at `{}`", workspace_name(root));
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

    /// A directory under the system's temporary one, empty, named for `name` and the process.
    fn empty_dir(name: &str) -> PathBuf {
        // cargo test runs a binary's tests on threads of one process, so the process id alone would not tell apart
        // two tests' directories.
        static MADE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = MADE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("gate-mutants-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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
        let dir = empty_dir("read");
        write_outcomes(&dir, values);
        let read = outcomes(&dir).unwrap();
        std::fs::remove_dir_all(dir).unwrap();
        read
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
        let dir = empty_dir("unreadable");
        assert!(outcomes(&dir).unwrap_err().contains("outcomes.json: No such file"));
        write_outcomes(&dir, &[json!({"scenario": "Baseline"})]);
        let problem = outcomes(&dir).unwrap_err();
        assert!(problem.ends_with("outcomes.json holds an outcome without a scenario or a summary"), "{problem}");
        std::fs::write(dir.join("outcomes.json"), "{}").unwrap();
        assert!(outcomes(&dir).unwrap_err().ends_with("outcomes.json holds no list of outcomes"));
        write_outcomes(&dir, &[baseline("Success")]);
        assert_eq!(outcomes(&dir), Ok(vec![Outcome { scenario: json!("Baseline"), summary: json!("Success") }]));
        std::fs::remove_dir_all(dir).unwrap();
    }

    const DIFF: &str = "diff --git a/engine/src/a.rs b/engine/src/a.rs\n--- a/engine/src/a.rs\n\
                        +++ b/engine/src/a.rs\n@@ -1 +1 @@\n-x\n+y\n";

    fn diff_under_str(diff: &str, root: &str, paths: &[&str]) -> String {
        let paths: Vec<OsString> = paths.iter().map(OsString::from).collect();
        String::from_utf8(diff_under(diff.as_bytes(), OsStr::new(root), &paths)).unwrap()
    }

    #[test]
    fn the_diff_cargo_mutants_reads_is_the_workspace_s_files_from_its_directory() {
        let diff = "diff --git a/ui/src/a.rs b/ui/src/a.rs\n--- a/ui/src/a.rs\n+++ b/ui/src/a.rs\n@@ -1 +1 @@\n-x\n+y\n\
                    diff --git a/gate.py b/gate.py\n--- a/gate.py\n+++ b/gate.py\n@@ -1 +1 @@\n-x\n+y\n";
        assert_eq!(diff_under_str(diff, "ui", &["ui/src/a.rs"]),
                   "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n@@ -1 +1 @@\n-x\n+y\n");
        let py = diff.find("diff --git a/gate.py").unwrap();
        assert_eq!(diff_under_str(diff, "", &["gate.py"]), &diff[py..]);
        // A line of the file that reads like a path under the workspace is the file's, and stays.
        let code = diff.replacen("+y\n", "+let p = \"a/ui/b/ui/\";\n", 1);
        assert!(diff_under_str(&code, "ui", &["ui/src/a.rs"]).contains("+let p = \"a/ui/b/ui/\";\n"));
        // So does a line before the first section, which no file's.
        assert_eq!(diff_under_str(&format!("noise\n{DIFF}"), "engine", &["engine/src/a.rs"]),
                   DIFF.replace("/engine/", "/"));
    }

    #[test]
    fn the_diff_of_several_files_keeps_each_section() {
        let diff = format!("{DIFF}{}", DIFF.replace("a.rs", "b.rs"));
        assert_eq!(diff_under_str(&diff, "engine", &["engine/src/a.rs", "engine/src/b.rs"]),
                   diff.replace("/engine/", "/"));
    }

    #[test]
    fn the_workspace_is_the_one_cargo_tells() {
        let dir = empty_dir("workspaces");
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
        let root = |path: &str| cargo_root(&dir, OsStr::new(path));
        assert_eq!(root("engine/src/lib.rs"), Ok("".into()));
        std::fs::remove_file(dir.join("Cargo.toml")).unwrap();
        assert_eq!(root("engine/src/lib.rs"), Ok("engine".into()));
        assert_eq!(root("tools/x/src/main.rs"), Ok("tools/x".into()));
        assert_eq!(root("src/lib.rs"), Err("no Cargo.toml lies above it".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// A run that exited with `exit_code`, stopped by the deadline or not, having said "it said so".
    fn done(exit_code: i32, stopped: bool) -> Finished {
        Finished { status: exit_code, stdout: vec![], stderr: b"it said so\n".to_vec(), stopped }
    }

    /// [`run_findings`] of `done` in the workspace at engine, under a budget of 30 seconds, having written
    /// `outcomes`, when given, to its mutants.out.
    fn engine_findings(done: &Finished, outcomes: Option<&[Value]>) -> Vec<Finding> {
        let dir = empty_dir("run");
        if let Some(outcomes) = outcomes {
            write_outcomes(&dir, outcomes);
        }
        let found = run_findings(done, &dir, OsStr::new("engine"), 30);
        std::fs::remove_dir_all(dir).unwrap();
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

    /// [`run_workspace`] in the copy `tree`, writing under `scratch`, on `paths` of the workspace at `root` of
    /// [`DIFF`], or of its version at the top when `root` is empty, with `seconds` left of a budget of 30.
    fn workspace_run(tree: &Path, scratch: &Path, root: &str, paths: &[&str], seconds: u64) -> (Vec<Finding>, bool) {
        let diff = if root.is_empty() { DIFF.replace("engine/", "") } else { DIFF.to_string() };
        let run = Run { diff: diff.as_bytes(), tree, scratch };
        let paths: Vec<OsString> = paths.iter().map(OsString::from).collect();
        let budget = Budget { seconds: 30, deadline: Instant::now() + Duration::from_secs(seconds) };
        run_workspace(&run, OsStr::new(root), &paths, &budget)
    }

    #[test]
    fn a_run_past_its_deadline_or_with_no_diff_starts_nothing() {
        // The tree is missing, so a cargo-mutants started there would be a could-not-start finding.
        let dir = empty_dir("unstarted");
        let (tree, scratch) = (dir.join("missing"), dir.join("run"));
        assert_eq!(workspace_run(&tree, &scratch, "engine", &["engine/src/b.rs"], 600), (vec![], false));
        let reached = |at: &str| {
            let message = format!("budget reached before cargo-mutants started on the workspace at `{at}`: the 30 \
                                   seconds of `--mutation-budget` left too little to run and stop it, so its mutants \
                                   were not tried.");
            (vec![finding(None, None, "budget-reached", message)], true)
        };
        // 15 seconds are the stop's grace and the wait for what a killed run printed.
        assert_eq!(workspace_run(&tree, &scratch, "engine", &["engine/src/a.rs"], 15), reached("engine"));
        assert_eq!(workspace_run(&tree, &scratch, "", &["src/a.rs"], 0), reached("."));
        assert!(!scratch.exists());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_diff_that_cannot_be_written_or_a_cargo_mutants_that_cannot_start_is_not_run() {
        let dir = empty_dir("unwritten");
        std::fs::write(dir.join("a-file"), "").unwrap();
        let (found, stopped) = workspace_run(&dir, &dir.join("a-file/run"), "engine", &["engine/src/a.rs"], 600);
        assert!(!stopped);
        assert_eq!(found, vec![not_run("could not write the change's diff for cargo-mutants: Not a directory (os \
                                        error 20)")]);
        let found = workspace_run(&dir.join("missing"), &dir.join("run"), "engine", &["engine/src/a.rs"], 600);
        assert_eq!(found, (vec![not_run("could not start cargo-mutants: No such file or directory (os error 2)")],
                           false));
        assert_eq!(not_run("it failed"), finding(None, None, "not-run",
                                                 "it failed; the change's Rust was not mutation-tested.".into()));
        std::fs::remove_dir_all(dir).unwrap();
    }
}
