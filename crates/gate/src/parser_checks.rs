//! The parser checks (crates/checks) on a change: its source files read by their languages' front ends, each side of
//! each file, and the checks run over them. Python (crates/lang-python) and Rust (crates/lang-rust) are the languages
//! read ([`Sources`]). What is read of Rust ([`RustChange`]) serves the checks that run a change's Rust too, as
//! passes_at_base.

use std::cell::OnceCell;
use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use checks::{complexity, integrity, names, ChangedFile, Finding, Side};
use lang_python::FIRST_LINE_BYTES;
use model::{SourceFile, Unreadable};

use crate::diff::{removed_lines, ChangedLines};
use crate::judge::Change;
use crate::settings::Settings;
use crate::{git, text, Unjudged};

/// A change's source files, read once for the checks that judge them, each language's by its front end.
///
/// A file is read on each side where it is a regular file that holds the language's source: at the base as git stores
/// it, and in the working tree. A side that is a symlink, a submodule, a directory or a special file is None: a
/// symlink's change is the name it points to, a submodule's object is a commit, and reading a FIFO or a device could
/// block or never end.
pub struct Sources<'a> {
    pub change: &'a Change,
    pub python: PythonChange<'a>,
    pub rust: RustChange<'a>,
}

impl<'a> Sources<'a> {
    /// The source files of `change`. Errs when git cannot list the base's tree or show a file of it, or a file of
    /// the working tree cannot be read.
    pub fn read(change: &'a Change) -> Result<Sources<'a>, Unjudged> {
        let stored = Stored::of(change)?;
        let gone = change.files.iter().filter(|f| f.before != f.after).filter_map(|f| f.before.clone()).collect();
        let (python, bases_not_python) = stored.files(&PYTHON)?;
        let (rust, _) = stored.files(&RUST)?;
        Ok(Sources {
            change,
            python: PythonChange { change, files: python, bases_not_python },
            rust: RustChange { change, files: rust, gone },
        })
    }
}

/// A change's Python: the files, and which of the change's paths are the code under test rather than its tests.
pub struct PythonChange<'a> {
    pub change: &'a Change,
    /// Each file of the change that holds Python on a side, a `.py` file or a script whose `#!` line names python
    /// (lang_python::is_python), in the change's order, read with crates/lang-python as [`Sources`] says. A file
    /// with a `.py` path on a side that gate.toml's `test-files.python` matches is read as test code whole on both.
    pub files: Vec<ChangedFile>,
    /// The base paths of [`files`] that held no Python by their name or first line, each read as the base of the
    /// Python file it became.
    ///
    /// [`files`]: PythonChange::files
    bases_not_python: HashSet<OsString>,
}

impl PythonChange<'_> {
    /// Whether `path`, on either side of the change, is Python code under test: a path named `*.py`, whatever is
    /// there, or one where a side of [`files`] held Python, that gate.toml places neither among the Python tests nor
    /// with what supports the tests (`test-files.support`). Unlike Rust's ([`RustChange::is_code`]), it does not ask
    /// what the change edits in the file: Python's test code is told by its file alone. gate.py's `_python_code`.
    ///
    /// [`files`]: PythonChange::files
    pub fn is_code(&self, path: &OsStr) -> bool {
        let settings = &self.change.settings;
        if python_test(settings, path) || settings.test_support.matches(path) {
            return false;
        }
        let at = |side: &Option<Side>| side.as_ref().is_some_and(|s| s.path == path);
        let held_python = |f: &ChangedFile| at(&f.after) || (at(&f.before) && !self.bases_not_python.contains(path));
        path.as_bytes().ends_with(b".py") || self.files.iter().any(held_python)
    }

    /// Whether the change edits Python code under test: whether a path on a side of one of its files [`is_code`].
    ///
    /// [`is_code`]: PythonChange::is_code
    pub fn edits_code(&self) -> bool {
        any_path(self.change, |path| self.is_code(path))
    }
}

/// A change's Rust: the files, and which of the change's paths are the code under test rather than its tests.
pub struct RustChange<'a> {
    pub change: &'a Change,
    /// Each file of the change with a .rs path on a side, in the change's order, read with crates/lang-rust as
    /// [`Sources`] says. A file gate.toml's `test-files.rust` matches is read as test code whole.
    pub files: Vec<ChangedFile>,
    /// The base paths the change deletes or renames, where a Cargo.toml may have stood.
    gone: Vec<OsString>,
}

impl RustChange<'_> {
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
        any_path(self.change, |path| self.is_code(path))
    }
}

/// Whether `holds` is true of a path on a side of one of `change`'s files.
fn any_path(change: &Change, holds: impl Fn(&OsStr) -> bool) -> bool {
    change.files.iter().flat_map(|f| [&f.before, &f.after]).flatten().any(|path| holds(path))
}

/// Whether `settings` place the file at `path` among the Python tests: a `.py` file `test-files.python` matches.
/// gate.py's `GateSettings.python_test`.
fn python_test(settings: &Settings, path: &OsStr) -> bool {
    path.as_bytes().ends_with(b".py") && settings.python_tests.matches(path)
}

/// The test-integrity check's findings on `sources`: the golden files the change edits ([`changed_golden_files`]),
/// when it edits a language's code under test ([`PythonChange::edits_code`], [`RustChange::edits_code`]), by path,
/// then the findings on Python, then those on Rust, each by path and line, as gate.py's `weakened_tests` lists golden
/// files before each language's findings. Each finding's severity says whether it fails the gate
/// ([`Finding::severity`]).
pub fn weakened_tests(sources: &Sources) -> Vec<Finding> {
    let (python_edited, rust_edited) = (sources.python.edits_code(), sources.rust.edits_code());
    let mut out = if python_edited || rust_edited { changed_golden_files(sources.change) } else { vec![] };
    out.extend(integrity::weakened_tests(&sources.python.files, python_edited));
    out.extend(integrity::weakened_tests(&sources.rust.files, rust_edited));
    out
}

/// The cognitive-complexity check's findings on `sources`, but on the files gate.toml's `complexity-exempt` names,
/// by path and line. A function takes its base score only from a function of its own language.
pub fn too_complex(sources: &Sources) -> Vec<Finding> {
    let exempt = |path: &OsStr| sources.change.settings.complexity_exempt.matches(path);
    let mut out = complexity::too_complex(&sources.python.files, &exempt);
    out.extend(complexity::too_complex(&sources.rust.files, &exempt));
    by_path_and_line(out)
}

/// The vague-names check's findings on `sources`, by path and line.
pub fn vague_names(sources: &Sources) -> Vec<Finding> {
    let mut out = names::vague_names(&sources.python.files, false);
    out.extend(names::vague_names(&sources.rust.files, true));
    by_path_and_line(out)
}

/// `findings` by path and line, a lineless one first, those of one place in the order given.
fn by_path_and_line(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by(|a, b| (&a.path, a.line.unwrap_or(0)).cmp(&(&b.path, b.line.unwrap_or(0))));
    findings
}

/// How one language's source files are told among a change's and read.
struct Language {
    /// Whether the file at `path`, from the tree's top level, holds the language's source. `head` gives the file's
    /// first [`FIRST_LINE_BYTES`] bytes, none for a file that cannot be read, and is called only when the name does
    /// not decide.
    holds: fn(path: &[u8], head: &dyn Fn() -> Vec<u8>) -> bool,
    /// Whether `settings` place among the language's tests the side at `path` of one of the language's files, whose
    /// other side, where the change gives it one, is at `other`.
    among_tests: fn(settings: &Settings, path: &OsStr, other: Option<&OsStr>) -> bool,
    /// Whether a file that holds the language's source in the working tree has its base read as the language's too,
    /// whatever the base's name and first line say: gate.py compares a Python file with its base under any name.
    base_of_any_name: bool,
    /// The front end's reading of `source`, the file at `path`.
    read: fn(source: &[u8], path: &OsStr, among_tests: bool) -> Result<SourceFile, Unreadable>,
}

const PYTHON: Language = Language {
    holds: |path, head| lang_python::is_python(path, head),
    // A file is a test file by either of its paths, so that a rename into or out of the tests loses none of them
    // unseen: gate.py's `weakened_tests`.
    among_tests: |settings, path, other| python_test(settings, path) || other.is_some_and(|o| python_test(settings, o)),
    base_of_any_name: true,
    read: |source, path, among_tests| lang_python::read(source, Path::new(path), among_tests),
};

const RUST: Language = Language {
    holds: |path, _| path.ends_with(b".rs"),
    among_tests: |settings, path, _| settings.rust_tests.matches(path),
    base_of_any_name: false,
    read: |source, _, among_tests| lang_rust::read(source, among_tests),
};

/// What git and the working tree hold of a change's files, for each [`Language`] to read its own from.
struct Stored<'a> {
    change: &'a Change,
    /// The base paths of the change's files that are regular files there.
    regular_at_base: HashSet<OsString>,
    /// The lines of its base the change removed from each file, by the file's path in the working tree.
    removed: ChangedLines,
}

impl<'a> Stored<'a> {
    /// Errs when git cannot list the base's tree.
    fn of(change: &'a Change) -> Result<Stored<'a>, Unjudged> {
        let base_paths: Vec<OsString> = change.files.iter().filter_map(|f| f.before.clone()).collect();
        Ok(Stored {
            change,
            regular_at_base: regular_files_at_base(change, &base_paths)?,
            removed: removed_lines(&change.diff),
        })
    }

    /// Each file of the change that holds `language`'s source on a side, in the change's order, each such side read;
    /// and the base paths among them that held none of it by their name or first line, read all the same
    /// ([`Language::base_of_any_name`]). Errs when git cannot show such a file at the base, or one cannot be read in
    /// the working tree.
    fn files(&self, language: &Language) -> Result<(Vec<ChangedFile>, HashSet<OsString>), Unjudged> {
        let change = self.change;
        let side = |path: &OsString, other: &Option<OsString>, source: &[u8]| {
            let among_tests = (language.among_tests)(&change.settings, path, other.as_deref());
            Side { source: (language.read)(source, path, among_tests), path: path.clone(), among_tests }
        };
        let (mut out, mut bases_holding_none) = (vec![], HashSet::new());
        for file in &change.files {
            let after = match file.after.as_ref().filter(|path| regular_file(&change.root.join(path))) {
                Some(path) => self.in_working_tree(path, language)?.map(|bytes| side(path, &file.before, &bytes)),
                None => None,
            };
            let whatever_it_holds = language.base_of_any_name && after.is_some();
            let before = match file.before.as_ref().filter(|path| self.regular_at_base.contains(*path)) {
                Some(path) => self.at_base(path, language, whatever_it_holds)?.map(|(blob, holds)| {
                    if !holds {
                        bases_holding_none.insert(path.clone());
                    }
                    side(path, &file.after, &blob)
                }),
                None => None,
            };
            if before.is_none() && after.is_none() {
                continue;
            }
            let lines =
                |from: &ChangedLines| file.after.as_ref().and_then(|p| from.get(p)).cloned().unwrap_or_default();
            out.push(ChangedFile { added: lines(&change.changed), removed: lines(&self.removed), before, after });
        }
        Ok((out, bases_holding_none))
    }

    /// The bytes of the base's regular file at `path`, when it holds `language`'s source or `whatever_it_holds`,
    /// with whether it holds that source. A file git cannot show is not the language's when its name would have to
    /// decide and does not, and an error otherwise.
    fn at_base(
        &self,
        path: &OsStr,
        language: &Language,
        whatever_it_holds: bool,
    ) -> Result<Option<(Vec<u8>, bool)>, Unjudged> {
        let blob = OnceCell::new();
        let show = || {
            blob.get_or_init(|| {
                let mut spec = OsString::from(format!("{}:", self.change.base));
                spec.push(path);
                git::output(&self.change.root, &[OsStr::new("cat-file"), OsStr::new("blob"), &spec])
            })
        };
        let head = || show().as_ref().map(|blob| blob[..blob.len().min(FIRST_LINE_BYTES)].to_vec()).unwrap_or_default();
        let holds = (language.holds)(path.as_bytes(), &head);
        if !whatever_it_holds && !holds {
            return Ok(None);
        }
        show();
        Ok(blob.into_inner().transpose()?.map(|bytes| (bytes, holds)))
    }

    /// The bytes of the working tree's regular file at `path`, when it holds `language`'s source.
    fn in_working_tree(&self, path: &OsStr, language: &Language) -> Result<Option<Vec<u8>>, Unjudged> {
        let full = self.change.root.join(path);
        let head = || {
            let mut head = vec![];
            let first = |file: std::fs::File| file.take(FIRST_LINE_BYTES as u64).read_to_end(&mut head);
            let read = std::fs::File::open(&full).and_then(first);
            if read.is_ok() { head } else { vec![] }
        };
        if !(language.holds)(path.as_bytes(), &head) {
            return Ok(None);
        }
        let read = std::fs::read(&full);
        read.map(Some).map_err(|e| Unjudged(format!("{} could not be read: {e}", text::path(full.as_os_str()))))
    }
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
            python: PathBuf::from("python3"),
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
