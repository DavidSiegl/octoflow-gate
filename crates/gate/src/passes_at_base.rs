//! The passes-at-base check: the tests a change adds or edits, run against the code the change started from. A test
//! that passes there does not test the change. Python's tests run with pytest (python_at_base.rs), and Rust's, here,
//! with cargo-nextest.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Instant;

use checks::{ChangedFile, Finding, Side};
use model::{Lines, Unreadable};

use crate::copies::{copy_base_with_tests, finding, in_scratch_directory, sort_by_place, Budget};
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
/// tests ([`beside_tests`]) in place of the base's. A test in an inline `#[cfg(test)]` module of a file the change
/// also edits outside those modules cannot be separated from the code it tests: it is a `not-run` finding at its fn.
/// Each Cargo target the tests are in ([`target`]) is built and run on its own, `cargo nextest run --locked` filtered
/// to its tests, with a target directory of its own outside the copy; one whose tests do not compile there counts as
/// failing, which is what the check expects of a test that needs the change, and gives nothing. A test the run did
/// not report, as one a cfg leaves out at the base, gives nothing.
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
        if rust.change.settings.rust_tests.matches(&t.path) || !rust.is_code(&t.path) {
            separable.push(t);
        } else {
            let message = format!(
                "`{}` is in an inline `#[cfg(test)]` module of a file the change also edits outside it, so it \
                 cannot be run apart from the code it tests, at the base.",
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
/// removing lines at one of the `removal_points` from just above its first line, as an `#[ignore]` taken off its
/// top, to just below its last; a removal just below its last line does not count yet (#57).
fn touches(lines: Lines, added: &BTreeSet<u32>, removal_points: &BTreeSet<u32>) -> bool {
    added.iter().any(|&n| lines.holds(n))
        || removal_points.iter().any(|&n| lines.first.saturating_sub(1) <= n && n <= lines.last)
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
/// tests' data under a tests directory by default, a Rust test file by gate.toml's `test-files.rust`, and a .rs file
/// the change edits only inside its test code, which [`RustChange::is_code`] does not count as code.
fn beside_tests(rust: &RustChange, path: &OsStr) -> bool {
    let settings = &rust.change.settings;
    settings.test_support.matches(path)
        || settings.rust_tests.matches(path)
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
    // The targets in the order their first test comes.
    let mut groups: Vec<(Target, Vec<&ChangedTest>)> = vec![];
    for t in tests {
        match target(&tree, &t.path) {
            Err(why) => {
                let message = format!("`{}` was not run at the base: {why}.", t.name);
                out.push(finding(Some(t.path.as_os_str()), Some(t.line), "not-run", message));
            }
            Ok(target) => match groups.iter_mut().find(|(seen, _)| *seen == target) {
                Some((_, mine)) => mine.push(t),
                None => groups.push((target, vec![t])),
            },
        }
    }
    let run = Run { tree: &tree, target_dir: &dir.join("target"), config: &dir.join("nextest.toml"), store: &store };
    for (n, (target, mine)) in groups.iter().enumerate() {
        let (found, stopped) = target_findings(&run, target, mine, budget);
        out.extend(found);
        if stopped {
            out.extend(groups[n + 1..].iter().map(|(later, _)| {
                let message = format!(
                    "the run at the base used its {} seconds (`--base-run-budget`) before it reached {}, so its \
                     changed tests were not run there.",
                    budget.seconds,
                    later.label()
                );
                finding(None, None, "not-run", message)
            }));
            break;
        }
    }
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

/// The Cargo target a changed Rust test is built in at the base, as [`target`] places it, and what names the test
/// there.
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

/// The Cargo target that builds the test file at `path`, a repository path, in `tree`, the copy of the base, by
/// Cargo's default places for targets: its package is the one whose Cargo.toml is nearest above it; under its
/// tests/, `x.rs` and a directory `x/` that holds a main.rs are the integration test x, a file below `x/` a module of
/// `x/main.rs`; under its src/bin/, likewise the binary x; src/main.rs is the package's binaries; elsewhere under
/// src/, a module of the library where src/lib.rs exists, else of the binaries. A file's module path is its path from
/// that root, as Rust's file modules read it, mod.rs naming its directory.
///
/// Errs with why, a clause, when no package holds it, or Cargo would not find it there by default. A target a
/// manifest places elsewhere (`[[test]] path = ...`), a module of src/main.rs where src/lib.rs exists too, and a
/// module a `#[path]` attribute names are not followed.
fn target(tree: &Path, path: &OsStr) -> Result<Target, String> {
    let parts: Vec<&OsStr> = path.as_bytes().split(|&b| b == b'/').map(OsStr::from_bytes).collect();
    let dirs = &parts[..parts.len() - 1];
    let mut package = None;
    for k in (0..=dirs.len()).rev() {
        let dir: PathBuf = dirs[..k].iter().collect();
        if tree.join(&dir).join("Cargo.toml").is_file() {
            package = Some((dir, k));
            break;
        }
    }
    let Some((package, depth)) = package else { return Err("no Cargo.toml lies above its file at the base".into()) };
    let parts: Vec<String> = parts[depth..].iter().map(|p| p.to_string_lossy().into_owned()).collect();
    // A directory of tests/ or src/bin/ is a target only through the main.rs in it.
    let holder = match parts.as_slice() {
        [src, bin, ..] if src == "src" && bin == "bin" => &parts[..3.min(parts.len())],
        [tests, ..] if tests == "tests" => &parts[..2.min(parts.len())],
        _ => &[],
    };
    if !holder.is_empty() && parts.len() > holder.len() {
        let main: PathBuf = holder.iter().collect();
        if !tree.join(&package).join(&main).join("main.rs").is_file() {
            return Err(format!("no `{}/main.rs` makes a target of its directory", holder.join("/")));
        }
    }
    let (args, module) = default_target(&tree.join(&package), &parts)
        .ok_or_else(|| "Cargo builds no test target from its place by default".to_string())?;
    Ok(Target { package: package.into_os_string(), args, module })
}

/// [`Target`]'s args and module for the file whose path from the package at `package` is `parts`, by the default
/// places [`target`] names; None where Cargo builds no target from that place. A directory of tests/ or src/bin/ is
/// taken for a target, as its main.rs makes it.
fn default_target(package: &Path, parts: &[String]) -> Option<(Vec<String>, Vec<String>)> {
    let mut modules: Vec<String> = parts.to_vec();
    let last = modules.last_mut()?;
    *last = last.strip_suffix(".rs").unwrap_or(last).to_string();
    let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
    let (args, mut rest) = match parts.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
        ["tests", _, ..] => (vec!["--test".to_string(), modules[1].clone()], modules[2..].to_vec()),
        ["src", "bin", _, ..] => (vec!["--bin".to_string(), modules[2].clone()], modules[3..].to_vec()),
        ["src", "main.rs"] => (words(&["--bins"]), vec![]),
        ["src", _, ..] => {
            let lib = package.join("src/lib.rs").is_file();
            (words(&[if lib { "--lib" } else { "--bins" }]), modules[1..].to_vec())
        }
        _ => return None,
    };
    // A target's root file, as src/lib.rs or tests/x/main.rs, and a mod.rs name no module of their own.
    if rest.last().is_some_and(|m| m == "mod") || (rest.len() == 1 && (rest[0] == "lib" || rest[0] == "main")) {
        rest.pop();
    }
    Some((args, rest))
}

/// The findings of `tests`, all in `target`, built and run with cargo-nextest as `run` says, within `budget`; and
/// whether the budget stopped it. A target whose tests do not compile gives nothing, as [`passes_at_base`] says.
fn target_findings(run: &Run, target: &Target, tests: &[&ChangedTest], budget: &Budget) -> (Vec<Finding>, bool) {
    let reached = |when: String| (vec![budget_reached(budget, "cargo nextest", &when)], true);
    let not_run = |message: String| (vec![finding(None, None, "not-run", message)], false);
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
        return (vec![], false);
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
    let found = tests.iter().filter(|t| passed.get(&named(t)) == Some(&true)).map(|t| {
        let message = format!("`{}` passes without the change, so it does not test it.", t.name);
        finding(Some(t.path.as_os_str()), Some(t.line), "passes-at-base", message)
    });
    (found.collect(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A directory under the system's temporary one, empty, named for `name` and the process.
    fn empty_dir(name: &str) -> PathBuf {
        // cargo test runs a binary's tests on threads of one process, so the process id alone would not tell apart
        // two tests' directories.
        static MADE: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = MADE.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("gate-{name}-{}-{n}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

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
    fn where_cargo_builds_a_test_file_and_what_names_its_tests() {
        let tree = empty_dir("targets");
        for path in ["Cargo.toml", "src/lib.rs", "src/bin/tool/main.rs", "tests/cli/main.rs", "engine/Cargo.toml",
                     "engine/tests/run/main.rs", "tools/x/Cargo.toml"] {
            put(&tree, path);
        }
        let words = |w: &[&str]| w.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for (path, package, args, module) in [
            ("tests/cli.rs", "", &["--test", "cli"][..], &[][..]),
            ("tests/cli/main.rs", "", &["--test", "cli"], &[]),
            ("tests/cli/args.rs", "", &["--test", "cli"], &["args"]),
            ("src/lib.rs", "", &["--lib"], &[]),
            ("src/main.rs", "", &["--bins"], &[]),
            ("src/calc/mod.rs", "", &["--lib"], &["calc"]),
            ("src/calc/tests.rs", "", &["--lib"], &["calc", "tests"]),
            ("src/bin/tool/main.rs", "", &["--bin", "tool"], &[]),
            ("src/bin/tool/args.rs", "", &["--bin", "tool"], &["args"]),
            ("engine/tests/run.rs", "engine", &["--test", "run"], &[]),
            ("engine/src/main.rs", "engine", &["--bins"], &[]),
            ("src/bin/solo.rs", "", &["--bin", "solo"], &[]),
            // The top level has no tests/run/main.rs: the package's is the one that counts.
            ("engine/tests/run/args.rs", "engine", &["--test", "run"], &["args"]),
            // engine has no src/lib.rs, though the top level has one.
            ("engine/src/calc.rs", "engine", &["--bins"], &["calc"]),
            ("tools/x/tests/run.rs", "tools/x", &["--test", "run"], &[]),
        ] {
            let expected = Target { package: package.into(), args: words(args), module: words(module) };
            assert_eq!(target(&tree, OsStr::new(path)), Ok(expected), "{path}");
        }
        for (path, why) in [
            ("benches/b.rs", "Cargo builds no test target from its place by default"),
            ("tests/common/mod.rs", "no `tests/common/main.rs` makes a target of its directory"),
            ("src/bin/gone/args.rs", "no `src/bin/gone/main.rs` makes a target of its directory"),
        ] {
            assert_eq!(target(&tree, OsStr::new(path)), Err(why.into()), "{path}");
        }
        std::fs::remove_file(tree.join("Cargo.toml")).unwrap();
        let why = "no Cargo.toml lies above its file at the base";
        assert_eq!(target(&tree, OsStr::new("tests/cli.rs")), Err(why.into()));
        std::fs::remove_dir_all(tree).unwrap();
    }

    #[test]
    fn a_budget_spent_before_a_target_is_built_stops_the_runs() {
        let dir = empty_dir("spent");
        let run = Run { tree: &dir, target_dir: &dir, config: &dir, store: &dir };
        let target = Target { package: "".into(), args: vec!["--test".into(), "calc".into()], module: vec![] };
        let budget = Budget { seconds: 1, deadline: Instant::now() };
        let test = ChangedTest { path: "tests/calc.rs".into(), name: "adds".into(), line: 2 };
        let (found, stopped) = target_findings(&run, &target, &[&test], &budget);
        assert!(stopped);
        assert_eq!(found, vec![finding(None, None, "budget-reached", "cargo nextest was stopped at 1 seconds \
            (`--base-run-budget`) before it built `--test calc` of the package at the top level, so none of them was \
            judged.".into())]);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
