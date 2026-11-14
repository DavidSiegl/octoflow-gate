//! What the quality file tells a reviewer about the added lines themselves, read from the working tree: the
//! suppressions they add, and the line ends a linter reads that git does not.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::LazyLock;

use regex::Regex;

use crate::diff::ChangedLines;
use crate::linters::Linter;
use crate::Unjudged;

/// How one kind of file silences something the gate runs, a linter or a coverage or mutation tool, which the quality
/// file lists for the reviewer: shell scripts and workflows, whose run scripts actionlint has shellcheck read,
/// Markdown, Rust and Go. Python is told and read its own way ([`python_suppressions`]), ahead of these.
struct Syntax {
    /// Whether the file at `path`, from the tree's top level, is of this kind.
    holds: fn(path: &[u8]) -> bool,
    /// Matches, anywhere in a line, a line that silences something.
    pattern: &'static str,
}

/// Matches a Python comment that silences ruff, mypy, coverage.py or mutmut.
const PYTHON_SUPPRESSION: &str = r"#.*\b(?:noqa|type:\s*ignore|pragma:\s*no\s*(?:cover|branch|mutate))\b";

/// The kinds of file, the first that holds a file deciding; a file of none holds no suppression the gate knows.
static SYNTAXES: [Syntax; 4] = [
    // A comment for shellcheck.
    Syntax {
        holds: |path| {
            path.ends_with(b".sh")
                || (path.starts_with(b".github/workflows/") && (path.ends_with(b".yml") || path.ends_with(b".yaml")))
        },
        pattern: r"#.*\bshellcheck\s+disable\b",
    },
    // pymarkdown's pragma, an HTML comment from the line's first column to its end (pymarkdownlnt 0.9.41's
    // PragmaToken.look_for_pragmas).
    Syntax { holds: |path| path.ends_with(b".md"), pattern: r"(?i)^<!---?\s*pyml\s+disable.*-->\s*$" },
    // An attribute for clippy and rustc: an allow or expect inside one attribute's brackets, also inside a cfg_attr,
    // as in `#![cfg_attr(test, allow(...))]`, or alone on a line, as rustfmt wraps a long cfg_attr's; and
    // cargo-mutants' own `#[mutants::skip]`, also inside a cfg_attr, wrapped or not, which keeps a function from
    // being mutated.
    Syntax {
        holds: |path| path.ends_with(b".rs"),
        pattern: concat!(
            r"#!?\[[^\]]*\b(?:allow|expect)\s*\(|^\s*(?:allow|expect)\s*\(",
            r"|#!?\[[^\]]*\bmutants::skip\b|^\s*mutants::skip\b"
        ),
    },
    // A directive for golangci-lint, `//nolint` or `//nolint:linter,...`, and `// nolint`, which its 1.x releases also
    // took; one for staticcheck, `//lint:ignore` or `//lint:file-ignore`; and the generated-code header
    // (https://go.dev/s/generatedcode), which takes the whole file out of the parser checks (lang_go::generated).
    Syntax {
        holds: |path| path.ends_with(b".go"),
        pattern: r"//\s*nolint\b|//lint:(?:file-)?ignore\b|^// Code generated .* DO NOT EDIT\.\r?$",
    },
];

/// The (path, line, text) of each added line, in the working tree at `root`, that silences something the gate runs,
/// in path and line order, the text stripped and cut to 160 characters. `changed` is the lines the change added, as
/// diff::changed_lines gives them. A file's suppressions are those of its kind: Python's
/// ([`python_suppressions`]) for a `.py` file or a script whose `#!` line names python, whatever else its name says,
/// and otherwise those of the first of [`SYNTAXES`] that holds it. Errs when a file the change adds lines to cannot
/// be read.
pub fn added_suppressions(root: &Path, changed: &ChangedLines) -> Result<Vec<(OsString, u32, String)>, Unjudged> {
    static PATTERNS: LazyLock<Vec<Regex>> =
        LazyLock::new(|| SYNTAXES.iter().map(|s| Regex::new(s.pattern).expect("a valid regex")).collect());
    let mut out = vec![];
    for (path, added, lines) in files_with_added_lines(root, changed)? {
        let text = |n: u32| lines.get(n as usize - 1).map(|l| String::from_utf8_lossy(l)).unwrap_or_default();
        let suppressing: Vec<u32> = if lang_python::is_python(path.as_bytes(), || lines[0].clone()) {
            python_suppressions(&lines).into_iter().filter(|n| added.contains(n)).collect()
        } else if let Some(k) = SYNTAXES.iter().position(|s| (s.holds)(path.as_bytes())) {
            added.iter().copied().filter(|&n| PATTERNS[k].is_match(&text(n))).collect()
        } else {
            continue;
        };
        for n in suppressing {
            out.push((path.clone(), n, crate::text::py_strip(&text(n)).chars().take(160).collect()));
        }
    }
    Ok(out)
}

/// The numbers, from 1 and in order, of the lines of a Python file that silence something: those where a comment
/// that [`PYTHON_SUPPRESSION`] matches starts, so that the same words in a string are none. `lines` is the file's
/// lines as [`files_with_added_lines`] splits them. In a file that crates/lang-python cannot read, each line the
/// pattern matches, so that a suppression in a file ruff reports as broken is still listed.
fn python_suppressions(lines: &[Vec<u8>]) -> Vec<u32> {
    static PATTERN: LazyLock<Regex> = LazyLock::new(|| Regex::new(PYTHON_SUPPRESSION).expect("a valid regex"));
    let numbered = |texts: Vec<(u32, String)>| -> Vec<u32> {
        let mut out: Vec<u32> = texts.into_iter().filter(|(_, text)| PATTERN.is_match(text)).map(|(n, _)| n).collect();
        out.dedup();
        out
    };
    match lang_python::comments(&lines.join(&b'\n')) {
        Some(comments) => numbered(comments),
        None => numbered((1..).zip(lines.iter().map(|l| String::from_utf8_lossy(l).into_owned())).collect()),
    }
}

/// (path, line, the character) for each file in `changed` that a linter of `reads` reads and whose working tree at
/// `root` holds one of that linter's [`Linter::line_ends`] at or before the last line the change added: the first
/// line that holds one, whichever commit added it. `reads` pairs each linter with the tracked files it reads. Errs
/// when such a file cannot be read.
pub fn stray_line_ends(
    root: &Path,
    changed: &ChangedLines,
    reads: &[(&Linter, Vec<OsString>)],
) -> Result<Vec<(OsString, u32, char)>, Unjudged> {
    let mut line_ends: std::collections::BTreeMap<&OsStr, BTreeSet<char>> = Default::default();
    for (linter, files) in reads {
        for f in files {
            line_ends.entry(f.as_os_str()).or_default().extend(linter.line_ends);
        }
    }
    let read: ChangedLines = changed
        .iter()
        .filter(|(p, _)| line_ends.contains_key(p.as_os_str()))
        .map(|(p, added)| (p.clone(), added.clone()))
        .collect();
    let mut out = vec![];
    for (path, added, lines) in files_with_added_lines(root, &read)? {
        let last = added.last().copied().unwrap_or(0) as usize;
        let ends = &line_ends[path.as_os_str()];
        let first = lines.iter().take(last).enumerate().find_map(|(k, line)| {
            let line = line.strip_suffix(b"\r").unwrap_or(line);
            ends.iter().find(|c| contains(line, c.encode_utf8(&mut [0; 4]).as_bytes())).map(|&c| (k as u32 + 1, c))
        });
        if let Some((n, c)) = first {
            out.push((path.clone(), n, c));
        }
    }
    Ok(out)
}

/// The failed condition a stray line end of [`stray_line_ends`] is.
pub fn stray_line_end_condition(path: &OsStr, n: u32, end: char) -> String {
    let escaped = match u32::from(end) {
        0x0d => "\\r".to_string(),
        c if c <= 0xff => format!("\\x{c:02x}"),
        c => format!("\\u{c:04x}"),
    };
    format!(
        "`{}:{n}` holds `{escaped}`, which a linter that reads the file ends a line at and git does not. Such a \
         linter numbers every later line past git's, or stops reading there; either way the gate cannot judge its \
         findings below it. Remove it, end the line there, or write it as an escape in a string.",
        crate::text::path(path)
    )
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|w| w == needle)
}

/// One file the change adds lines to: its path, those lines' numbers, and its lines as [`files_with_added_lines`]
/// splits them.
type AddedTo<'a> = (&'a OsString, &'a BTreeSet<u32>, Vec<Vec<u8>>);

/// (path, the lines `changed` says the change added to it, its lines) for each file in `changed` with added lines,
/// in path order, as the working tree at `root` has it.
///
/// The lines are numbered as git numbers them: split at `\n` alone, each without it, so line n is `lines[n - 1]`.
/// A symlink's one line is the name it points to, which is what git diffs, not its target's text. A directory, such
/// as a submodule, whose commit is what changed, is left out. Errs when a file cannot be read.
fn files_with_added_lines<'a>(
    root: &Path,
    changed: &'a ChangedLines,
) -> Result<Vec<AddedTo<'a>>, Unjudged> {
    let mut out = vec![];
    for (path, added) in changed.iter().filter(|(_, added)| !added.is_empty()) {
        let full = root.join(path);
        let read = match full.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() => {
                std::fs::read_link(&full).map(|target| target.into_os_string().into_vec())
            }
            Ok(meta) if meta.is_dir() => continue,
            _ => std::fs::read(&full),
        };
        let bytes = read.map_err(|e| {
            Unjudged(format!("cannot read {}, which the change edits: {e}", crate::text::path(path)))
        })?;
        out.push((path, added, bytes.split(|&b| b == b'\n').map(<[u8]>::to_vec).collect()));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linters::LINTERS;
    use crate::scratch::ScratchDir;

    /// A new directory holding `files`, each a path under it and its content.
    fn tree(files: &[(&str, &[u8])]) -> ScratchDir {
        let root = ScratchDir::new();
        for (path, content) in files {
            std::fs::create_dir_all(root.join(path).parent().unwrap()).unwrap();
            std::fs::write(root.join(path), content).unwrap();
        }
        root
    }

    fn lines(entries: &[(&str, &[u32])]) -> ChangedLines {
        entries.iter().map(|(p, ns)| (OsString::from(p), ns.iter().copied().collect())).collect()
    }

    #[test]
    fn suppressions_on_added_lines_are_listed_by_their_file_s_syntax() {
        let root = tree(&[
            ("a.sh", b"x # shellcheck disable=SC2086\n# shellcheck disable=SC1\n"),
            ("b.md", b"<!-- pyml disable-next-line md013-->\n"),
            ("c.txt", b"# shellcheck disable=SC1\n"),
        ]);
        let found = added_suppressions(&root, &lines(&[("a.sh", &[1]), ("b.md", &[1]), ("c.txt", &[1])])).unwrap();
        assert_eq!(
            found,
            [
                ("a.sh".into(), 1, "x # shellcheck disable=SC2086".into()),
                ("b.md".into(), 1, "<!-- pyml disable-next-line md013-->".into())
            ]
        );
    }

    #[test]
    fn in_python_only_a_comment_is_a_suppression_whatever_the_file_s_name() {
        let root = tree(&[
            ("f.py", b"hint = \"add # noqa: F401\"\ndoc = \"\"\"\n# type: ignore\n\"\"\"\nimport os  # noqa: F401\n"),
            ("bin/tool", b"#!/usr/bin/env python3\nprint('# noqa')  # pragma: no cover\n"),
            ("run.sh", b"#!/usr/bin/python3\n# shellcheck disable=SC1\nx = 1  # type: ignore\n"),
            ("form.py", b"a = 1\x0c\nimport os  # noqa: F401\n"),
        ]);
        let all = [1, 2, 3, 4, 5];
        let added = lines(&[("f.py", &all), ("bin/tool", &all[..2]), ("run.sh", &all[..3]), ("form.py", &[2])]);
        assert_eq!(
            added_suppressions(&root, &added).unwrap(),
            [
                ("bin/tool".into(), 2, "print('# noqa')  # pragma: no cover".into()),
                ("f.py".into(), 5, "import os  # noqa: F401".into()),
                ("form.py".into(), 2, "import os  # noqa: F401".into()),
                ("run.sh".into(), 3, "x = 1  # type: ignore".into()),
            ]
        );
    }

    #[test]
    fn in_python_that_cannot_be_read_each_line_the_pattern_matches_is_a_suppression() {
        let root = tree(&[("f.py", b"import os  # noqa: F401\ns = \"\"\"unterminated # noqa\n")]);
        assert_eq!(
            added_suppressions(&root, &lines(&[("f.py", &[1, 2])])).unwrap(),
            [
                ("f.py".into(), 1, "import os  # noqa: F401".into()),
                ("f.py".into(), 2, "s = \"\"\"unterminated # noqa".into())
            ]
        );
    }

    #[test]
    fn rust_s_allow_expect_and_mutants_skip_are_suppressions() {
        let rust = Regex::new(SYNTAXES.iter().find(|s| (s.holds)(b"a.rs")).unwrap().pattern).unwrap();
        for line in [
            "#![cfg_attr(not(test), allow(clippy::all))]",
            "#[expect(dead_code)]",
            "    allow(clippy::needless_return)",
            "#[mutants::skip]",
            "#[cfg_attr(test, mutants::skip)]",
            "    #[mutants::skip] fn f() {}",
            "    mutants::skip",
            "    mutants::skip,",
        ] {
            assert!(rust.is_match(line), "{line}");
        }
        for line in ["let mutants = skip(1);", "Write `#[deny(lint)]`.", "fn allow_all() {}"] {
            assert!(!rust.is_match(line), "{line}");
        }
    }

    #[test]
    fn go_s_nolint_and_lint_ignore_are_suppressions() {
        let go = Regex::new(SYNTAXES.iter().find(|s| (s.holds)(b"a.go")).unwrap().pattern).unwrap();
        for line in [
            "x := f() //nolint:errcheck // closed below",
            "//nolint",
            "\tdefer f.Close() // nolint",
            "//lint:ignore SA1019 the old API is what we test",
            "//lint:file-ignore U1000 generated",
            "// Code generated by hand. DO NOT EDIT.",
            "// Code generated by protoc-gen-go. DO NOT EDIT.\r",
        ] {
            assert!(go.is_match(line), "{line}");
        }
        for line in [
            "nolintCount := 0",
            "// lint the file",
            "//lint:check SA1019",
            "// Code generated by hand. Edit freely.",
            "\t// Code generated x DO NOT EDIT.",
        ] {
            assert!(!go.is_match(line), "{line}");
        }
    }

    #[test]
    fn a_stray_line_end_up_to_the_last_added_line_is_found() {
        let root = tree(&[
            (".github/workflows/w.yml", "a\nb\u{2028}c\r\nd\re\n".as_bytes()),
            ("late.sh", b"a\nb\nc\rd\n"),
        ]);
        let changed = lines(&[(".github/workflows/w.yml", &[4]), ("late.sh", &[2])]);
        let actionlint = LINTERS.iter().find(|l| l.name == "actionlint").unwrap();
        let shellcheck = LINTERS.iter().find(|l| l.name == "shellcheck").unwrap();
        let reads =
            [(actionlint, vec![".github/workflows/w.yml".into()]), (shellcheck, vec!["late.sh".into()])];
        let found = stray_line_ends(&root, &changed, &reads).unwrap();
        assert_eq!(found, [(".github/workflows/w.yml".into(), 2, '\u{2028}')]);
        assert!(stray_line_end_condition(OsStr::new("w"), 2, '\u{2028}').starts_with("`w:2` holds `\\u2028`, "));
        assert!(stray_line_end_condition(OsStr::new("w"), 2, '\r').starts_with("`w:2` holds `\\r`, "));
    }
}
