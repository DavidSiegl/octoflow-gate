//! The linters the gate runs over the files a tree tracks, and how it reads what they print. Ported so far:
//! pymarkdown, shellcheck and actionlint, which print a finding a line; ruff and mypy come with Python
//! (gate-standalone/16), clippy with Rust (14).

use std::collections::{HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Component, Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use regex::bytes::Regex;

use crate::glob::Globs;
use crate::process;
use crate::report::Finding;
use crate::settings::Settings;
use crate::text;

/// A linter that prints a finding a line as `path:line[:col]: detail`.
pub struct Linter {
    pub name: &'static str,
    /// Globs of the tracked files it reads unless gate.toml gives others ([`Settings::linter_globs`]); the gate does
    /// not run a linter that none of them match.
    globs: &'static [&'static str],
    /// The characters besides `\n` at which the linter ends a line of a file it reads and git does not; a `\r`
    /// counts only where no `\n` follows it. Below one, its line numbers are not git's, or it stops reading, so the
    /// gate refuses a change below one (suppressions::stray_line_ends).
    pub line_ends: &'static [char],
    /// The command line, given the pyproject.toml whose rules decide and the tracked files it reads.
    command: fn(rules: &Path, files: &[OsString]) -> Vec<OsString>,
    /// What follows the location of a finding, fully matched: groups `rule` and `message`, and `severity` where the
    /// linter grades its findings, else the finding is an error.
    detail: &'static str,
}

/// The linters, in the order the gate runs them.
pub static LINTERS: [Linter; 3] = [
    Linter {
        name: "pymarkdown",
        globs: &["**/*.md"],
        line_ends: &['\r'],
        command: |rules, files| argv(&["pymarkdown", "--config"], Some(rules), &["--strict-config", "scan"], files),
        detail: r"(?P<rule>MD\d+): (?P<message>.+)",
    },
    // shellcheck and actionlint have no table in pyproject.toml, so they run on their defaults: --norc and an empty
    // config file keep a .shellcheckrc or .github/actionlint.yaml, which a change could add, from loosening them.
    Linter {
        name: "shellcheck",
        globs: &["**/*.sh"],
        line_ends: &['\r'],
        command: |_, files| argv(&["shellcheck", "--norc", "-f", "gcc"], None, &[], files),
        detail: r"(?P<severity>error|warning|note): (?P<message>.+) \[(?P<rule>SC\d+)\]",
    },
    // Its YAML parser, ported from libyaml, also ends a line at NEL, U+2028 and U+2029, although YAML 1.2 does not
    // (https://yaml.org/spec/1.2.2/#54-line-break-characters). Observed with actionlint 1.7.12.
    Linter {
        name: "actionlint",
        globs: &[".github/workflows/*.yml", ".github/workflows/*.yaml"],
        line_ends: &['\r', '\u{85}', '\u{2028}', '\u{2029}'],
        command: |_, files| argv(&["actionlint", "-config-file", "/dev/null"], None, &[], files),
        detail: r"(?P<message>.+) \[(?P<rule>[\w-]+)\]",
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
    fn name_of(&self, shown: &[u8], root: &Path) -> Option<OsString> {
        let relative = lexical_rel_path(&root.join(OsStr::from_bytes(shown)), root).into_os_string();
        if self.names.contains(&relative) {
            return Some(relative);
        }
        let relative = std::str::from_utf8(relative.as_bytes()).ok()?;
        self.lossy.get(relative).cloned().flatten()
    }
}

/// `path` relative to `start`, both normalized lexically first, as Python's `os.path.relpath` gives it.
fn lexical_rel_path(path: &Path, start: &Path) -> PathBuf {
    let normal = |p: &Path| -> PathBuf {
        let mut out = PathBuf::from("/");
        for part in p.components() {
            match part {
                Component::ParentDir => {
                    out.pop();
                }
                Component::Normal(name) => out.push(name),
                _ => {}
            }
        }
        out
    };
    crate::paths::rel_path(&normal(path), &normal(start))
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
            .filter(|f| f.as_bytes() != b"mutants" && !f.as_bytes().starts_with(b"mutants/"))
            .filter(|f| self.reads(settings, f))
            .cloned()
            .collect()
    }

    /// The linter's findings over `files`, the tracked files it reads ([`Linter::files`]), in the tree whose top
    /// level is `root`, wherever they are, and the failed conditions it caused, each a sentence; none when it ran as
    /// it should. `rules` is the pyproject.toml whose rules decide. A linter that is missing, fails or does not
    /// finish within [`process::TOOL_TIMEOUT`] seconds is a condition.
    pub fn run(&self, root: &Path, rules: &Path, files: &[OsString], tracked: &Tracked) -> (Vec<Finding>, Vec<String>) {
        let argv = (self.command)(rules, files);
        let argv: Vec<&OsStr> = argv.iter().map(OsString::as_os_str).collect();
        let name = self.name;
        let done = match process::run(&argv, root, Duration::from_secs(process::TOOL_TIMEOUT)) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return (vec![], vec![format!("`{name}` is not installed: it is not on PATH.")])
            }
            Err(e) => return (vec![], vec![format!("`{name}` could not be started: {e}.")]),
            Ok(done) if done.stopped => {
                return (vec![], vec![format!("`{name}` did not finish within {} seconds.", process::TOOL_TIMEOUT)])
            }
            Ok(done) => done,
        };
        let found = self.findings(&done.stdout, root, tracked);
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

    /// The findings in what the linter printed, `output`, its paths made relative to `root`, the tree's top level,
    /// and its rules prefixed with its name. A line that is not a finding on a tracked file, such as a summary, is
    /// skipped. Only `\n` ends a line, so a name holding a carriage return or U+2028 stays whole; a name that is not
    /// UTF-8 may be printed as it is, or with U+FFFD for what is not, as [`Tracked`] reads it.
    pub fn findings(&self, output: &[u8], root: &Path, tracked: &Tracked) -> Vec<Finding> {
        static AT: LazyLock<Regex> = LazyLock::new(|| {
            Regex::new(r"(?s-u)^(?P<path>[^:]+):(?:(?P<line>[0-9]+):(?:[0-9]+:)?)? (?P<detail>.+)$").expect("valid")
        });
        let detail = Regex::new(&format!("(?s)^(?:{})$", self.detail.replace(".+", "(?-u:.)+")))
            .expect("a linter's detail is a valid regex");
        let mut out = vec![];
        for line in output.split(|&b| b == b'\n') {
            // Spaces, tabs and the \r of a \r\n only: other whitespace, such as U+2028, can be part of a name.
            let line = trim_bytes(line, b" \t\r");
            let Some(at) = AT.captures(line) else { continue };
            let Some(path) = tracked.name_of(&at["path"], root) else { continue };
            let Some(found) = detail.captures(&at["detail"]) else { continue };
            let group = |name: &str| found.name(name).map(|m| text::lossless(m.as_bytes()));
            out.push(Finding {
                path: Some(path),
                line: at.name("line").and_then(|n| std::str::from_utf8(n.as_bytes()).ok()?.parse().ok()),
                severity: group("severity").filter(|s| !s.is_empty()).unwrap_or_else(|| "error".into()),
                rule: match group("rule") {
                    Some(rule) if !rule.is_empty() => format!("{} {rule}", self.name),
                    _ => self.name.to_string(),
                },
                message: text::py_strip(&group("message").unwrap_or_default()).to_string(),
            });
        }
        out
    }
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

    #[test]
    fn findings_are_read_by_their_location_and_detail() {
        let tracked = Tracked::new(vec!["a.sh".into(), "d i r/b.sh".into(), OsString::from_vec(b"c\xff.sh".to_vec())]);
        let printed = "a.sh:3:1: warning: Quote this. [SC2086]\r\n\
                       /top/d i r/b.sh:7: error: Bad. [SC1000]\n\
                       c\u{fffd}.sh:1: note: Lossy. [SC2001]\n\
                       untracked.sh:1:1: error: X [SC1]\n\
                       In total 3 findings\n";
        let found = linter("shellcheck").findings(printed.as_bytes(), Path::new("/top"), &tracked);
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
    fn a_finding_without_a_severity_is_an_error() {
        let tracked = Tracked::new(vec!["README.md".into()]);
        let found = linter("pymarkdown").findings(b"README.md:2:1: MD013: Line length", Path::new("/t"), &tracked);
        assert_eq!((found[0].severity.as_str(), found[0].rule.as_str()), ("error", "pymarkdown MD013"));
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
        let missing = Linter { command: |_, _| vec!["gate-no-such-program".into()], ..*linter("shellcheck") };
        let (found, conditions) = missing.run(Path::new("."), Path::new("p"), &[], &Tracked::new(vec![]));
        assert!(found.is_empty());
        assert_eq!(conditions, ["`shellcheck` is not installed: it is not on PATH."]);
    }
}
