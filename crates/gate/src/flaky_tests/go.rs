//! flaky-tests' Go half: the changed tests rerun with go test under `go test -shuffle`'s seeds.

use std::time::Duration;

use crate::copies::{finding, OnCopy, ScratchCopy};
use crate::gotest::{self, Packages, Ran};
use crate::report::Finding;
use crate::sources::GoChange;

use super::{stopped_not_rerun, unread_not_rerun, Reruns, RUN_TIMEOUT};

/// How long building a Go package's tests before their reruns may take (gotest::build), which [`RUN_TIMEOUT`] then
/// leaves out.
const BUILD_TIMEOUT: Duration = Duration::from_secs(300);

/// The check's findings on `go`, in no order, as python::python_findings gives them.
///
/// Reruns the Go tests the change touched (gotest::changed_tests) once under each of [`SEEDS`](super::SEEDS), each
/// package's together, as `go test -shuffle=<seed>` orders them, and reports at its func each test whose outcome,
/// failed or passed, differs between seeds (`mixed-results`), one that failed under every seed that judged it, at least
/// two (`failed-every-run`), and one that fewer than two runs passed or failed (`not-rerun`), as
/// python::python_findings does and for the same reasons. The other tests of the package are not run, so only the
/// changed tests' order varies.
///
/// The tests run in the check's copy of the working tree, each package's once its tests are built (gotest::build). A
/// build that cannot start or takes longer than [`BUILD_TIMEOUT`] is a `not-rerun` finding on the package's tests. A go
/// test run that cannot start, takes longer than [`RUN_TIMEOUT`] or tells nothing (gotest::run), or finds that the
/// package's tests do not build in the copy, ends the reruns of that package's tests with a `not-rerun` finding saying
/// so, after what the runs before it showed; the other packages' tests are rerun still. A changed test file that cannot
/// be read or parsed is a `not-rerun` finding too.
pub(super) fn go_findings<'a>(go: &'a GoChange) -> (Vec<Finding>, Option<OnCopy<'a>>) {
    let (packages, unreadable) = gotest::changed_tests(go);
    let out = unreadable.into_iter().map(unread_not_rerun).collect();
    let run = (!packages.is_empty()).then(|| -> OnCopy { Box::new(move |copy| go_rerun_findings(copy, &packages)) });
    (out, run)
}

/// [`go_findings`]' findings on `packages`, the changed tests by their package's directory, rerun in `copy`, with the
/// tests it builds in its scratch directory.
fn go_rerun_findings(copy: &ScratchCopy, packages: &Packages) -> Vec<Finding> {
    let (tree, tmp) = (&copy.tree, &copy.scratch);
    let mut out = vec![];
    for (n, (package, tests)) in packages.iter().enumerate() {
        let names: Vec<&str> = tests.iter().map(|t| t.name.as_str()).collect();
        let dir = tree.join(package);
        let which = format!("the changed tests of {}", gotest::label(package));
        let binary = tmp.join(format!("go-tests-{n}.test"));
        if let Err(problem) = gotest::build(&dir, &binary, BUILD_TIMEOUT, "building the package's tests") {
            out.push(stopped_not_rerun(&problem.clause, Some(&which), &[]));
            continue;
        }
        let reruns = Reruns::run(|seed| {
            let doing = format!("rerunning them under seed {seed}");
            match gotest::run(&dir, &names, &[&format!("-shuffle={seed}")], RUN_TIMEOUT, &doing) {
                Ok(Ran::Outcomes(outcomes)) => Ok(outcomes),
                Ok(Ran::Unbuilt) => Err(format!("go test, {doing}, could not build the package's tests in the copy \
                                                 of the working tree")),
                Err(problem) => Err(problem.clause),
            }
        });
        for t in tests {
            if let Some((rule, message)) = reruns.judge(&t.name, &t.name) {
                out.push(finding(Some(&t.path), Some(t.line), rule, message));
            }
        }
        if let Some((_, problem)) = &reruns.stopped {
            out.push(stopped_not_rerun(problem, Some(&which), &reruns.seeds));
        }
    }
    out
}
