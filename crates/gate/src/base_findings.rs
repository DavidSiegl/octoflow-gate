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
