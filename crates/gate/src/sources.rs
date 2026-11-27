//! The change's source files, each language's part of it read once by its front end (crates/lang-python,
//! crates/lang-rust, crates/lang-go) for every check that judges or runs it: the parser checks (parser_checks.rs),
//! passes-at-base, surviving-mutants and flaky-tests. Each language change, [`PythonChange`], [`RustChange`] and
//! [`GoChange`], answers the same questions of its part: its files read, which paths are its code under test
//! (`is_code`), which go with its tests in a copy of the base (`beside_tests`), and, for Python and Go, whose runner
//! checks read a file's source again, the bytes it read of a side (`source_at_base`, `source_now`).

use std::collections::{BTreeSet, HashMap, HashSet};
use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use checks::{integrity, ChangedFile, Side};
use lang_python::{Unit, FIRST_LINE_BYTES};
use model::{SourceFile, Unreadable};

use crate::change::{under_mutmut_state, Change};
use crate::diff::{removed_lines, ChangedLines};
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
    /// The contents at the base of each of the change's files that is a regular file there, by its base path.
    at_base: Blobs,
    /// The base paths the change deletes or renames, where a project's file ([`Language::project`]) may have stood.
    gone: HashSet<PathBuf>,
}

/// Files' contents by their paths, shared between [`Sources`] and the language changes that read them.
type Blobs = HashMap<OsString, Rc<[u8]>>;

impl<'a> Sources<'a> {
    /// The source files of `change`. Errs when git cannot list the base's tree or show a file of it, or a file of
    /// the working tree cannot be read.
    pub fn read(change: &'a Change) -> Result<Sources<'a>, Unjudged> {
        let stored = Stored::of(change)?;
        let settings = &change.settings;
        // A file is a Python test file by either of its paths, so that a rename into or out of the tests loses none
        // of them unseen.
        let python_tests = |path: &OsStr, other: Option<&OsStr>, _| {
            python_test(settings, path) || other.is_some_and(|o| python_test(settings, o))
        };
        let python = PythonChange::of(change, stored.files(&PYTHON, &python_tests)?);
        let rust = RustChange::of(change, &stored)?;
        let go = GoChange::of(change, stored.files(&GO, &|path, _, _| lang_go::among_tests(path.as_bytes()))?, &stored);
        Ok(Sources { change, python, rust, go, at_base: stored.at_base, gone: stored.gone })
    }

    /// (path, bytes) of each of the tree's judged regular files (Change::judged) outside mutmut's state whose path
    /// ends in `suffix`, as the change's base holds them and as its working tree does. A base path is the file's
    /// there, which the change may rename. Errs when one of the working tree's files cannot be read.
    pub(crate) fn trees(&self, suffix: &[u8]) -> Result<(TreeFiles, TreeFiles), Unjudged> {
        trees(self.change, &self.at_base, suffix)
    }

    /// Whether the working tree's file at `path`, from the tree's top, holds `language`'s source as the language
    /// changes tell it: by its name or first line, in one of the language's projects where it has them.
    pub(crate) fn holds_source(&self, path: &OsStr, language: &Language) -> bool {
        holds_source(self.change, &self.gone, path, language)
    }

    /// (path, bytes) of each of the working tree's judged regular files (Change::judged) outside mutmut's state that
    /// holds `language`'s source ([`Sources::holds_source`]), is not generated ([`Language::generated`]) and `wanted`
    /// accepts, in byte order of their paths. Errs when one cannot be read.
    pub(crate) fn tree_sources(
        &self,
        language: &Language,
        wanted: impl Fn(&OsStr) -> bool,
    ) -> Result<TreeFiles, Unjudged> {
        let files = working_tree_files(self.change, |path| wanted(path) && self.holds_source(path, language))?;
        Ok(files.into_iter().filter(|(_, bytes)| !(language.generated)(bytes)).collect())
    }
}

/// Each file a check could not read or parse, by its path from the tree's top level, with what kept it unread.
pub type UnreadFiles = Vec<(OsString, Unreadable)>;

/// The bytes a language change read of its files' sides, as [`Stored::files`] read them: a key for each side of the
/// change's files that the language read, and for no other path.
#[derive(Default)]
struct SideBytes {
    /// Each base side's, by its path at the base.
    at_base: Blobs,
    /// Each working-tree side's, by its path in the working tree.
    now: HashMap<OsString, Vec<u8>>,
}

/// A change's Python: the files, and which of the change's paths are the code under test rather than its tests.
pub struct PythonChange<'a> {
    pub change: &'a Change,
    /// Each file of the change that holds Python on a side, a `.py` file or a script whose `#!` line names python
    /// (lang_python::is_python), in the change's order, read with crates/lang-python as [`Sources`] says. A file
    /// with a `.py` path on a side that gate.toml's `test-files.python` matches is read as test code whole on both.
    pub files: Vec<ChangedFile>,
    /// The paths on a side of the change that are Python code under test, as [`PythonChange::is_code`] says.
    code: HashSet<OsString>,
    /// The bytes of each side of `files`, which [`PythonChange::source_at_base`] and [`PythonChange::source_now`] give.
    bytes: SideBytes,
}

impl<'a> PythonChange<'a> {
    /// `change`'s Python, from the files `read` of it.
    fn of(change: &'a Change, read: LanguageRead) -> PythonChange<'a> {
        let settings = &change.settings;
        // The paths where a side of a file held Python: a base read only as the base of a Python file holds none.
        let bases = read.files.iter().filter_map(|f| f.before.as_ref()).map(|side| side.path.as_os_str());
        let held: HashSet<&OsStr> = (read.files.iter().filter_map(|f| f.after.as_ref()))
            .map(|side| side.path.as_os_str())
            .chain(bases.filter(|path| !read.bases_holding_none.contains(*path)))
            .collect();
        let code = paths_where(change, |path| {
            !python_test(settings, path)
                && !settings.test_support.matches(path)
                && (path.as_bytes().ends_with(b".py") || held.contains(path))
        });
        PythonChange { change, files: read.files, code, bytes: read.bytes }
    }

    /// Whether `path`, on either side of the change, is Python code under test: a path named `*.py`, whatever is
    /// there, or one where a side of [`files`] held Python, that gate.toml places neither among the Python tests nor
    /// with what supports the tests (`test-files.support`). Unlike Rust's ([`RustChange::is_code`]), it does not ask
    /// what the change edits in the file: Python's test code is told by its file alone. A path on no side of the
    /// change is not code under test.
    ///
    /// [`files`]: PythonChange::files
    pub fn is_code(&self, path: &OsStr) -> bool {
        self.code.contains(path)
    }

    /// Whether the change edits Python code under test: whether a path on a side of one of its files [`is_code`].
    ///
    /// [`is_code`]: PythonChange::is_code
    pub fn edits_code(&self) -> bool {
        !self.code.is_empty()
    }

    /// Whether `path`, one of the change's, belongs with Python's tests rather than with the code they test, as a copy
    /// of the base the tests run in takes it from the working tree: a Python test file by gate.toml's
    /// `test-files.python`, or a file that goes with the tests by its `test-files.support`, such as a conftest.py or a
    /// fixture's data.
    pub fn beside_tests(&self, path: &OsStr) -> bool {
        let settings = &self.change.settings;
        python_test(settings, path) || settings.test_support.matches(path)
    }

    /// The bytes of the base side of [`files`] at `base_path`, its path at the base; None where no file of the
    /// change was read as Python there.
    ///
    /// [`files`]: PythonChange::files
    pub fn source_at_base(&self, base_path: &OsStr) -> Option<&[u8]> {
        self.bytes.at_base.get(base_path).map(|blob| &blob[..])
    }

    /// The bytes of the working-tree side of [`files`] at `path`; None where no file of the change was read as
    /// Python there.
    ///
    /// [`files`]: PythonChange::files
    pub fn source_now(&self, path: &OsStr) -> Option<&[u8]> {
        self.bytes.now.get(path).map(Vec::as_slice)
    }

    /// The units (lang_python::Unit) the change touched, each with its file's path, in the Python files it leaves
    /// as regular files in the working tree, adds lines to or removes lines from, and whose paths `in_scope` accepts;
    /// and each such file that does not parse.
    /// `touched(unit, added, removal_points)` says whether the change touched a unit of a file, given the lines the
    /// change added to the file and the points it only removed lines at (Change::removal_points).
    pub fn changed_units(
        &self,
        in_scope: impl Fn(&OsStr) -> bool,
        touched: impl Fn(&Unit, &BTreeSet<u32>, &BTreeSet<u32>) -> bool,
    ) -> (Vec<(OsString, Unit)>, UnreadFiles) {
        let none = BTreeSet::new();
        let (mut units, mut unreadable) = (vec![], vec![]);
        for file in &self.files {
            let Some(path) = file.after.as_ref().map(|after| &after.path) else { continue };
            let removed = self.change.removal_points.get(path).unwrap_or(&none);
            if (file.added.is_empty() && removed.is_empty()) || !in_scope(path) {
                continue;
            }
            // Every working-tree side of `files` has its bytes (SideBytes); one without would go untested unseen.
            let Some(source) = self.source_now(path) else {
                unreadable.push((path.clone(), Unreadable { line: None, why: "could not be read".into() }));
                continue;
            };
            match lang_python::units(source) {
                Ok(read) => {
                    let touched = read.into_iter().filter(|unit| touched(unit, &file.added, removed));
                    units.extend(touched.map(|unit| (path.clone(), unit)));
                }
                Err(unreadable_file) => unreadable.push((path.clone(), unreadable_file)),
            }
        }
        (units, unreadable)
    }
}

/// A change's Rust: the files, and which of the change's paths are the code under test rather than its tests.
pub struct RustChange<'a> {
    pub change: &'a Change,
    /// Each file of the change with a .rs path on a side, in the change's order, read with crates/lang-rust as
    /// [`Sources`] says. A side [`RustChange::among_tests`] places among the tests is read as test code whole.
    pub files: Vec<ChangedFile>,
    /// The paths on a side of the change that are Rust code under test, as [`RustChange::is_code`] says.
    code: HashSet<OsString>,
    /// The paths of the base's .rs files that only test code takes in (lang_rust::test_modules); none when the
    /// change touches no .rs path.
    test_modules_at_base: HashSet<OsString>,
    /// The same of the working tree's .rs files.
    test_modules_now: HashSet<OsString>,
}

impl<'a> RustChange<'a> {
    /// `change`'s Rust, its files read from `stored` by [`RustChange::among_tests`]. Errs when git cannot list the
    /// working tree's files, or one cannot be read.
    fn of(change: &'a Change, stored: &Stored) -> Result<RustChange<'a>, Unjudged> {
        let (mut test_modules_at_base, mut test_modules_now) = (HashSet::new(), HashSet::new());
        if any_path(change, |path| path.as_bytes().ends_with(b".rs")) {
            let (base, now) = trees(change, &stored.at_base, b".rs")?;
            test_modules_at_base = lang_rust::test_modules(&base);
            test_modules_now = lang_rust::test_modules(&now);
        }
        let mut rust =
            RustChange { change, files: vec![], code: HashSet::new(), test_modules_at_base, test_modules_now };
        rust.files = stored.files(&RUST, &|path, _, at_base| rust.among_tests(path, at_base))?.files;
        rust.code = rust.code_paths(&stored.gone);
        Ok(rust)
    }

    /// The paths on a side of the change that are Rust code under test, as [`RustChange::is_code`] says, `gone` being
    /// the base paths the change deletes or renames, where a Cargo.toml may have stood.
    fn code_paths(&self, gone: &HashSet<PathBuf>) -> HashSet<OsString> {
        let change = self.change;
        // Whether the change edits code at each path a file was read at, outside the test code of a file that is not
        // among the tests whole on each side it has.
        let mut edited: HashMap<&OsStr, bool> = HashMap::new();
        for f in &self.files {
            let sides = || [&f.before, &f.after].into_iter().flatten();
            let edits = !sides().all(|s| s.among_tests) && integrity::edits_code(f);
            for side in sides() {
                *edited.entry(&side.path).or_default() |= edits;
            }
        }
        paths_where(change, |path| {
            path.as_bytes().ends_with(b".rs")
                && !change.settings.rust_tests.matches(path)
                && edited.get(path).copied().unwrap_or(true)
                && in_project(change, gone, path, CARGO_MANIFEST)
        })
    }

    /// Whether the file at `path`, at the base when `at_base` and in the working tree otherwise, is among Rust's tests
    /// as a whole: gate.toml's `test-files.rust` matches it, or it is a .rs file only test code takes in there, as
    /// lang_rust::test_modules says. So a file whose `#[cfg(test)]` the change adds or removes is read as test code
    /// on one side only.
    pub fn among_tests(&self, path: &OsStr, at_base: bool) -> bool {
        let test_modules = if at_base { &self.test_modules_at_base } else { &self.test_modules_now };
        self.change.settings.rust_tests.matches(path) || test_modules.contains(path)
    }

    /// Whether `path`, on either side of the change, is Rust code under test: a .rs file of a Cargo package
    /// ([`in_project`]) that `test-files.rust` does not match, which the change adds, deletes, renames or edits
    /// outside its test code ([`integrity::edits_code`]), unless it is among the tests whole on each side it has
    /// ([`RustChange::among_tests`]). A path no file was read at, as a symlink's, counts as edited; a path on no side
    /// of the change is not code under test.
    pub fn is_code(&self, path: &OsStr) -> bool {
        self.code.contains(path)
    }

    /// Whether the change edits Rust code under test: whether a path on a side of one of its files [`is_code`].
    ///
    /// [`is_code`]: RustChange::is_code
    pub fn edits_code(&self) -> bool {
        !self.code.is_empty()
    }

    /// Whether `path`, one of the change's, belongs with Rust's tests rather than with the code they test, as a copy
    /// of the base the tests run in takes it from the working tree: a file gate.toml's `test-support` names, as
    /// Cargo's integration tests' data under a tests directory by default, a Rust test file as a whole in the working
    /// tree ([`RustChange::among_tests`]), and a .rs file the change edits only inside its test code, which
    /// [`RustChange::is_code`] does not count as code.
    pub fn beside_tests(&self, path: &OsStr) -> bool {
        self.change.settings.test_support.matches(path)
            || self.among_tests(path, false)
            || (path.as_bytes().ends_with(b".rs") && !self.is_code(path))
    }
}

/// A change's Go: the files, and which of the change's paths are the code under test rather than its tests.
pub struct GoChange<'a> {
    pub change: &'a Change,
    /// Each file of the change that is Go source of a Go module on a side ([`GO`]), in the change's order, read with
    /// crates/lang-go as [`Sources`] says. A `_test.go` file is read as test code whole.
    pub files: Vec<ChangedFile>,
    /// The paths on a side of the change that are Go code under test, as [`GoChange::is_code`] says.
    code: HashSet<OsString>,
    /// The bytes of each side of `files`, which [`GoChange::source_at_base`] and [`GoChange::source_now`] give.
    bytes: SideBytes,
}

impl<'a> GoChange<'a> {
    /// `change`'s Go, from the files `read` of it from `stored`.
    fn of(change: &'a Change, read: LanguageRead, stored: &Stored) -> GoChange<'a> {
        let code = paths_where(change, |path| {
            let bytes = path.as_bytes();
            bytes.ends_with(b".go")
                && !lang_go::among_tests(bytes)
                && !in_testdata(bytes)
                && in_project(change, &stored.gone, path, GO_MANIFEST)
        });
        GoChange { change, files: read.files, code, bytes: read.bytes }
    }

    /// Whether `path`, on either side of the change, is Go code under test: a `.go` file of a Go module
    /// ([`in_project`]) that is not a `_test.go` file and lies in no `testdata` directory, which holds a test's inputs.
    ///
    /// It counts more paths than [`files`] holds, as a file the gate does not judge may still be code the tests run:
    /// vendored and generated files are built into the module's packages, and a symlink's target is; a file under a
    /// directory starting with `.` or `_` is counted too, as erring wide only arms expected-value-changed, a finding
    /// to weigh. A path no file was read at counts, as Rust's does. Go has no test code inside a file of code, so,
    /// unlike Rust's ([`RustChange::is_code`]), it does not ask what the change edits in the file. A path on no side
    /// of the change is not code under test.
    ///
    /// [`files`]: GoChange::files
    pub fn is_code(&self, path: &OsStr) -> bool {
        self.code.contains(path)
    }

    /// Whether the change edits Go code under test: whether a path on a side of one of its files [`is_code`].
    ///
    /// [`is_code`]: GoChange::is_code
    pub fn edits_code(&self) -> bool {
        !self.code.is_empty()
    }

    /// Whether `path`, one of the change's, belongs with Go's tests rather than with the code they test, as a copy of
    /// the base the tests run in takes it from the working tree: a file gate.toml's `test-support` names, a
    /// `_test.go` file, and a file under a `testdata` directory, which holds the tests' inputs.
    pub fn beside_tests(&self, path: &OsStr) -> bool {
        let bytes = path.as_bytes();
        self.change.settings.test_support.matches(path) || lang_go::among_tests(bytes) || in_testdata(bytes)
    }

    /// The bytes of the base side of [`files`] at `base_path`, its path at the base; None where no file of the
    /// change was read as Go there.
    ///
    /// [`files`]: GoChange::files
    pub fn source_at_base(&self, base_path: &OsStr) -> Option<&[u8]> {
        self.bytes.at_base.get(base_path).map(|blob| &blob[..])
    }

    /// The bytes of the working-tree side of [`files`] at `path`; None where no file of the change was read as Go
    /// there.
    ///
    /// [`files`]: GoChange::files
    pub fn source_now(&self, path: &OsStr) -> Option<&[u8]> {
        self.bytes.now.get(path).map(Vec::as_slice)
    }
}

/// Whether `path`, from the tree's top, lies in a `testdata` directory, where Go keeps a test's inputs.
fn in_testdata(path: &[u8]) -> bool {
    path.split(|&b| b == b'/').any(|part| part == b"testdata")
}

/// Whether `holds` is true of a path on a side of one of `change`'s files.
fn any_path(change: &Change, holds: impl Fn(&OsStr) -> bool) -> bool {
    change.files.iter().flat_map(|f| [&f.before, &f.after]).flatten().any(|path| holds(path))
}

/// The paths on a side of one of `change`'s files of which `holds` is true.
fn paths_where(change: &Change, holds: impl Fn(&OsStr) -> bool) -> HashSet<OsString> {
    change.files.iter().flat_map(|f| [&f.before, &f.after]).flatten().filter(|path| holds(path)).cloned().collect()
}

/// Whether `settings` place the file at `path` among the Python tests: a `.py` file `test-files.python` matches.
pub(crate) fn python_test(settings: &Settings, path: &OsStr) -> bool {
    path.as_bytes().ends_with(b".py") && settings.python_tests.matches(path)
}

/// Files of a tree, as the front ends' imports take them: (path from the tree's top level, bytes).
pub(crate) type TreeFiles = Vec<(OsString, Vec<u8>)>;

/// (path, bytes) of each of the tree's judged regular files (Change::judged) outside mutmut's state
/// ([`under_mutmut_state`]) whose path ends in `suffix`, as `change`'s base holds them and as its working tree does,
/// `at_base` being the contents at the base of the change's files that are regular files there, by base path
/// ([`Stored::at_base`]). A base path is the file's there, which the change may rename. Errs when one of the working
/// tree's files cannot be read.
fn trees(
    change: &Change,
    at_base: &Blobs,
    suffix: &[u8],
) -> Result<(TreeFiles, TreeFiles), Unjudged> {
    let now = working_tree_files(change, |path| path.as_bytes().ends_with(suffix))?;
    // A file the change leaves alone is the same at the base; one it touches is git's there, under its base path.
    let touched: HashSet<&OsStr> = change.files.iter().filter_map(|f| f.after.as_deref()).collect();
    let mut base: TreeFiles =
        now.iter().filter(|(path, _)| !touched.contains(path.as_os_str())).cloned().collect();
    let changed = at_base.iter().filter(|(path, _)| path.as_bytes().ends_with(suffix));
    base.extend(changed.map(|(path, bytes)| (path.clone(), bytes.to_vec())));
    Ok((base, now))
}

/// (path, bytes) of each of the regular files the gate judges in `change`'s working tree (Change::judged) outside
/// mutmut's state ([`under_mutmut_state`]) whose path `wanted` holds of, as the working tree has them, in byte order
/// of their paths. Errs when one cannot be read.
fn working_tree_files(change: &Change, wanted: impl Fn(&OsStr) -> bool) -> Result<TreeFiles, Unjudged> {
    let mut out = vec![];
    for path in change.judged.iter().map(OsString::as_os_str).filter(|p| wanted(p)) {
        let full = change.root.join(path);
        if under_mutmut_state(path) || !regular_file(&full) {
            continue;
        }
        let read = std::fs::read(&full);
        let bytes = read.map_err(|e| Unjudged(format!("{} could not be read: {e}", text::path(full.as_os_str()))))?;
        out.push((path.to_os_string(), bytes));
    }
    Ok(out)
}

/// How one language's source files are told among a change's and read.
pub(crate) struct Language {
    /// Whether the file at `path`, from the tree's top level, holds the language's source. `head` gives the file's
    /// first [`FIRST_LINE_BYTES`] bytes, none for a file that cannot be read, and is called only when the name does
    /// not decide.
    holds: fn(path: &[u8], head: &dyn Fn() -> Vec<u8>) -> bool,
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

pub(crate) const PYTHON: Language = Language {
    holds: |path, head| lang_python::is_python(path, head),
    base_of_any_name: true,
    project: None,
    generated: |_| false,
    read: |source, path, among_tests| lang_python::read(source, Path::new(path), among_tests),
};

pub(crate) const RUST: Language = Language {
    holds: |path, _| path.ends_with(b".rs"),
    base_of_any_name: false,
    // Rust outside a Cargo package is still read, though it is no code under test ([`RustChange::is_code`]).
    project: None,
    generated: |_| false,
    read: |source, _, among_tests| lang_rust::read(source, among_tests),
};

pub(crate) const GO: Language = Language {
    holds: |path, _| lang_go::is_go(path),
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
    /// The contents at the base of each of the change's files that is a regular file there, by its base path.
    at_base: Blobs,
    /// The lines of its base the change removed from each file, by the file's path in the working tree.
    removed: ChangedLines,
    /// The base paths the change deletes or renames, where a project's file ([`Language::project`]) may have stood.
    gone: HashSet<PathBuf>,
}

impl<'a> Stored<'a> {
    /// Errs when git cannot list the base's tree or show a file of it.
    fn of(change: &'a Change) -> Result<Stored<'a>, Unjudged> {
        let base_paths: Vec<OsString> = change.files.iter().filter_map(|f| f.before.clone()).collect();
        let gone = change.files.iter().filter(|f| f.before != f.after).filter_map(|f| f.before.as_deref());
        Ok(Stored {
            change,
            at_base: regular_files_at_base(change, &base_paths)?.into_iter().map(|(p, b)| (p, b.into())).collect(),
            removed: removed_lines(&change.diff),
            gone: gone.map(PathBuf::from).collect(),
        })
    }

    /// Each file of the change that holds `language`'s source on a side, inside one of its projects where it has them
    /// ([`Language::project`]) and not generated ([`Language::generated`]), in the change's order, each such side read,
    /// with the bytes of each side read, and the base paths among them that held none of it by their name or first
    /// line, read all the same ([`Language::base_of_any_name`]). `among_tests(path, other, at_base)` says whether a
    /// side at `path`, the base's when `at_base`, whose other side, where the change gives it one, is at `other`, is
    /// among the language's tests as a whole, and is read as test code. Errs when such a file cannot be read in the
    /// working tree.
    fn files(
        &self,
        language: &Language,
        among_tests: &dyn Fn(&OsStr, Option<&OsStr>, bool) -> bool,
    ) -> Result<LanguageRead, Unjudged> {
        let change = self.change;
        let side = |path: &OsString, other: &Option<OsString>, source: &[u8], at_base: bool| {
            let among_tests = among_tests(path, other.as_deref(), at_base);
            Side { source: (language.read)(source, path, among_tests), path: path.clone(), among_tests }
        };
        let (mut out, mut bytes, mut bases_holding_none) = (vec![], SideBytes::default(), HashSet::new());
        for file in &change.files {
            let after = match file.after.as_ref().filter(|path| regular_file(&change.root.join(path))) {
                Some(path) => self.in_working_tree(path, language)?.map(|source| {
                    let after = side(path, &file.before, &source, false);
                    bytes.now.insert(path.clone(), source);
                    after
                }),
                None => None,
            };
            let whatever_it_holds = language.base_of_any_name && after.is_some();
            let before = file.before.as_ref().and_then(|path| {
                let (blob, holds) = self.at_base(path, language, whatever_it_holds)?;
                if !holds {
                    bases_holding_none.insert(path.clone());
                }
                let before = side(path, &file.after, blob, true);
                bytes.at_base.insert(path.clone(), Rc::clone(blob));
                Some(before)
            });
            if before.is_none() && after.is_none() {
                continue;
            }
            let lines =
                |from: &ChangedLines| file.after.as_ref().and_then(|p| from.get(p)).cloned().unwrap_or_default();
            out.push(ChangedFile { added: lines(&change.changed), removed: lines(&self.removed), before, after });
        }
        Ok(LanguageRead { files: out, bytes, bases_holding_none })
    }

    /// The bytes of the base's file at `path`, when it is a regular file there that holds `language`'s source or
    /// `whatever_it_holds`, lies in one of the language's projects ([`in_its_project`]) and is not generated
    /// ([`Language::generated`]), with whether it holds that source.
    fn at_base(&self, path: &OsStr, language: &Language, whatever_it_holds: bool) -> Option<(&Rc<[u8]>, bool)> {
        let blob = self.at_base.get(path)?;
        let holds = (language.holds)(path.as_bytes(), &|| blob[..blob.len().min(FIRST_LINE_BYTES)].to_vec());
        if (!whatever_it_holds && !holds) || !in_its_project(self.change, &self.gone, path, language) {
            return None;
        }
        Some((blob, holds)).filter(|(blob, _)| !(language.generated)(blob))
    }

    /// The bytes of the working tree's regular file at `path`, when it holds `language`'s source in one of its
    /// projects ([`holds_source`]) and is not generated ([`Language::generated`]).
    fn in_working_tree(&self, path: &OsStr, language: &Language) -> Result<Option<Vec<u8>>, Unjudged> {
        if !holds_source(self.change, &self.gone, path, language) {
            return Ok(None);
        }
        let full = self.change.root.join(path);
        let read = std::fs::read(&full);
        let bytes = read.map_err(|e| Unjudged(format!("{} could not be read: {e}", text::path(full.as_os_str()))))?;
        Ok(Some(bytes).filter(|bytes| !(language.generated)(bytes)))
    }
}

/// One language's part of a change, as [`Stored::files`] reads it.
struct LanguageRead {
    /// Each file of the change that holds the language's source on a side, each such side read.
    files: Vec<ChangedFile>,
    /// The bytes of each side of `files`.
    bytes: SideBytes,
    /// The base paths of `files` that held none of the language's source by their name or first line, each read as
    /// the base of the language's file it became ([`Language::base_of_any_name`]).
    bases_holding_none: HashSet<OsString>,
}

/// Whether the working tree's file at `path`, from the tree's top, holds `language`'s source by its name or its first
/// line ([`Language::holds`]), in one of the language's projects where it has them ([`in_its_project`], `gone` as
/// there). A file that is not a regular one, or cannot be read, has no first line: opening a FIFO, or a symlink to
/// one, would block ([`Sources`]).
fn holds_source(change: &Change, gone: &HashSet<PathBuf>, path: &OsStr, language: &Language) -> bool {
    let head = || {
        let full = change.root.join(path);
        let mut head = vec![];
        let first = |file: std::fs::File| file.take(FIRST_LINE_BYTES as u64).read_to_end(&mut head);
        let read = regular_file(&full).then(|| std::fs::File::open(&full).and_then(first));
        if matches!(read, Some(Ok(_))) { head } else { vec![] }
    };
    (language.holds)(path.as_bytes(), &head) && in_its_project(change, gone, path, language)
}

/// Whether `path`, the path of a side that holds `language`'s source, lies in one of the language's projects
/// ([`in_project`], `gone` as there), as it must to be read as the language's where the language has them
/// ([`Language::project`]). It is asked after [`Language::holds`], which most paths fail by their name alone, as it
/// walks up the path's directories with a `stat` at each.
fn in_its_project(change: &Change, gone: &HashSet<PathBuf>, path: &OsStr, language: &Language) -> bool {
    language.project.is_none_or(|manifest| in_project(change, gone, path, manifest))
}

/// (path, bytes) of each of `paths`, each a path of the tree at `change`'s base, that is a regular file there, as
/// git's tree records it: not a symlink or a submodule. Errs when git cannot list the base's tree or show one of the
/// files.
fn regular_files_at_base(change: &Change, paths: &[OsString]) -> Result<HashMap<OsString, Vec<u8>>, Unjudged> {
    if paths.is_empty() {
        return Ok(HashMap::new());
    }
    // The whole tree, listed once: naming the paths instead would put as many on one command line as the change
    // touches, more than the system allows (E2BIG) for a change to a large vendored tree.
    let listed = git::output(&change.root, &["ls-tree", "-r", "-z", "--full-tree", &change.base])?;
    let wanted: HashSet<&[u8]> = paths.iter().map(|p| p.as_bytes()).collect();
    // Each entry is `<mode> <type> <object>\t<path>`, NUL-ended; a regular file's mode is 100644 or 100755.
    let regular = listed.split(|&b| b == 0).filter(|entry| entry.starts_with(b"100"));
    let (mut found, mut objects) = (vec![], vec![]);
    for entry in regular {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else { continue };
        let (path, object) = (&entry[tab + 1..], entry[..tab].rsplit(|&b| b == b' ').next().unwrap_or_default());
        if wanted.contains(path) {
            found.push(OsStr::from_bytes(path).to_os_string());
            objects.push(object);
        }
    }
    Ok(found.into_iter().zip(git::objects(&change.root, &objects)?).collect())
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
