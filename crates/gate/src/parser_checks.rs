//! The parser checks (crates/checks) on a change: its source files read by their languages' front ends, each side of
//! each file, and the checks run over them. Python (crates/lang-python), Rust (crates/lang-rust) and Go
//! (crates/lang-go) are the languages read ([`Sources`]). What is read of Rust ([`RustChange`]) serves the checks that
//! run a change's Rust too, as passes_at_base.

use std::cell::OnceCell;
use std::collections::{BTreeSet, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use checks::{complexity, integrity, names, ChangedFile, Finding, Side};
use lang_python::{Unit, FIRST_LINE_BYTES};
use model::{SourceFile, Unreadable};

use crate::diff::{removal_points, removed_lines, ChangedLines};
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
    pub go: GoChange<'a>,
}

impl<'a> Sources<'a> {
    /// The source files of `change`. Errs when git cannot list the base's tree or show a file of it, or a file of
    /// the working tree cannot be read.
    pub fn read(change: &'a Change) -> Result<Sources<'a>, Unjudged> {
        let stored = Stored::of(change)?;
        let (python, bases_not_python) = stored.files(&PYTHON)?;
        let (rust, _) = stored.files(&RUST)?;
        let (go, _) = stored.files(&GO)?;
        Ok(Sources {
            change,
            python: PythonChange { change, files: python, bases_not_python },
            rust: RustChange { change, files: rust, gone: stored.gone.clone() },
            go: GoChange { change, files: go, gone: stored.gone },
        })
    }
}

/// (path, why) for each file a check could not read or parse, the path from the tree's top level and why a clause a
/// finding can open with, as model::Unreadable's.
pub type UnreadFiles = Vec<(OsString, String)>;

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
    /// what the change edits in the file: Python's test code is told by its file alone.
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

    /// The units (lang_python::Unit) the change touched, each with its file's path, in the Python files it leaves
    /// as regular files in the working tree, adds lines to or removes lines from, and whose paths `in_scope` accepts;
    /// and each such file that cannot be read or does not parse.
    /// `touched(unit, added, removal_points)` says whether the change touched a unit of a file, given the lines the
    /// change added to the file and the points it only removed lines at (diff::removal_points).
    pub fn changed_units(
        &self,
        in_scope: impl Fn(&OsStr) -> bool,
        touched: impl Fn(&Unit, &BTreeSet<u32>, &BTreeSet<u32>) -> bool,
    ) -> (Vec<(OsString, Unit)>, UnreadFiles) {
        let removals = removal_points(&self.change.diff);
        let (mut units, mut unreadable) = (vec![], vec![]);
        for file in &self.files {
            let Some(path) = file.after.as_ref().map(|after| &after.path) else { continue };
            let removed = removals.get(path).cloned().unwrap_or_default();
            if (file.added.is_empty() && removed.is_empty()) || !in_scope(path) {
                continue;
            }
            let read = std::fs::read(self.change.root.join(path))
                .map_err(|e| format!("could not be read ({e})"))
                .and_then(|source| lang_python::units(&source).map_err(|unreadable| unreadable.why));
            match read {
                Ok(read) => {
                    let touched = read.into_iter().filter(|unit| touched(unit, &file.added, &removed));
                    units.extend(touched.map(|unit| (path.clone(), unit)));
                }
                Err(why) => unreadable.push((path.clone(), why)),
            }
        }
        (units, unreadable)
    }
}

/// A change's Rust: the files, and which of the change's paths are the code under test rather than its tests.
pub struct RustChange<'a> {
    pub change: &'a Change,
    /// Each file of the change with a .rs path on a side, in the change's order, read with crates/lang-rust as
    /// [`Sources`] says. A file gate.toml's `test-files.rust` matches is read as test code whole.
    pub files: Vec<ChangedFile>,
    /// The base paths the change deletes or renames, where a Cargo.toml may have stood.
    gone: HashSet<PathBuf>,
}

impl RustChange<'_> {
    /// Whether `path`, on either side of the change, is Rust code under test: a .rs file of a Cargo package
    /// ([`in_project`]) that gate.toml's `test-files.rust` does not place among the tests, which the change
    /// adds, deletes, renames or edits outside its test code ([`integrity::edits_code`]). A path no file was read at,
    /// as a symlink's, counts as edited.
    pub fn is_code(&self, path: &OsStr) -> bool {
        let change = self.change;
        if !path.as_bytes().ends_with(b".rs")
            || change.settings.rust_tests.matches(path)
            || !in_project(change, &self.gone, path, CARGO_MANIFEST)
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

/// A change's Go: the files, and which of the change's paths are the code under test rather than its tests.
pub struct GoChange<'a> {
    pub change: &'a Change,
    /// Each file of the change that is Go source of a Go module on a side ([`GO`]), in the change's order, read with
    /// crates/lang-go as [`Sources`] says. A `_test.go` file is read as test code whole.
    pub files: Vec<ChangedFile>,
    /// The base paths the change deletes or renames, where a go.mod may have stood.
    gone: HashSet<PathBuf>,
}

impl GoChange<'_> {
    /// Whether `path`, on either side of the change, is Go code under test: a `.go` file of a Go module
    /// ([`in_project`]) that is not a `_test.go` file and lies in no `testdata` directory, which holds a test's inputs.
    ///
    /// It counts more paths than [`files`] holds, as a file the gate does not judge may still be code the tests run:
    /// vendored and generated files are built into the module's packages, and a symlink's target is; a file under a
    /// directory starting with `.` or `_` is counted too, as erring wide only arms expected-value-changed, a finding
    /// to weigh. A path no file was read at counts, as Rust's does. Go has no test code inside a file of code, so,
    /// unlike Rust's ([`RustChange::is_code`]), it does not ask what the change edits in the file.
    ///
    /// [`files`]: GoChange::files
    pub fn is_code(&self, path: &OsStr) -> bool {
        let bytes = path.as_bytes();
        bytes.ends_with(b".go")
            && !lang_go::among_tests(bytes)
            && !bytes.split(|&b| b == b'/').any(|part| part == b"testdata")
            && in_project(self.change, &self.gone, path, GO_MANIFEST)
    }

    /// Whether the change edits Go code under test: whether a path on a side of one of its files [`is_code`].
    ///
    /// [`is_code`]: GoChange::is_code
    pub fn edits_code(&self) -> bool {
        any_path(self.change, |path| self.is_code(path))
    }
}

/// Whether `holds` is true of a path on a side of one of `change`'s files.
fn any_path(change: &Change, holds: impl Fn(&OsStr) -> bool) -> bool {
    change.files.iter().flat_map(|f| [&f.before, &f.after]).flatten().any(|path| holds(path))
}

/// Whether `settings` place the file at `path` among the Python tests: a `.py` file `test-files.python` matches.
pub(crate) fn python_test(settings: &Settings, path: &OsStr) -> bool {
    path.as_bytes().ends_with(b".py") && settings.python_tests.matches(path)
}

/// The test-integrity check's findings on `sources`: the golden files the change edits ([`changed_golden_files`]),
/// when it edits a language's code under test ([`PythonChange::edits_code`], [`RustChange::edits_code`],
/// [`GoChange::edits_code`]), by path, then the findings on Python, then those on Rust, then those on Go, each by path
/// and line. Each finding's severity says whether it fails the gate ([`Finding::severity`]).
pub fn weakened_tests(sources: &Sources) -> Vec<Finding> {
    let python_edited = sources.python.edits_code();
    let rust_edited = sources.rust.edits_code();
    let go_edited = sources.go.edits_code();
    let mut out = if python_edited || rust_edited || go_edited { changed_golden_files(sources.change) } else { vec![] };
    out.extend(integrity::weakened_tests(&sources.python.files, python_edited));
    out.extend(integrity::weakened_tests(&sources.rust.files, rust_edited));
    out.extend(integrity::weakened_tests(&sources.go.files, go_edited));
    out
}

/// The cognitive-complexity check's findings on `sources`, but on the files gate.toml's `complexity-exempt` names,
/// by path and line. A function takes its base score only from a function of its own language.
pub fn too_complex(sources: &Sources) -> Vec<Finding> {
    let exempt = |path: &OsStr| sources.change.settings.complexity_exempt.matches(path);
    let mut out = complexity::too_complex(&sources.python.files, &exempt);
    out.extend(complexity::too_complex(&sources.rust.files, &exempt));
    out.extend(complexity::too_complex(&sources.go.files, &exempt));
    by_path_and_line(out)
}

/// The vague-names check's findings on `sources`, by path and line.
pub fn vague_names(sources: &Sources) -> Vec<Finding> {
    let mut out = names::vague_names(&sources.python.files, false);
    out.extend(names::vague_names(&sources.rust.files, true));
    // Go names its types and its variables in MixedCaps, as `runInfo` and `RunInfo`.
    out.extend(names::vague_names(&sources.go.files, true));
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
    /// Whether the side at `path` of one of the language's files, whose other side, where the change gives it one, is
    /// at `other`, is among the language's tests: by `settings`, or, for a language whose tool fixes its test files, as
    /// Go's fixes `_test.go`, by that tool's rule.
    among_tests: fn(settings: &Settings, path: &OsStr, other: Option<&OsStr>) -> bool,
    /// The name of the file that makes its directory and those below it a project of the language, as Go's `go.mod`:
    /// a file of the language outside every such directory ([`in_project`]) is not read as the language's. None for a
    /// language whose files are read wherever they are.
    project: Option<&'static str>,
    /// Whether a file that holds the language's source, `source` being all its bytes, is generated, as its generator
    /// marks it, and so not read as the language's: a generator's output is not code written to be judged.
    generated: fn(source: &[u8]) -> bool,
    /// Whether a file that holds the language's source in the working tree has its base read as the language's too,
    /// whatever the base's name and first line say, so that a file is compared with its base under any name.
    base_of_any_name: bool,
    /// The front end's reading of `source`, the file at `path`.
    read: fn(source: &[u8], path: &OsStr, among_tests: bool) -> Result<SourceFile, Unreadable>,
}

const PYTHON: Language = Language {
    holds: |path, head| lang_python::is_python(path, head),
    // A file is a test file by either of its paths, so that a rename into or out of the tests loses none of them
    // unseen.
    among_tests: |settings, path, other| python_test(settings, path) || other.is_some_and(|o| python_test(settings, o)),
    base_of_any_name: true,
    project: None,
    generated: |_| false,
    read: |source, path, among_tests| lang_python::read(source, Path::new(path), among_tests),
};

const RUST: Language = Language {
    holds: |path, _| path.ends_with(b".rs"),
    among_tests: |settings, path, _| settings.rust_tests.matches(path),
    base_of_any_name: false,
    // Rust outside a Cargo package is still read, though it is no code under test ([`RustChange::is_code`]).
    project: None,
    generated: |_| false,
    read: |source, _, among_tests| lang_rust::read(source, among_tests),
};

const GO: Language = Language {
    holds: |path, _| lang_go::is_go(path),
    among_tests: |_, path, _| lang_go::among_tests(path.as_bytes()),
    base_of_any_name: false,
    project: Some(GO_MANIFEST),
    generated: lang_go::generated,
    read: |source, _, among_tests| lang_go::read(source, among_tests),
};

/// The files whose directory, and those below it, are a Rust package and a Go module.
const CARGO_MANIFEST: &str = "Cargo.toml";
pub(crate) const GO_MANIFEST: &str = "go.mod";

/// What git and the working tree hold of a change's files, for each [`Language`] to read its own from.
struct Stored<'a> {
    change: &'a Change,
    /// The base paths of the change's files that are regular files there.
    regular_at_base: HashSet<OsString>,
    /// The lines of its base the change removed from each file, by the file's path in the working tree.
    removed: ChangedLines,
    /// The base paths the change deletes or renames, where a project's file ([`Language::project`]) may have stood.
    gone: HashSet<PathBuf>,
}

impl<'a> Stored<'a> {
    /// Errs when git cannot list the base's tree.
    fn of(change: &'a Change) -> Result<Stored<'a>, Unjudged> {
        let base_paths: Vec<OsString> = change.files.iter().filter_map(|f| f.before.clone()).collect();
        let gone = change.files.iter().filter(|f| f.before != f.after).filter_map(|f| f.before.as_deref());
        Ok(Stored {
            change,
            regular_at_base: regular_files_at_base(change, &base_paths)?,
            removed: removed_lines(&change.diff),
            gone: gone.map(PathBuf::from).collect(),
        })
    }

    /// Each file of the change that holds `language`'s source on a side, inside one of its projects where it has them
    /// ([`Language::project`]) and not generated ([`Language::generated`]), in the change's order, each such side read;
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

    /// The bytes of the base's regular file at `path`, when it holds `language`'s source or `whatever_it_holds`, lies
    /// in one of the language's projects ([`Stored::in_its_project`]) and is not generated ([`Language::generated`]),
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
        if (!whatever_it_holds && !holds) || !self.in_its_project(path, language) {
            return Ok(None);
        }
        show();
        let bytes = blob.into_inner().transpose()?;
        Ok(bytes.filter(|bytes| !(language.generated)(bytes)).map(|bytes| (bytes, holds)))
    }

    /// The bytes of the working tree's regular file at `path`, when it holds `language`'s source, lies in one of the
    /// language's projects ([`Stored::in_its_project`]) and is not generated ([`Language::generated`]).
    fn in_working_tree(&self, path: &OsStr, language: &Language) -> Result<Option<Vec<u8>>, Unjudged> {
        let full = self.change.root.join(path);
        let head = || {
            let mut head = vec![];
            let first = |file: std::fs::File| file.take(FIRST_LINE_BYTES as u64).read_to_end(&mut head);
            let read = std::fs::File::open(&full).and_then(first);
            if read.is_ok() { head } else { vec![] }
        };
        if !(language.holds)(path.as_bytes(), &head) || !self.in_its_project(path, language) {
            return Ok(None);
        }
        let read = std::fs::read(&full);
        let bytes = read.map_err(|e| Unjudged(format!("{} could not be read: {e}", text::path(full.as_os_str()))))?;
        Ok(Some(bytes).filter(|bytes| !(language.generated)(bytes)))
    }

    /// Whether `path`, the path of a side that holds `language`'s source, lies in one of the language's projects
    /// ([`in_project`]), as it must to be read as the language's where the language has them ([`Language::project`]).
    /// It is asked after [`Language::holds`], which most paths fail by their name alone, as it walks up the path's
    /// directories with a `stat` at each.
    fn in_its_project(&self, path: &OsStr, language: &Language) -> bool {
        language.project.is_none_or(|manifest| in_project(self.change, &self.gone, path, manifest))
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

/// Whether a file named `manifest`, as a Cargo.toml or a go.mod, lies in the directory of `path`, from the tree's top,
/// or one above it up to the top: in the working tree, or at the base where the change deletes or renames it, as
/// `gone`, the base paths the change deletes or renames, says. One test serves both sides of a file, so a manifest
/// the change adds also places the file's base in a project.
fn in_project(change: &Change, gone: &HashSet<PathBuf>, path: &OsStr, manifest: &str) -> bool {
    project_dir(path, manifest, |m| gone.contains(m) || change.root.join(m).is_file()).is_some()
}

/// The directory, from the tree's top, empty for the top, of the project `path`, a path from the tree's top, lies in:
/// the nearest one above it, its own included, that holds a file named `manifest`, as a go.mod, by `holds_manifest`,
/// asked of that file's path from the top. None when no directory up to the top holds one.
pub(crate) fn project_dir<'p>(
    path: &'p OsStr,
    manifest: &str,
    holds_manifest: impl Fn(&Path) -> bool,
) -> Option<&'p Path> {
    Path::new(path).ancestors().skip(1).find(|dir| holds_manifest(&dir.join(manifest)))
}

/// A test-integrity expected-value-changed finding, on the whole file, for each golden file (Settings::golden) that
/// `change` edits or deletes, by path, which the gate weighs as it does a changed literal in an assertion.
///
/// [`weakened_tests`] lists them only when the change also edits code outside its tests, which only a language can
/// tell: a golden file is then an expected value outside any assertion, which `UPDATE_GOLDEN=1` rewrites to whatever
/// the code now writes. A golden file the change adds, or renames without editing, has no old value to lose.
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
