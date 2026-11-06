//! What a change is, read from `git diff -U0`: the files it touches, the lines each gains or loses, and the paths as
//! git quotes them. The diff is bytes, as git prints it, and each path is the file's own name in bytes, so a line or
//! a name that is not UTF-8 is judged like any other.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::sync::LazyLock;

use regex::bytes::Regex;

use crate::git;
use crate::Unjudged;

/// Line numbers, from 1, by a file's path relative to the tree's top. Which tree, and which lines, each producer
/// says; a file with no lines maps to an empty set, which differs from its absence.
pub type ChangedLines = BTreeMap<OsString, BTreeSet<u32>>;

/// `git diff -U0` from `base` to the working tree's tracked files under `root`, in the one shape [`changed_lines`]
/// parses, whatever the user's git config. With `pathspecs`, git's, only the files they match: a rename from a file
/// they leave out is an added file. Only git's `\n` ends a line of it, so nothing inside an added line can pass for
/// a header. Errs as [`git::output`] does.
pub fn change_diff(root: &Path, base: &str, pathspecs: &[&str]) -> Result<Vec<u8>, Unjudged> {
    let mut args = vec![
        "-c",
        "core.quotePath=false",
        "diff",
        "-U0",
        "-M",
        "--no-color",
        "--no-ext-diff",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        base,
        "--",
    ];
    args.extend_from_slice(pathspecs);
    git::output(root, &args)
}

/// One tracked file a change touches: its path at the base and in the working tree, None on the side where it does
/// not exist; a rename pairs the two as [`change_diff`] does.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    pub before: Option<OsString>,
    pub after: Option<OsString>,
}

/// Each tracked file under `root` changed since `base`; `pathspecs` as for [`change_diff`]. Errs as
/// [`git::output`] does.
pub fn changed_files(root: &Path, base: &str, pathspecs: &[&str]) -> Result<Vec<ChangedFile>, Unjudged> {
    let mut args = vec!["diff", "--name-status", "-z", "-M", "--no-ext-diff", base, "--"];
    args.extend_from_slice(pathspecs);
    let listed = git::output(root, &args)?;
    let fields: Vec<&[u8]> = listed.split(|&b| b == 0).collect();
    let name = |k: usize| OsString::from_vec(fields[k].to_vec());
    let mut out = Vec::new();
    let mut i = 0;
    // -z gives each entry as its status, then one path, or two for a rename; the list ends with an empty field.
    while i + 1 < fields.len() {
        let status = fields[i];
        if status.starts_with(b"R") && i + 2 < fields.len() {
            out.push(ChangedFile { before: Some(name(i + 1)), after: Some(name(i + 2)) });
            i += 3;
            continue;
        }
        let path = name(i + 1);
        out.push(match status {
            b"A" => ChangedFile { before: None, after: Some(path) },
            b"D" => ChangedFile { before: Some(path), after: None },
            _ => ChangedFile { before: Some(path.clone()), after: Some(path) },
        });
        i += 2;
    }
    Ok(out)
}

/// The lines each file gains in a `git diff -U0`, by its path in the new tree.
///
/// A file the diff touches without adding a line, such as a pure rename, one that only loses lines or an empty new
/// file, maps to an empty set; a deleted file is left out. Only `diff --git` and `@@` lines are read inside a hunk,
/// since an added line can itself start with `+++ `.
pub fn changed_lines(diff: &[u8]) -> ChangedLines {
    hunk_positions(diff, |hunk| hunk.new.collect())
}

/// For each file of a `git diff -U0`, by its path in the new tree, the new tree's lines after which a hunk that only
/// removes lines removed them, 0 for the top of the file. The files are [`changed_lines`]'; one without such a hunk
/// maps to an empty set.
pub fn removal_points(diff: &[u8]) -> ChangedLines {
    hunk_positions(diff, |hunk| if hunk.new.is_empty() { vec![hunk.new.start] } else { vec![] })
}

/// For each file of a `git diff -U0`, by its path in the new tree, the lines of its old side that the diff removes,
/// as the old side numbers them. The files are [`changed_lines`]', so a deleted file is left out; one that removes
/// no line maps to an empty set.
pub fn removed_lines(diff: &[u8]) -> ChangedLines {
    hunk_positions(diff, |hunk| hunk.old.collect())
}

/// The lines a hunk spans on each side, as its `@@` line gives them; empty on the side it adds or removes nothing.
/// git gives an empty side's start as the line before it, so a hunk that only removes lines starts its new side at
/// the line it removed them after.
struct Hunk {
    old: std::ops::Range<u32>,
    new: std::ops::Range<u32>,
}

impl Hunk {
    /// The hunk of an `@@` line, None for one git would not write.
    fn parse(line: &[u8]) -> Option<Hunk> {
        static HUNK: LazyLock<Regex> =
            LazyLock::new(|| Regex::new(r"^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@").expect("a valid regex"));
        let m = HUNK.captures(line)?;
        let number = |k: usize| std::str::from_utf8(m.get(k)?.as_bytes()).ok()?.parse::<u32>().ok();
        // A side without a count spans one line.
        let span = |start: usize, count: usize| -> Option<std::ops::Range<u32>> {
            let start = number(start)?;
            let count = if m.get(count).is_some() { number(count)? } else { 1 };
            Some(start..start.checked_add(count)?)
        };
        Some(Hunk { old: span(1, 2)?, new: span(3, 4)? })
    }
}

/// The lines that `positions` gives for each hunk of each file of a `git diff -U0`, by the file's path in the new
/// tree, the files as [`changed_lines`] says.
fn hunk_positions(diff: &[u8], positions: impl Fn(Hunk) -> Vec<u32>) -> ChangedLines {
    let mut out = ChangedLines::new();
    let mut path: Option<OsString> = None;
    let mut in_hunk = false;
    for line in diff.split(|&b| b == b'\n') {
        if let Some(names) = line.strip_prefix(b"diff --git ") {
            path = note_path(&mut out, diff_git_path(names));
            in_hunk = false;
        } else if line.starts_with(b"@@") {
            in_hunk = true;
            if let (Some(path), Some(hunk)) = (&path, Hunk::parse(line)) {
                out.entry(path.clone()).or_default().extend(positions(hunk));
            }
        } else if !in_hunk {
            path = header_path(&mut out, path, line);
        }
    }
    out
}

/// `path`, entered in `out` with no lines yet unless it is there or None.
fn note_path(out: &mut ChangedLines, path: Option<OsString>) -> Option<OsString> {
    if let Some(path) = &path {
        out.entry(path.clone()).or_default();
    }
    path
}

/// The file the diff is about after a header `line` outside a hunk, `path` the one it was about before, kept in
/// `out` as [`changed_lines`] says.
fn header_path(out: &mut ChangedLines, path: Option<OsString>, line: &[u8]) -> Option<OsString> {
    if line.starts_with(b"deleted file mode") {
        if let Some(path) = &path {
            out.remove(path);
            return None;
        }
    }
    if let Some(name) = line.strip_prefix(b"rename to ") {
        return note_path(out, Some(unquote(name)));
    }
    if let Some(name) = line.strip_prefix(b"+++ ") {
        // git ends the name with a tab when it holds a space.
        let name = unquote(name.strip_suffix(b"\t").unwrap_or(name));
        let name = name.as_bytes();
        return note_path(
            out,
            (name != b"/dev/null").then(|| OsStr::from_bytes(name.strip_prefix(b"b/").unwrap_or(name)).to_owned()),
        );
    }
    path
}

/// The path of a `diff --git` line's `a/<path> b/<path>`; None when the two differ, as for a rename, which later
/// lines name.
fn diff_git_path(names: &[u8]) -> Option<OsString> {
    static QUOTED_PAIR: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r#"(?s-u)^("(?:[^"\\]|\\.)*") ("(?:[^"\\]|\\.)*")$"#).expect("a valid regex")
    });
    let (a, b) = match QUOTED_PAIR.captures(names) {
        Some(m) => (unquote(&m[1]).into_vec(), unquote(&m[2]).into_vec()),
        None => {
            let half = names.len().saturating_sub(1) / 2;
            (names[..half].to_vec(), names.get(half + 1..).unwrap_or_default().to_vec())
        }
    };
    match (a.strip_prefix(b"a/"), b.strip_prefix(b"b/")) {
        (Some(a), Some(b)) if a == b => Some(OsString::from_vec(b.to_vec())),
        _ => None,
    }
}

/// A path as git quotes it when it holds a quote, a backslash or a control character, or, with core.quotePath on,
/// a byte above 0x7f; any other name as it is.
pub fn unquote(name: &[u8]) -> OsString {
    let Some(inner) = name.strip_prefix(b"\"").and_then(|n| n.strip_suffix(b"\"")) else {
        return OsString::from_vec(name.to_vec());
    };
    let mut out = Vec::with_capacity(inner.len());
    let mut rest = inner;
    while let Some((&first, after)) = rest.split_first() {
        rest = after;
        if first != b'\\' {
            out.push(first);
            continue;
        }
        let is_octal = |d: &&[u8]| (b'0'..=b'3').contains(&d[0]) && d[1..].iter().all(|c| (b'0'..=b'7').contains(c));
        let octal = rest.get(..3).filter(is_octal);
        if let Some(digits) = octal {
            out.push(digits.iter().fold(0u8, |n, d| n * 8 + (d - b'0')));
            rest = &rest[3..];
            continue;
        }
        let Some((&escaped, after)) = rest.split_first() else {
            // A lone backslash at the end stays as it is.
            out.push(b'\\');
            break;
        };
        rest = after;
        out.push(match escaped {
            b'a' => 0x07,
            b'b' => 0x08,
            b'f' => 0x0c,
            b'n' => b'\n',
            b'r' => b'\r',
            b't' => b'\t',
            b'v' => 0x0b,
            other => other,
        });
    }
    OsString::from_vec(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(entries: &[(&str, &[u32])]) -> ChangedLines {
        entries.iter().map(|(p, ns)| (OsString::from(p), ns.iter().copied().collect())).collect()
    }

    const DIFF: &[u8] = b"diff --git a/f b/f\n--- a/f\n+++ b/f\n@@ -2,3 +1,0 @@\n-x\n-y\n-z\n\
        @@ -9 +7,2 @@\n-q\n+r\n+s\n\
        diff --git a/gone b/gone\ndeleted file mode 100644\n--- a/gone\n+++ /dev/null\n@@ -1 +0,0 @@\n-x\n\
        diff --git a/old b/new\nsimilarity index 100%\nrename from old\nrename to new\n";

    #[test]
    fn each_reading_of_the_hunks() {
        assert_eq!(changed_lines(DIFF), lines(&[("f", &[7, 8]), ("new", &[])]));
        assert_eq!(removal_points(DIFF), lines(&[("f", &[1]), ("new", &[])]));
        assert_eq!(removed_lines(DIFF), lines(&[("f", &[2, 3, 4, 9]), ("new", &[])]));
    }

    #[test]
    fn a_quoted_pair_names_its_path() {
        assert_eq!(diff_git_path(br#""a/x\"y" "b/x\"y""#), Some(OsString::from("x\"y")));
        assert_eq!(diff_git_path(b"a/a b b/a b"), Some(OsString::from("a b")));
        assert_eq!(diff_git_path(b"a/x b/y"), None);
    }

    #[test]
    fn unquote_takes_octal_bytes_and_c_escapes() {
        assert_eq!(unquote(br#""\303\251\t\\""#).as_bytes(), "\u{e9}\t\\".as_bytes());
        assert_eq!(unquote(b"plain").as_bytes(), b"plain");
        assert_eq!(unquote(br#""\377""#).as_bytes(), b"\xff");
    }
}
