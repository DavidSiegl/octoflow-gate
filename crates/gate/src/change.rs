//! The change the gate judges, from a base commit to the working tree, as every check is given it.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::diff::{change_diff, changed_files, changed_lines, removal_points, ChangedFile, ChangedLines};
use crate::git::{judged_files, UntrackedAdded};
use crate::settings::Settings;
use crate::Unjudged;

/// The directory in the tree mutmut runs in where it keeps its state: its copy of the code with the mutants, and each
/// module's results in `<path>.meta` beside it. What a tree tracks there is no part of the change the gate reads
/// ([`Change::from_tree`]), and a change that commits files there fails the gate (judge::judge_change).
pub const MUTMUT_STATE: &str = "mutants";
/// The git pathspecs of every path outside [`MUTMUT_STATE`], and of the paths inside it (a test holds them to it).
const OUTSIDE_MUTMUT_STATE: &str = ":(top,exclude,literal)mutants";
const INSIDE_MUTMUT_STATE: &str = ":(top,literal)mutants";

/// The change the gate judges, as [`Change::from_tree`] reads it, and as each check is given it.
pub struct Change {
    /// The repository's top level; its working tree is the change's new side.
    pub root: PathBuf,
    /// The commit the change is judged against, a full sha.
    pub base: String,
    /// `git diff -U0` from base to the working tree (diff::change_diff), of the files the gate sees: those of
    /// git::judged_files, but for a directory holding a repository of its own that git does not track.
    pub diff: Vec<u8>,
    /// `changed_lines(diff)`.
    pub changed: ChangedLines,
    /// `removal_points(diff)`: for each file of changed, the lines of the working tree after which the change only
    /// removed lines.
    pub removal_points: ChangedLines,
    /// diff::changed_files from base, of the files the gate sees.
    pub files: Vec<ChangedFile>,
    /// The files the change adds or edits under [`MUTMUT_STATE`], the one part of it the fields above leave out, in
    /// path order: a file it adds, renames into it or changes there in any way but deleting it.
    pub mutmut_state: Vec<OsString>,
    /// The files the gate judges in the working tree, by their paths from root in byte order, as git::judged_files
    /// listed them before any test command ran: what a test writes in the tree is not judged, and the linters, the
    /// copies and the checks that read the tree's files take this list rather than listing the tree again.
    pub judged: Vec<OsString>,
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
    /// mutmut_state names instead. Errs when git cannot list the tree's files or diff them against base, or a copy of
    /// its index cannot be made to diff its untracked files through (git::UntrackedAdded).
    pub fn from_tree(root: &Path, base: &str, settings: Settings, python: Option<&Path>) -> Result<Change, Unjudged> {
        let judged = judged_files(root)?;
        let tree = UntrackedAdded::new(root)?;
        let diff = change_diff(&tree, base, &[OUTSIDE_MUTMUT_STATE])?;
        let mut mutmut_state: Vec<OsString> =
            changed_files(&tree, base, &[INSIDE_MUTMUT_STATE])?.into_iter().filter_map(|f| f.after).collect();
        mutmut_state.sort();
        Ok(Change {
            root: root.to_path_buf(),
            base: base.to_string(),
            changed: changed_lines(&diff),
            removal_points: removal_points(&diff),
            files: changed_files(&tree, base, &[OUTSIDE_MUTMUT_STATE])?,
            diff,
            mutmut_state,
            judged,
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
