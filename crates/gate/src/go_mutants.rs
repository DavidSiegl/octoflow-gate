//! The surviving-mutants check's Go half: gremlins (<https://gremlins.dev>) mutates the Go functions a change edits
//! and runs each mutant against the tests of its package. The check itself, and its Rust half, are mutants.rs.

use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Instant;

use checks::Finding;
use regex::bytes::Regex;

use crate::copies::{copy_tracked, finding, in_scratch_directory, Budget};
use crate::diff::removal_points;
use crate::mutants::{dir_name, KILL_WAIT, STOP_GRACE, SURVIVOR_EXAMPLES};
use crate::parser_checks::GoChange;
use crate::paths::rel_path;
use crate::process::{run_with, Extras, Finished};
use crate::text;

/// How each status gremlins 0.6 prints for a mutant counts: a mutant that LIVED survived the tests of its package; a
/// KILLED one, and one whose tests outlived gremlins' timeout, did not; one NOT COVERED is on a line no test runs, so
/// gremlins did not try it, and could not have found it caught. NOT VIABLE, which does not build, and SKIPPED, outside
/// the lines the gate gave it, were not tried.
const SURVIVED: &str = "LIVED";
const TRIED: [&str; 3] = ["LIVED", "KILLED", "TIMED OUT"];
const NOT_COVERED: &str = "NOT COVERED";

/// A Go function the change edits, which gremlins mutates.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Edited {
    /// Its file, from the tree's top.
    path: OsString,
    /// Its name, a method's as `Stack.Push` (lang_go::read).
    name: String,
    /// The lines from its `func` to its closing brace, its doc comment left out.
    line: u32,
    last: u32,
}

/// The Go functions the change that `go` reads edits, in path and line order, and a `not-run` finding for each file
/// whose functions cannot be told, as one that does not parse.
///
/// A function counts when the change adds a line to it, from its `func` line to its closing brace, or removes lines
/// from between two of those, as a deleted guard clause needs a test as much as an added one: a function or method,
/// with a body, of a Go file that is code under test (GoChange::is_code) and read in the working tree, so not
/// generated, vendored or a symlink. One whose file's base has a function of its name with the same declaration, its
/// signature and body (lang_go::read's body digest), whatever its comments and layout, does not count: an edit to its
/// comments or layout alone changes nothing gremlins mutates.
pub(crate) fn edited_functions(go: &GoChange) -> (Vec<Edited>, Vec<Finding>) {
    let removals = removal_points(&go.change.diff);
    let (mut edited, mut unread) = (vec![], vec![]);
    for file in &go.files {
        let Some(after) = file.after.as_ref().filter(|side| !side.among_tests && go.is_code(&side.path)) else {
            continue;
        };
        let removed = removals.get(&after.path).cloned().unwrap_or_default();
        if file.added.is_empty() && removed.is_empty() {
            continue;
        }
        let source = match &after.source {
            Ok(source) => source,
            Err(unreadable) => {
                let message = format!("{}, so its functions were not mutation-tested.", unreadable.why);
                unread.push(finding(Some(&after.path), unreadable.line, "not-run", message));
                continue;
            }
        };
        let base = file.before.as_ref().and_then(|side| side.source.as_ref().ok());
        let unchanged = |f: &model::Function| {
            base.is_some_and(|base| base.functions.iter().any(|b| b.name == f.name && b.body == f.body))
        };
        for f in &source.functions {
            let (line, last) = (f.line, f.lines.last);
            let added = file.added.range(line..=last).next().is_some();
            if (added || removed.iter().any(|&n| line <= n && n < last)) && !unchanged(f) {
                edited.push(Edited { path: after.path.clone(), name: f.name.clone(), line, last });
            }
        }
    }
    (edited, unread)
}

/// Go's findings of the surviving-mutants check on `functions`, [`edited_functions`]' of `go`, in no order.
///
/// Runs gremlins, until `budget` runs out, on each Go module that holds one of them, the directory of the go.mod
/// nearest above its file, in a copy of the files git tracks, as [`on_copy`] says. Each function with mutants that
/// survived is a `survived` finding at its `func` line, with how many of the mutants tried of it they were and the
/// first [`SURVIVOR_EXAMPLES`] of them; one with mutants on lines no test runs an `untested` one, with how many. A
/// function gremlins makes no mutant of gives nothing. The budget running out is a `budget-reached` finding; a
/// function whose file lies under no go.mod in the copy, as where the change deletes it, a `not-mutated` one; a run
/// that cannot be made or fails a `not-run` one saying why, and a copy that cannot be removed afterwards a
/// `not-removed` one.
pub(crate) fn go_mutants(go: &GoChange, functions: &[Edited], budget: &Budget) -> Vec<Finding> {
    if functions.is_empty() {
        return vec![];
    }
    in_scratch_directory("not-run", "to run gremlins on a copy of the tree in", |dir| {
        on_copy(&go.change.root, &go.change.base, dir, functions, budget)
    })
}

/// The findings on `functions` of gremlins run on each Go module that holds one of them, in path order of the
/// modules, in a copy of the files git tracks at `root` made in `dir`, an empty directory, beside each run's files.
/// `base` is the change's base, which gremlins is told to diff against.
///
/// gremlins mutates only the lines it reads as changed from `git diff --merge-base <base>`, whose names it takes as
/// paths from the module's directory: the git a run finds first on PATH gives it the module's edited functions as a
/// diff ([`diff_of`]) instead, and passes any other command to git. So it mutates the edited functions whole, though
/// the copy has no repository to diff.
///
/// A run the deadline stops is a `budget-reached` finding beside what the mutants it had tried showed, and each module
/// after it a `not-run` one; a copy that cannot be made is the one `not-run` finding.
fn on_copy(root: &Path, base: &str, dir: &Path, functions: &[Edited], budget: &Budget) -> Vec<Finding> {
    let tree = dir.join("tree");
    if let Err(e) = copy_tracked(root, &tree) {
        return vec![not_run(&format!("could not copy the tree for gremlins: {e}"))];
    }
    let mut out = vec![];
    let mut modules: BTreeMap<PathBuf, Vec<&Edited>> = BTreeMap::new();
    for f in functions {
        match Path::new(&f.path).ancestors().skip(1).find(|d| tree.join(d).join("go.mod").is_file()) {
            Some(module) => modules.entry(module.to_path_buf()).or_default().push(f),
            None => out.push(finding(
                Some(&f.path),
                Some(f.line),
                "not-mutated",
                format!("`{}` was not mutation-tested: no go.mod lies above its file.", f.name),
            )),
        }
    }
    let modules: Vec<_> = modules.into_iter().collect();
    for (n, (module, mine)) in modules.iter().enumerate() {
        let run = Run { tree: &tree, scratch: &dir.join(format!("run-{n}")), base };
        let (found, stopped) = run_module(&run, module, mine, budget);
        out.extend(found);
        if stopped {
            out.extend(modules[n + 1..].iter().map(|(later, _)| {
                not_run(&format!(
                    "the budget ran out before gremlins reached the module at `{}`",
                    dir_name(later.as_os_str())
                ))
            }));
            break;
        }
    }
    out
}

/// The diff gremlins reads as the change to the module at `module`, a directory from the tree's top: for each file of
/// `functions`, each in the module, a section named by the file's path from the module's directory, with a hunk that
/// adds the lines of each function, from its `func` line to its last. The lines added are blank, as gremlins reads only
/// where they are.
fn diff_of(module: &Path, functions: &[&Edited]) -> Vec<u8> {
    let mut by_file: BTreeMap<&OsStr, Vec<&Edited>> = BTreeMap::new();
    for f in functions {
        by_file.entry(f.path.as_os_str()).or_default().push(f);
    }
    let mut out = vec![];
    for (path, mut mine) in by_file {
        mine.sort_by_key(|f| f.line);
        let name = rel_path(Path::new(path), module).into_os_string().into_vec();
        let name = name.as_slice();
        out.extend([b"diff --git a/", name, b" b/", name, b"\n--- a/", name, b"\n+++ b/", name, b"\n"].concat());
        for f in mine {
            let n = f.last - f.line + 1;
            out.extend(format!("@@ -0,0 +{},{n} @@\n", f.line).bytes());
            out.extend(b"+\n".repeat(n as usize));
        }
    }
    out
}

/// What a run of gremlins on one module reads and where it writes.
struct Run<'a> {
    /// The copy of the tracked files, in which gremlins makes its own copies to mutate.
    tree: &'a Path,
    /// A directory the run makes for the diff it reads and the git that gives it, outside the copy.
    scratch: &'a Path,
    /// The change's base, as gremlins' `--diff` names it.
    base: &'a str,
}

/// The findings of one run of gremlins in the module at `module`, a directory from the tree's top, on `functions`, as
/// `run` says, within `budget`; and whether the deadline stopped it, or was too near to start it.
fn run_module(run: &Run, module: &Path, functions: &[&Edited], budget: &Budget) -> (Vec<Finding>, bool) {
    let place = format!("the module at `{}`", dir_name(module.as_os_str()));
    let left = budget.deadline.checked_duration_since(Instant::now() + STOP_GRACE + KILL_WAIT);
    let Some(left) = left.filter(|left| !left.is_zero()) else {
        let message = format!(
            "budget reached before gremlins started on {place}: the {} seconds of `--mutation-budget` left too little \
             to run and stop it, so its mutants were not tried.",
            budget.seconds
        );
        return (vec![finding(None, None, "budget-reached", message)], true);
    };
    let path = match git_for_gremlins(run.scratch, &diff_of(module, functions)) {
        Ok(path) => path,
        Err(e) => return (vec![not_run(&format!("could not write the change's diff for gremlins: {e}"))], false),
    };
    let argv = ["gremlins", "unleash", "--diff", run.base].map(OsStr::new);
    // gremlins colours its output only on a terminal, which a process the gate starts has none of; NO_COLOR makes
    // sure.
    let env = [("PATH", path.as_os_str()), ("NO_COLOR", OsStr::new("1"))];
    let extras = Extras { env: &env, soft_stop: Some(STOP_GRACE) };
    match run_with(&argv, &run.tree.join(module), left, &extras) {
        Ok(done) => (run_findings(&done, module, functions, &place, budget.seconds), done.stopped),
        Err(e) => (vec![not_run(&format!("could not start gremlins: {e}"))], false),
    }
}

/// Writes `diff` and, beside it, an executable `git` that prints it when asked `git diff --merge-base ...` and runs
/// the git of the gate's PATH for anything else, into `scratch`, a directory it makes; gives back the PATH that finds
/// that `git` first. Errs when a file cannot be written.
fn git_for_gremlins(scratch: &Path, diff: &[u8]) -> std::io::Result<OsString> {
    let bin = scratch.join("bin");
    std::fs::create_dir_all(&bin)?;
    let diff_file = scratch.join("change.diff");
    std::fs::write(&diff_file, diff)?;
    let path = std::env::var_os("PATH").unwrap_or_default();
    let mut script = b"#!/bin/sh\nif [ \"$1\" = diff ] && [ \"$2\" = --merge-base ]; then exec cat ".to_vec();
    script.extend(quoted(diff_file.as_os_str()));
    script.extend(b"; fi\nPATH=");
    script.extend(quoted(&path));
    script.extend(b" exec git \"$@\"\n");
    let git = bin.join("git");
    std::fs::write(&git, script)?;
    std::fs::set_permissions(&git, std::fs::Permissions::from_mode(0o755))?;
    let mut found = bin.into_os_string();
    if !path.is_empty() {
        found.push(":");
        found.push(&path);
    }
    Ok(found)
}

/// `word` as one word of sh, in single quotes.
fn quoted(word: &OsStr) -> Vec<u8> {
    let mut out = b"'".to_vec();
    for &b in word.as_bytes() {
        match b {
            b'\'' => out.extend(b"'\\''"),
            b => out.push(b),
        }
    }
    out.push(b'\'');
    out
}

/// The findings of a finished run of gremlins, `done`, in the module at `module` (named as `place` in them), on
/// `functions`, under a budget of `budget` seconds: those of the mutants it printed ([`printed_mutants`],
/// [`function_findings`]), and a `budget-reached` one when the deadline stopped it. A `not-run` finding alone when it
/// failed, as it does when a test fails before any mutant: gremlins first runs the module's tests for their coverage.
///
/// gremlins writes its JSON report only when it finishes, so the gate reads the line it prints for each mutant, which a
/// stopped run has printed for those it tried.
fn run_findings(done: &Finished, module: &Path, functions: &[&Edited], place: &str, budget: u64) -> Vec<Finding> {
    if done.status != 0 && !done.stopped {
        let stderr = text::lossless(&done.stderr);
        let error = stderr.split('\n').find_map(|line| line.strip_prefix("ERROR: ")).map(text::clip);
        let said = error.unwrap_or_else(|| done.said());
        return vec![not_run(&format!("gremlins failed in {place}, exiting with status {}: {said}", done.status))];
    }
    let mutants = printed_mutants(&done.stdout, module);
    let mut out = function_findings(functions, &mutants);
    if done.stopped {
        let tried = mutants.iter().filter(|m| TRIED.contains(&m.status.as_str())).count();
        let when = match mutants.is_empty() {
            true => format!("before gremlins had tried a mutant of {place}"),
            false => format!("after {tried} of the mutants gremlins made of {place}"),
        };
        let message = format!(
            "budget reached {when}: it was stopped at {budget} seconds (`--mutation-budget`), so the mutants it did \
             not try may survive too."
        );
        out.push(finding(None, None, "budget-reached", message));
    }
    out
}

/// A mutant gremlins printed.
#[derive(Debug, PartialEq, Eq)]
struct Mutant {
    /// Its status and kind, as gremlins words them: `LIVED` and `CONDITIONALS_NEGATION`.
    status: String,
    kind: String,
    /// Its file, from the tree's top, and where in it the token it changes starts.
    path: OsString,
    line: u32,
    column: u32,
}

/// The mutants in `stdout`, what gremlins printed in the module at `module`, a directory from the tree's top, in the
/// order it printed them: a line each, `<status> <kind> at <file>:<line>:<column>`, the status padded on the left and
/// the file from the module's directory. Any other line, as its summary, is none.
fn printed_mutants(stdout: &[u8], module: &Path) -> Vec<Mutant> {
    static MUTANT: LazyLock<Regex> = LazyLock::new(|| {
        let place = r"(?P<file>.+):(?P<line>[0-9]+):(?P<column>[0-9]+)";
        Regex::new(&format!(r"(?-u)^ *(?P<status>[A-Z]+(?: [A-Z]+)?) (?P<kind>[A-Z_]+) at {place}\r?$"))
            .expect("a valid regex")
    });
    let mut out = vec![];
    for line in stdout.split(|&b| b == b'\n') {
        let Some(m) = MUTANT.captures(line) else { continue };
        let number = |name: &str| std::str::from_utf8(&m[name]).ok()?.parse::<u32>().ok();
        let (Some(at), Some(column)) = (number("line"), number("column")) else { continue };
        out.push(Mutant {
            status: text::lossless(&m["status"]),
            kind: text::lossless(&m["kind"]),
            path: module.join(OsStr::from_bytes(&m["file"])).into_os_string(),
            line: at,
            column,
        });
    }
    out
}

/// For each of `functions`, in their order, a `survived` finding when `mutants` hold mutants of it, on its lines,
/// that survived its tests, with how many of those tried and the first [`SURVIVOR_EXAMPLES`] of them, each by its line,
/// column and kind, as `line 13, column 11: CONDITIONALS_NEGATION`; and an `untested` one when they hold mutants of it
/// on lines no test runs, with how many.
fn function_findings(functions: &[&Edited], mutants: &[Mutant]) -> Vec<Finding> {
    let mut out = vec![];
    for f in functions {
        let mine: Vec<&Mutant> =
            mutants.iter().filter(|m| m.path == f.path && f.line <= m.line && m.line <= f.last).collect();
        let tried = mine.iter().filter(|m| TRIED.contains(&m.status.as_str())).count();
        let survived: Vec<&&Mutant> = mine.iter().filter(|m| m.status == SURVIVED).collect();
        if !survived.is_empty() {
            let examples: Vec<String> = (survived.iter().take(SURVIVOR_EXAMPLES))
                .map(|m| format!("line {}, column {}: {}", m.line, m.column, m.kind))
                .collect();
            let message = format!(
                "{} of {tried} mutants of `{}` survived its tests: {}.",
                survived.len(),
                f.name,
                examples.join("; ")
            );
            out.push(finding(Some(&f.path), Some(f.line), "survived", message));
        }
        let untested = mine.iter().filter(|m| m.status == NOT_COVERED).count();
        if untested > 0 {
            let message = format!(
                "{untested} mutants of `{}` are on lines no test runs, so none of them can make one fail.",
                f.name
            );
            out.push(finding(Some(&f.path), Some(f.line), "untested", message));
        }
    }
    out
}

/// A `not-run` finding: `problem`, a clause, and that the change's Go was not mutation-tested.
fn not_run(problem: &str) -> Finding {
    finding(None, None, "not-run", format!("{problem}; the change's Go was not mutation-tested."))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edited(path: &str, name: &str, line: u32, last: u32) -> Edited {
        Edited { path: path.into(), name: name.into(), line, last }
    }

    #[test]
    fn the_diff_gremlins_reads_adds_each_function_s_lines_named_from_the_module() {
        let (big, add, other) =
            (edited("m/calc.go", "Big", 11, 14), edited("m/calc.go", "Add", 6, 8), edited("m/sub/s.go", "S", 3, 3));
        let diff = String::from_utf8(diff_of(Path::new("m"), &[&big, &other, &add])).unwrap();
        assert_eq!(
            diff,
            "diff --git a/calc.go b/calc.go\n--- a/calc.go\n+++ b/calc.go\n@@ -0,0 +6,3 @@\n+\n+\n+\n\
             @@ -0,0 +11,4 @@\n+\n+\n+\n+\n\
             diff --git a/sub/s.go b/sub/s.go\n--- a/sub/s.go\n+++ b/sub/s.go\n@@ -0,0 +3,1 @@\n+\n"
        );
        let at_top = String::from_utf8(diff_of(Path::new(""), &[&edited("calc.go", "Add", 6, 6)])).unwrap();
        assert!(at_top.starts_with("diff --git a/calc.go b/calc.go\n"), "{at_top}");
    }

    #[test]
    fn the_mutants_are_read_from_what_gremlins_prints() {
        let printed = b"Starting...\nGathering coverage... done in 1s\n     SKIPPED ARITHMETIC_BASE at calc.go:7:11\n\
                        \x20      LIVED CONDITIONALS_BOUNDARY at sub/a b.go:13:11\n\
                        \x20NOT COVERED INVERT_NEGATIVES at calc.go:20:2\n\
                        Killed: 0, Lived: 1, Not covered: 1\n";
        let mutant = |status: &str, kind: &str, path: &str, line, column| Mutant {
            status: status.into(),
            kind: kind.into(),
            path: path.into(),
            line,
            column,
        };
        assert_eq!(
            printed_mutants(printed, Path::new("m")),
            [
                mutant("SKIPPED", "ARITHMETIC_BASE", "m/calc.go", 7, 11),
                mutant("LIVED", "CONDITIONALS_BOUNDARY", "m/sub/a b.go", 13, 11),
                mutant("NOT COVERED", "INVERT_NEGATIVES", "m/calc.go", 20, 2),
            ]
        );
        let at_top = printed_mutants(b"       LIVED X at calc.go:1:2\n", Path::new(""));
        assert_eq!(at_top[0].path, OsString::from("calc.go"));
    }

    #[test]
    fn each_function_is_judged_by_its_own_mutants() {
        let printed = b"   LIVED CONDITIONALS_BOUNDARY at calc.go:13:11\n  KILLED ARITHMETIC_BASE at calc.go:13:15\n\
                        TIMED OUT INCREMENT_DECREMENT at calc.go:12:3\n\
                        \x20  LIVED CONDITIONALS_NEGATION at calc.go:13:11\n\
                        NOT VIABLE INVERT_NEGATIVES at calc.go:12:9\n NOT COVERED ARITHMETIC_BASE at calc.go:14:9\n\
                        \x20  KILLED ARITHMETIC_BASE at calc.go:7:11\n   LIVED ARITHMETIC_BASE at other.go:12:1\n";
        let mutants = printed_mutants(printed, Path::new(""));
        let (add, big) = (edited("calc.go", "Add", 6, 8), edited("calc.go", "Big", 11, 15));
        let found = function_findings(&[&add, &big], &mutants);
        assert_eq!(
            found,
            [
                finding(
                    Some(OsStr::new("calc.go")),
                    Some(11),
                    "survived",
                    "2 of 4 mutants of `Big` survived its tests: line 13, column 11: CONDITIONALS_BOUNDARY; line 13, \
                     column 11: CONDITIONALS_NEGATION."
                        .into()
                ),
                finding(
                    Some(OsStr::new("calc.go")),
                    Some(11),
                    "untested",
                    "1 mutants of `Big` are on lines no test runs, so none of them can make one fail.".into()
                ),
            ]
        );
    }

    /// A run that exited with `exit_code`, stopped by the deadline or not, having printed `stdout` and `stderr`.
    fn done(exit_code: i32, stopped: bool, stdout: &[u8], stderr: &[u8]) -> Finished {
        Finished { status: exit_code, stdout: stdout.to_vec(), stderr: stderr.to_vec(), stopped }
    }

    #[test]
    fn a_run_that_fails_is_not_run_and_one_stopped_counts_what_it_tried() {
        let big = edited("m/calc.go", "Big", 11, 15);
        let place = "the module at `m`";
        let found = |done: &Finished| run_findings(done, Path::new("m"), &[&big], place, 30);
        let failed = done(1, false, b"--- FAIL: TestNo\n", b"go: no module dependencies to download\n\
                                                          ERROR: failed to gather coverage: exit status 1\n");
        assert_eq!(found(&failed), [not_run(
            "gremlins failed in the module at `m`, exiting with status 1: failed to gather coverage: exit status 1"
        )]);
        let unsaid = done(2, false, b"", b"panic: oh\n");
        assert_eq!(found(&unsaid), [not_run("gremlins failed in the module at `m`, exiting with status 2: panic: oh")]);
        let printed = b"   LIVED CONDITIONALS_BOUNDARY at calc.go:13:11\nNOT COVERED X at calc.go:14:1\n";
        let stopped = done(0, true, printed, b"");
        let rules: Vec<String> = found(&stopped).into_iter().map(|f| f.rule).collect();
        assert_eq!(rules, ["survived", "untested", "budget-reached"]);
        assert_eq!(found(&stopped)[2].message, "budget reached after 1 of the mutants gremlins made of the module at \
            `m`: it was stopped at 30 seconds (`--mutation-budget`), so the mutants it did not try may survive too.");
        let early = found(&done(0, true, b"Starting...\n", b""));
        assert_eq!(early[0].message, "budget reached before gremlins had tried a mutant of the module at `m`: it was \
            stopped at 30 seconds (`--mutation-budget`), so the mutants it did not try may survive too.");
        assert_eq!(found(&done(0, false, b"Starting...\n", b"")), []);
    }

    #[test]
    fn a_word_is_quoted_for_sh() {
        assert_eq!(quoted(OsStr::new("/tmp/it's here")), b"'/tmp/it'\\''s here'");
    }
}
