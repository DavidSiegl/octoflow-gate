//! The Go tests a change adds or edits, and `go test` runs of some of them in a copy of a tree: what flaky-tests' and
//! passes-at-base's Go halves share, as pytest.rs is for their Python halves.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use model::Unreadable;
use serde::Deserialize;

use crate::changed_tests::{changed_tests as changed_tests_of, ChangedTest};
use crate::junit::Outcome;
use crate::parser_checks::GoChange;
use crate::process::{run_with, Extras, Finished};

/// Go tests by the directory of their package, from the tree's top, `.` for the top.
pub type Packages = BTreeMap<PathBuf, Vec<ChangedTest>>;

/// The Go tests that `go` touches as changed_tests::changed_tests says, each a top-level `TestX`, `FuzzX` or `ExampleX`
/// of a `_test.go` file, as `go test -run` selects one: a subtest runs with its test, and a benchmark only under
/// `-bench`, by their package ([`Packages`]); and each `_test.go` file the change edits that could not be read, with
/// why.
pub fn changed_tests<'a>(go: &'a GoChange) -> (Packages, Vec<(&'a OsStr, &'a Unreadable)>) {
    let runs = |t: &model::Test| {
        !t.name.contains('/') && ["Test", "Fuzz", "Example"].iter().any(|prefix| t.name.starts_with(prefix))
    };
    let (tests, unread) = changed_tests_of(go.change, &go.files, runs);
    let mut by_package = Packages::new();
    for t in tests.into_iter().filter(|t| lang_go::among_tests(t.path.as_encoded_bytes())) {
        let dir = Path::new(&t.path).parent().filter(|dir| !dir.as_os_str().is_empty()).unwrap_or(Path::new("."));
        by_package.entry(dir.to_path_buf()).or_default().push(t);
    }
    let unread = unread.into_iter().filter(|(path, _)| lang_go::among_tests(path.as_encoded_bytes())).collect();
    (by_package, unread)
}

/// The package in the directory `dir`, from the tree's top, as findings name it: "the package at `sub`", or "the
/// package at the top level" for `.`.
pub fn label(dir: &Path) -> String {
    if dir == Path::new(".") {
        "the package at the top level".into()
    } else {
        format!("the package at `{}`", crate::text::path(dir.as_os_str()))
    }
}

/// What a [`run`] that finished told.
#[derive(Debug, PartialEq, Eq)]
pub enum Ran {
    /// The outcome go test reported for each of the tests it was given that it ran: Failed, Skipped or Passed. A test
    /// it did not report, as a name no function of the package has, is left out.
    Outcomes(HashMap<String, Outcome>),
    /// The package's tests did not build or load, as `unbuilt` tells it: what the test files need is not there.
    Unbuilt,
}

/// A [`run`] that told nothing.
#[derive(Debug)]
pub struct Problem {
    /// What went wrong, as a clause a finding can open with: "go test, rerunning them under seed 3, exited with status
    /// 2: ...".
    pub clause: String,
    /// Whether it was its timeout that stopped go test.
    pub overran: bool,
}

/// One line of `go test -json`'s output (`go doc test2json`), with what the gate reads of it.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Event {
    action: String,
    /// The test the event is about, a subtest after a slash; None for one about the package.
    test: Option<String>,
    /// What the line printed, when the action is "output".
    output: Option<String>,
    /// The package that did not build, on the package's "fail" when its tests did not; go 1.24 and later only.
    failed_build: Option<String>,
}

/// Whether go test's standard output, `stdout`, of which `events` are the JSON lines, tells that the package's tests
/// did not build or load: by an event's `FailedBuild`, or, from a go before 1.24, which has no such field, by the
/// package's `FAIL\t<package> [build failed]` line, as an event's output, or `FAIL\t<package> [setup failed]`, as a
/// plain line outside the JSON, when an import cannot be found.
fn unbuilt(stdout: &[u8], events: &[Event]) -> bool {
    let says_unbuilt = |line: &str| {
        let line = line.trim_end();
        line.starts_with("FAIL\t") && (line.ends_with(" [build failed]") || line.ends_with(" [setup failed]"))
    };
    let plain = stdout.split(|&b| b == b'\n').filter_map(|line| std::str::from_utf8(line).ok());
    let package_output = events.iter().filter(|e| e.test.is_none()).filter_map(|e| e.output.as_deref());
    events.iter().any(|e| e.failed_build.is_some()) || plain.chain(package_output).any(says_unbuilt)
}

/// Builds the tests of the Go package in `dir` into `binary`, a path that does not exist yet, as `go test -c -o
/// <binary> .`, for at most `timeout`, as process::run_with runs a tool: the [`run`]s in `dir` after it then find the
/// package and what it imports in go's build cache, so that their own timeouts go to the tests alone. A build that
/// fails is left for those runs to tell ([`Ran::Unbuilt`]). Errs with a [`Problem`], its clause naming `doing`, as
/// "building the package's tests", when go could not start or took longer than `timeout`.
pub fn build(dir: &Path, binary: &Path, timeout: Duration, doing: &str) -> Result<(), Problem> {
    let argv = ["go", "test", "-c", "-o"].map(OsStr::new).into_iter().chain([binary.as_os_str(), OsStr::new(".")]);
    let argv: Vec<&OsStr> = argv.collect();
    let done = run_with(&argv, dir, timeout, &Extras::default())
        .map_err(|e| Problem { clause: format!("could not start go test, {doing}: {e}"), overran: false })?;
    if done.stopped {
        let clause = format!("go test, {doing}, did not finish within {} seconds", timeout.as_secs_f64());
        return Err(Problem { clause, overran: true });
    }
    Ok(())
}

/// Runs the tests `names` of the Go package in `dir`, once, as `go test -json -count=1 <flags> -run '^(names)$' .`,
/// for at most `timeout`, as process::run_with runs a tool, killed at once at the timeout; `flags` are go test's,
/// such as `-shuffle=3`. Gives what it told ([`Ran`]); a [`Problem`], its clause naming `doing`, as "running the
/// changed tests at the base", when go could not start, took longer than `timeout`, exited with a status other than 0
/// or 1, or failed without reporting a test or a build that failed, as a TestMain that exits does.
///
/// `-count=1` keeps the go tool's test cache from answering in place of a run. The names must be Go identifiers, which
/// `-run` reads as a regular expression.
pub fn run(dir: &Path, names: &[&str], flags: &[&str], timeout: Duration, doing: &str) -> Result<Ran, Problem> {
    let pattern = format!("^({})$", names.join("|"));
    let mut argv: Vec<&OsStr> = ["go", "test", "-json", "-count=1"].map(OsStr::new).to_vec();
    argv.extend(flags.iter().map(OsStr::new));
    argv.extend([OsStr::new("-run"), OsStr::new(&pattern), OsStr::new(".")]);
    let problem = |clause: String| Problem { clause, overran: false };
    let done = run_with(&argv, dir, timeout, &Extras::default())
        .map_err(|e| problem(format!("could not start go test, {doing}: {e}")))?;
    if done.stopped {
        let clause = format!("go test, {doing}, did not finish within {} seconds", timeout.as_secs_f64());
        return Err(Problem { clause, overran: true });
    }
    let events: Vec<Event> =
        done.stdout.split(|&b| b == b'\n').filter_map(|line| serde_json::from_slice(line).ok()).collect();
    let said = || {
        let printed: String = events.iter().filter_map(|e| e.output.as_deref()).collect();
        let stderr = done.stderr.clone();
        Finished { status: done.status, stdout: printed.into_bytes(), stderr, stopped: false }.said()
    };
    if ![0, 1].contains(&done.status) {
        return Err(problem(format!("go test, {doing}, exited with status {}: {}", done.status, said())));
    }
    if unbuilt(&done.stdout, &events) {
        return Ok(Ran::Unbuilt);
    }
    let asked: HashSet<&str> = names.iter().copied().collect();
    let mut outcomes = HashMap::new();
    for event in &events {
        let Some(test) = event.test.as_deref().filter(|test| asked.contains(test)) else { continue };
        let outcome = match event.action.as_str() {
            "pass" => Outcome::Passed,
            "fail" => Outcome::Failed,
            "skip" => Outcome::Skipped,
            _ => continue,
        };
        outcomes.insert(test.to_string(), outcome);
    }
    if done.status == 1 && !outcomes.values().any(|outcome| *outcome == Outcome::Failed) {
        return Err(problem(format!("go test, {doing}, failed without a test failing: {}", said())));
    }
    Ok(Ran::Outcomes(outcomes))
}
