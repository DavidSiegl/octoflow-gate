//! Go's half of the passes-at-base check: the Go tests a change adds or edits, run with go test against the code the
//! change started from.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Instant;

use checks::Finding;

use crate::base_findings::{budget_reached, submodules_left_empty, unread_not_run};
use crate::copies::{copy_base_with_tests, finding, in_scratch_directory, Budget};
use crate::gotest::{self, Packages, Ran};
use crate::junit::Outcome;
use crate::parser_checks::GoChange;

/// Go's findings of the passes-at-base check on `go`, in no order.
///
/// When the change edits Go code under test ([`GoChange::edits_code`]), runs the Go tests it adds or edits
/// (gotest::changed_tests) against the base's code, by `budget`'s deadline, and reports at its func each one that
/// passed there (rule `passes-at-base`): it does not test the change. A change to the tests alone has no "without the
/// change" to run them in.
///
/// The tests run in a copy of the base (copies::copy_base_with_tests) with the change's files that go with the tests
/// ([`beside_tests`]) in place of the base's: each package's changed tests together, once, from the package's
/// directory, as `go test -json -count=1 -run '^(TestA|TestB)$' .`. A package whose tests do not build there counts
/// as failing each of them, which is what the check expects of a test that needs the change, and gives nothing; so
/// does a test that fails or skips there, or that the run does not report.
///
/// The budget running out is a `budget-reached` finding, and each package after it a `not-run` one. A changed test
/// file that cannot be read or parsed, a copy that cannot be made, submodules the copy leaves empty, and a go test
/// that cannot start or tells nothing (gotest::run) are `not-run` findings saying so; a copy that cannot be removed
/// afterwards is a `not-removed` one.
pub fn go_at_base(go: &GoChange, budget: &Budget) -> Vec<Finding> {
    if !go.edits_code() {
        return vec![];
    }
    let (packages, unread) = gotest::changed_tests(go);
    let mut out: Vec<Finding> = unread.into_iter().map(unread_not_run).collect();
    if !packages.is_empty() {
        out.extend(in_scratch_directory("not-run", "to run the changed Go tests at the base in", |dir| {
            base_run_findings(go, dir, &packages, budget)
        }));
    }
    out
}

/// Whether a repository path belongs with Go's tests rather than with the code they test, as the copy the tests run
/// in at the base takes it from the working tree: a file gate.toml's `test-support` names, a `_test.go` file, and a
/// file under a `testdata` directory, which holds the tests' inputs.
fn beside_tests(go: &GoChange, path: &OsStr) -> bool {
    let bytes = path.as_bytes();
    go.change.settings.test_support.matches(path)
        || lang_go::among_tests(bytes)
        || bytes.split(|&b| b == b'/').any(|part| part == b"testdata")
}

/// The findings on `packages`, run within `budget` in a copy of the base made in `dir`, an empty directory.
fn base_run_findings(go: &GoChange, dir: &Path, packages: &Packages, budget: &Budget) -> Vec<Finding> {
    let tree = dir.join("tree");
    let empty = match copy_base_with_tests(go.change, &tree, &dir.join("base.tar"), |p| beside_tests(go, p)) {
        Ok(empty) => empty,
        Err(e) => {
            let message = format!("could not copy the base to run the changed Go tests in: {e}; they were not run \
                                   there.");
            return vec![finding(None, None, "not-run", message)];
        }
    };
    let mut out: Vec<Finding> = submodules_left_empty(&empty, "Go").into_iter().collect();
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
                out.extend(packages.keys().skip(n + 1).map(|later| not_reached(budget, later)));
                break;
            }
            Some(Err(problem)) => {
                out.push(finding(None, None, "not-run", problem.clause));
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

/// The `not-run` finding on the package at `package`, whose changed tests `budget` ran out before.
fn not_reached(budget: &Budget, package: &Path) -> Finding {
    let message = format!(
        "the run at the base used its {} seconds (`--base-run-budget`) before it reached {}, so its changed tests \
         were not run there.",
        budget.seconds,
        gotest::label(package)
    );
    finding(None, None, "not-run", message)
}
