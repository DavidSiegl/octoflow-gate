//! The passes-at-base check: the tests a change adds or edits, run against the code the change started from. A test
//! that passes there does not test the change. Python's tests run with pytest (python.rs), Rust's with cargo-nextest
//! (rust.rs) and Go's with go test (go.rs), each in the one copy of the base this module makes; the findings they give
//! alike are here.

mod go;
mod python;
mod rust;

use std::ffi::OsStr;

use checks::Finding;
use model::Unreadable;

use crate::copies::{finding, in_base_copy, sort_by_place, Budget};
use crate::sources::Sources;

use go::go_at_base;
use python::python_at_base;
use rust::rust_at_base;

/// The passes-at-base check's findings on `sources`, by path and line, a finding on no path or line first; each rule
/// is the check's own, without its name, and each severity "warning".
///
/// Runs the tests the change adds or edits against the base's code and reports each one that passed there (rule
/// `passes-at-base`): Python's as [`python_at_base`] says, then Rust's as [`rust_at_base`] does, then Go's as
/// [`go_at_base`] does. The runs of all three may take `budget` seconds, counted from the start of the check, so that
/// Rust's have what Python's left of them, and Go's what both left.
///
/// The three run in turn in one copy of the base (copies::in_base_copy), made only when one of them has tests to run
/// there, with the change's files that go with the tests in place of the base's: those gate.toml places with them
/// (`test-files.support`), and those that go with the tests of any of the three languages (PythonChange::beside_tests,
/// RustChange::beside_tests, GoChange::beside_tests) but for one that a language with tests to run there counts as
/// code under test (PythonChange::is_code, RustChange::is_code, GoChange::is_code). So a Go test that reads a `.py`
/// file under its package's `testdata/` sees the working tree's, unless a changed Python test runs there too, and then
/// one that imports it the base's. A copy that cannot be made is a `not-run` finding in place of all their runs,
/// submodules it leaves empty one more, and a copy that cannot be removed afterwards a `not-removed` one.
pub fn passes_at_base(sources: &Sources, budget: u64) -> Vec<Finding> {
    let budget = Budget::starting_now(budget);
    let (mut out, python) = python_at_base(&sources.python, &budget);
    let (rust_found, rust) = rust_at_base(&sources.rust, &budget);
    let (go_found, go) = go_at_base(&sources.go, &budget);
    out.extend(rust_found.into_iter().chain(go_found));
    let (python_runs, rust_runs, go_runs) = (python.is_some(), rust.is_some(), go.is_some());
    let runs = python.into_iter().chain(rust).chain(go).collect();
    let beside = |p: &OsStr| {
        let support = sources.rust.change.settings.test_support.matches(p);
        let tests = sources.python.beside_tests(p) || sources.rust.beside_tests(p) || sources.go.beside_tests(p);
        let code = (python_runs && sources.python.is_code(p))
            || (rust_runs && sources.rust.is_code(p))
            || (go_runs && sources.go.is_code(p));
        support || (tests && !code)
    };
    let unjudged = "the changed tests were not run at the base";
    out.extend(in_base_copy(sources.rust.change, beside, "not-run", unjudged, runs));
    sort_by_place(&mut out);
    out
}

/// The check's finding that `budget` ran out `when`, a clause such as "while it collected the changed tests",
/// stopping `runner`, the test runner as it is named, such as "cargo nextest".
fn budget_reached(budget: &Budget, runner: &str, when: &str) -> Finding {
    let message = format!(
        "{runner} was stopped at {} seconds (`--base-run-budget`) {when}, so none of them was judged.",
        budget.seconds
    );
    finding(None, None, "budget-reached", message)
}

/// The `not-run` finding on `place`, a Cargo target or a Go package as findings name it, whose changed tests `budget`
/// ran out before. Python's names its tests' group instead (python.rs), which reads as the tests themselves.
fn not_reached(budget: &Budget, place: &str) -> Finding {
    let message = format!(
        "the run at the base used its {} seconds (`--base-run-budget`) before it reached {place}, so its changed tests \
         were not run there.",
        budget.seconds
    );
    finding(None, None, "not-run", message)
}

/// The `not-run` finding on a file the change edits that could not be read, at `path`, for `unreadable`'s reason:
/// the tests in it were not run.
fn unread_not_run((path, unreadable): (&OsStr, &Unreadable)) -> Finding {
    let message = format!("{}, so its tests were not run at the base.", unreadable.why);
    finding(Some(path), unreadable.line, "not-run", message)
}
