//! The gate's judgement of one change: what it reads of the change, and the report it gives on it, as gate.py's
//! judge_change does: the linters, and the checks beside them, over Python and Rust.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use crate::config_edits::{cargo_home_in, clippy_config_files, edited_config};
use crate::coverage::new_coverage;
use crate::diff::{change_diff, changed_files, changed_lines, ChangedFile, ChangedLines};
use crate::flaky_tests::flaky_tests;
use crate::linters::{tracked_from, Linter, LINTERS};
use crate::mutants::surviving_mutants;
use crate::parser_checks::{too_complex, vague_names, weakened_tests, Sources};
use crate::passes_at_base::passes_at_base;
use crate::process::tool_env;
use crate::python_mutants::Mutmut;
use crate::report::{Finding, QualityReport};
use crate::settings::{JudgedBy, Settings};
use crate::suppressions::{added_suppressions, stray_line_end_condition, stray_line_ends};
use crate::test_commands::test_results;
use crate::text::{self, series};
use crate::{git, Unjudged};

/// The directory in the tree mutmut runs in where it keeps its state: its copy of the code with the mutants, and each
/// module's results in `<path>.meta` beside it. What a tree tracks there is no part of the change the gate reads
/// ([`Change::from_tree`]), and a change that commits files there fails the gate ([`judge_change`]).
pub const MUTMUT_STATE: &str = "mutants";
/// The git pathspecs of every path outside [`MUTMUT_STATE`], and of the paths inside it (a test holds them to it).
const OUTSIDE_MUTMUT_STATE: &str = ":(top,exclude,literal)mutants";
const INSIDE_MUTMUT_STATE: &str = ":(top,literal)mutants";

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

/// The change the gate judges, as [`Change::from_tree`] reads it, and as each check is given it.
pub struct Change {
    /// The repository's top level; its working tree is the change's new side.
    pub root: PathBuf,
    /// The commit the change is judged against, a full sha.
    pub base: String,
    /// `git diff -U0` from base to the working tree (diff::change_diff), of the files the gate sees.
    pub diff: Vec<u8>,
    /// `changed_lines(diff)`.
    pub changed: ChangedLines,
    /// diff::changed_files from base, of the files the gate sees.
    pub files: Vec<ChangedFile>,
    /// The files the change adds or edits under [`MUTMUT_STATE`], the one part of it the fields above leave out, in
    /// path order: a file it adds, renames into it or changes there in any way but deleting it.
    pub mutmut_state: Vec<OsString>,
    /// How the gate reads the project, as its gate.toml says.
    pub settings: Settings,
    /// The program that runs the judged project's Python, in which the checks expect its pytest, pytest-randomly and
    /// mutmut: an absolute path, or a name without a slash, which PATH finds. Nothing has looked for it: one that is
    /// missing, or lacks a module, is for the check that runs it to report.
    pub python: PathBuf,
}

impl Change {
    /// The change from `base`, a full sha, to the working tree of the repository whose top level is `root`, read by
    /// `settings`, its Python the one `--python` names, `python`, else the settings', else `python3`.
    ///
    /// Its diff, changed and files show the change as if git tracked nothing under [`MUTMUT_STATE`] on either side,
    /// so nothing that reads them selects a test, a function or a changed line there, or counts an edit there as a
    /// change to the code. A file moved out of it is an added file, and one moved into it a deleted one, which
    /// mutmut_state names instead. Errs when git cannot diff the tree against base.
    pub fn from_tree(root: &Path, base: &str, settings: Settings, python: Option<&Path>) -> Result<Change, Unjudged> {
        let diff = change_diff(root, base, &[OUTSIDE_MUTMUT_STATE])?;
        let mut mutmut_state: Vec<OsString> =
            changed_files(root, base, &[INSIDE_MUTMUT_STATE])?.into_iter().filter_map(|f| f.after).collect();
        mutmut_state.sort();
        Ok(Change {
            root: root.to_path_buf(),
            base: base.to_string(),
            changed: changed_lines(&diff),
            files: changed_files(root, base, &[OUTSIDE_MUTMUT_STATE])?,
            diff,
            mutmut_state,
            python: match (python, &settings.python) {
                (Some(given), _) => program(given, &crate::paths::abs_path(Path::new("."))),
                (None, Some(own)) => program(Path::new(own), root),
                (None, None) => PathBuf::from("python3"),
            },
            settings,
        })
    }
}

/// The program `named` as a process started in any directory finds it: a name without a slash as it is, for PATH to
/// find, and a path made absolute from `start`, an absolute directory, and normalized (paths::normpath).
fn program(named: &Path, start: &Path) -> PathBuf {
    if named.as_os_str().as_bytes().contains(&b'/') {
        crate::paths::normpath(&start.join(named))
    } else {
        named.to_path_buf()
    }
}

/// Whether `path`, relative to the tree's top, is [`MUTMUT_STATE`] or under it.
pub fn under_mutmut_state(path: &OsStr) -> bool {
    let path = path.as_bytes();
    path.strip_prefix(MUTMUT_STATE.as_bytes()).is_some_and(|rest| rest.is_empty() || rest.starts_with(b"/"))
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
/// (coverage::NewCoverage::condition). The tests are those test_commands::test_results gives, run before the
/// linters. Suppressions the change added are listed, not counted against it.
///
/// Errs when it cannot judge: `cwd` is not in a git work tree, `base` names no commit, the settings cannot be read
/// or the rules are no file, a test or coverage report cannot be read, a report a test command names cannot be
/// removed before it runs, a file the change adds lines to cannot be read, or a source file the parser checks read
/// cannot be shown at the base or read in the working tree. Ctrl-C during a process it runs ends the gate
/// (process::run).
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
    let to_weigh = judge_checks(&change, &mutmut, budgets, &mut conditions, &mut issues)?;

    conditions.extend(tests.problems);
    if let Some(failing) = tests.failing.as_ref().filter(|failing| !failing.is_empty()) {
        conditions.push(format!("{} tests fail; each is listed below.", failing.len()));
    }
    let coverage = if tests.coverage.is_empty() { None } else { Some(new_coverage(&tests.coverage, &root, changed)?) };
    conditions.extend(coverage.as_ref().and_then(|c| c.condition()));

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
        edited_config: edited_config(&change.diff, changed, &change.settings),
        clippy_config: clippy_config_files(&change.diff, changed, cargo_home_in(&root, &tool_env()).as_deref()),
        rules_from,
        settings_from,
        config_notes: change.settings.build_config.clone(),
    })
}

/// Runs the checks beside the linters on `change`, in the order gate.py's `gate_checks` runs them: test-integrity,
/// cognitive-complexity, flaky-tests, surviving-mutants and passes-at-base, the last two's runs within the seconds
/// `budgets` gives, (mutation, base run), mutmut set up as `mutmut` says, and vague-names. Appends to `conditions`
/// a failed condition for each check with findings of severity "error", in that order, and to `issues` those
/// findings; returns the others, to weigh.
/// Errs as [`Sources::read`] does, having appended nothing.
fn judge_checks(
    change: &Change,
    mutmut: &Mutmut,
    (mutation_budget, base_run_budget): (u64, u64),
    conditions: &mut Vec<String>,
    issues: &mut Vec<Finding>,
) -> Result<Vec<Finding>, Unjudged> {
    let sources = Sources::read(change)?;
    let found = [
        weakened_tests(&sources),
        too_complex(&sources),
        named("flaky-tests", flaky_tests(&sources.python)),
        named("surviving-mutants", surviving_mutants(&sources, mutmut, mutation_budget)),
        named("passes-at-base", passes_at_base(&sources, base_run_budget)),
        vague_names(&sources),
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

/// `findings` of the check named `check`, as its find gives them (copies::finding), with the check's name before
/// each rule: gate.py's `GateCheck.judged`, for a check whose findings never fail the gate.
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
    fn a_program_is_a_name_for_path_to_find_or_a_path_from_the_start() {
        let start = Path::new("/tree");
        for (named, found) in [
            ("python3.13", "python3.13"),
            (".venv/bin/python", "/tree/.venv/bin/python"),
            ("./python", "/tree/python"),
            ("../venv/bin/python", "/venv/bin/python"),
            ("/usr/bin/python3", "/usr/bin/python3"),
        ] {
            assert_eq!(program(Path::new(named), start), PathBuf::from(found), "{named}");
        }
    }

    #[test]
    fn mutmut_s_state_is_the_directory_and_what_is_under_it() {
        for (path, under) in [("mutants", true), ("mutants/a.py", true), ("mutants.py", false), ("a/mutants", false)] {
            assert_eq!(under_mutmut_state(OsStr::new(path)), under, "{path}");
        }
    }

    #[test]
    fn the_mutmut_state_pathspecs_name_its_directory() {
        assert_eq!(OUTSIDE_MUTMUT_STATE, format!(":(top,exclude,literal){MUTMUT_STATE}"));
        assert_eq!(INSIDE_MUTMUT_STATE, format!(":(top,literal){MUTMUT_STATE}"));
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
