//! The parser checks (crates/checks) on a change: its source files read by their languages' front ends, each side of
//! each file, and the checks run over them. Rust is the one language read so far (crates/lang-rust); Python comes
//! with gate-standalone/15-16. What is read ([`RustChange`]) serves the checks that run a change's Rust too, as
//! passes_at_base.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use checks::{complexity, integrity, names, ChangedFile, Finding, Side};

use crate::diff::removed_lines;
use crate::judge::Change;
use crate::{git, text, Unjudged};

/// A change's Rust, read once for each check that judges it: the files, and which of the change's paths are the code
/// under test rather than its tests.
pub struct RustChange<'a> {
    pub change: &'a Change,
    /// Each file of the change with a .rs path on a side, read with crates/lang-rust, as [`RustChange::read`] says.
    pub files: Vec<ChangedFile>,
    /// The base paths the change deletes or renames, where a Cargo.toml may have stood.
    gone: Vec<OsString>,
}

impl<'a> RustChange<'a> {
    /// The Rust of `change`: each file with a .rs path on a side, in the change's order, read with crates/lang-rust
    /// at the base as git stores it and in the working tree, each where it is a regular file. A side that is not .rs,
    /// or is a symlink, a submodule, a directory or a special file, is None: a symlink's change is the name it points
    /// to, a submodule's object is a commit, and reading a FIFO or a device could block or never end. A file
    /// gate.toml's `test-files.rust` matches is read as test code whole.
    ///
    /// Errs when git cannot list the base's tree or show a file of it, or a file of the working tree cannot be read.
    pub fn read(change: &'a Change) -> Result<RustChange<'a>, Unjudged> {
        let gone = change.files.iter().filter(|f| f.before != f.after).filter_map(|f| f.before.clone()).collect();
        Ok(RustChange { change, files: rust_files(change)?, gone })
    }

    /// Whether `path`, on either side of the change, is Rust code under test: a .rs file of a Cargo package
    /// ([`in_cargo_package`]) that gate.toml's `test-files.rust` does not place among the tests, which the change
    /// adds, deletes, renames or edits outside its test code ([`integrity::edits_code`]). A path no file was read at,
    /// as a symlink's, counts as edited. gate.py's `_rust_code`.
    pub fn is_code(&self, path: &OsStr) -> bool {
        let change = self.change;
        if !path.as_bytes().ends_with(b".rs")
            || change.settings.rust_tests.matches(path)
            || !in_cargo_package(change, &self.gone, path)
        {
            return false;
        }
        let at = |side: &Option<Side>| side.as_ref().is_some_and(|s| s.path == path);
        let mut read = self.files.iter().filter(|f| at(&f.before) || at(&f.after)).peekable();
        read.peek().is_none() || read.any(integrity::edits_code)
    }

    /// Whether the change edits Rust code under test: whether a path on a side of one of its files [`is_code`].
    ///
    /// [`is_code`]: RustChange::is_code
    pub fn edits_code(&self) -> bool {
        self.change.files.iter().flat_map(|f| [&f.before, &f.after]).flatten().any(|p| self.is_code(p))
    }
}

/// The test-integrity check's findings on `rust`: the golden files the change edits ([`changed_golden_files`]),
/// when it edits the code under test ([`RustChange::edits_code`]), by path, then the findings on Rust, by path and
/// line, as gate.py's `weakened_tests` lists golden files before each language's findings. Each finding's severity
/// says whether it fails the gate ([`Finding::severity`]).
pub fn weakened_tests(rust: &RustChange) -> Vec<Finding> {
    let code_changed = rust.edits_code();
    let mut out = if code_changed { changed_golden_files(rust.change) } else { vec![] };
    out.extend(integrity::weakened_tests(&rust.files, code_changed));
    out
}

/// The cognitive-complexity check's findings on `rust`, but on the files gate.toml's `complexity-exempt` names.
pub fn too_complex(rust: &RustChange) -> Vec<Finding> {
    complexity::too_complex(&rust.files, &|path: &OsStr| rust.change.settings.complexity_exempt.matches(path))
}

/// The vague-names check's findings on `rust`.
pub fn vague_names(rust: &RustChange) -> Vec<Finding> {
    names::vague_names(&rust.files, true)
}

/// [`RustChange::read`]'s files.
fn rust_files(change: &Change) -> Result<Vec<ChangedFile>, Unjudged> {
    let rs = |path: &Option<OsString>| path.clone().filter(|p| p.as_bytes().ends_with(b".rs"));
    let removed = removed_lines(&change.diff);
    let base_rs: Vec<OsString> = change.files.iter().filter_map(|f| rs(&f.before)).collect();
    let regular_at_base = regular_files_at_base(change, &base_rs)?;
    let mut out = vec![];
    for file in &change.files {
        let before = rs(&file.before).filter(|p| regular_at_base.contains(p));
        let after = rs(&file.after);
        if before.is_none() && after.is_none() {
            continue;
        }
        let read = |path: OsString, bytes: &[u8]| {
            let among_tests = change.settings.rust_tests.matches(&path);
            Side { source: lang_rust::read(bytes, among_tests), path, among_tests }
        };
        let before = match before {
            Some(path) => {
                let mut spec = OsString::from(format!("{}:", change.base));
                spec.push(&path);
                let blob = git::output(&change.root, &[OsStr::new("cat-file"), OsStr::new("blob"), &spec])?;
                Some(read(path, &blob))
            }
            None => None,
        };
        let after = match after.filter(|p| regular_file(&change.root.join(p))) {
            Some(path) => {
                let bytes = std::fs::read(change.root.join(&path)).map_err(|e| {
                    Unjudged(format!("{} could not be read: {e}", text::path(change.root.join(&path).as_os_str())))
                })?;
                Some(read(path, &bytes))
            }
            None => None,
        };
        let lines = |of: &Option<OsString>, from: &crate::diff::ChangedLines| {
            of.as_ref().and_then(|p| from.get(p)).cloned().unwrap_or_default()
        };
        out.push(ChangedFile {
            added: lines(&file.after, &change.changed),
            removed: lines(&file.after, &removed),
            before,
            after,
        });
    }
    Ok(out)
}

/// Those of `paths`, each a path of the tree at `change`'s base, that are regular files there, as git's tree records
/// them: not a symlink or a submodule. Errs when git cannot list the base's tree.
fn regular_files_at_base(change: &Change, paths: &[OsString]) -> Result<HashSet<OsString>, Unjudged> {
    if paths.is_empty() {
        return Ok(HashSet::new());
    }
    // The whole tree, listed once: naming the paths instead would put as many on one command line as the change
    // touches, more than the system allows (E2BIG) for a change to a large vendored tree.
    let listed = git::output(&change.root, &["ls-tree", "-r", "-z", "--full-tree", &change.base])?;
    let wanted: HashSet<&[u8]> = paths.iter().map(|p| p.as_bytes()).collect();
    // Each entry is `<mode> <type> <object>\t<path>`, NUL-ended; a regular file's mode is 100644 or 100755.
    let regular = listed.split(|&b| b == 0).filter(|entry| entry.starts_with(b"100"));
    let paths = regular.filter_map(|entry| entry.iter().position(|&b| b == b'\t').map(|k| &entry[k + 1..]));
    Ok(paths.filter(|p| wanted.contains(p)).map(|p| OsStr::from_bytes(p).to_os_string()).collect())
}

/// Whether `path` is a regular file, not a symlink to one.
fn regular_file(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// Whether a Cargo.toml lies in the directory of `path`, from the tree's top, or one above it up to the top: in the
/// working tree, or at the base where the change deletes or renames it, as `gone`, the base paths the change deletes
/// or renames, says.
fn in_cargo_package(change: &Change, gone: &[OsString], path: &OsStr) -> bool {
    let mut dir = Path::new(path).parent();
    while let Some(d) = dir {
        let manifest = d.join("Cargo.toml");
        if gone.iter().any(|g| Path::new(g) == manifest) || change.root.join(&manifest).is_file() {
            return true;
        }
        dir = d.parent();
    }
    false
}

/// A test-integrity expected-value-changed finding, on the whole file, for each golden file (Settings::golden) that
/// `change` edits or deletes, by path, which the gate weighs as it does a changed literal in an assertion.
///
/// [`weakened_tests`] lists them only when the change also edits code outside its tests, which only a language can
/// tell: a golden file is then an expected value outside any assertion, which `UPDATE_GOLDEN=1` rewrites to whatever
/// the code now writes. A golden file the change adds, or renames without editing, has no old value to lose. gate.py's
/// `_changed_golden_files`.
fn changed_golden_files(change: &Change) -> Vec<Finding> {
    let removals = crate::diff::removal_points(&change.diff);
    let finding = |path: &OsString, what: &str| Finding {
        path: Some(path.clone()),
        line: None,
        severity: "warning".into(),
        rule: "test-integrity expected-value-changed".into(),
        message: format!("a golden file, the expected output of a test, {what}"),
    };
    let mut out = vec![];
    for file in &change.files {
        let Some(before) = file.before.as_ref().filter(|p| change.settings.golden.matches(p)) else { continue };
        match &file.after {
            None => out.push(finding(
                before,
                "deleted in a change that also edits the code under test; check that no test still needs it.",
            )),
            Some(after)
                if change.changed.get(after).is_some_and(|l| !l.is_empty())
                    || removals.get(after).is_some_and(|l| !l.is_empty()) =>
            {
                out.push(finding(
                    after,
                    "changed in a change that also edits the code under test; check that its diff is what the \
                     change means the output to become.",
                ))
            }
            Some(_) => {}
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::settings::Settings;

    #[test]
    fn golden_files_edited_or_deleted_are_found_but_not_added_or_renamed_ones() {
        let file = |before: Option<&str>, after: Option<&str>| crate::diff::ChangedFile {
            before: before.map(OsString::from),
            after: after.map(OsString::from),
        };
        let golden = |name: &str| format!("tests/golden/{name}");
        let change = Change {
            root: PathBuf::from("/"),
            base: "b".into(),
            // Removes the one line of `cut`; `edited` gains one.
            diff: b"diff --git a/tests/golden/cut b/tests/golden/cut\n--- a/tests/golden/cut\n\
                    +++ b/tests/golden/cut\n@@ -1 +0,0 @@\n-x\n"
                .to_vec(),
            changed: [(golden("edited").into(), [1].into()), (golden("cut").into(), [].into())].into(),
            files: vec![
                file(Some(&golden("gone")), None),
                file(Some(&golden("edited")), Some(&golden("edited"))),
                file(Some(&golden("cut")), Some(&golden("cut"))),
                file(None, Some(&golden("new"))),
                file(Some(&golden("old")), Some(&golden("moved"))),
                file(Some("src/edited"), Some("src/edited")),
            ],
            mutmut_state: vec![],
            settings: Settings::default(),
        };
        let found: Vec<(String, String)> = changed_golden_files(&change)
            .into_iter()
            .map(|f| (text::path(f.path.as_deref().unwrap()), f.message.split(" in a change").next().unwrap().into()))
            .collect();
        let expected = |name: &str, what: &str| {
            (golden(name), format!("a golden file, the expected output of a test, {what}"))
        };
        assert_eq!(found, vec![expected("cut", "changed"), expected("edited", "changed"), expected("gone", "deleted")]);
    }
}
