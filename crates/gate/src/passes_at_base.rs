//! The passes-at-base check: the tests a change adds or edits, run against the code the change started from. A test
//! that passes there does not test the change. Python's tests run with pytest (python_at_base.rs), and Rust's, here,
//! with cargo-nextest.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use checks::{ChangedFile, Finding, Side};
use model::{Lines, Unreadable};
use serde::Deserialize;

use crate::copies::{copy_base_with_tests, failed, finding, in_scratch_directory, sort_by_place, Budget};
use crate::diff::removal_points;
use crate::junit::{junit_outcomes, Outcome};
use crate::parser_checks::{RustChange, Sources};
use crate::process::{run_with, Extras};
use crate::python_at_base::python_at_base;
use crate::{text, Unjudged};

/// The profile of the nextest configuration the runs at the base take with `--config-file`, in place of the copy's
/// own .config/nextest.toml. It writes the JUnit report the gate reads to `<profile>/junit.xml` in the
/// configuration's store directory, which nextest otherwise puts under the workspace's target directory whatever
/// `--target-dir` says (<https://nexte.st/docs/machine-readable/junit/>).
const NEXTEST_PROFILE: &str = "gate-base";

/// cargo-nextest's exit status when the tests could not be built (nextest-metadata's `NextestExitCode::BUILD_FAILED`).
/// Cargo exits with it for any error, rustc's among them.
const NEXTEST_BUILD_FAILED: i32 = 101;

/// The passes-at-base check's findings on `sources`, by path and line, a finding on no path or line first; each rule
/// is the check's own, without its name, and each severity "warning".
///
/// Runs the tests the change adds or edits against the base's code and reports each one that passed there (rule
/// `passes-at-base`): Python's as python_at_base::python_at_base says, then Rust's as [`rust_at_base`] does. Each
/// language's runs may take `budget` seconds, Python's counted from when its copy of the base is made and Rust's
/// from the start of the check, so that Rust's have what Python's left of them.
pub fn passes_at_base(sources: &Sources, budget: u64) -> Vec<Finding> {
    let budget = Budget::starting_now(budget);
    let mut out = python_at_base(&sources.python, budget.seconds);
    out.extend(rust_at_base(&sources.rust, &budget));
    sort_by_place(&mut out);
    out
}

/// The check's finding that `budget` ran out `when`, a clause such as "while it collected the changed tests",
/// stopping `runner`, the test runner as it is named, such as "cargo nextest".
pub fn budget_reached(budget: &Budget, runner: &str, when: &str) -> Finding {
    let message = format!(
        "{runner} was stopped at {} seconds (`--base-run-budget`) {when}, so none of them was judged.",
        budget.seconds
    );
    finding(None, None, "budget-reached", message)
}

/// Rust's findings of the check on `rust`, in no order.
///
/// When the change edits Rust code under test ([`RustChange::edits_code`]), runs the Rust tests it adds or edits
/// ([`changed_tests`]) against the base's code with cargo-nextest, by `budget`'s deadline, and reports at its fn
/// each one that passed there (rule `passes-at-base`): it does not test the change. A change to the tests alone has
/// no "without the change" to run them in.
///
/// The tests run in a copy of the base (copies::copy_base_with_tests) with the change's files that go with the
/// tests ([`beside_tests`]) in place of the base's. A test in a file the change also edits outside its test code, its
/// items marked `#[cfg(test)]`, cannot be separated from the code it tests: it is a `not-run` finding at its fn.
/// Each Cargo target the tests are in ([`CrateLayout::targets`]) is built and run on its own, `cargo nextest run
/// --locked` filtered to its tests, with a target directory of its own outside the copy; one whose tests do not
/// compile there counts as failing, which is what the check expects of a test that needs the change, and gives
/// nothing. A test the run did not report, as one a cfg leaves out at the base, gives nothing, and one built in several
/// targets is reported only when it passed in each.
///
/// The budget running out is a `budget-reached` finding, and each target after it a `not-run` one. A changed test
/// file that does not parse, a test whose target cannot be placed, a copy that cannot be made, submodules the copy
/// leaves empty, and a cargo-nextest that cannot start or fails otherwise are `not-run` findings saying so; a copy
/// that cannot be removed afterwards is a `not-removed` one.
fn rust_at_base(rust: &RustChange, budget: &Budget) -> Vec<Finding> {
    if !rust.edits_code() {
        return vec![];
    }
    let (tests, mut out) = changed_tests(rust);
    let mut separable = vec![];
    for t in tests {
        if !rust.is_code(&t.path) {
            separable.push(t);
        } else {
            let message = format!(
                "`{}` is in a file whose code the change also edits outside its `#[cfg(test)]` items, so it cannot be \
                 run apart from the code it tests, at the base.",
                t.name
            );
            out.push(finding(Some(t.path.as_os_str()), Some(t.line), "not-run", message));
        }
    }
    if !separable.is_empty() {
        out.extend(in_scratch_directory("not-run", "to run the changed Rust tests at the base in", |dir| {
            base_run_findings(rust, dir, &separable, budget)
        }));
    }
    out
}

/// A Rust test the change adds or edits, as [`changed_tests`] finds one, in the working tree.
struct ChangedTest {
    /// Its file's path in the working tree.
    path: OsString,
    /// Its name, the path from its file to it, as model::Test's.
    name: String,
    /// Its fn's line, where a finding on it goes.
    line: u32,
}

/// The Rust tests that the change adds lines to or removes lines from, their outer attributes included, in the files
/// it leaves in the working tree, in the change's order, then by where they start; and a `not-run` finding for each
/// file the change edits that does not parse, when it holds tests by its place or its base held test code.
fn changed_tests(rust: &RustChange) -> (Vec<ChangedTest>, Vec<Finding>) {
    let removals = removal_points(&rust.change.diff);
    let (mut tests, mut out) = (vec![], vec![]);
    for file in &rust.files {
        let Some(after) = &file.after else { continue };
        let removed = removals.get(&after.path).cloned().unwrap_or_default();
        if file.added.is_empty() && removed.is_empty() {
            continue;
        }
        let source = match &after.source {
            Ok(source) => source,
            Err(unreadable) => {
                out.extend(unreadable_tests(file, after, unreadable));
                continue;
            }
        };
        let touched = source.tests.iter().filter(|t| touches(t.lines, &file.added, &removed));
        tests.extend(touched.map(|t| ChangedTest { path: after.path.clone(), name: t.name.clone(), line: t.line }));
    }
    (tests, out)
}

/// Whether the change touches a test that spans `lines` in the working tree, by adding one of them (`added`) or
/// removing lines at one of the `removal_points` (diff::removal_points) from just above its first line, as an
/// `#[ignore]` taken off its top, to just above its last. A removal after its last line, the `}`, is below the test,
/// as a deleted neighbour's is, and does not count.
fn touches(lines: Lines, added: &BTreeSet<u32>, removal_points: &BTreeSet<u32>) -> bool {
    added.iter().any(|&n| lines.holds(n))
        || removal_points.iter().any(|&n| lines.first.saturating_sub(1) <= n && n < lines.last)
}

/// The `not-run` finding on `file`, which the change edits and leaves as `after`, unreadable for `unreadable`'s
/// reason; None when it neither holds tests by its place nor held test code at the base, so no test was lost.
fn unreadable_tests(file: &ChangedFile, after: &Side, unreadable: &Unreadable) -> Option<Finding> {
    let base_source = file.before.as_ref().and_then(|b| b.source.as_ref().ok());
    if !after.among_tests && !base_source.is_some_and(|s| !s.test_lines.is_empty()) {
        return None;
    }
    let message = format!("{}, so its tests were not run at the base.", unreadable.why);
    Some(finding(Some(after.path.as_os_str()), unreadable.line, "not-run", message))
}

/// Whether a repository path belongs with Rust's tests rather than with the code they test, as the copy the tests run
/// in at the base takes it from the working tree: a file gate.toml's `test-support` names, as Cargo's integration
/// tests' data under a tests directory by default, a Rust test file as a whole in the working tree
/// ([`RustChange::among_tests`]), and a .rs file the change edits only inside its test code, which
/// [`RustChange::is_code`] does not count as code.
fn beside_tests(rust: &RustChange, path: &OsStr) -> bool {
    let settings = &rust.change.settings;
    settings.test_support.matches(path)
        || rust.among_tests(path, false)
        || (path.as_bytes().ends_with(b".rs") && !rust.is_code(path))
}

/// The findings on `tests`, built and run within `budget` in a copy of the base made in `dir`, an empty directory,
/// beside the runs' target directory and nextest configuration.
fn base_run_findings(rust: &RustChange, dir: &Path, tests: &[ChangedTest], budget: &Budget) -> Vec<Finding> {
    let (tree, store) = (dir.join("tree"), dir.join("nextest"));
    let copied = copy_base_with_tests(rust.change, &tree, &dir.join("base.tar"), |p| beside_tests(rust, p))
        .and_then(|empty| write_nextest_config(&dir.join("nextest.toml"), &store).map(|()| empty));
    let empty = match copied {
        Ok(empty) => empty,
        Err(e) => {
            let message = format!("could not copy the base to run the changed Rust tests in: {e}; they were not run \
                                   there.");
            return vec![finding(None, None, "not-run", message)];
        }
    };
    let mut out = vec![];
    if !empty.is_empty() {
        let named: Vec<String> = empty.iter().map(|(path, why)| format!("`{path}`, {why}")).collect();
        let message = format!(
            "the copy of the base leaves submodules empty, so a changed Rust test that needs one, if any does, was \
             not judged: {}.",
            named.join("; ")
        );
        out.push(finding(None, None, "not-run", message));
    }
    let layout = match CrateLayout::of(&tree, budget) {
        Ok(layout) => layout,
        Err(e) => {
            let message = format!("could not read the copy of the base's Rust files: {e}; the changed Rust tests were \
                                   not run there.");
            out.push(finding(None, None, "not-run", message));
            return out;
        }
    };
    let (placed, groups) = group_by_target(layout, tests);
    out.extend(placed);
    let run = Run { tree: &tree, target_dir: &dir.join("target"), config: &dir.join("nextest.toml"), store: &store };
    out.extend(run_groups(&run, &groups, tests, budget));
    out
}

/// The Cargo targets that build `tests`, as `layout` places their files, each in the order its first test comes, with
/// the indices in `tests` of the tests it builds; and a `not-run` finding for each test no target builds.
fn group_by_target(mut layout: CrateLayout, tests: &[ChangedTest]) -> (Vec<Finding>, Vec<(Target, Vec<usize>)>) {
    let (mut out, mut groups): (Vec<Finding>, Vec<(Target, Vec<usize>)>) = (vec![], vec![]);
    for (i, t) in tests.iter().enumerate() {
        let targets = match layout.targets(&t.path) {
            Ok(targets) => targets,
            Err(why) => {
                let message = format!("`{}` was not run at the base: {why}.", t.name);
                out.push(finding(Some(t.path.as_os_str()), Some(t.line), "not-run", message));
                continue;
            }
        };
        for target in targets {
            match groups.iter_mut().find(|(seen, _)| *seen == target) {
                Some((_, mine)) => mine.push(i),
                None => groups.push((target, vec![i])),
            }
        }
    }
    (out, groups)
}

/// The findings of running `groups`, targets with the indices of the tests of `tests` each builds, as [`run_target`]
/// runs one, in turn, within `budget`. A test built in several targets, as a module two integration tests share,
/// passes at the base only if it passes in each of them. When the budget stops a run, each target after it is a
/// `not-run` finding.
fn run_groups(run: &Run, groups: &[(Target, Vec<usize>)], tests: &[ChangedTest], budget: &Budget) -> Vec<Finding> {
    let mut out = vec![];
    // Whether each test passed in every target run so far that builds it; false for one in no target.
    let mut passed = vec![false; tests.len()];
    for (_, mine) in groups {
        mine.iter().for_each(|&i| passed[i] = true);
    }
    for (n, (target, mine)) in groups.iter().enumerate() {
        let theirs: Vec<&ChangedTest> = mine.iter().map(|&i| &tests[i]).collect();
        let each = match run_target(run, target, &theirs, budget) {
            Ran::Outcomes(each) => each,
            Ran::NotRun(f) => {
                out.push(f);
                vec![false; mine.len()]
            }
            Ran::Stopped(f) => {
                out.push(f);
                out.extend(groups[n + 1..].iter().map(|(later, _)| {
                    let message = format!(
                        "the run at the base used its {} seconds (`--base-run-budget`) before it reached {}, so its \
                         changed tests were not run there.",
                        budget.seconds,
                        later.label()
                    );
                    finding(None, None, "not-run", message)
                }));
                return out;
            }
        };
        mine.iter().zip(each).for_each(|(&i, ok)| passed[i] &= ok);
    }
    let found = tests.iter().zip(passed).filter(|(_, ok)| *ok).map(|(t, _)| {
        let message = format!("`{}` passes without the change, so it does not test it.", t.name);
        finding(Some(t.path.as_os_str()), Some(t.line), "passes-at-base", message)
    });
    out.extend(found);
    out
}

/// Writes, to a new file at `path`, the nextest configuration of the runs at the base: its store in `store`, and
/// [`NEXTEST_PROFILE`]'s JUnit report there. Errs, a clause, when it cannot.
fn write_nextest_config(path: &Path, store: &Path) -> Result<(), String> {
    use std::io::Write;
    let store = store.to_str().ok_or_else(|| format!("{} is not UTF-8", text::path(store.as_os_str())))?;
    // A JSON string is a TOML basic string.
    let quoted = serde_json::to_string(store).map_err(|e| e.to_string())?;
    let config = format!("[store]\ndir = {quoted}\n\n[profile.{NEXTEST_PROFILE}.junit]\npath = \"junit.xml\"\n");
    let written = std::fs::File::create_new(path).and_then(|mut f| f.write_all(config.as_bytes()));
    written.map_err(|e| format!("cannot write {}: {e}", text::path(path.as_os_str())))
}

/// Where the runs at the base build and what they read.
struct Run<'a> {
    /// The copy of the base.
    tree: &'a Path,
    /// Cargo's target directory, outside the copy.
    target_dir: &'a Path,
    /// The nextest configuration [`write_nextest_config`] wrote, and the store it names.
    config: &'a Path,
    store: &'a Path,
}

/// A Cargo target a changed Rust test is built in at the base, as [`CrateLayout::targets`] places it, and what names
/// the test there.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Target {
    /// The directory, from the copy's top, of the package's Cargo.toml; empty for the top.
    package: OsString,
    /// cargo's target selection, as `["--test", "cli"]` or `["--lib"]`.
    args: Vec<String>,
    /// The module path within the target of the test's file, as `["calc", "tests"]`.
    module: Vec<String>,
}

impl Target {
    /// The target, in prose, as findings name it.
    fn label(&self) -> String {
        let place = if self.package.is_empty() {
            "the top level".to_string()
        } else {
            format!("`{}`", text::path(&self.package))
        };
        format!("`{}` of the package at {place}", self.args.join(" "))
    }
}

/// Which Cargo targets of a copy of the base build which of its Rust files: the module tree its `.rs` files make
/// (lang_rust::modules), each crate root's file matched to a target's by `cargo metadata`.
struct CrateLayout {
    /// The copy, its path with no link in it, as the paths `cargo metadata` gives start.
    tree: PathBuf,
    /// The places of each `.rs` file of the copy, by its path from the copy's top, as lang_rust::modules gives them.
    modules: HashMap<OsString, Vec<(OsString, Vec<String>)>>,
    /// The targets `cargo metadata` lists, run in each directory of the copy it has been run in, by the directory's
    /// path from the copy's top, each with its package's directory from there; or why that failed, a clause.
    packages: HashMap<PathBuf, Result<Vec<(OsString, CargoTarget)>, String>>,
    /// When `cargo metadata` is stopped, as the budget of the runs at the base says.
    deadline: Instant,
}

/// A target of a package, as `cargo metadata` describes it
/// (<https://doc.rust-lang.org/cargo/commands/cargo-metadata.html>).
#[derive(Clone, Deserialize)]
struct CargoTarget {
    name: String,
    /// Its kinds, as `["lib"]`, `["cdylib", "rlib"]`, `["test"]` or `["custom-build"]`.
    kind: Vec<String>,
    /// Its crate root's file, an absolute path.
    src_path: PathBuf,
}

/// What [`CrateLayout`] reads of `cargo metadata`'s output.
#[derive(Deserialize)]
struct CargoMetadata {
    packages: Vec<CargoPackage>,
}

/// A package of [`CargoMetadata`].
#[derive(Deserialize)]
struct CargoPackage {
    targets: Vec<CargoTarget>,
    /// Its Cargo.toml, an absolute path.
    manifest_path: PathBuf,
}

impl CrateLayout {
    /// The layout of the copy of the base at `tree`, its `cargo metadata` runs stopped at `budget`'s deadline. Errs,
    /// a clause, when the copy's files cannot be listed or read.
    fn of(tree: &Path, budget: &Budget) -> Result<CrateLayout, String> {
        let tree = tree.canonicalize().map_err(|e| failed("resolve", tree, &e))?;
        let mut files = vec![];
        rust_files(&tree, Path::new(""), &mut files)?;
        let modules = lang_rust::modules(&files);
        Ok(CrateLayout { tree, modules, packages: HashMap::new(), deadline: budget.deadline })
    }

    /// The targets that build the Rust file at `path`, a path from the copy's top: for each crate root whose module
    /// tree reaches it, the target of the package nearest above that root's file, by `cargo metadata` run there, whose
    /// crate root it is, with the file's module path from that root. A library of any kind is selected by `--lib`, a
    /// target of another kind by its kind and name, as `--test cli`; a build script runs no tests and builds none.
    ///
    /// Errs with why, a clause, when no target builds the file: no Cargo.toml lies above a root of it, `cargo
    /// metadata` fails or is stopped there, or no target is built from it.
    fn targets(&mut self, path: &OsStr) -> Result<Vec<Target>, String> {
        let places = self.modules.get(path).cloned().unwrap_or_default();
        let (mut out, mut whys) = (vec![], vec![]);
        for (root, module) in places {
            match self.target_of_root(&root) {
                Ok((package, args)) => out.push(Target { package, args, module }),
                Err(why) => whys.push(why),
            }
        }
        if out.is_empty() {
            whys.dedup();
            return Err(if whys.is_empty() { "it is not in the copy of the base".to_string() } else { whys.join("; ") });
        }
        Ok(out)
    }

    /// The package directory, from the copy's top, and cargo's target selection of the target whose crate root is
    /// the file at `root`, a path from the copy's top; or why there is none, as [`CrateLayout::targets`] says.
    fn target_of_root(&mut self, root: &OsStr) -> Result<(OsString, Vec<String>), String> {
        let shown = text::path(root);
        let dir = Path::new(root)
            .ancestors()
            .skip(1)
            .find(|dir| self.tree.join(dir).join("Cargo.toml").is_file())
            .ok_or_else(|| format!("no Cargo.toml lies above its crate root `{shown}` at the base"))?
            .to_path_buf();
        if !self.packages.contains_key(&dir) {
            let read = self.cargo_metadata(&dir);
            self.packages.insert(dir.clone(), read);
        }
        let targets = self.packages[&dir].as_ref().map_err(String::clone)?;
        let file = self.tree.join(root);
        let (package, target) = targets
            .iter()
            .find(|(_, t)| t.src_path == file && !t.kind.iter().any(|k| k == "custom-build"))
            .ok_or_else(|| format!("no Cargo target at the base is built from its crate root `{shown}`"))?;
        let kind = ["bin", "test", "bench", "example"].into_iter().find(|k| target.kind.iter().any(|t| t == k));
        let args = match kind {
            Some(kind) => vec![format!("--{kind}"), target.name.clone()],
            None => vec!["--lib".to_string()],
        };
        Ok((package.clone(), args))
    }

    /// The targets `cargo metadata --no-deps`, run in `dir`, a directory from the copy's top, lists, each with its
    /// package's directory from the copy's top. Errs, a clause, when it cannot start, is stopped at the deadline,
    /// fails, or prints what cannot be read so.
    fn cargo_metadata(&self, dir: &Path) -> Result<Vec<(OsString, CargoTarget)>, String> {
        let argv = ["cargo", "metadata", "--no-deps", "--format-version", "1"].map(OsStr::new);
        let extras = Extras { env: &[("CARGO_TERM_COLOR", OsStr::new("never"))], soft_stop: None };
        let left = self.deadline.saturating_duration_since(Instant::now());
        let shown = if dir.as_os_str().is_empty() {
            "the top level".to_string()
        } else {
            format!("`{}`", text::path(dir.as_os_str()))
        };
        let done = run_with(&argv, &self.tree.join(dir), left, &extras)
            .map_err(|e| format!("`cargo metadata` could not start at {shown} at the base: {e}"))?;
        if done.stopped {
            return Err(format!("`cargo metadata` at {shown} was stopped by `--base-run-budget`"));
        }
        if done.status != 0 {
            let said = done.said();
            return Err(format!("`cargo metadata` at {shown} exited with status {} at the base: {said}", done.status));
        }
        let read: CargoMetadata = serde_json::from_slice(&done.stdout)
            .map_err(|e| format!("`cargo metadata` at {shown} printed what the gate cannot read: {e}"))?;
        let mut out = vec![];
        for package in read.packages {
            let manifest_dir = package.manifest_path.parent().unwrap_or(Path::new(""));
            let Ok(from_top) = manifest_dir.strip_prefix(&self.tree) else { continue };
            out.extend(package.targets.into_iter().map(|t| (from_top.as_os_str().to_owned(), t)));
        }
        Ok(out)
    }
}

/// Adds to `out` each `.rs` file below `dir`, a directory from the top of the tree at `tree`, as (its path from the
/// top, its bytes); a link is followed to a file, but not to a directory. Errs, a clause, when a directory cannot be
/// listed or a file read.
fn rust_files(tree: &Path, dir: &Path, out: &mut Vec<(OsString, Vec<u8>)>) -> Result<(), String> {
    let full = tree.join(dir);
    for entry in std::fs::read_dir(&full).map_err(|e| failed("list", &full, &e))? {
        let entry = entry.map_err(|e| failed("list", &full, &e))?;
        let path = dir.join(entry.file_name());
        let kind = entry.file_type().map_err(|e| failed("read", &entry.path(), &e))?;
        if kind.is_dir() {
            rust_files(tree, &path, out)?;
        } else if path.as_os_str().as_bytes().ends_with(b".rs") && entry.path().is_file() {
            let bytes = std::fs::read(entry.path()).map_err(|e| failed("read", &entry.path(), &e))?;
            out.push((path.into_os_string(), bytes));
        }
    }
    Ok(())
}

/// What running a target's changed tests at the base came to.
enum Ran {
    /// Whether each test passed, in the order the tests were given: every case nextest reported under its name
    /// passed. A test nextest did not report, as one a cfg leaves out, did not pass, nor did any of a target whose
    /// tests do not compile, which is what the check expects of tests that need the change.
    Outcomes(Vec<bool>),
    /// The `not-run` finding saying why the tests were not judged.
    NotRun(Finding),
    /// The `budget-reached` finding of the budget that stopped the run.
    Stopped(Finding),
}

/// The outcome of `tests`, all in `target`, built and run with cargo-nextest as `run` says, within `budget`.
fn run_target(run: &Run, target: &Target, tests: &[&ChangedTest], budget: &Budget) -> Ran {
    let reached = |when: String| Ran::Stopped(budget_reached(budget, "cargo nextest", &when));
    let not_run = |message: String| Ran::NotRun(finding(None, None, "not-run", message));
    let left = budget.deadline.saturating_duration_since(Instant::now());
    if left.is_zero() {
        return reached(format!("before it built {}", target.label()));
    }
    let named = |t: &ChangedTest| target.module.iter().chain([&t.name]).cloned().collect::<Vec<_>>().join("::");
    let mut filter: Vec<String> = tests.iter().map(|t| format!("test(={})", named(t))).collect();
    filter.sort();
    filter.dedup();
    let report = run.store.join(NEXTEST_PROFILE).join("junit.xml");
    match std::fs::remove_file(&report) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
            let report = text::path(report.as_os_str());
            return not_run(format!("could not remove the report of an earlier run, {report}: {e}"));
        }
        _ => {}
    }
    let filter = filter.join(" | ");
    let mut argv: Vec<&OsStr> = ["cargo", "nextest", "run", "--locked", "--no-fail-fast", "--no-tests=pass"]
        .map(OsStr::new)
        .to_vec();
    argv.extend([OsStr::new("--target-dir"), run.target_dir.as_os_str(), OsStr::new("--config-file")]);
    argv.extend([run.config.as_os_str(), OsStr::new("--profile"), OsStr::new(NEXTEST_PROFILE)]);
    argv.extend(target.args.iter().map(OsStr::new));
    argv.extend([OsStr::new("-E"), OsStr::new(&filter)]);
    let extras = Extras { env: &[("CARGO_TERM_COLOR", OsStr::new("never"))], soft_stop: None };
    let done = match run_with(&argv, &run.tree.join(&target.package), left, &extras) {
        Ok(done) => done,
        Err(e) => {
            let label = target.label();
            return not_run(format!(
                "could not start cargo nextest to run the changed Rust tests of {label} at the base: {e}"
            ));
        }
    };
    if done.stopped {
        return reached(format!("while it built or ran {}", target.label()));
    }
    // Only a compile error is the tests not compiling.
    let compile_error = regex::Regex::new(r"(?m)^error: could not compile ").expect("a valid pattern");
    if done.status == NEXTEST_BUILD_FAILED && compile_error.is_match(&text::lossless(&done.stderr)) {
        return Ran::Outcomes(vec![false; tests.len()]);
    }
    let cases = match junit_outcomes(&report) {
        Ok(cases) => cases,
        Err(Unjudged(e)) => {
            return not_run(format!(
                "cargo nextest, running the changed Rust tests of {} at the base, exited with status {} and wrote no \
                 report the gate can read ({e}): {}",
                target.label(),
                done.status,
                done.said()
            ))
        }
    };
    // junit_outcomes names a case by its binary's id, a dot and the test's name. A name the target builds into
    // several binaries, as --bins can, or that two tests of one module share under cfgs, passed only if each passed.
    let mut passed: BTreeMap<String, bool> = BTreeMap::new();
    for (case, outcome, _) in cases {
        let name = case.rsplit_once('.').map_or(case.as_str(), |(_, name)| name).to_string();
        *passed.entry(name).or_insert(true) &= outcome == Outcome::Passed;
    }
    Ran::Outcomes(tests.iter().map(|t| passed.get(&named(t)) == Some(&true)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

    fn put(tree: &Path, path: &str) {
        let path = tree.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, "").unwrap();
    }

    #[test]
    fn a_target_names_its_selection_and_package_in_prose() {
        let target = |package: &str, args: &[&str]| Target {
            package: package.into(),
            args: args.iter().map(|a| a.to_string()).collect(),
            module: vec![],
        };
        assert_eq!(target("", &["--test", "cli"]).label(), "`--test cli` of the package at the top level");
        assert_eq!(target("engine", &["--lib"]).label(), "`--lib` of the package at `engine`");
    }

    #[test]
    fn the_targets_of_a_file_are_those_of_the_crate_roots_that_reach_it() {
        let tree = ScratchDir::new();
        let manifest = "[package]\nname = \"calc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                        [[test]]\nname = \"cli\"\npath = \"checks/cli.rs\"\n";
        for (path, source) in [
            ("Cargo.toml", manifest),
            ("src/lib.rs", "mod calc;\n"),
            ("src/calc/mod.rs", "mod tests;\n"),
            ("src/calc/tests.rs", ""),
            ("src/main.rs", "mod cli;\n#[path = \"../support/args.rs\"]\nmod args;\nfn main() {}\n"),
            ("src/cli.rs", ""),
            ("support/args.rs", ""),
            ("checks/cli.rs", ""),
            ("tests/a.rs", "mod common;\n"),
            ("tests/b.rs", "mod common;\n"),
            ("tests/common/mod.rs", ""),
            ("tests/orphan/helpers.rs", ""),
            ("benches/speed.rs", ""),
            ("examples/demo.rs", "fn main() {}\n"),
            ("build.rs", "fn main() {}\n"),
            ("engine/Cargo.toml", "[package]\nname = \"engine\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
            ("engine/src/lib.rs", ""),
        ] {
            put(&tree, path);
            std::fs::write(tree.join(path), source).unwrap();
        }
        let budget = Budget { seconds: 60, deadline: Instant::now() + std::time::Duration::from_secs(60) };
        let mut layout = CrateLayout::of(&tree, &budget).unwrap();
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let target = |package: &str, args: &[&str], module: &[&str]| Target {
            package: package.into(),
            args: words(args),
            module: words(module),
        };
        for (path, expected) in [
            ("src/lib.rs", vec![target("", &["--lib"], &[])]),
            ("src/calc/tests.rs", vec![target("", &["--lib"], &["calc", "tests"])]),
            // A module of src/main.rs, though the package has src/lib.rs, and one a #[path] places.
            ("src/cli.rs", vec![target("", &["--bin", "calc"], &["cli"])]),
            ("support/args.rs", vec![target("", &["--bin", "calc"], &["args"])]),
            // A [[test]] with a path of its own.
            ("checks/cli.rs", vec![target("", &["--test", "cli"], &[])]),
            ("tests/common/mod.rs", vec![
                target("", &["--test", "a"], &["common"]),
                target("", &["--test", "b"], &["common"]),
            ]),
            ("benches/speed.rs", vec![target("", &["--bench", "speed"], &[])]),
            ("examples/demo.rs", vec![target("", &["--example", "demo"], &[])]),
            ("engine/src/lib.rs", vec![target("engine", &["--lib"], &[])]),
        ] {
            let mut found = layout.targets(OsStr::new(path)).unwrap();
            found.sort_by(|a, b| a.args.cmp(&b.args));
            assert_eq!(found, expected, "{path}");
        }
        for (path, why) in [
            ("tests/orphan/helpers.rs", "no Cargo target at the base is built from its crate root \
                                         `tests/orphan/helpers.rs`"),
            ("build.rs", "no Cargo target at the base is built from its crate root `build.rs`"),
            ("src/gone.rs", "it is not in the copy of the base"),
        ] {
            assert_eq!(layout.targets(OsStr::new(path)), Err(why.into()), "{path}");
        }
    }

    #[test]
    fn a_crate_root_with_no_package_or_a_package_cargo_cannot_read_has_no_target() {
        let tree = ScratchDir::new();
        put(&tree, "loose/a.rs");
        put(&tree, "broken/Cargo.toml");
        put(&tree, "broken/src/lib.rs");
        std::fs::write(tree.join("broken/Cargo.toml"), "[package\n").unwrap();
        let budget = Budget { seconds: 60, deadline: Instant::now() + std::time::Duration::from_secs(60) };
        let mut layout = CrateLayout::of(&tree, &budget).unwrap();
        let why = "no Cargo.toml lies above its crate root `loose/a.rs` at the base";
        assert_eq!(layout.targets(OsStr::new("loose/a.rs")), Err(why.into()));
        let why = layout.targets(OsStr::new("broken/src/lib.rs")).unwrap_err();
        assert!(why.starts_with("`cargo metadata` at `broken` exited with status 101 at the base: "), "{why}");
    }

    #[test]
    fn a_cargo_nextest_that_cannot_start_leaves_the_target_not_run() {
        let dir = ScratchDir::new();
        let missing = dir.join("missing");
        let run = Run { tree: &missing, target_dir: &dir, config: &dir, store: &dir };
        let target = Target { package: "".into(), args: vec!["--test".into(), "calc".into()], module: vec![] };
        let budget = Budget { seconds: 60, deadline: Instant::now() + std::time::Duration::from_secs(60) };
        let test = ChangedTest { path: "tests/calc.rs".into(), name: "adds".into(), line: 2 };
        let Ran::NotRun(found) = run_target(&run, &target, &[&test], &budget) else { panic!("the run started") };
        assert!(found.message.starts_with("could not start cargo nextest to run the changed Rust tests of `--test \
                                           calc` of the package at the top level at the base: "), "{}", found.message);
    }

    #[test]
    fn a_budget_spent_before_a_target_is_built_stops_the_runs() {
        let dir = ScratchDir::new();
        let run = Run { tree: &dir, target_dir: &dir, config: &dir, store: &dir };
        let target = Target { package: "".into(), args: vec!["--test".into(), "calc".into()], module: vec![] };
        let budget = Budget { seconds: 1, deadline: Instant::now() };
        let test = ChangedTest { path: "tests/calc.rs".into(), name: "adds".into(), line: 2 };
        let Ran::Stopped(found) = run_target(&run, &target, &[&test], &budget) else { panic!("the run went on") };
        assert_eq!(found, finding(None, None, "budget-reached", "cargo nextest was stopped at 1 seconds \
            (`--base-run-budget`) before it built `--test calc` of the package at the top level, so none of them was \
            judged.".into()));
    }
}
