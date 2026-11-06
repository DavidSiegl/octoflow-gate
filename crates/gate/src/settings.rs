//! How the gate reads the project it judges, as its gate.toml says (docs/quality-gate.md, "The gate's settings").

use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path};

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;

use crate::glob::Globs;
use crate::paths::real_path;
use crate::report::{BUILD_CONFIG_FILES, SETTINGS_FILE};
use crate::Unjudged;

/// The linters gate.toml may choose the files of, each a name its `[linters]` table may hold.
pub const LINTER_NAMES: [&str; 6] = ["ruff", "mypy", "pymarkdown", "shellcheck", "actionlint", "clippy"];

/// The settings a project is judged by: [`Settings::default`] for one without gate.toml, else what its gate.toml
/// says, a default for each key it leaves out. gate.py's GateSettings.
#[derive(Clone, Debug)]
pub struct Settings {
    pub python_tests: Globs,
    pub rust_tests: Globs,
    /// Files that are not tests but go with them rather than with the code they test, such as fixtures and their
    /// data: the checks that run tests take them from the working tree, as they do the tests.
    pub test_support: Globs,
    /// Files that hold a test's expected output, which the test-integrity check reads as expected values.
    pub golden: Globs,
    /// Files whose functions the cognitive-complexity check does not score.
    pub complexity_exempt: Globs,
    /// (path, note) for each file beyond BUILD_CONFIG_FILES that alters how the project is built or judged, in
    /// gate.toml's order: the note, a sentence, follows "The change edits `path`." in the quality file.
    pub build_config: Vec<(String, String)>,
    /// (linter, globs) for each linter whose files gate.toml chooses, among [`LINTER_NAMES`]; the rest read their
    /// own. No globs means the linter does not run.
    pub linters: Vec<(String, Globs)>,
    /// Seconds; `--mutation-budget` and `--base-run-budget` replace them.
    pub mutation_budget: u64,
    pub base_run_budget: u64,
    pub test_commands: Vec<TestCommand>,
}

/// A command that runs the project's tests, one `[[test]]` entry of gate.toml, which the gate runs and whose reports
/// it judges (tests.rs); `--tests` or `--coverage` replace all of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCommand {
    /// What findings about the command call it.
    pub name: String,
    /// The argv, run in the tree's top level; never through a shell. Never empty.
    pub command: Vec<String>,
    /// The JUnit and the Cobertura reports it writes, paths inside the tree from its top level.
    pub junit: Vec<String>,
    pub coverage: Vec<String>,
}

impl Default for Settings {
    fn default() -> Settings {
        Settings {
            python_tests: Globs::new(&["**/tests/**/*.py", "**/test_*.py", "**/*_test.py"]),
            rust_tests: Globs::new(&["**/tests/**/*.rs", "**/tests.rs"]),
            test_support: Globs::new(&["**/conftest.py", "**/tests/**"]),
            golden: Globs::new(&["**/tests/**/golden/**"]),
            complexity_exempt: Globs::new(&["tests/**", "**/tests/**/*.rs", "**/tests.rs"]),
            build_config: vec![],
            linters: vec![],
            mutation_budget: 600,
            base_run_budget: 120,
            test_commands: vec![],
        }
    }
}

impl Settings {
    /// The settings in the gate.toml at `path`. Errs, naming `path` and, where there is one, the key, when the file
    /// cannot be read or is not TOML, or when it holds a key the gate does not know or a value of the wrong kind: a
    /// misspelt key must never leave the gate judging by the default.
    pub fn read(path: &Path) -> Result<Settings, Unjudged> {
        let shown = path.display();
        let source = std::fs::read_to_string(path)
            .map_err(|e| Unjudged(format!("cannot read the gate's settings {shown}: {e}")))?;
        let file: File = toml::from_str(&source)
            .map_err(|e| Unjudged(format!("cannot read the gate's settings {shown}: {}", e.to_string().trim_end())))?;
        let invalid = |why: String| Unjudged(format!("{shown}: {why}"));
        let mut settings = Settings::default();
        if let Some(files) = file.test_files {
            let globs = |g: Option<Vec<String>>, default: Globs| g.map_or(default, |g| Globs::new(&g));
            settings.python_tests = globs(files.python, settings.python_tests);
            settings.rust_tests = globs(files.rust, settings.rust_tests);
            settings.test_support = globs(files.support, settings.test_support);
            settings.golden = globs(files.golden, settings.golden);
        }
        if let Some(exempt) = file.complexity.and_then(|c| c.exempt) {
            settings.complexity_exempt = Globs::new(&exempt);
        }
        for (name, note) in file.build_config.map(|n| n.0).unwrap_or_default() {
            settings.build_config.push(build_config_entry(name, note).map_err(invalid)?);
        }
        for (name, globs) in file.linters.map(|l| l.0).unwrap_or_default() {
            settings.linters.push(linter_entry(name, &globs).map_err(invalid)?);
        }
        if let Some(budgets) = file.budgets {
            settings.mutation_budget = budget("mutation", budgets.mutation, settings.mutation_budget).map_err(invalid)?;
            settings.base_run_budget = budget("base-run", budgets.base_run, settings.base_run_budget).map_err(invalid)?;
        }
        for (k, test) in file.test.unwrap_or_default().into_iter().enumerate() {
            settings.test_commands.push(test.command(k).map_err(invalid)?);
        }
        Ok(settings)
    }

    /// The settings the gate judges the work tree at `root` by, and where they came from when that is not root's
    /// own gate.toml, for the quality file to say.
    ///
    /// They are those of the gate.toml at `given`, a path absolute or relative to the current directory, when given;
    /// else those of the gate.toml beside `rules`, the absolute path of the pyproject.toml the gate reads, which is
    /// root's own unless `--config` named another: a CI that judges a change by main's rules thereby judges it by
    /// main's settings too, never by the change's own. A gate.toml that is there but no file, such as a dangling
    /// link, is an error to read; with none there, the settings are the defaults.
    ///
    /// Where they came from is None when the settings are root's own gate.toml's, or the defaults for want of one;
    /// else the file as a code span, or "the defaults (there is no `path`)". Errs as [`Settings::read`] does.
    pub fn for_tree(root: &Path, rules: &Path, given: Option<&Path>) -> Result<(Settings, Option<String>), Unjudged> {
        let path = match given {
            Some(given) => std::path::absolute(given).unwrap_or_else(|_| given.to_path_buf()),
            None => rules.parent().unwrap_or(Path::new("/")).join(SETTINGS_FILE),
        };
        let there = given.is_some() || path.symlink_metadata().is_ok();
        let settings = if there { Settings::read(&path)? } else { Settings::default() };
        if tree_s_own(root, &path, SETTINGS_FILE) {
            return Ok((settings, None));
        }
        let shown = crate::text::path(path.as_os_str());
        Ok((settings, Some(if there { format!("`{shown}`") } else { format!("the defaults (there is no `{shown}`)") })))
    }

    /// The globs gate.toml gives the linter `name`, None when it gives none and the linter reads its own.
    pub fn linter_globs(&self, name: &str) -> Option<&Globs> {
        self.linters.iter().find(|(linter, _)| linter == name).map(|(_, globs)| globs)
    }
}

/// The `[build-config]` entry `name = note`, as [`Settings::build_config`] holds it; errs, with why, when `name` is
/// build configuration already or `note` is blank.
fn build_config_entry(name: String, note: String) -> Result<(String, String), String> {
    if BUILD_CONFIG_FILES.contains(&name.as_str()) || name == SETTINGS_FILE {
        return Err(format!("`build-config.{name}` is build configuration already, with a note of the gate's own"));
    }
    if crate::text::py_strip(&note).is_empty() {
        return Err(format!("`build-config.{name}` must be a sentence, the note the quality file gives the file"));
    }
    Ok((name, note))
}

/// The `[linters]` entry `name = globs`, as [`Settings::linters`] holds it; errs, with why, when `name` is not among
/// [`LINTER_NAMES`], or names clippy with globs that take in no Cargo.toml.
fn linter_entry(name: String, globs: &[String]) -> Result<(String, Globs), String> {
    if !LINTER_NAMES.contains(&name.as_str()) {
        return Err(format!("unknown key `linters.{name}`"));
    }
    // Clippy lints only the workspaces of the manifests among its files: globs that take in none would turn it off
    // as silently as a typo would.
    let last = |g: &String| g.rsplit('/').next().unwrap_or_default().to_string();
    if name == "clippy"
        && !globs.is_empty()
        && !globs.iter().any(|g| Globs::new(&[last(g)]).matches(OsStr::new("Cargo.toml")))
    {
        return Err(format!(
            "`linters.{name}` must take in the Cargo.toml of each workspace clippy lints, or be [] to turn it off"
        ));
    }
    Ok((name, Globs::new(globs)))
}

/// The seconds `budgets.{key}` gives, `default` when `seconds` is None as the key is left out; errs, with why, when
/// it gives fewer than 1.
fn budget(key: &str, seconds: Option<i64>, default: u64) -> Result<u64, String> {
    match seconds {
        None => Ok(default),
        Some(n) => u64::try_from(n)
            .ok()
            .filter(|&n| n >= 1)
            .ok_or_else(|| format!("`budgets.{key}` must be a whole number of seconds, at least 1, not {n}")),
    }
}

/// Whether `path`, an absolute path, names the file `name` at the top level of the work tree at `root`, also through
/// a symbolic link on either side.
pub fn tree_s_own(root: &Path, path: &Path, name: &str) -> bool {
    real_path(path) == real_path(&root.join(name))
}

/// gate.toml as TOML gives it, each key the gate knows and no other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    #[serde(rename = "test-files")]
    test_files: Option<TestFiles>,
    complexity: Option<Complexity>,
    #[serde(rename = "build-config")]
    build_config: Option<InOrder<String>>,
    linters: Option<InOrder<Vec<String>>>,
    budgets: Option<Budgets>,
    test: Option<Vec<TestEntry>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestFiles {
    python: Option<Vec<String>>,
    rust: Option<Vec<String>>,
    support: Option<Vec<String>>,
    golden: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Complexity {
    exempt: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Budgets {
    mutation: Option<i64>,
    #[serde(rename = "base-run")]
    base_run: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TestEntry {
    name: String,
    command: Vec<String>,
    #[serde(default)]
    junit: Vec<String>,
    #[serde(default)]
    coverage: Vec<String>,
}

impl TestEntry {
    /// The entry, the `k`th `[[test]]` from 0, as a command; errs with why it cannot be one, naming its key.
    fn command(self, k: usize) -> Result<TestCommand, String> {
        let key = format!("test[{k}]");
        if crate::text::py_strip(&self.name).is_empty() {
            return Err(format!("`{key}.name` must be a non-empty string"));
        }
        if self.command.is_empty() {
            return Err(format!("`{key}.command` must be a non-empty list of strings"));
        }
        for (r, paths) in [("junit", &self.junit), ("coverage", &self.coverage)] {
            // The gate removes each before the command runs, so one outside the tree would be a file of anyone's.
            if let Some(outside) = paths.iter().find(|p| !inside_tree(p)) {
                return Err(format!(
                    "`{key}.{r}` must name paths inside the tree, from its top level, not {}",
                    py_repr(outside)
                ));
            }
        }
        Ok(TestCommand { name: self.name, command: self.command, junit: self.junit, coverage: self.coverage })
    }
}

/// Whether `path` is relative and, normalized as Python's `os.path.normpath` would, does not climb out.
fn inside_tree(path: &str) -> bool {
    let path = Path::new(path);
    if path.is_absolute() {
        return false;
    }
    let mut depth = 0usize;
    for part in path.components() {
        match part {
            Component::ParentDir if depth == 0 => return false,
            Component::ParentDir => depth -= 1,
            Component::Normal(_) => depth += 1,
            _ => {}
        }
    }
    true
}

/// `text` as Python's `repr` writes a str without quotes or backslashes in it, which is what a gate.toml path is
/// in practice; otherwise near enough for a message.
fn py_repr(text: &str) -> String {
    if text.contains('\'') && !text.contains('"') {
        format!("\"{text}\"")
    } else {
        format!("'{}'", text.replace('\\', "\\\\").replace('\'', "\\'"))
    }
}

/// A TOML table read as its (key, value) pairs in the file's order, which a map type would lose.
struct InOrder<V>(Vec<(String, V)>);

impl<'de, V: Deserialize<'de>> Deserialize<'de> for InOrder<V> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Pairs<V>(std::marker::PhantomData<V>);
        impl<'de, V: Deserialize<'de>> Visitor<'de> for Pairs<V> {
            type Value = InOrder<V>;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a table")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<InOrder<V>, A::Error> {
                let mut pairs = Vec::new();
                while let Some((key, value)) = map.next_entry::<String, V>()? {
                    pairs.push((key, value));
                }
                Ok(InOrder(pairs))
            }
        }
        deserializer.deserialize_map(Pairs(std::marker::PhantomData))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The settings of a gate.toml holding `source`, or why the gate refuses it.
    fn read(source: &str) -> Result<Settings, String> {
        static N: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let n = N.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let path = std::env::temp_dir().join(format!("gate-settings-{}-{n}.toml", std::process::id()));
        std::fs::write(&path, source).unwrap();
        let read = Settings::read(&path).map_err(|Unjudged(why)| why.replace(&path.display().to_string(), "<path>"));
        std::fs::remove_file(&path).unwrap();
        read
    }

    #[test]
    fn the_repository_s_own_gate_toml_reads() {
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../gate.toml");
        let settings = Settings::read(&path).unwrap();
        let names: Vec<&str> = settings.test_commands.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["cargo-nextest", "cargo-nextest-gate", "pytest"]);
    }

    #[test]
    fn build_config_keeps_the_file_s_order() {
        let settings = read("[build-config]\nz = \"Z.\"\na = \"A.\"\n").unwrap();
        assert_eq!(settings.build_config, [("z".into(), "Z.".into()), ("a".into(), "A.".into())]);
    }

    #[test]
    fn unknown_keys_and_wrong_values_are_refused() {
        for (source, why) in [
            ("[test-file]\n", "unknown field `test-file`"),
            ("[test-files]\npyton = []\n", "unknown field `pyton`"),
            ("[linters]\nflake8 = []\n", "<path>: unknown key `linters.flake8`"),
            ("[budgets]\nmutation = 0\n", "<path>: `budgets.mutation` must be a whole number of seconds, at least 1"),
            ("[budgets]\nmutation = true\n", ""),
            ("[build-config]\n\"uv.lock\" = \"x\"\n", "<path>: `build-config.uv.lock` is build configuration already"),
            ("[build-config]\nx = \" \"\n", "<path>: `build-config.x` must be a sentence"),
            ("[linters]\nclippy = [\"**/*.rs\"]\n", "<path>: `linters.clippy` must take in the Cargo.toml"),
            ("[[test]]\nname = \"t\"\n", "missing field `command`"),
            ("[[test]]\nname = \"\"\ncommand = [\"x\"]\n", "<path>: `test[0].name` must be a non-empty string"),
            ("[[test]]\nname = \"t\"\ncommand = []\n", "<path>: `test[0].command` must be a non-empty list"),
            (
                "[[test]]\nname = \"t\"\ncommand = [\"x\"]\njunit = [\"a/../../j.xml\"]\n",
                "<path>: `test[0].junit` must name paths inside the tree, from its top level, not 'a/../../j.xml'",
            ),
        ] {
            let got = read(source).err().unwrap_or_else(|| panic!("{source:?} was read"));
            assert!(got.contains(why), "{source:?}: {got}");
        }
    }

    #[test]
    fn a_linter_takes_its_globs() {
        let settings = read("[linters]\nclippy = []\nshellcheck = [\"bin/*\"]\n").unwrap();
        assert!(settings.linter_globs("shellcheck").unwrap().matches(OsStr::new("bin/x")));
        assert!(settings.linter_globs("clippy").unwrap().globs().is_empty());
        assert!(settings.linter_globs("ruff").is_none());
    }
}
