//! The gate's judgement of one change: what it reads of the change, and the report it gives on it, from the linters
//! and the checks beside them, over Python, Rust and Go.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::{Path, PathBuf};

use crate::change::{Change, MUTMUT_STATE};
use crate::config_edits::{cargo_home_in, edited_config, tool_config, touched_paths};
use crate::coverage::new_coverage;
use crate::diff::ChangedLines;
use crate::flaky_tests::flaky_tests;
use crate::linters::{tracked_from, Linter, LINTERS};
use crate::mutants::surviving_mutants;
use crate::parser_checks::{
    dependency_cycles, duplicated_blocks, too_complex, vague_names, weakened_tests, RustChange, Sources,
};
use crate::passes_at_base::passes_at_base;
use crate::process::tool_env;
use crate::python_mutants::Mutmut;
use crate::report::{Finding, QualityReport};
use crate::settings::JudgedBy;
use crate::suppressions::{added_suppressions, stray_line_end_condition, stray_line_ends};
use crate::test_commands::test_results;
use crate::text::{self, series};
use crate::{git, Unjudged};

/// What the command line names for the judgement beside the base; each left empty means the tree's own, or none.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Given {
    /// `--config`: the gate.toml to judge by, as [`JudgedBy::for_tree`] takes it.
    pub settings: Option<PathBuf>,
    /// JUnit and coverage reports of the tree's tests, which replace gate.toml's `[[test]]` commands when either is
    /// given (test_commands::test_results).
    pub tests: Vec<PathBuf>,
    pub coverage: Vec<PathBuf>,
    /// Seconds the surviving-mutants check's runs may take, in place of gate.toml's `budgets.mutation`.
    pub mutation_budget: Option<u64>,
    /// Seconds the passes-at-base check's runs may take, in place of gate.toml's `budgets.base-run`.
    pub base_run_budget: Option<u64>,
    /// The judged project's Python, in place of gate.toml's `python`: a path, absolute or relative to the current
    /// directory, or a name without a slash, which PATH finds.
    pub python: Option<PathBuf>,
    /// A coverage.py data file of the tree's test run with a context per test, by which the surviving-mutants check
    /// narrows mutmut's tests (python_mutants::Mutmut::test_contexts): absolute or relative to the current directory.
    pub test_contexts: Option<PathBuf>,
}

/// The gate's report on the change from `base` to the working tree of the git repository at `cwd`.
///
/// The change is [`Change::from_tree`]'s, read by the settings [`JudgedBy::for_tree`] gives for `given.settings`.
/// Runs each of [`LINTERS`] whose files the repository tracks outside [`MUTMUT_STATE`] over those files, with the
/// rules that go with those settings, and lists the findings on lines the change added and the lineless ones on
/// files it touched.
///
/// The gate is ERROR when the change adds or edits files under [`MUTMUT_STATE`], which is always mutmut's scratch
/// state committed by accident; when there is such a linter finding; when a linter is missing, fails or prints
/// nothing readable; when a file a linter reads and the change adds lines to holds a line end git does not count
/// (suppressions::stray_line_ends); when a check beside the linters ([`judge_checks`]) has a finding of severity
/// "error", each of which is listed beside the linters' while the others are listed to weigh; when a test fails, or a
/// test command's tests could not be judged; or when the coverage reports put new code below the minimum
/// (coverage::NewCoverage::condition), new code being the lines the change added but Rust test code's
/// ([`lines_to_cover`]). The tests are those test_commands::test_results gives, run before the
/// linters. Suppressions the change added are listed, not counted against it.
///
/// Errs when it cannot judge: `cwd` is not in a git work tree, `base` names no commit, the settings cannot be read
/// or the rules are no file, a test or coverage report cannot be read, a report a test command names cannot be
/// removed before it runs, a file the change adds lines to cannot be read, or a source file the parser checks read
/// cannot be shown at the base or read in the working tree. Ctrl-C, SIGTERM or SIGHUP during a process it runs ends the
/// gate (process::run).
pub fn judge_change(cwd: &Path, base: &str, given: &Given) -> Result<QualityReport, Unjudged> {
    let done = git::run(cwd, &["rev-parse", "--show-toplevel"])?;
    if done.status != 0 {
        return Err(Unjudged(format!("{} is not in a git work tree", text::path(cwd.as_os_str()))));
    }
    let root = PathBuf::from(OsString::from_vec(first_line(done.stdout)));
    let done = git::run(&root, &["rev-parse", "--verify", "--quiet", &format!("{base}^{{commit}}")])?;
    if done.status != 0 {
        return Err(Unjudged(format!("{base} names no commit in {}", text::path(root.as_os_str()))));
    }
    let base_sha = text::lossless(&first_line(done.stdout));
    let JudgedBy { settings, settings_from, rules, rules_from } =
        JudgedBy::for_tree(&root, given.settings.as_deref())?;
    let rules = rules.as_deref();
    let change = Change::from_tree(&root, &base_sha, settings, given.python.as_deref())?;
    let changed = &change.changed;
    let tests = test_results(&root, &change.settings.test_commands, &given.tests, &given.coverage)?;

    let tracked = tracked_from(&git::output(&root, &["ls-files", "-z"])?);
    let mut conditions = mutmut_state_condition(&change.mutmut_state);
    let mut findings = vec![];
    let mut reads: Vec<(&Linter, Vec<OsString>)> = vec![];
    for linter in &LINTERS {
        let files = linter.files(&tracked, &change.settings);
        if !files.is_empty() {
            let (found, problems) = linter.run(&root, rules, &files, &tracked);
            findings.extend(found);
            conditions.extend(problems);
        }
        reads.push((linter, files));
    }
    let found = findings.len();
    let mut issues = on_changed_lines(findings, changed);
    let elsewhere = found - issues.len();
    if !issues.is_empty() {
        conditions.insert(0, format!("{} findings on lines the change added; each is listed below.", issues.len()));
    }
    for (path, n, end) in stray_line_ends(&root, changed, &reads)? {
        conditions.push(stray_line_end_condition(&path, n, end));
    }
    let budgets = (
        given.mutation_budget.unwrap_or(change.settings.mutation_budget),
        given.base_run_budget.unwrap_or(change.settings.base_run_budget),
    );
    let test_contexts = given.test_contexts.as_deref().map(crate::paths::abs_path);
    let mutmut = Mutmut { rules, test_contexts: test_contexts.as_deref() };
    let sources = Sources::read(&change)?;
    let to_weigh = judge_checks(&sources, &mutmut, budgets, &mut conditions, &mut issues)?;

    conditions.extend(tests.problems);
    if let Some(failing) = tests.failing.as_ref().filter(|failing| !failing.is_empty()) {
        conditions.push(format!("{} tests fail; each is listed below.", failing.len()));
    }
    let coverage = match tests.coverage.is_empty() {
        true => None,
        false => Some(new_coverage(&tests.coverage, &root, &lines_to_cover(&sources.rust))?),
    };
    conditions.extend(coverage.as_ref().and_then(|c| c.condition()));
    let touched = touched_paths(&change.diff, changed);

    Ok(QualityReport {
        head: text::lossless(&first_line(git::output(&root, &["rev-parse", "HEAD"])?)),
        base: base_sha,
        conditions,
        elsewhere,
        issues,
        to_weigh,
        failing_tests: tests.failing,
        coverage,
        suppressions: added_suppressions(&root, changed)?,
        edited_config: edited_config(&touched, &change.settings),
        tool_config: tool_config(&root, &touched, cargo_home_in(&root, &tool_env()).as_deref()),
        rules_from,
        settings_from,
        config_notes: change.settings.build_config.clone(),
    })
}

/// Runs the checks beside the linters on `change`, in this order: test-integrity, cognitive-complexity,
/// dependency-cycles, duplicated-blocks, flaky-tests, surviving-mutants and passes-at-base, the last two's runs within
/// the seconds `budgets` gives, (mutation, base run), mutmut set up as `mutmut` says, and vague-names. Appends to
/// `conditions` a failed condition for each check with findings of severity "error", in that order, and to `issues`
/// those findings; returns the others, to weigh. Errs as [`dependency_cycles`] and [`duplicated_blocks`] do, having
/// appended nothing.
fn judge_checks(
    sources: &Sources,
    mutmut: &Mutmut,
    (mutation_budget, base_run_budget): (u64, u64),
    conditions: &mut Vec<String>,
    issues: &mut Vec<Finding>,
) -> Result<Vec<Finding>, Unjudged> {
    let found = [
        weakened_tests(sources),
        too_complex(sources),
        dependency_cycles(sources)?,
        duplicated_blocks(sources)?,
        named("flaky-tests", flaky_tests(sources)),
        named("surviving-mutants", surviving_mutants(sources, mutmut, mutation_budget)),
        named("passes-at-base", passes_at_base(sources, base_run_budget)),
        vague_names(sources),
    ];
    let mut to_weigh = vec![];
    for found in found {
        let (errors, weighed): (Vec<Finding>, Vec<Finding>) = found.into_iter().partition(|f| f.severity == "error");
        if let Some(first) = errors.first() {
            conditions.push(format!("{} findings of the `{}` check; each is listed below.", errors.len(), first.check()));
        }
        issues.extend(errors);
        to_weigh.extend(weighed);
    }
    Ok(to_weigh)
}

/// The lines the change added that its coverage is judged on: all but those of Rust test code in the working tree, as
/// its front end reads it (model::SourceFile::test_lines), a `#[cfg(test)]` item or a file among the tests whole. A
/// passing test runs its own lines, so they would cover the code it lacks tests of. A file the front end could not
/// read keeps every line, unless the project's settings place it among the tests (checks::Side::among_tests).
fn lines_to_cover(rust: &RustChange) -> ChangedLines {
    let mut out = rust.change.changed.clone();
    for side in rust.files.iter().filter_map(|f| f.after.as_ref()) {
        let Some(added) = out.get_mut(&side.path) else { continue };
        match &side.source {
            Ok(source) => added.retain(|&n| !source.test_lines.iter().any(|l| l.holds(n))),
            Err(_) if side.among_tests => added.clear(),
            Err(_) => {}
        }
    }
    out
}

/// `findings` of the check named `check`, as its find gives them (copies::finding), with the check's name before
/// each rule, or as the whole rule of a finding that has none of its own. Their severity stays "warning": this is
/// for a check whose findings never fail the gate.
fn named(check: &str, findings: Vec<Finding>) -> Vec<Finding> {
    let rule = |own: String| if own.is_empty() { check.to_string() } else { format!("{check} {own}") };
    findings.into_iter().map(|f| Finding { rule: rule(f.rule), ..f }).collect()
}

/// What git printed, up to the newline that ends its one line.
fn first_line(mut printed: Vec<u8>) -> Vec<u8> {
    if let Some(end) = printed.iter().position(|&b| b == b'\n') {
        printed.truncate(end);
    }
    printed
}

/// The failed condition, as a list of one, naming the files a change commits under [`MUTMUT_STATE`], `committed`;
/// none when it commits none.
fn mutmut_state_condition(committed: &[OsString]) -> Vec<String> {
    if committed.is_empty() {
        return vec![];
    }
    let named: Vec<String> = committed.iter().map(|p| format!("`{}`", text::path(p))).collect();
    vec![format!(
        "The change adds or edits {} under `{MUTMUT_STATE}/`, where mutmut keeps its scratch state, which is never \
         part of a change; the gate judged none of them. Remove them from the change.",
        series(&named, Some(5))
    )]
}

/// The linter `findings` on lines the change added, and the lineless ones on files it touched, `changed` being its
/// lines; by path and line, a lineless finding first.
fn on_changed_lines(findings: Vec<Finding>, changed: &ChangedLines) -> Vec<Finding> {
    let mut out: Vec<Finding> = findings
        .into_iter()
        .filter(|f| {
            let added = f.path.as_ref().and_then(|p| changed.get(p));
            added.is_some_and(|added| f.line.is_none_or(|n| added.contains(&n)))
        })
        .collect();
    out.sort_by(|a, b| (&a.path, a.line.unwrap_or(0)).cmp(&(&b.path, b.line.unwrap_or(0))));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(path: &str, line: Option<u32>) -> Finding {
        Finding { path: Some(path.into()), line, severity: "error".into(), rule: "r".into(), message: "m".into() }
    }

    #[test]
    fn only_findings_on_added_lines_and_lineless_ones_on_touched_files_count() {
        let changed: ChangedLines = [("a.sh".into(), [3].into()), ("b.sh".into(), [].into())].into();
        let found = vec![
            finding("a.sh", Some(3)),
            finding("a.sh", Some(2)),
            finding("b.sh", None),
            finding("a.sh", None),
            finding("c.sh", None),
        ];
        let kept = on_changed_lines(found, &changed);
        assert_eq!(kept, vec![finding("a.sh", None), finding("a.sh", Some(3)), finding("b.sh", None)]);
    }

    #[test]
    fn the_mutmut_state_condition_names_at_most_five_files() {
        assert_eq!(mutmut_state_condition(&[]), Vec::<String>::new());
        let files: Vec<OsString> = (1..=7).map(|k| format!("mutants/{k}.py").into()).collect();
        let condition = &mutmut_state_condition(&files)[0];
        assert!(condition.starts_with("The change adds or edits `mutants/1.py`, "), "{condition}");
        assert!(condition.contains("`mutants/5.py` and 2 more under `mutants/`"), "{condition}");
    }
}
