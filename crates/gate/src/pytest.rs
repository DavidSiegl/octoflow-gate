//! Running pytest in a copy of a tree, for the checks that run a change's Python tests: which configuration each
//! test file takes, the run itself, and how pytest names a test in what it prints and in its JUnit report. The
//! Python is the judged project's (judge::Change::python). gate.py's `_pytest` and what stands around it.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use lang_python::Unit;
use regex::Regex;

use crate::parser_checks::{python_test, PythonChange, UnreadFiles};
use crate::paths::rel_path;
use crate::process::{run_with, Extras, Finished};
use crate::report::Finding;
use crate::text;

/// The changed tests a check runs: the line of each one's def, by its file's path and its qualified name, `test_f`
/// or `TestC.test_m`. The path is from the copy's top, or from a group's directory ([`Group::own`]).
pub type Tests = BTreeMap<(OsString, String), u32>;

/// The test functions and methods the change touched in the test files it left in the working tree
/// (PythonChange::changed_units, of the files gate.toml's `test-files.python` names): a unit whose own name starts
/// with `test`, to which the change added a line, or from which it removed one, between the statement before it and
/// its end. What lies above its def, such as its decorators and comments, may have changed it, and a decorator
/// replaced or removed leaves no line of the def itself changed; lines removed right after its last line count too,
/// though they may have been the blank lines below it. And each such file that could not be read or parsed.
/// gate.py's `_changed_python_tests`.
pub fn changed_tests(python: &PythonChange) -> (Tests, UnreadFiles) {
    let in_scope = |path: &OsStr| python_test(&python.change.settings, path);
    let (units, unreadable) = python.changed_units(in_scope, |unit, added, removal_points| {
        added.iter().any(|&n| unit.floor < n && n <= unit.lines.last)
            || removal_points.iter().any(|&n| unit.floor <= n && n <= unit.lines.last)
    });
    let named_test = |unit: &Unit| unit.name.rsplit('.').next().is_some_and(|own| own.starts_with("test"));
    let tests = units.into_iter().filter(|(_, unit)| named_test(unit));
    (tests.map(|(path, unit)| ((path, unit.name), unit.line)).collect(), unreadable)
}

/// How the gate runs pytest in a copy of a tree: with which Python, where, and by what configuration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Setup {
    /// The program that runs Python, as judge::Change::python names it.
    pub python: PathBuf,
    /// pytest's rootdir, the configuration's directory, from which its node ids and reports name paths. Absolute.
    pub root: PathBuf,
    /// The configuration file, given with `-c`, so that pytest looks for none above the copy. Absolute.
    pub inifile: PathBuf,
    /// The copy's top directory: where pytest runs, so that `python -m` puts it on the path, and above which no
    /// conftest.py is imported. `root` is it or lies under it.
    pub top: PathBuf,
}

impl Setup {
    /// `root`, by its path in the copy; empty for the top.
    fn place(&self) -> PathBuf {
        let place = rel_path(&self.root, &self.top);
        if place == Path::new(".") {
            PathBuf::new()
        } else {
            place
        }
    }

    /// `name`, a test file's path or a node id from `root`, as pytest is to be given it, run from `top`.
    pub fn argument(&self, name: &OsStr) -> OsString {
        self.place().join(name).into_os_string()
    }
}

/// Changed test files that take one pytest configuration, as [`groups`] finds them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    /// How to run them, rooted at the configuration's directory.
    pub setup: Setup,
    /// The files, by their paths in the copy.
    pub paths: BTreeSet<OsString>,
}

impl Group {
    /// Those of `tests`, keyed by their paths in the copy, whose files are the group's, keyed by their paths from the
    /// group's directory, as pytest names them run there.
    pub fn own(&self, tests: &Tests) -> Tests {
        let place = self.setup.place();
        let from_place = |path: &OsString| match path.as_bytes().strip_prefix(place.as_os_str().as_bytes()) {
            // Every file of the group lies under its directory, so what follows the directory's path is a slash.
            Some(rest) if !place.as_os_str().is_empty() => OsStr::from_bytes(&rest[1.min(rest.len())..]).to_os_string(),
            _ => path.clone(),
        };
        let own = tests.iter().filter(|((path, _), _)| self.paths.contains(path));
        own.map(|((path, name), line)| ((from_place(path), name.clone()), *line)).collect()
    }

    /// `finding`, its path one from the group's directory or None, with its path in the copy instead.
    pub fn in_tree(&self, finding: Finding) -> Finding {
        let path = finding.path.map(|path| self.setup.argument(&path));
        Finding { path, ..finding }
    }

    /// The group's tests, as a finding about all of them names them when the change's tests fall in more than one
    /// group: by their configuration file's path in the copy.
    pub fn label(&self) -> String {
        // The empty configuration is the one file given with -c that lies outside the copy ([`config`]).
        if !self.setup.inifile.starts_with(&self.setup.top) {
            return "the changed tests with no pytest configuration".into();
        }
        let inifile = rel_path(&self.setup.inifile, &self.setup.top);
        format!("the changed tests configured by {}", text::path(inifile.as_os_str()))
    }
}

/// How a finding about all of each of `groups`' tests names them, by their configuration, when there is more than
/// one group; None for each when there is one, whose tests are all the changed tests.
pub fn labels(groups: &[Group]) -> Vec<Option<String>> {
    groups.iter().map(|group| (groups.len() > 1).then(|| group.label())).collect()
}

/// `paths`, test files by their paths in `tree`, a copy of a tree, grouped by the configuration pytest takes for
/// each, to run with `python`: the one [`inifile`] finds in the file's directory, else in the nearest directory above
/// it that has one, up to `tree`'s top and never above it, as pytest 9's `locate_config` walks up from the files it
/// is given (its `_pytest/config/findpaths.py`). In the order of their directories. Files with none share an empty
/// configuration written in `tmp`, as [`config`] writes it. Errs as [`config`] does.
pub fn groups(python: &Path, tree: &Path, tmp: &Path, paths: &BTreeSet<OsString>) -> std::io::Result<Vec<Group>> {
    // Each directory read, by its path in the tree, and the configuration it holds.
    let mut seen: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    // The files under each configuration, by its directory's path in the tree; those with none under "".
    let mut found: BTreeMap<OsString, (Option<PathBuf>, BTreeSet<OsString>)> = BTreeMap::new();
    for path in paths {
        let mut place = Path::new(path).parent().unwrap_or(Path::new(""));
        let own = loop {
            let own = seen.entry(place.to_path_buf()).or_insert_with(|| inifile(&tree.join(place))).clone();
            match place.parent() {
                Some(above) if own.is_none() => place = above,
                _ => break own,
            }
        };
        let key = if own.is_some() { place.as_os_str().to_os_string() } else { OsString::new() };
        found.entry(key).or_insert_with(|| (own, BTreeSet::new())).1.insert(path.clone());
    }
    let mut out = vec![];
    for (place, (own, members)) in found {
        let inifile = match own {
            Some(own) => own,
            None => empty_ini(tmp)?,
        };
        let setup = Setup { python: python.to_path_buf(), root: tree.join(place), inifile, top: tree.to_path_buf() };
        out.push(Group { setup, paths: members });
    }
    Ok(out)
}

/// The pytest configuration to run all the tests of `tree`, a copy of a tree, by from its top: its own, as
/// [`inifile`] finds it, else an empty pytest.ini that this writes in `tmp`, the private directory beside the copy,
/// so that pytest, given it with `-c`, looks for none above the copy, where another account may write. Errs when
/// that file cannot be written, or `tmp` has one already.
pub fn config(tree: &Path, tmp: &Path) -> std::io::Result<PathBuf> {
    match inifile(tree) {
        Some(own) => Ok(own),
        None => empty_ini(tmp),
    }
}

/// The path of an empty pytest.ini that this writes in `tmp`, where none is yet.
fn empty_ini(tmp: &Path) -> std::io::Result<PathBuf> {
    let path = tmp.join("pytest.ini");
    std::fs::File::create_new(&path)?;
    Ok(path)
}

/// The file in the directory `dir` that pytest 9 takes its configuration from when run there: the first of
/// pytest.toml, .pytest.toml, pytest.ini and .pytest.ini, a pyproject.toml with a `[tool.pytest]` table that is not
/// empty, a tox.ini with a `[pytest]` section and a setup.cfg with `[tool:pytest]`, as pytest's `locate_config`
/// orders them. None when it has none. A candidate that cannot be read or parsed is taken, so that pytest reports
/// what is wrong with it.
fn inifile(dir: &Path) -> Option<PathBuf> {
    /// An ini file's section lines, each `[name]` with its name captured.
    static SECTION: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?m)^\[\s*([^]\n]*?)\s*]").expect("a regex"));
    for name in ["pytest.toml", ".pytest.toml", "pytest.ini", ".pytest.ini", "pyproject.toml", "tox.ini", "setup.cfg"] {
        let path = dir.join(name);
        if !path.is_file() {
            continue;
        }
        let section = match name {
            "pyproject.toml" => None,
            "tox.ini" => Some("pytest"),
            "setup.cfg" => Some("tool:pytest"),
            _ => return Some(path),
        };
        let Ok(source) = std::fs::read_to_string(&path) else { return Some(path) };
        let configures = match section {
            Some(section) => SECTION.captures_iter(&source).any(|found| &found[1] == section),
            None => match source.parse::<toml::Table>() {
                // An empty [tool.pytest] table is not one; an empty [tool.pytest.ini_options] is, and fills it.
                Ok(table) => table.get("tool").and_then(|tool| tool.get("pytest")).is_some_and(truthy),
                Err(_) => true,
            },
        };
        if configures {
            return Some(path);
        }
    }
    None
}

/// Whether Python takes the TOML `value` for true: not an empty table, array or string, zero or false.
pub(crate) fn truthy(value: &toml::Value) -> bool {
    match value {
        toml::Value::Table(table) => !table.is_empty(),
        toml::Value::Array(array) => !array.is_empty(),
        toml::Value::String(string) => !string.is_empty(),
        toml::Value::Integer(n) => *n != 0,
        toml::Value::Float(n) => *n != 0.0,
        toml::Value::Boolean(b) => *b,
        toml::Value::Datetime(_) => true,
    }
}

/// A pytest run that told nothing: it could not start, was stopped or exited with a status the caller did not expect.
#[derive(Debug)]
pub struct Problem {
    /// What went wrong, as a clause a finding can open with: "pytest, collecting the changed tests, exited with
    /// status 4: ...".
    pub clause: String,
    /// What the run printed until its timeout stopped it; None for any other problem.
    pub stopped: Option<Finished>,
}

/// pytest run by `setup` with `args` after the gate's own, for at most `timeout`, and what it printed; a
/// [`Problem`], its clause naming `doing`, as "collecting the changed tests", when it could not start, took longer
/// than `timeout` or exited with a status outside `ok`. Status 0 is all passed, 1 some failed.
///
/// It runs at `setup.top` as process::run_with runs a tool, killed at once at the timeout, with `env`'s variables
/// beside process::tool_env's: at verbosity -1, for test cases too, without its cache, taking no configuration but
/// `setup.inifile` and no conftest.py from above `setup.top`, its output unbuffered, so that what it printed before
/// a kill is whole. pytest writes a name that is not UTF-8 as its bytes, so the node ids in its output hold paths as
/// git's do, and only `\n` ends a line. pytest-randomly 5.0.0 cannot shuffle such a name: under `-p randomly` pytest
/// stops with an internal error.
pub fn run(
    setup: &Setup,
    doing: &str,
    args: &[OsString],
    timeout: Duration,
    ok: &[i32],
    env: &[(&str, &OsStr)],
) -> Result<Finished, Problem> {
    // --verbosity sets the level outright, and pytest reads the project's addopts and PYTEST_ADDOPTS before the
    // command line, so a -v or -q of theirs cannot change what the collection prints. The list of collected tests
    // follows verbosity_test_cases where the project's configuration sets it, and -o overrides that; in pytest 9.0
    // no other option changes the list.
    // Without -c pytest looks for its configuration in every directory above the test files, and the copy is in a
    // temporary directory another account may write to, along with a conftest.py beside it: -c ends that search,
    // and --confcutdir keeps conftest.py files above the copy from being imported.
    let mut argv: Vec<&OsStr> = vec![setup.python.as_os_str()];
    argv.extend(["-m", "pytest", "-p", "no:cacheprovider", "-c"].map(OsStr::new));
    argv.extend([setup.inifile.as_os_str(), OsStr::new("--rootdir"), setup.root.as_os_str()]);
    argv.extend([OsStr::new("--confcutdir"), setup.top.as_os_str()]);
    argv.extend(["--verbosity=-1", "-o", "verbosity_test_cases=-1"].map(OsStr::new));
    argv.extend(args.iter().map(OsString::as_os_str));
    let mut all_env = env.to_vec();
    let unbuffered = [("PYTHONUNBUFFERED", "1"), ("PYTHONIOENCODING", "utf-8:surrogateescape")];
    all_env.extend(unbuffered.map(|(name, set_to)| (name, OsStr::new(set_to))));
    let extras = Extras { env: &all_env, soft_stop: None };
    let finished = match run_with(&argv, &setup.top, timeout, &extras) {
        Ok(finished) => finished,
        Err(e) => {
            let python = text::path(setup.python.as_os_str());
            let clause = format!("could not start pytest with the Python `{python}`, {doing}: {e}");
            return Err(Problem { clause, stopped: None });
        }
    };
    if finished.stopped {
        let clause = format!("pytest, {doing}, did not finish within {} seconds", timeout.as_secs_f64());
        return Err(Problem { clause, stopped: Some(finished) });
    }
    if !ok.contains(&finished.status) {
        let clause = format!("pytest, {doing}, exited with status {}: {}", finished.status, finished.said());
        return Err(Problem { clause, stopped: None });
    }
    Ok(finished)
}

/// The path, the qualified name and the parameters of the pytest node id `id`, such as `tests/t.py`,
/// `TestX.test_y` and `[a::b]` for `tests/t.py::TestX::test_y[a::b]`; "" for a node without parameters. As pytest's
/// JUnit report splits it: the parameters start at the first `[`, and only the `::` before them separate names. The
/// qualified name is the one lang_python::Unit gives the test; it and the parameters are written by text::lossless.
pub fn node_parts(id: &OsStr) -> (OsString, String, String) {
    let id = id.as_bytes();
    let (head, params) = id.split_at(id.iter().position(|&b| b == b'[').unwrap_or(id.len()));
    let mut names: Vec<&[u8]> = vec![];
    let mut rest = head;
    while let Some(at) = rest.windows(2).position(|pair| pair == b"::") {
        names.push(&rest[..at]);
        rest = &rest[at + 2..];
    }
    names.push(rest);
    let path = OsString::from_vec(names.remove(0).to_vec());
    (path, text::lossless(&names.join(&b"."[..])), text::lossless(params))
}

/// How junit::junit_outcomes names the test of the pytest node id `id`: pytest's JUnit classname, the path dotted
/// without its `.py` and the classes after it, then a dot and the test's name with its parameters, as pytest's
/// `mangle_test_address` makes them.
pub fn junit_case(id: &OsStr) -> String {
    let (path, name, params) = node_parts(id);
    let module = text::path(&path);
    let module = module.strip_suffix(".py").unwrap_or(&module).replace('/', ".");
    if name.is_empty() {
        return format!("{module}{params}");
    }
    format!("{module}.{name}{params}")
}

/// The node ids, one per parameter set and sorted, of those of `tests` that `collected`, a `pytest --collect-only`
/// that [`run`] ran, listed; `tests` are keyed by their paths from the run's rootdir. Errs with the problem, a
/// clause naming `doing`, when it exited with status 0 but listed no node id in a form the gate can read.
pub fn listed_tests(collected: &Finished, doing: &str, tests: &Tests) -> Result<Vec<OsString>, String> {
    let files: BTreeSet<&OsString> = tests.keys().map(|(path, _)| path).collect();
    // At verbosity -1 pytest lists one node id a line, unindented, before its summary; a warnings summary repeats
    // some, indented or not. A collection that found tests but listed none in that form cannot be judged.
    let lines = collected.stdout.split(|&b| b == b'\n').filter(|line| line.windows(2).any(|pair| pair == b"::"));
    let ids: BTreeSet<OsString> = lines
        .map(|line| OsString::from_vec(line.to_vec()))
        .filter(|id| files.contains(&node_parts(id).0))
        .collect();
    if collected.status == 0 && ids.is_empty() {
        return Err(format!("pytest, {doing}, printed no node id the gate could read: {}", collected.said()));
    }
    let wanted = |id: &OsString| {
        let (path, name, _) = node_parts(id);
        tests.contains_key(&(path, name))
    };
    Ok(ids.into_iter().filter(wanted).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new empty directory for the test `name`.
    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gate-pytest-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Writes `contents` to `path` under `dir`, with the directories above it.
    fn write(dir: &Path, path: &str, contents: &str) {
        let path = dir.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    fn printed(status: i32, stdout: &str) -> Finished {
        Finished { status, stdout: stdout.into(), stderr: vec![], stopped: false }
    }

    #[test]
    fn a_node_id_is_a_path_the_names_after_it_and_the_parameters() {
        let parts = |id: &str| {
            let (path, name, params) = node_parts(OsStr::new(id));
            (path.into_string().unwrap(), name, params)
        };
        let strings = |(a, b, c): (&str, &str, &str)| (a.to_string(), b.to_string(), c.to_string());
        assert_eq!(parts("tests/t.py::TestX::test_y[a::b]"), strings(("tests/t.py", "TestX.test_y", "[a::b]")));
        assert_eq!(parts("t.py::test_y"), strings(("t.py", "test_y", "")));
        assert_eq!(parts("t.py"), strings(("t.py", "", "")));
        assert_eq!(junit_case(OsStr::new("tests/t.py::TestX::test_y[a::b]")), "tests.t.TestX.test_y[a::b]");
        assert_eq!(junit_case(OsStr::new("t.py::test_y")), "t.test_y");
    }

    #[test]
    fn the_configuration_is_the_first_file_pytest_would_take() {
        let dir = scratch("inifile");
        assert_eq!(inifile(&dir), None);
        write(&dir, "setup.cfg", "[metadata]\nname = x\n");
        write(&dir, "tox.ini", "[tox]\n");
        write(&dir, "pyproject.toml", "[tool.pytest]\n[tool.ruff]\n");
        assert_eq!(inifile(&dir), None);
        write(&dir, "setup.cfg", "[metadata]\n[ tool:pytest ]\n");
        assert_eq!(inifile(&dir), Some(dir.join("setup.cfg")));
        write(&dir, "tox.ini", "[tox]\n[pytest]\n");
        assert_eq!(inifile(&dir), Some(dir.join("tox.ini")));
        write(&dir, "pyproject.toml", "[tool.pytest.ini_options]\n");
        assert_eq!(inifile(&dir), Some(dir.join("pyproject.toml")));
        write(&dir, "pyproject.toml", "[tool.ruff]\n");
        assert_eq!(inifile(&dir), Some(dir.join("tox.ini")));
        // One that cannot be parsed is taken, for pytest to say what is wrong with it.
        write(&dir, "pyproject.toml", "[tool\n");
        assert_eq!(inifile(&dir), Some(dir.join("pyproject.toml")));
        write(&dir, ".pytest.ini", "");
        assert_eq!(inifile(&dir), Some(dir.join(".pytest.ini")));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_configuration_is_found_as_pytest_finds_it() {
        // TestFlakyTests' cases (tests/test_gate.py): the files of a directory, and the one pytest takes.
        type Files = [(&'static str, &'static str)];
        let cases: [(&Files, Option<&str>); 10] = [
            (&[], None),
            (&[("pyproject.toml", "[project]\n"), ("tox.ini", "[tox]\n"), ("setup.cfg", "[metadata]\n")], None),
            (&[("pyproject.toml", "[tool.pytest]\n")], None),
            (&[("pyproject.toml", "[tool.pytest.ini_options]\n"), ("pytest.ini", "")], Some("pytest.ini")),
            (&[("pyproject.toml", "[tool.pytest.ini_options]\n"), ("tox.ini", "[pytest]\n")], Some("pyproject.toml")),
            (&[("pyproject.toml", "[tool.pytest]\naddopts = []\n")], Some("pyproject.toml")),
            (&[("tox.ini", "[pytest]\n"), ("setup.cfg", "[tool:pytest]\n")], Some("tox.ini")),
            (&[("tox.ini", "[tool:pytest]\n"), ("setup.cfg", "[tool:pytest]\n")], Some("setup.cfg")),
            (&[(".pytest.ini", ""), ("pytest.toml", "")], Some("pytest.toml")),
            (&[("pyproject.toml", "not toml [")], Some("pyproject.toml")),
        ];
        for (n, (files, expected)) in cases.into_iter().enumerate() {
            let dir = scratch(&format!("inifile-case-{n}"));
            for (name, contents) in files {
                write(&dir, name, contents);
            }
            assert_eq!(inifile(&dir), expected.map(|name| dir.join(name)), "{files:?}");
            std::fs::remove_dir_all(&dir).unwrap();
        }
    }

    #[test]
    fn a_group_s_label_names_its_configuration_or_its_lack_of_one() {
        let group = |inifile: &str| {
            let top = PathBuf::from("/w/tree");
            let setup = Setup { python: "py".into(), root: top.clone(), inifile: inifile.into(), top };
            Group { setup, paths: BTreeSet::new() }
        };
        assert_eq!(group("/w/pytest.ini").label(), "the changed tests with no pytest configuration");
        assert_eq!(group("/w/tree/..ci/pytest.ini").label(), "the changed tests configured by ..ci/pytest.ini");
        assert_eq!(group("/w/tree/b/tox.ini").label(), "the changed tests configured by b/tox.ini");
    }

    #[test]
    fn a_run_that_tells_nothing_is_a_problem_that_says_why() {
        use std::os::unix::fs::PermissionsExt;
        let dir = scratch("run");
        // A Python that is a shell script, running `script` whatever it is asked.
        let run_script = |script: &str, timeout: u64| {
            let python = dir.join("python");
            std::fs::write(&python, format!("#!/bin/sh\n{script}\n")).unwrap();
            std::fs::set_permissions(&python, std::fs::Permissions::from_mode(0o755)).unwrap();
            let setup = Setup { python, root: dir.clone(), inifile: dir.join("pytest.ini"), top: dir.clone() };
            run(&setup, "collecting", &[], Duration::from_secs(timeout), &[0, 5], &[])
        };
        assert_eq!(run_script("echo listed; exit 5", 10).unwrap().stdout, b"listed\n");
        let failed = run_script("echo 'ERROR: usage' >&2; exit 4", 10).unwrap_err();
        assert_eq!(failed.clause, "pytest, collecting, exited with status 4: ERROR: usage");
        assert!(failed.stopped.is_none());
        let late = run_script("echo started; exec sleep 30", 1).unwrap_err();
        assert_eq!(late.clause, "pytest, collecting, did not finish within 1 seconds");
        assert_eq!(late.stopped.unwrap().stdout, b"started\n");
        let python = dir.join("none");
        let setup = Setup { python, root: dir.clone(), inifile: dir.join("pytest.ini"), top: dir.clone() };
        let missing = run(&setup, "collecting", &[], Duration::from_secs(10), &[0], &[]).unwrap_err();
        let named = format!("could not start pytest with the Python `{}`, collecting: ", dir.join("none").display());
        assert_eq!(missing.clause, format!("{named}No such file or directory (os error 2)"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn an_empty_configuration_that_cannot_be_written_is_an_error() {
        // gate.py's test_an_empty_configuration_that_cannot_be_written_is_a_finding, as far as the error: the checks
        // make a finding of it, and no run of the gate can be made to fail writing into the directory it has made.
        let dir = scratch("empty-ini");
        let (tree, tmp) = (dir.join("tree"), dir.join("tmp"));
        write(&tree, "test_top.py", "");
        let paths: BTreeSet<OsString> = [OsString::from("test_top.py")].into();
        let kind = |written: std::io::Result<PathBuf>| written.map_err(|e| e.kind());
        // No directory to write it in.
        assert_eq!(groups(Path::new("py"), &tree, &tmp, &paths).unwrap_err().kind(), std::io::ErrorKind::NotFound);
        assert_eq!(kind(config(&tree, &tmp)), Err(std::io::ErrorKind::NotFound));
        // One is there already, which may be another's.
        write(&tmp, "pytest.ini", "[pytest]\n");
        assert_eq!(groups(Path::new("py"), &tree, &tmp, &paths).unwrap_err().kind(), std::io::ErrorKind::AlreadyExists);
        assert_eq!(kind(config(&tree, &tmp)), Err(std::io::ErrorKind::AlreadyExists));
        // A tree with a configuration of its own needs none written.
        write(&tree, "pytest.ini", "");
        assert_eq!(kind(config(&tree, &tmp)), Ok(tree.join("pytest.ini")));
        assert_eq!(groups(Path::new("py"), &tree, &tmp, &paths).unwrap().len(), 1);
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn test_files_are_grouped_by_the_nearest_configuration_above_them() {
        let dir = scratch("groups");
        let (tree, tmp) = (dir.join("tree"), dir.join("tmp"));
        std::fs::create_dir_all(&tmp).unwrap();
        write(&tree, "a/pytest.ini", "");
        write(&tree, "a/b/tox.ini", "[tox]\n");
        for path in ["a/b/test_deep.py", "a/test_a.py", "test_top.py", "c/test_c.py"] {
            write(&tree, path, "");
        }
        let paths: BTreeSet<OsString> =
            ["a/b/test_deep.py", "a/test_a.py", "test_top.py", "c/test_c.py"].map(OsString::from).into();
        let found = groups(Path::new("py"), &tree, &tmp, &paths).unwrap();
        let files = |g: &Group| -> Vec<String> { g.paths.iter().map(|p| text::path(p)).collect() };
        let shown: Vec<(PathBuf, PathBuf, Vec<String>)> =
            found.iter().map(|g| (g.setup.root.clone(), g.setup.inifile.clone(), files(g))).collect();
        assert_eq!(
            shown,
            [
                (tree.clone(), tmp.join("pytest.ini"), vec!["c/test_c.py".to_string(), "test_top.py".into()]),
                (tree.join("a"), tree.join("a/pytest.ini"), vec!["a/b/test_deep.py".into(), "a/test_a.py".into()]),
            ]
        );
        assert_eq!(
            labels(&found),
            [
                Some("the changed tests with no pytest configuration".to_string()),
                Some("the changed tests configured by a/pytest.ini".to_string()),
            ]
        );
        assert_eq!(labels(&found[1..]), [None]);

        // A group names its tests from its own directory, and its findings from the copy's top.
        let tests: Tests =
            [(("a/b/test_deep.py".into(), "test_x".to_string()), 3), (("test_top.py".into(), "test_y".to_string()), 1)]
                .into();
        assert_eq!(found[1].own(&tests), [(("b/test_deep.py".into(), "test_x".to_string()), 3)].into());
        assert_eq!(found[0].own(&tests), [(("test_top.py".into(), "test_y".to_string()), 1)].into());
        assert_eq!(found[1].setup.argument(OsStr::new("b/test_deep.py::test_x")), "a/b/test_deep.py::test_x");
        assert_eq!(found[0].setup.argument(OsStr::new("test_top.py")), "test_top.py");
        let finding = |path: Option<&str>| crate::copies::finding(path.map(OsStr::new), Some(3), "r", "m".into());
        assert_eq!(found[1].in_tree(finding(Some("b/test_deep.py"))), finding(Some("a/b/test_deep.py")));
        assert_eq!(found[1].in_tree(finding(None)), finding(None));
        // The empty configuration is written once: a second grouping beside the same copy cannot.
        assert!(groups(Path::new("py"), &tree, &tmp, &paths).is_err());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_listed_tests_are_the_wanted_ones_among_the_node_ids_printed() {
        let test = |name: &str, line: u32| ((OsString::from("t.py"), name.to_string()), line);
        let tests: Tests = [test("test_a", 1), test("T.test_b", 5)].into();
        let stdout = "t.py::test_a[1]\nt.py::test_a[0]\nt.py::T::test_b\nt.py::test_other\nu.py::test_a\n\
                      \n  t.py::test_a[0]: a warning\n3 tests collected in 0.01s\n";
        let ids = listed_tests(&printed(0, stdout), "collecting", &tests).unwrap();
        assert_eq!(ids, ["t.py::T::test_b", "t.py::test_a[0]", "t.py::test_a[1]"].map(OsString::from));
        let none = listed_tests(&printed(5, "no tests collected\n"), "collecting", &tests).unwrap();
        assert_eq!(none, Vec::<OsString>::new());
        assert_eq!(
            listed_tests(&printed(0, "<Module t.py>\n"), "collecting", &tests).unwrap_err(),
            "pytest, collecting, printed no node id the gate could read: <Module t.py>"
        );
    }
}
