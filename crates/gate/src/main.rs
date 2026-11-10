//! The `gate` binary: judges the change the git working tree around the current directory makes since a commit, as
//! gate::judge::judge_change does, and writes the quality file and quality.json.
//!
//! ```text
//! gate --base REV [--config PYPROJECT] [--config GATE_TOML] [--tests JUNIT]... [--coverage XML]...
//!      [--out FILE] [--json FILE] [--mutation-budget SECONDS] [--base-run-budget SECONDS]
//!      [--test-contexts COVERAGE] [--rust-syntax PATH] [--python PATH]
//! ```
//!
//! Its flags are gate.py's, and `--python`, which names what gate.py takes from the Python that runs it
//! (judge::Change::python). `--rust-syntax` names gate.py's helper, which this binary does not need. Exit status 0 for
//! GATE: OK, 1 for GATE: ERROR, 2 when it could not judge the change or the arguments are bad, and 130 for Ctrl-C; with
//! 2 or 130 it writes no report.

use std::ffi::{OsStr, OsString};
use std::io::Write;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use gate::judge::{judge_change, Given};
use gate::paths::real_path;
use gate::process;
use gate::report::Verdict;
use gate::text::{self, series};
use gate::Unjudged;

/// The exit status for a change the gate could not judge, and for bad arguments.
const EXIT_UNJUDGED: u8 = 2;

const USAGE: &str = "usage: gate --base REV [--config FILE] [--coverage XML] [--tests XML] [--out FILE] [--json FILE]
            [--mutation-budget SECONDS] [--base-run-budget SECONDS] [--test-contexts COVERAGE]
            [--rust-syntax PATH] [--python PATH] [--help]";

const HELP: &str = "
The Rust port of gate.py. It judges by ruff, mypy, pymarkdown, shellcheck, actionlint and clippy over the files the
repository tracks, the checks test-integrity, cognitive-complexity, vague-names, flaky-tests, passes-at-base and
surviving-mutants over Python and Rust, gate.toml's [[test]] commands, the tests and coverage they report, files
committed under mutmut's mutants/, stray line ends, suppressions added and edited build configuration. It writes the
quality file: GATE: OK or GATE: ERROR on its first
line, then the failed conditions, the findings on lines added since --base, failing tests, coverage of new code,
suppressions added and edited build configuration. Exit status 0 for OK, 1 for ERROR, 2 when it could not judge,
writing nothing. --out FILE also writes quality.json, the same report as data.

options:
  --help                  show this help and exit
  --base REV              the commit the change is measured from
  --config FILE           a file named *.toml but pyproject.toml holds the gate's settings, used instead of the
                          tree's own gate.toml; any other is a pyproject.toml whose rules the linters take instead of
                          the tree's own, and beside which the settings are then looked for. Given once for each
  --coverage XML          a Cobertura or LCOV coverage report of the tree; given more than once, the reports are
                          merged by path and line. With --coverage or --tests, gate.toml's [[test]] commands do not
                          run: these reports are what is judged
  --tests XML             a JUnit report of the tree's tests; given more than once, a failing test in any of them
                          counts. With --tests or --coverage, gate.toml's [[test]] commands do not run
  --out FILE              write the quality file here instead of to standard output, and quality.json beside it,
                          its name FILE's with the .md made .json, or with .json added
  --json FILE             write quality.json, the report as data, here; with --out, instead of beside FILE
  --mutation-budget SECONDS
                          the seconds the surviving-mutants check may run mutmut and cargo-mutants for, in place of
                          gate.toml's budgets.mutation
  --base-run-budget SECONDS
                          the seconds the passes-at-base check may run the changed tests at the base for, in place
                          of gate.toml's budgets.base-run
  --test-contexts COVERAGE
                          a coverage.py data file of the tree's test run with a context per test, as
                          `pytest --cov-context=test` writes it: the surviving-mutants check then has mutmut run
                          only the tests it records reaching the changed functions
  --rust-syntax PATH      accepted for gate.py's sake, and ignored
  --python PATH           the Python of the judged project, in which its pytest, pytest-randomly and mutmut are
                          installed, in place of gate.toml's python: a path, or a name without a slash for PATH to
                          find. Without either, python3";

/// The command line, parsed: what [`gate_command`] runs.
#[derive(Debug, PartialEq, Eq)]
struct Args {
    base: String,
    given: Given,
    /// Where the quality file goes; None for standard output.
    out: Option<PathBuf>,
    /// Where quality.json goes; None for nowhere. With `out`, never the file `out` names.
    json: Option<PathBuf>,
}

/// What a command line asks for.
#[derive(Debug, PartialEq, Eq)]
enum Asked {
    Run(Box<Args>),
    Help,
}

fn main() -> ExitCode {
    if let Err(e) = process::catch_interrupt() {
        return unjudged(&format!("cannot catch Ctrl-C: {e}"));
    }
    match parse_args(std::env::args_os().skip(1)) {
        Ok(Asked::Run(args)) => gate_command(&args),
        Ok(Asked::Help) => {
            println!("{USAGE}\n{HELP}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{USAGE}\ngate: error: {e}");
            ExitCode::from(EXIT_UNJUDGED)
        }
    }
}

/// Writes the report judge_change gives for the current directory as `args` says: the quality file to `args.out`,
/// else to standard output, and quality.json to `args.json`, if given. Returns 0 for GATE: OK and 1 for GATE: ERROR;
/// [`EXIT_UNJUDGED`], having written nothing, when the change could not be judged or a file could not be written.
/// Ctrl-C ends the process (process::exit_interrupted), having written nothing.
fn gate_command(args: &Args) -> ExitCode {
    let judged = std::env::current_dir()
        .map_err(|e| Unjudged(format!("cannot read the current directory: {e}")))
        .and_then(|cwd| judge_change(&cwd, &args.base, &args.given));
    if process::interrupted() {
        process::exit_interrupted();
    }
    let report = match judged {
        Ok(report) => report,
        Err(Unjudged(e)) => return unjudged(&e),
    };
    let markdown = report.markdown();
    let mut contents: Vec<(&Path, String)> = vec![];
    if let Some(out) = &args.out {
        contents.push((out, markdown.clone()));
    }
    if let Some(json) = &args.json {
        contents.push((json, report.json()));
    }
    if let Err(Unjudged(e)) = write_files(&contents) {
        return unjudged(&e);
    }
    let verdict = report.verdict().word();
    if !contents.is_empty() {
        let written: Vec<String> = contents.iter().map(|(path, _)| text::path(path.as_os_str())).collect();
        eprintln!("GATE: {verdict}, in {}", series(&written, None));
    }
    if args.out.is_none() {
        let mut stdout = std::io::stdout().lock();
        if let Err(e) = stdout.write_all(markdown.as_bytes()).and_then(|()| stdout.flush()) {
            return unjudged(&format!("cannot write the quality file to standard output: {e}"));
        }
    }
    ExitCode::from(if report.verdict() == Verdict::Ok { 0 } else { 1 })
}

fn unjudged(message: &str) -> ExitCode {
    eprintln!("error: {message}");
    ExitCode::from(EXIT_UNJUDGED)
}

/// The command line `argv`, without the program's name, as gate.py's parse_args reads it: each option as `--name
/// value` or `--name=value`, a single-valued one given twice taking the last; `--help` or `-h` as an option asks for
/// help, whatever else is there. Errs, with the message for after "error: ", on an unknown or incomplete option, a
/// missing --base, a budget that is not a whole number of seconds of at least 1, --config naming more than one
/// pyproject.toml or more than one file of settings, or --json naming the file --out writes. Unlike
/// argparse, it takes no abbreviation of an option's name.
fn parse_args(argv: impl IntoIterator<Item = OsString>) -> Result<Asked, String> {
    let mut argv = argv.into_iter();
    let mut base = None;
    let mut configs: Vec<PathBuf> = vec![];
    let mut given = Given::default();
    let (mut out, mut json) = (None, None);
    let mut unrecognized: Vec<String> = vec![];
    while let Some(arg) = argv.next() {
        let (name, inline) = split_inline(arg);
        let Some(name) = name.to_str().map(str::to_string) else {
            unrecognized.push(text::path(&name));
            continue;
        };
        if name == "--help" || name == "-h" {
            return Ok(Asked::Help);
        }
        let mut value = || {
            inline.clone().or_else(|| argv.next()).ok_or_else(|| format!("argument {name}: expected one argument"))
        };
        match name.as_str() {
            "--base" => {
                let rev = value()?.into_string();
                base = Some(rev.map_err(|rev| format!("argument --base: not UTF-8: {}", text::path(&rev)))?);
            }
            "--config" => configs.push(value()?.into()),
            "--coverage" => given.coverage.push(value()?.into()),
            "--tests" => given.tests.push(value()?.into()),
            "--out" => out = Some(PathBuf::from(value()?)),
            "--json" => json = Some(PathBuf::from(value()?)),
            "--base-run-budget" => {
                given.base_run_budget = Some(seconds(&value()?).map_err(|e| format!("argument {name}: {e}"))?);
            }
            "--mutation-budget" => {
                given.mutation_budget = Some(seconds(&value()?).map_err(|e| format!("argument {name}: {e}"))?);
            }
            "--python" => {
                let python = value()?;
                // As gate.toml's `python` (settings::Settings::python).
                if text::py_strip(&text::path(&python)).is_empty() {
                    return Err(format!("argument {name}: must be the path or the name of a Python"));
                }
                given.python = Some(python.into());
            }
            "--test-contexts" => given.test_contexts = Some(value()?.into()),
            "--rust-syntax" => {
                value()?;
            }
            _ => unrecognized.push(name),
        }
    }
    let base = base.ok_or("the following arguments are required: --base")?;
    if !unrecognized.is_empty() {
        return Err(format!("unrecognized arguments: {}", unrecognized.join(" ")));
    }
    let (settings, rules): (Vec<PathBuf>, Vec<PathBuf>) = configs.into_iter().partition(|c| names_settings(c));
    if rules.len() > 1 || settings.len() > 1 {
        return Err("--config names at most one pyproject.toml and one file of settings".into());
    }
    given.rules = rules.into_iter().next();
    given.settings = settings.into_iter().next();
    if let Some(out) = &out {
        let json = json.get_or_insert_with(|| json_path_for(out));
        if real_path(json) == real_path(out) {
            return Err("--json names the file --out writes".into());
        }
    }
    Ok(Asked::Run(Box::new(Args { base, given, out, json })))
}

/// `arg` as an option's name and the value it carries after `=`, as in `--name=value`; the whole of `arg` and None
/// when it carries none, as only a long option, one starting `--`, can.
fn split_inline(arg: OsString) -> (OsString, Option<OsString>) {
    let bytes = arg.as_bytes();
    match bytes.iter().position(|&b| b == b'=') {
        Some(k) if bytes.starts_with(b"--") => {
            (OsStr::from_bytes(&bytes[..k]).to_os_string(), Some(OsStr::from_bytes(&bytes[k + 1..]).to_os_string()))
        }
        _ => (arg, None),
    }
}

/// A time budget given on the command line, in whole seconds, at least 1; errs with why `text` is not one.
fn seconds(text: &OsStr) -> Result<u64, String> {
    let shown = text::path(text);
    let n: i64 = shown.trim().parse().map_err(|_| format!("expected a whole number of seconds, not '{shown}'"))?;
    u64::try_from(n).ok().filter(|&n| n >= 1).ok_or_else(|| format!("must be at least 1, not {n}"))
}

/// Whether `gate --config path` names a file of the gate's settings rather than a pyproject.toml of rules.
fn names_settings(path: &Path) -> bool {
    path.as_os_str().as_bytes().ends_with(b".toml") && path.file_name() != Some(OsStr::new("pyproject.toml"))
}

/// Where quality.json goes beside the quality file at `out`: `out` with its `.md` made `.json`, or with `.json` added
/// when it does not end in `.md`.
fn json_path_for(out: &Path) -> PathBuf {
    let out = out.as_os_str().as_bytes();
    let stem = out.strip_suffix(b".md").unwrap_or(out);
    PathBuf::from(OsString::from_vec([stem, b".json"].concat()))
}

/// Makes each text the whole contents of the file at its path, for each (path, text) of `contents`, so that each
/// file is written whole or keeps what it held.
///
/// Each is written aside, and only once every one is written are they renamed into place, so a file that cannot be
/// written, or Ctrl-C before the renames, leaves every file as it was; only a rename that fails after another
/// succeeded leaves some new and some old. A symlink at a path is followed, so the link stays and its target
/// changes, and an existing file keeps its permissions. Errs naming the file, as given, that cannot be written.
/// Ctrl-C ends the process (process::exit_interrupted) with every file as it was.
fn write_files(contents: &[(&Path, String)]) -> Result<(), Unjudged> {
    // (path as given, its real path, the file written aside)
    let mut aside: Vec<(&Path, PathBuf, PathBuf)> = vec![];
    let written = write_aside(contents, &mut aside).and_then(|()| {
        if process::interrupted() {
            remove_aside(&aside);
            process::exit_interrupted();
        }
        aside.iter().try_for_each(|(given, real, tmp)| std::fs::rename(tmp, real).map_err(|e| (*given, e)))
    });
    remove_aside(&aside);
    written.map_err(|(path, e)| Unjudged(format!("cannot write {}: {e}", text::path(path.as_os_str()))))
}

/// Writes each of `contents` beside its real path, as [`write_files`] does, noting each file written in `aside`;
/// errs with the path, as given, that could not be.
fn write_aside<'a>(
    contents: &[(&'a Path, String)],
    aside: &mut Vec<(&'a Path, PathBuf, PathBuf)>,
) -> Result<(), (&'a Path, std::io::Error)> {
    for (given, text) in contents {
        let real = real_path(given);
        let tmp = PathBuf::from(OsString::from_vec(
            [real.as_os_str().as_bytes(), format!(".tmp.{}", std::process::id()).as_bytes()].concat(),
        ));
        aside.push((given, real.clone(), tmp.clone()));
        std::fs::write(&tmp, text).map_err(|e| (*given, e))?;
        match std::fs::metadata(&real) {
            Ok(was) => std::fs::set_permissions(&tmp, was.permissions()).map_err(|e| (*given, e))?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err((*given, e)),
        }
    }
    Ok(())
}

/// Removes what is left of the files written aside; one renamed into place, or never written, is gone already.
fn remove_aside(aside: &[(&Path, PathBuf, PathBuf)]) {
    for (_, _, tmp) in aside {
        // Nothing to report: the file is either gone, as expected, or a stray the error already reported explains.
        let _ = std::fs::remove_file(tmp);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(args: &[&str]) -> Result<Asked, String> {
        parse_args(args.iter().map(OsString::from))
    }

    fn run(args: &[&str]) -> Args {
        match parse(args) {
            Ok(Asked::Run(args)) => *args,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn config_files_are_rules_or_settings_by_name() {
        let args = run(&["--base", "main", "--config", "a/gate.toml", "--config=b/pyproject.toml"]);
        assert_eq!(args.given.settings, Some("a/gate.toml".into()));
        assert_eq!(args.given.rules, Some("b/pyproject.toml".into()));
        let err = parse(&["--base", "m", "--config", "a.toml", "--config", "b.toml"]).unwrap_err();
        assert_eq!(err, "--config names at most one pyproject.toml and one file of settings");
    }

    #[test]
    fn reports_repeat_and_the_json_goes_beside_the_quality_file() {
        let args = run(&["--base=m", "--tests", "a.xml", "--tests", "b.xml", "--coverage", "c.xml", "--out", "q.md"]);
        assert_eq!(args.base, "m");
        assert_eq!(args.given.tests, [PathBuf::from("a.xml"), "b.xml".into()]);
        assert_eq!(args.given.coverage, [PathBuf::from("c.xml")]);
        assert_eq!(args.json, Some("q.json".into()));
        assert_eq!(run(&["--base", "m", "--out", "q.txt"]).json, Some("q.txt.json".into()));
        assert_eq!(run(&["--base", "m"]).json, None);
        assert_eq!(parse(&["--base", "m", "--out", "q.md", "--json", "./q.md"]).unwrap_err(),
                   "--json names the file --out writes");
    }

    #[test]
    fn bad_arguments_are_refused_and_help_is_asked_for() {
        assert_eq!(parse(&[]).unwrap_err(), "the following arguments are required: --base");
        assert_eq!(parse(&["--base"]).unwrap_err(), "argument --base: expected one argument");
        assert_eq!(parse(&["--base", "m", "--bogus"]).unwrap_err(), "unrecognized arguments: --bogus");
        assert_eq!(parse(&["--base", "m", "--mutation-budget", "0"]).unwrap_err(),
                   "argument --mutation-budget: must be at least 1, not 0");
        assert_eq!(parse(&["--base", "m", "--base-run-budget", "1.5"]).unwrap_err(),
                   "argument --base-run-budget: expected a whole number of seconds, not '1.5'");
        assert_eq!(run(&["--base", "m", "--base-run-budget", "7"]).given.base_run_budget, Some(7));
        assert_eq!(run(&["--base", "m", "--mutation-budget", "5"]).given.mutation_budget, Some(5));
        assert_eq!(run(&["--base", "m", "--python=.venv/bin/python"]).given.python, Some(".venv/bin/python".into()));
        assert_eq!(parse(&["--base", "m", "--python", " "]).unwrap_err(),
                   "argument --python: must be the path or the name of a Python");
        assert_eq!(parse(&["--base", "m", "--python="]).unwrap_err(),
                   "argument --python: must be the path or the name of a Python");
        assert_eq!(run(&["--base", "m", "--test-contexts", ".coverage"]).given.test_contexts, Some(".coverage".into()));
        assert!(parse(&["--base", "m", "--rust-syntax", "x"]).is_ok());
        assert_eq!(parse(&["--base", "m", "-x", "y"]).unwrap_err(), "unrecognized arguments: -x y");
        assert_eq!(parse(&["--base", "m", "x=y"]).unwrap_err(), "unrecognized arguments: x=y");
        assert_eq!(parse(&["--bogus", "--help"]), Ok(Asked::Help));
    }

    #[test]
    fn files_are_written_through_links_keeping_their_mode() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let dir = std::env::temp_dir().join(format!("gate-main-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let target = dir.join("target.md");
        std::fs::write(&target, "old").unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
        symlink(&target, dir.join("link.md")).unwrap();
        let link = dir.join("link.md");
        let other = dir.join("q.json");
        write_files(&[(&link, "new\r\n".into()), (&other, "{}".into())]).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "new\r\n");
        assert!(std::fs::symlink_metadata(&link).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::metadata(&target).unwrap().permissions().mode() & 0o777, 0o640);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "{}");
        let dangling = dir.join("dangling.md");
        symlink("unwritten.md", &dangling).unwrap();
        write_files(&[(&dangling, "first".into())]).unwrap();
        assert!(std::fs::symlink_metadata(&dangling).unwrap().file_type().is_symlink());
        assert_eq!(std::fs::read_to_string(dir.join("unwritten.md")).unwrap(), "first");
        std::fs::remove_file(&dangling).unwrap();
        std::fs::remove_file(dir.join("unwritten.md")).unwrap();
        let missing = dir.join("no/such/dir/q.md");
        let err = write_files(&[(&other, "changed".into()), (&missing, "x".into())]).unwrap_err();
        assert!(err.0.starts_with(&format!("cannot write {}: ", missing.display())), "{}", err.0);
        assert_eq!(std::fs::read_to_string(&other).unwrap(), "{}");
        let mut left: Vec<_> = std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name()).collect();
        left.sort();
        assert_eq!(left, ["link.md", "q.json", "target.md"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
