//! Go's half of the passes-at-base check: the Go tests a change adds or edits, run with go test against the code the
//! change started from.

use std::time::Instant;

use checks::Finding;

use crate::base_findings::{budget_reached, not_reached, unread_not_run};
use crate::copies::{finding, Budget, OnCopy, ScratchCopy};
use crate::gotest::{self, Packages, Ran};
use crate::junit::Outcome;
use crate::sources::GoChange;

/// Go's findings of the passes-at-base check on `go`, in no order: those it gives before the run at the base, and that
/// run on the check's copy of the base, None when there is no test to run there.
///
/// When the change edits Go code under test ([`GoChange::edits_code`]), runs the Go tests it adds or edits
/// (gotest::changed_tests) against the base's code, by `budget`'s deadline, and reports at its func each one that
/// passed there (rule `passes-at-base`): it does not test the change. A change to the tests alone has no "without the
/// change" to run them in.
///
/// Each package's changed tests run together, once, from the package's directory in the copy, as `go test -json
/// -count=1 -run '^(TestA|TestB)$' .`. A package whose tests do not build there counts as failing each of them, which
/// is what the check expects of a test that needs the change, and gives nothing; so does a test that fails or skips
/// there, or that the run does not report.
///
/// The budget running out is a `budget-reached` finding, and each package after it a `not-run` one. A changed test
/// file that cannot be read or parsed, and a go test that cannot start or tells nothing (gotest::run), are `not-run`
/// findings saying so.
pub fn go_at_base<'a>(go: &'a GoChange, budget: &'a Budget) -> (Vec<Finding>, Option<OnCopy<'a>>) {
    if !go.edits_code() {
        return (vec![], None);
    }
    let (packages, unread) = gotest::changed_tests(go);
    let out = unread.into_iter().map(unread_not_run).collect();
    let run = (!packages.is_empty())
        .then(|| -> OnCopy { Box::new(move |copy| base_run_findings(copy, &packages, budget)) });
    (out, run)
}

/// The findings on `packages`, run within `budget` in `copy`, a copy of the base.
fn base_run_findings(copy: &ScratchCopy, packages: &Packages, budget: &Budget) -> Vec<Finding> {
    let tree = &copy.tree;
    let mut out = vec![];
    for (n, (package, tests)) in packages.iter().enumerate() {
        let label = gotest::label(package);
        let left = budget.deadline.saturating_duration_since(Instant::now());
        let names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
        let doing = format!("running the changed tests of {label} at the base");
        let ran = (!left.is_zero()).then(|| gotest::run(&tree.join(package), &names, &[], left, &doing));
        let outcomes = match ran {
            Some(Ok(Ran::Outcomes(outcomes))) => outcomes,
            Some(Ok(Ran::Unbuilt)) => continue,
            None | Some(Err(gotest::Problem { overran: true, .. })) => {
                let when = if left.is_zero() { "before it ran" } else { "while it ran" };
                out.push(budget_reached(budget, "go test", &format!("{when} the changed tests of {label}")));
                out.extend(packages.keys().skip(n + 1).map(|later| not_reached(budget, &gotest::label(later))));
                break;
            }
            Some(Err(problem)) => {
                out.push(finding(None, None, "not-run", format!("{}; they were not run there.", problem.clause)));
                continue;
            }
        };
        for t in tests.iter().filter(|t| outcomes.get(&t.name) == Some(&Outcome::Passed)) {
            let message = format!("`{}` passes without the change, so it does not test it.", t.name);
            out.push(finding(Some(t.path.as_os_str()), Some(t.line), "passes-at-base", message));
        }
    }
    out
}

