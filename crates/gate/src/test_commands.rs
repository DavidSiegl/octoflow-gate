//! The tree's tests, as the gate judges them: by running gate.toml's `[[test]]` commands and reading the reports they
//! write, or by reading the reports `--tests` and `--coverage` name.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::junit::failing_tests;
use crate::process;
use crate::settings::TestCommand;
use crate::Unjudged;

/// What the gate judges of a tree's tests.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TestResults {
    /// The (test, what failed) of each failing test of the JUnit reports read; None when none was read.
    pub failing: Option<Vec<(String, String)>>,
    /// The Cobertura or LCOV reports to read, absolute paths.
    pub coverage: Vec<PathBuf>,
    /// A failed condition for each command whose tests could not be judged.
    pub problems: Vec<String>,
}

/// What the gate judges of the tests of the work tree at `root`.
///
/// `tests` and `coverage`, JUnit and coverage reports given on the command line, paths absolute or relative to the
/// current directory, replace `commands` when either is given: a CI that already ran the tests then reads their
/// reports. Otherwise [`run_test_commands`] runs `commands`. Errs when one of `tests` cannot be read, or as
/// [`run_test_commands`] does.
pub fn test_results(
    root: &Path,
    commands: &[TestCommand],
    tests: &[PathBuf],
    coverage: &[PathBuf],
) -> Result<TestResults, Unjudged> {
    if !tests.is_empty() || !coverage.is_empty() || commands.is_empty() {
        let absolute = |p: &PathBuf| crate::paths::abs_path(p);
        let failing = if tests.is_empty() {
            None
        } else {
            let mut failing = vec![];
            for report in tests {
                failing.extend(failing_tests(&absolute(report))?);
            }
            Some(failing)
        };
        return Ok(TestResults { failing, coverage: coverage.iter().map(absolute).collect(), problems: vec![] });
    }
    run_test_commands(root, commands)
}

/// Runs each of `commands`, in order, at the top level of the work tree at `root`, as [`process::run_to_stderr`]
/// runs a program, and gives back what the reports of those whose tests could be judged say: the failing tests of
/// their JUnit reports, None when none of them names one; their coverage reports; and a failed condition for each
/// command whose tests could not be judged, whose reports are not read.
///
/// Before a command runs, the reports it names are removed, so that one an earlier run left is never read. Its tests
/// could not be judged when it cannot start, when it leaves a report it names missing, or a JUnit report that cannot
/// be read, or when it exits non-zero while its JUnit reports list no failing test: then what failed was not a test.
/// A coverage report is read later, by coverage::new_coverage. The commands take as long as they take. Errs when a
/// report cannot be removed.
pub fn run_test_commands(root: &Path, commands: &[TestCommand]) -> Result<TestResults, Unjudged> {
    let mut results = TestResults::default();
    for command in commands {
        for report in command.junit.iter().chain(&command.coverage) {
            match std::fs::remove_file(root.join(report)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => {
                    return Err(Unjudged(format!(
                        "cannot remove {report}, which the test command {} writes: {e}",
                        command.name
                    )))
                }
                _ => {}
            }
        }
        match run_test_command(root, command) {
            Err(problem) => results
                .problems
                .push(format!("The test command `{}` {problem}, so its tests were not judged.", command.name)),
            Ok(found) => {
                if !command.junit.is_empty() {
                    results.failing.get_or_insert_with(Vec::new).extend(found);
                }
                results.coverage.extend(command.coverage.iter().map(|report| root.join(report)));
            }
        }
    }
    Ok(results)
}

/// Runs `command` as [`run_test_commands`] says: the failing tests of its JUnit reports, or why its tests could not
/// be judged, a clause after "The test command `name`".
fn run_test_command(root: &Path, command: &TestCommand) -> Result<Vec<(String, String)>, String> {
    let argv: Vec<&OsStr> = command.command.iter().map(OsStr::new).collect();
    let status = process::run_to_stderr(&argv, root).map_err(|e| format!("could not start: {}", strerror(&e)))?;
    for report in command.junit.iter().chain(&command.coverage) {
        if !root.join(report).is_file() {
            return Err(format!("wrote no `{report}`, which gate.toml names as its report"));
        }
    }
    let mut found = vec![];
    for report in &command.junit {
        found.extend(failing_tests(&root.join(report)).map_err(|_| {
            format!("left `{report}`, which gate.toml names as its report, unreadable as JUnit")
        })?);
    }
    if status != 0 && found.is_empty() {
        return Err(format!("exited {status}, but its JUnit reports list no failing test"));
    }
    Ok(found)
}

/// The system's words for `e`, as Python's `OSError.strerror` gives them: "No such file or directory" without
/// Rust's " (os error 2)".
fn strerror(e: &std::io::Error) -> String {
    let said = e.to_string();
    match said.rfind(" (os error ") {
        Some(k) => said[..k].to_string(),
        None => said,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

    fn command(name: &str, script: &str, junit: &[&str], coverage: &[&str]) -> TestCommand {
        TestCommand {
            name: name.into(),
            command: ["sh", "-c", script].map(String::from).to_vec(),
            junit: junit.iter().map(|s| s.to_string()).collect(),
            coverage: coverage.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn each_command_is_judged_by_what_it_leaves() {
        let root = ScratchDir::new();
        // A report an earlier run left, which the run must not read.
        std::fs::write(root.join("stale.xml"), "<testsuite/>").unwrap();
        let failing = r#"<testsuite><testcase classname="t" name="x"><failure message="m"/></testcase></testsuite>"#;
        let commands = [
            command("ok", "echo '<testsuite/>' > j.xml; echo cov > c.xml", &["j.xml"], &["c.xml"]),
            command("fails", &format!("echo '{failing}' > f.xml; exit 1"), &["f.xml"], &[]),
            command("stale", "true", &["stale.xml"], &[]),
            command("broken", "echo '<testsuite/>' > b.xml; exit 101", &["b.xml"], &[]),
            command("garbled", "echo '<' > g.xml", &["g.xml"], &[]),
            TestCommand { command: vec!["gate-no-such-program".into()], ..command("missing", "", &[], &[]) },
        ];
        let results = run_test_commands(&root, &commands).unwrap();
        assert_eq!(results.failing, Some(vec![("t.x".into(), "m".into())]));
        assert_eq!(results.coverage, [root.join("c.xml")]);
        assert_eq!(
            results.problems,
            [
                "The test command `stale` wrote no `stale.xml`, which gate.toml names as its report, so its tests \
                 were not judged.",
                "The test command `broken` exited 101, but its JUnit reports list no failing test, so its tests were \
                 not judged.",
                "The test command `garbled` left `g.xml`, which gate.toml names as its report, unreadable as JUnit, \
                 so its tests were not judged.",
                "The test command `missing` could not start: No such file or directory, so its tests were not judged.",
            ]
        );
    }
}
