//! Property tests of the gate's parsers of what git wrote, its diffs and its quoted paths: tests/test_properties.py's,
//! ported. proptest generates the inputs, PROPTEST_CASES of them, else 25; git is the oracle, run in a temporary
//! directory.

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicU32, Ordering};

use gate::diff::{changed_lines, unquote};
use proptest::prelude::*;

/// A tree: file contents by path, both as text.
type Tree = BTreeMap<String, String>;

/// A new, empty git repository, removed when dropped; git in it reads neither the user's nor the system's
/// configuration, either of which could change what a diff looks like.
struct Repo(PathBuf);

impl Repo {
    fn new() -> Repo {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir()
            .join(format!("gate-diff-properties-{}-{}", std::process::id(), N.fetch_add(1, Ordering::SeqCst)));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let repo = Repo(dir);
        repo.git(&["init", "-q"]);
        repo
    }

    /// git's standard output; panics when git fails.
    fn git(&self, args: &[&str]) -> Vec<u8> {
        let done = Command::new("git")
            .args(args)
            .current_dir(&self.0)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_AUTHOR_NAME", "t")
            .env("GIT_AUTHOR_EMAIL", "t@example.com")
            .env("GIT_COMMITTER_NAME", "t")
            .env("GIT_COMMITTER_EMAIL", "t@example.com")
            .output()
            .unwrap();
        assert!(done.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&done.stderr));
        done.stdout
    }

    /// Commits `tree` as the repository's whole content, replacing what it held; the commit's hash.
    fn commit(&self, tree: &Tree) -> String {
        for entry in std::fs::read_dir(&self.0).unwrap() {
            let entry = entry.unwrap().path();
            if entry.file_name() != Some(".git".as_ref()) {
                if entry.is_dir() {
                    std::fs::remove_dir_all(&entry).unwrap();
                } else {
                    std::fs::remove_file(&entry).unwrap();
                }
            }
        }
        for (path, content) in tree {
            let full = self.0.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        }
        self.git(&["add", "-A"]);
        self.git(&["commit", "-q", "--allow-empty", "-m", "tree"]);
        String::from_utf8(self.git(&["rev-parse", "HEAD"])).unwrap().trim().to_string()
    }

    /// The number of lines `git diff --numstat` says each file of `new` gains, by its path in `new`.
    fn added_counts(&self, base: &str, new: &str) -> BTreeMap<OsString, usize> {
        let listed = self.git(&["-c", "core.quotePath=false", "diff", "--numstat", "-z", "-M", base, new]);
        let fields: Vec<&[u8]> = listed.split(|&b| b == 0).collect();
        let mut counts = BTreeMap::new();
        let mut i = 0;
        // Each entry is "added<TAB>deleted<TAB>path"; a rename's path is empty, its old and new paths the next two.
        while i + 1 < fields.len() {
            let mut parts = fields[i].splitn(3, |&b| b == b'\t');
            let added: usize = std::str::from_utf8(parts.next().unwrap()).unwrap().parse().unwrap();
            let path = parts.nth(1).unwrap();
            let path = if path.is_empty() {
                i += 3;
                fields[i - 1]
            } else {
                i += 1;
                path
            };
            counts.insert(OsString::from_vec(path.to_vec()), added);
        }
        counts
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// File names that git quotes, or that split wrongly in a naive parser: spaces, quotes, backslashes, tabs, newlines,
/// control characters and non-ASCII, besides anything else. Never "/" or NUL, which no file name holds, nor .git*,
/// which changes how git treats the tree.
fn file_names() -> impl Strategy<Value = String> {
    let special = prop::sample::select(vec![' ', '"', '\\', '\t', '\n', '\u{e9}', '\u{1}', '-']);
    let any = any::<char>().prop_filter("no / or NUL", |&c| c != '/' && c != '\0');
    prop::collection::vec(prop_oneof![special, any], 1..=12).prop_map(String::from_iter).prop_filter(
        "a name git and the file system take",
        |n| !matches!(n.as_str(), "." | ".." | "sub dir") && !n.starts_with(".git") && n.len() <= 255,
    )
}

fn paths() -> impl Strategy<Value = String> {
    (prop::sample::select(vec!["", "sub dir/"]), file_names()).prop_map(|(dir, name)| format!("{dir}{name}"))
}

/// A line is a few parts from a small vocabulary, so trees share lines and git has real hunks to find. The
/// vocabulary has the line breaks git does not split at, and text that looks like the diff's own header lines.
fn contents() -> impl Strategy<Value = String> {
    let part = prop::sample::select(vec![
        "x",
        "y",
        " ",
        "\r",
        "\u{c}",
        "\u{85}",
        "\u{2028}",
        "@@ -1 +1,9 @@",
        "diff --git a/q b/q",
        "+++ b/q",
        "--- a/q",
        "\\ No newline at end of file",
    ]);
    let line = prop::collection::vec(part, 0..=3).prop_map(|parts| parts.concat());
    (prop::collection::vec(line, 0..=6), any::<bool>()).prop_map(|(lines, end)| {
        let ends = !lines.is_empty() && end;
        lines.join("\n") + if ends { "\n" } else { "" }
    })
}

/// What becomes of a file of the base tree.
#[derive(Clone, Debug)]
enum Fate {
    Keep,
    Edit(String),
    Delete,
    /// Renamed to the path, keeping its content, or with this one.
    Rename(String, Option<String>),
}

/// (base, new): two trees, the new one the base with files kept, edited, deleted and renamed (with or without an
/// edit), and files added.
fn changes() -> impl Strategy<Value = (Tree, Tree)> {
    let fate = prop_oneof![
        Just(Fate::Keep),
        contents().prop_map(Fate::Edit),
        Just(Fate::Delete),
        (paths(), prop::option::of(contents())).prop_map(|(to, content)| Fate::Rename(to, content)),
    ];
    (
        prop::collection::btree_map(paths(), contents(), 0..=4),
        prop::collection::vec(fate, 4),
        prop::collection::btree_map(paths(), contents(), 0..=3),
    )
        .prop_map(|(base, fates, added)| {
            let mut new = Tree::new();
            for ((path, content), fate) in base.iter().zip(fates) {
                match fate {
                    Fate::Keep => {
                        new.insert(path.clone(), content.clone());
                    }
                    Fate::Edit(edited) => {
                        new.insert(path.clone(), edited);
                    }
                    Fate::Delete => {}
                    // A rename to a path the base or the new tree already has is a deletion.
                    Fate::Rename(to, edited) => {
                        if !base.contains_key(&to) && !new.contains_key(&to) {
                            new.insert(to, edited.unwrap_or_else(|| content.clone()));
                        }
                    }
                }
            }
            for (path, content) in added {
                if !base.contains_key(&path) {
                    new.entry(path).or_insert(content);
                }
            }
            (base, new)
        })
}

/// The lines git numbers in `content`: split at "\n" alone, a final one ending the last line.
fn file_lines(content: &str) -> Vec<&str> {
    if content.is_empty() {
        return vec![];
    }
    content.strip_suffix('\n').unwrap_or(content).split('\n').collect()
}

fn is_subsequence(short: &[&str], long: &[&str]) -> bool {
    let mut rest = long.iter();
    short.iter().all(|x| rest.any(|y| y == x))
}

fn cases() -> u32 {
    std::env::var("PROPTEST_CASES").ok().and_then(|n| n.parse().ok()).unwrap_or(25)
}

/// changed_lines on what `git diff -U0`, run as the gate runs it (diff::change_diff), gets from real git for two
/// trees: it names each changed file and exactly its added lines.
fn check_changed_lines(base: &Tree, new: &Tree) -> Result<(), TestCaseError> {
    let repo = Repo::new();
    let (base_commit, new_commit) = (repo.commit(base), repo.commit(new));
    let diff = repo.git(&[
        "-c",
        "core.quotePath=false",
        "diff",
        "-U0",
        "-M",
        "--no-color",
        "--no-ext-diff",
        "--src-prefix=a/",
        "--dst-prefix=b/",
        &base_commit,
        &new_commit,
        "--",
    ]);
    let got = changed_lines(&diff);
    let counts = repo.added_counts(&base_commit, &new_commit);
    let want: BTreeSet<OsString> =
        new.iter().filter(|(path, content)| base.get(*path) != Some(content)).map(|(path, _)| path.into()).collect();
    prop_assert_eq!(got.keys().cloned().collect::<BTreeSet<_>>(), want);
    for (path, added) in &got {
        let name = path.to_str().unwrap();
        let lines = file_lines(&new[name]);
        prop_assert!(added.iter().all(|&n| (1..=lines.len()).contains(&(n as usize))), "{name:?}");
        prop_assert_eq!(added.len(), counts[path], "{:?}", name);
        // Whichever lines git chose to call added, the others must be the file's own lines at the base, in order:
        // those at its path, or, for a path new to the tree, those of a file the change deleted, which git may pair
        // with it as a rename.
        let kept: Vec<&str> =
            lines.iter().enumerate().filter(|(k, _)| !added.contains(&(*k as u32 + 1))).map(|(_, l)| *l).collect();
        let before: Vec<&String> = match base.get(name) {
            Some(content) => vec![content],
            None => base.iter().filter(|(p, _)| !new.contains_key(*p)).map(|(_, c)| c).collect(),
        };
        prop_assert!(kept.is_empty() || before.iter().any(|c| is_subsequence(&kept, &file_lines(c))), "{name:?}");
    }
    Ok(())
}

/// unquote against git's own quoting of a file name, with core.quotePath on and off.
fn check_unquote(name: &str) -> Result<(), TestCaseError> {
    let repo = Repo::new();
    std::fs::write(repo.0.join(name), "").unwrap();
    repo.git(&["add", "-A"]);
    for quote_path in ["true", "false"] {
        let listed = repo.git(&["-c", &format!("core.quotePath={quote_path}"), "ls-files"]);
        let listed = listed.strip_suffix(b"\n").unwrap_or(&listed);
        let got = unquote(listed);
        prop_assert_eq!(got.as_bytes(), name.as_bytes(), "core.quotePath={}", quote_path);
    }
    Ok(())
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(cases()))]

    #[test]
    fn changed_lines_names_each_changed_file_and_exactly_its_added_lines((base, new) in changes()) {
        check_changed_lines(&base, &new)?;
    }

    #[test]
    fn unquote_undoes_git_s_quoting(name in file_names()) {
        check_unquote(&name)?;
    }
}

#[test]
fn a_lone_carriage_return_before_a_hunk_header_is_part_of_the_line() {
    // Read as a header, it added nine lines to a one-line file.
    let base: Tree = [("00", ""), ("000", ""), ("0000", "")].map(|(p, c)| (p.to_string(), c.to_string())).into();
    let mut new = base.clone();
    new.insert("0".into(), "\r@@ -1 +1,9 @@".into());
    check_changed_lines(&base, &new).unwrap();
}

#[test]
fn a_character_above_0x7f_unquotes_from_its_bytes() {
    // Quoted as two octal escapes, one per byte of its UTF-8, which became two characters.
    check_unquote("\u{80}").unwrap();
}

