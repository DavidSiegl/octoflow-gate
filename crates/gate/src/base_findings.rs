//! The findings the passes-at-base check gives alike on each language's tests, Python's (python_at_base.rs), Rust's
//! (passes_at_base.rs) and Go's (go_at_base.rs): kept apart from all three so that each imports them without one
//! language's module importing another's.

use std::ffi::OsStr;

use checks::Finding;
use model::Unreadable;

use crate::copies::{finding, Budget};

/// The check's finding that `budget` ran out `when`, a clause such as "while it collected the changed tests",
/// stopping `runner`, the test runner as it is named, such as "cargo nextest".
pub fn budget_reached(budget: &Budget, runner: &str, when: &str) -> Finding {
    let message = format!(
        "{runner} was stopped at {} seconds (`--base-run-budget`) {when}, so none of them was judged.",
        budget.seconds
    );
    finding(None, None, "budget-reached", message)
}

/// The `not-run` finding on `place`, a Cargo target or a Go package as findings name it, whose changed tests `budget`
/// ran out before. Python's names its tests' group instead (python_at_base.rs), which reads as the tests themselves.
pub fn not_reached(budget: &Budget, place: &str) -> Finding {
    let message = format!(
        "the run at the base used its {} seconds (`--base-run-budget`) before it reached {place}, so its changed tests \
         were not run there.",
        budget.seconds
    );
    finding(None, None, "not-run", message)
}

/// The `not-run` finding on a file the change edits that could not be read, at `path`, for `unreadable`'s reason:
/// the tests in it were not run.
pub fn unread_not_run((path, unreadable): (&OsStr, &Unreadable)) -> Finding {
    let message = format!("{}, so its tests were not run at the base.", unreadable.why);
    finding(Some(path), unreadable.line, "not-run", message)
}

/// The `not-run` finding on `empty`, the submodules a copy of the base leaves empty, each with its path and why, as
/// copies::copy_base_with_tests gives them, which a changed test of `language`, as "Rust", may need; None for none.
pub fn submodules_left_empty(empty: &[(String, String)], language: &str) -> Option<Finding> {
    if empty.is_empty() {
        return None;
    }
    let named: Vec<String> = empty.iter().map(|(path, why)| format!("`{path}`, {why}")).collect();
    let message = format!(
        "the copy of the base leaves submodules empty, so a changed {language} test that needs one, if any does, was \
         not judged: {}.",
        named.join("; ")
    );
    Some(finding(None, None, "not-run", message))
}
