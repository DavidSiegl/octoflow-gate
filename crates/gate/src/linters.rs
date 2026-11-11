//! The linters the gate runs over the files a tree tracks, and how it reads what they print: ruff, mypy, pymarkdown,
//! shellcheck and actionlint, which print a finding a line, clippy (clippy.rs) and golangci-lint (golangci.rs).

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::LazyLock;
use std::time::Duration;

use regex::bytes::Regex;

use crate::glob::Globs;
use crate::copies::scratch_directory;
use crate::judge::under_mutmut_state;
use crate::paths::{normpath, rel_path};
use crate::process;
use crate::report::Finding;
use crate::settings::Settings;
use crate::text;

/// A linter the gate runs: which tracked files it reads, and how it runs over them and reads its findings.
pub struct Linter {
    pub name: &'static str,
    /// Globs of the tracked files it reads unless gate.toml gives others ([`Settings::linter_globs`]); the gate does
    /// not run a linter that none of them match.
    globs: &'static [&'static str],
    /// The characters besides `\n` at which the linter ends a line of a file it reads and git does not; a `\r`
    /// counts only where no `\n` follows it. Below one, its line numbers are not git's, or it stops reading, so the
    /// gate refuses a change below one (suppressions::stray_line_ends).
    pub line_ends: &'static [char],
    kind: Kind,
}

/// How a linter runs and what it prints.
#[derive(Clone, Copy)]
enum Kind {
    /// One run over the files, printing a finding a line as `path:line[:col]: detail`.
    Lines {
        /// The command line, given the pyproject.toml whose rules decide, None for the linter's defaults, and the
        /// files it reads. Either way, no other file the tree holds configures the run.
        command: fn(rules: Option<&Path>, files: &[OsString]) -> Vec<OsString>,
        /// Where it runs and how its files are named: true for an empty directory of the gate's own and absolute
        /// paths, for a linter that takes configuration from the directory it runs in whatever it is told; false
        /// for the tree's top level and paths from there.
        apart: bool,
        /// What follows the location of a finding, fully matched: groups `rule` and `message`, and `severity` where
        /// the linter grades its findings, else the finding is an error.
        detail: &'static str,
    },
    /// cargo clippy over the workspaces of the manifests among the files, as [`clippy::run`] runs it.
    Clippy,
    /// golangci-lint over the modules of the go.mod files among the files, as [`golangci::run`] runs it.
    GolangciLint,
}

/// The linters, in the order the gate runs them.
pub static LINTERS: [Linter; 7] = [
    Linter {
        name: "ruff",
        globs: &["**/*.py"],
        line_ends: &['\r'],
        kind: Kind::Lines {
            command: |rules, files| {
                let words = ["--output-format", "concise", "--no-cache"];
                match rules {
                    Some(rules) => argv(&["ruff", "check", "--config"], Some(rules), &words, files),
                    None => argv(&["ruff", "check", "--isolated"], None, &words, files),
                }
            },
            apart: false,
            detail: r"(?P<rule>[\w-]+):? (?:\[\*\] )?(?P<message>.+)",
        },
    },
    // Which files mypy checks is one of its rules, [tool.mypy] files; without rules, they are the files it reads. A
    // configuration file named "" is none at all, and keeps mypy from looking for one: observed with mypy 2.4.0,
    // which has no flag for it. A note elaborates an error; it is not one.
    Linter {
        name: "mypy",
        globs: &["**/*.py"],
        line_ends: &['\r'],
        kind: Kind::Lines {
            command: |rules, files| {
                let words = ["--show-column-numbers", "--no-error-summary", "--cache-dir", "/dev/null"];
                match rules {
                    Some(rules) => argv(&["mypy", "--config-file"], Some(rules), &words, &[]),
                    None => argv(&["mypy", "--config-file", ""], None, &words, files),
                }
            },
            apart: false,
            detail: r"(?P<severity>error): (?P<message>.+?)(?:  \[(?P<rule>[\w-]+)\])?",
        },
    },
    // pymarkdown adds to the configuration it is given what a pyproject.toml, .pymarkdown, .pymarkdown.yml or
    // .pymarkdown.yaml in its current directory says, and has no flag against it: observed with pymarkdownlnt
    // 0.9.41, in which a tree's own `[tool.pymarkdown]` turned off a rule that the file --config named left on.
    Linter {
        name: "pymarkdown",
        globs: &["**/*.md"],
        line_ends: &['\r'],
        kind: Kind::Lines {
            command: |rules, files| match rules {
                Some(rules) => argv(&["pymarkdown", "--config"], Some(rules), &["--strict-config", "scan"], files),
                None => argv(&["pymarkdown", "--strict-config", "scan"], None, &[], files),
            },
            apart: true,
            detail: r"(?P<rule>MD\d+): (?P<message>.+)",
        },
    },
    // shellcheck and actionlint have no table in pyproject.toml, so they run on their defaults: --norc and an empty
    // config file keep a .shellcheckrc or .github/actionlint.yaml, which a change could add, from loosening them.
    Linter {
        name: "shellcheck",
        globs: &["**/*.sh"],
        line_ends: &['\r'],
        kind: Kind::Lines {
            command: |_, files| argv(&["shellcheck", "--norc", "-f", "gcc"], None, &[], files),
            apart: false,
            detail: r"(?P<severity>error|warning|note): (?P<message>.+) \[(?P<rule>SC\d+)\]",
        },
    },
    // Its YAML parser, ported from libyaml, also ends a line at NEL, U+2028 and U+2029, although YAML 1.2 does not
    // (https://yaml.org/spec/1.2.2/#54-line-break-characters). Observed with actionlint 1.7.12.
    Linter {
        name: "actionlint",
        globs: &[".github/workflows/*.yml", ".github/workflows/*.yaml"],
        line_ends: &['\r', '\u{85}', '\u{2028}', '\u{2029}'],
        kind: Kind::Lines {
            command: |_, files| argv(&["actionlint", "-config-file", "/dev/null"], None, &[], files),
            apart: false,
            detail: r"(?P<message>.+) \[(?P<rule>[\w-]+)\]",
        },
    },
    // rustc numbers lines at "\n" alone, though Rust counts a lone "\r", NEL, U+2028 and U+2029 as whitespace:
    // observed with clippy 0.1.99 (rustc 1.99.1), whose finding below a line holding all four was on git's line.
    // Its manifests are among its files only to say which workspaces it lints.
    Linter { name: "clippy", globs: &["**/*.rs", "**/Cargo.toml"], line_ends: &[], kind: Kind::Clippy },
    // The go tool, whose parser golangci-lint's linters share, numbers lines at "\n" alone and reads a "\r" as
    // whitespace (https://go.dev/ref/spec#Tokens). Its go.mod files are among its files only to say which modules it
    // lints.
    Linter {
        name: "golangci-lint",
        globs: &["**/*.go", "**/go.mod"],
        line_ends: &[],
        kind: Kind::GolangciLint,
    },
];

/// `words`, then `path` when given, then `more`, then `files`, as an argv.
fn argv(words: &[&str], path: Option<&Path>, more: &[&str], files: &[OsString]) -> Vec<OsString> {
    let mut out: Vec<OsString> = words.iter().map(OsString::from).collect();
    out.extend(path.map(|p| p.as_os_str().to_owned()));
    out.extend(more.iter().map(OsString::from));
    out.extend(files.iter().cloned());
    out
}

/// The files a tree tracks, as `git ls-files -z` names them, which a path a linter printed is read against.
pub struct Tracked {
    /// In git's order.
    pub listed: Vec<OsString>,
    names: HashSet<OsString>,
    /// Each name that is not UTF-8 by how a tool that prints U+FFFD for what is not shows it; None for a showing two
    /// names share, which then stands for neither.
    lossy: HashMap<String, Option<OsString>>,
}

impl Tracked {
    pub fn new(listed: Vec<OsString>) -> Tracked {
        let mut lossy: HashMap<String, Option<OsString>> = HashMap::new();
        for name in &listed {
            if let std::borrow::Cow::Owned(shown) = String::from_utf8_lossy(name.as_bytes()) {
                lossy.entry(shown).and_modify(|was| *was = None).or_insert_with(|| Some(name.clone()));
            }
        }
        Tracked { names: listed.iter().cloned().collect(), listed, lossy }
    }

    /// The tracked file that `shown`, a path a linter printed, absolute or relative to `root`, names; None when it
    /// names none.
    pub(crate) fn name_of(&self, shown: &[u8], root: &Path) -> Option<OsString> {
        let relative = rel_path(&normpath(&root.join(OsStr::from_bytes(shown))), &normpath(root)).into_os_string();
        if self.names.contains(&relative) {
            return Some(relative);
        }
        let relative = std::str::from_utf8(relative.as_bytes()).ok()?;
        self.lossy.get(relative).cloned().flatten()
    }
}

impl Linter {
    /// Whether the linter reads the file at `path`, by the globs gate.toml gives it, else its own.
    pub fn reads(&self, settings: &Settings, path: &OsStr) -> bool {
        static OWN: LazyLock<Vec<Globs>> = LazyLock::new(|| LINTERS.iter().map(|l| Globs::new(l.globs)).collect());
        let own = LINTERS.iter().position(|l| l.name == self.name).map(|k| &OWN[k]);
        settings.linter_globs(self.name).or(own).is_some_and(|globs| globs.matches(path))
    }

    /// The files of `tracked` the linter reads, by `settings`, in git's order, none under mutmut's state, which no
    /// part of the judgement reads.
    pub fn files(&self, tracked: &Tracked, settings: &Settings) -> Vec<OsString> {
        tracked
            .listed
            .iter()
            .filter(|f| !under_mutmut_state(f))
            .filter(|f| self.reads(settings, f))
            .cloned()
            .collect()
    }

    /// The linter's findings over `files`, the tracked files it reads ([`Linter::files`]), in the tree whose top
    /// level is `root`, an absolute path, wherever they are, and the failed conditions it caused, each a sentence;
    /// none when it ran as it should. `rules` is the pyproject.toml whose rules decide, an absolute path, for a
    /// linter that takes them: with None such a linter runs on its defaults. A linter that is missing, fails or does
    /// not finish within [`process::TOOL_TIMEOUT`] seconds is a condition, as is a directory to run it in that
    /// cannot be made or removed.
    pub fn run(
        &self,
        root: &Path,
        rules: Option<&Path>,
        files: &[OsString],
        tracked: &Tracked,
    ) -> (Vec<Finding>, Vec<String>) {
        let (command, apart, detail) = match self.kind {
            Kind::Lines { command, apart, detail } => (command, apart, detail),
            Kind::Clippy => return crate::clippy::run(self.name, root, files, tracked),
            Kind::GolangciLint => return crate::golangci::run(self.name, root, files, tracked),
        };
        let name = self.name;
        if !apart {
            return read_run(name, detail, &command(rules, files), root, root, tracked);
        }
        let dir = match scratch_directory(&std::env::temp_dir()) {
            Ok(dir) => dir,
            Err(e) => return (vec![], vec![format!("`{name}` did not run: could not make a directory to run it in: {e}.")]),
        };
        let files: Vec<OsString> = files.iter().map(|f| root.join(f).into_os_string()).collect();
        let (found, mut conditions) = read_run(name, detail, &command(rules, &files), &dir, root, tracked);
        if let Err(e) = std::fs::remove_dir_all(&dir) {
            conditions.push(format!("Could not remove the directory `{name}` ran in, `{}`: {e}.", dir.display()));
        }
        (found, conditions)
    }
}

/// [`Linter::run`]'s findings and failed conditions for the line-printing linter `name`, run as `argv` in `cwd`:
/// what it printed is read by `detail` as [`findings`] reads it, against `root` and `tracked`.
fn read_run(
    name: &str,
    detail: &str,
    argv: &[OsString],
    cwd: &Path,
    root: &Path,
    tracked: &Tracked,
) -> (Vec<Finding>, Vec<String>) {
    let argv: Vec<&OsStr> = argv.iter().map(OsString::as_os_str).collect();
    let done = match tool_process(name, &argv, cwd) {
        Err(problem) => return (vec![], vec![problem]),
        Ok(done) => done,
    };
    let found = findings(name, detail, &done.stdout, root, tracked);
    // Exit status 1 means it found something, any other but 0 that it failed.
    if done.status == 0 || (done.status == 1 && !found.is_empty()) {
        return (found, vec![]);
    }
    let printed = [done.stderr, done.stdout].concat();
    let printed = text::lossless(&printed);
    let said = printed.split('\n').map(text::py_strip).find(|l| !l.is_empty()).unwrap_or("");
    let said: String = said.chars().take(200).collect();
    if done.status == 1 {
        return (vec![], vec![format!("`{name}` reported findings, but none the gate could read: {said}")]);
    }
    (vec![], vec![format!("`{name}` failed with exit status {}: {said}", done.status)])
}

/// `argv` run in `cwd` for the linter `name`, as [`process::run`] runs it, within [`process::TOOL_TIMEOUT`] seconds:
/// the finished process, whatever its exit status; or, when it could not be started or did not finish, the failed
/// condition, a sentence naming the linter.
pub(crate) fn tool_process(name: &str, argv: &[&OsStr], cwd: &Path) -> Result<process::Finished, String> {
    match process::run(argv, cwd, Duration::from_secs(process::TOOL_TIMEOUT)) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let program = argv.first().map(|p| p.to_string_lossy()).unwrap_or_default();
            let program = if program == name { "it".to_string() } else { format!("`{program}`") };
            Err(format!("`{name}` is not installed: {program} is not on PATH."))
        }
        Err(e) => Err(format!("`{name}` could not be started: {e}.")),
        Ok(done) if done.stopped => Err(format!("`{name}` did not finish within {} seconds.", process::TOOL_TIMEOUT)),
        Ok(done) => Ok(done),
    }
}

/// The findings in what the linter `name` printed, `output`, a finding a line whose `detail` is as [`Kind::Lines`]
/// says, its paths made relative to `root`, the tree's top level, and its rules prefixed with its name. A line that is
/// not a finding on a tracked file, such as a summary, is skipped. Only `\n` ends a line, so a name holding a carriage
/// return or U+2028 stays whole; a name that is not UTF-8 may be printed as it is, or with U+FFFD for what is not, as
/// [`Tracked`] reads it.
fn findings(name: &str, detail: &str, output: &[u8], root: &Path, tracked: &Tracked) -> Vec<Finding> {
    static AT: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r"(?s-u)^(?P<path>[^:]+):(?:(?P<line>[0-9]+):(?:[0-9]+:)?)? (?P<detail>.+)$").expect("valid")
    });
    let detail = Regex::new(&format!("(?s)^(?:{})$", detail.replace(".+", "(?-u:.)+")))
        .expect("a linter's detail is a valid regex");
    let top = [root.as_os_str().as_bytes(), b"/"].concat();
    let mut out = vec![];
    for line in output.split(|&b| b == b'\n') {
        // Spaces, tabs and the \r of a \r\n only: other whitespace, such as U+2028, can be part of a name.
        let line = trim_bytes(line, b" \t\r");
        // A linter run apart names its files by absolute paths, under a root whose own path may hold a colon.
        let line = line.strip_prefix(top.as_slice()).unwrap_or(line);
        let Some(at) = AT.captures(line) else { continue };
        let Some(path) = tracked.name_of(&at["path"], root) else { continue };
        let Some(found) = detail.captures(&at["detail"]) else { continue };
        let group = |name: &str| found.name(name).map(|m| text::lossless(m.as_bytes()));
        out.push(Finding {
            path: Some(path),
            line: at.name("line").and_then(|n| std::str::from_utf8(n.as_bytes()).ok()?.parse().ok()),
            severity: group("severity").filter(|s| !s.is_empty()).unwrap_or_else(|| "error".into()),
            rule: match group("rule") {
                Some(rule) if !rule.is_empty() => format!("{name} {rule}"),
                _ => name.to_string(),
            },
            message: text::py_strip(&group("message").unwrap_or_default()).to_string(),
        });
    }
    out
}

/// `bytes` without any of `strip` at either end.
fn trim_bytes<'a>(bytes: &'a [u8], strip: &[u8]) -> &'a [u8] {
    let start = bytes.iter().position(|b| !strip.contains(b)).unwrap_or(bytes.len());
    let end = bytes.iter().rposition(|b| !strip.contains(b)).map_or(start, |k| k + 1);
    &bytes[start..end]
}

/// The tracked files `git ls-files -z` names in `listed`, in its order.
pub fn tracked_from(listed: &[u8]) -> Tracked {
    Tracked::new(listed.split(|&b| b == 0).filter(|n| !n.is_empty()).map(|n| OsString::from_vec(n.to_vec())).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linter(name: &str) -> &'static Linter {
        LINTERS.iter().find(|l| l.name == name).unwrap()
    }

    /// The findings in `output`, as the line-reading linter `name` prints them.
    fn read(name: &str, output: &[u8], root: &Path, tracked: &Tracked) -> Vec<Finding> {
        let Kind::Lines { detail, .. } = linter(name).kind else { panic!("{name} prints no finding a line") };
        findings(name, detail, output, root, tracked)
    }

    #[test]
    fn findings_are_read_by_their_location_and_detail() {
        let tracked = Tracked::new(vec!["a.sh".into(), "d i r/b.sh".into(), OsString::from_vec(b"c\xff.sh".to_vec())]);
        let printed = "a.sh:3:1: warning: Quote this. [SC2086]\r\n\
                       /top/d i r/b.sh:7: error: Bad. [SC1000]\n\
                       c\u{fffd}.sh:1: note: Lossy. [SC2001]\n\
                       untracked.sh:1:1: error: X [SC1]\n\
                       In total 3 findings\n";
        let found = read("shellcheck", printed.as_bytes(), Path::new("/top"), &tracked);
        let got: Vec<_> =
            found.iter().map(|f| (f.path.clone().unwrap(), f.line, f.severity.as_str(), f.rule.as_str())).collect();
        assert_eq!(
            got,
            [
                (OsString::from("a.sh"), Some(3), "warning", "shellcheck SC2086"),
                (OsString::from("d i r/b.sh"), Some(7), "error", "shellcheck SC1000"),
                (OsString::from_vec(b"c\xff.sh".to_vec()), Some(1), "note", "shellcheck SC2001"),
            ]
        );
        assert_eq!(found[0].message, "Quote this.");
    }

    #[test]
    fn an_absolute_path_is_read_under_a_root_whose_path_holds_a_colon() {
        // pymarkdown runs apart, on absolute paths, which the location's `[^:]+` alone would cut at the root's colon.
        let tracked = Tracked::new(vec!["README.md".into()]);
        let found = read("pymarkdown", b"/w/a:b/repo/README.md:3:1: MD047: End", Path::new("/w/a:b/repo"), &tracked);
        assert_eq!(fields(&found), [("README.md", Some(3), "error", "pymarkdown MD047", "End")]);
    }

    #[test]
    fn a_finding_without_a_severity_is_an_error() {
        let tracked = Tracked::new(vec!["README.md".into()]);
        let found = read("pymarkdown", b"README.md:2:1: MD013: Line length", Path::new("/t"), &tracked);
        assert_eq!((found[0].severity.as_str(), found[0].rule.as_str()), ("error", "pymarkdown MD013"));
    }

    /// Each finding's path, line, severity, rule and message.
    fn fields(found: &[Finding]) -> Vec<(&str, Option<u32>, &str, &str, &str)> {
        let mut out = vec![];
        for f in found {
            let path = f.path.as_ref().unwrap().to_str().unwrap();
            out.push((path, f.line, f.severity.as_str(), f.rule.as_str(), f.message.as_str()));
        }
        out
    }

    #[test]
    fn ruff_s_rule_is_a_code_or_a_name_before_a_colon_and_its_fix_mark_is_left_out() {
        let tracked = Tracked::new(vec!["orchestrator.py".into()]);
        let printed = "orchestrator.py:595:18: F402 Import `field` from line 55 shadowed\n\
                       orchestrator.py:1:8: invalid-syntax: Expected an expression\n\
                       orchestrator.py:6:8: F401 [*] `os` imported but unused\n\
                       build/x.py:1:1: F401 unused\n\
                       Found 4 errors.\n";
        let found = read("ruff", printed.as_bytes(), Path::new("/repo"), &tracked);
        assert_eq!(
            fields(&found),
            [
                ("orchestrator.py", Some(595), "error", "ruff F402", "Import `field` from line 55 shadowed"),
                ("orchestrator.py", Some(1), "error", "ruff invalid-syntax", "Expected an expression"),
                ("orchestrator.py", Some(6), "error", "ruff F401", "`os` imported but unused"),
            ]
        );
    }

    #[test]
    fn mypy_s_notes_are_dropped_and_an_error_without_a_code_is_mypy_s_own() {
        let tracked = Tracked::new(vec!["orchestrator.py".into()]);
        let printed = "orchestrator.py:594:5: error: Need type annotation for \"tokens\"  [var-annotated]\n\
                       orchestrator.py:594:5: note: See https://x\n\
                       orchestrator.py:7: error: Bad [x] thing\n";
        let found = read("mypy", printed.as_bytes(), Path::new("/repo"), &tracked);
        assert_eq!(
            fields(&found),
            [
                ("orchestrator.py", Some(594), "error", "mypy var-annotated", "Need type annotation for \"tokens\""),
                ("orchestrator.py", Some(7), "error", "mypy", "Bad [x] thing"),
            ]
        );
    }

    #[test]
    fn a_name_ruff_prints_lossily_is_not_guessed_between_two_files() {
        let names = [&b"a\xe9.py"[..], b"a\xe8.py"].map(|n| OsString::from_vec(n.to_vec()));
        let printed = "a\u{fffd}.py:1:8: F401 `os` imported but unused\n";
        assert!(read("ruff", printed.as_bytes(), Path::new("/repo"), &Tracked::new(names.to_vec())).is_empty());
    }

    #[test]
    fn gate_toml_s_globs_replace_a_linter_s_own() {
        let mut settings = Settings::default();
        assert!(linter("actionlint").reads(&settings, OsStr::new(".github/workflows/ci.yml")));
        settings.linters.push(("actionlint".into(), Globs::new::<&str>(&[])));
        assert!(!linter("actionlint").reads(&settings, OsStr::new(".github/workflows/ci.yml")));
    }

    #[test]
    fn a_missing_linter_is_a_condition() {
        let Kind::Lines { detail, .. } = linter("shellcheck").kind else { unreachable!() };
        let kind = Kind::Lines { command: |_, _| vec!["gate-no-such-program".into()], apart: false, detail };
        let missing = Linter { kind, ..*linter("shellcheck") };
        let (found, conditions) = missing.run(Path::new("."), None, &[], &Tracked::new(vec![]));
        assert!(found.is_empty());
        assert_eq!(conditions, ["`shellcheck` is not installed: `gate-no-such-program` is not on PATH."]);
        let argv = [OsStr::new("gate-no-such-program")];
        let problem = tool_process("gate-no-such-program", &argv, Path::new(".")).unwrap_err();
        assert_eq!(problem, "`gate-no-such-program` is not installed: it is not on PATH.");
    }
}
