//! How the gate reads the project it judges, as its gate.toml says (docs/quality-gate.md, "The gate's settings").

use std::ffi::OsStr;
use std::fmt;
use std::path::{Component, Path, PathBuf};

use serde::de::{Deserializer, MapAccess, Visitor};
use serde::Deserialize;

use crate::glob::Globs;
use crate::paths::real_path;
use crate::report::{BUILD_CONFIG_FILES, RULES_FILE, SETTINGS_FILE};
use crate::Unjudged;

/// The linters gate.toml may choose the files of, each a name its `[linters]` table may hold.
pub const LINTER_NAMES: [&str; 7] =
    ["ruff", "mypy", "pymarkdown", "shellcheck", "actionlint", "clippy", "golangci-lint"];

/// The settings a project is judged by: [`Settings::default`] for one without gate.toml, else what its gate.toml
/// says, a default for each key it leaves out.
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
    /// Files the dependency-cycles check leaves out of every cycle, their imports and the imports of them both.
    pub cycles_exempt: Globs,
    /// Files the duplicated-blocks check neither judges nor matches a block against.
    pub duplicates_exempt: Globs,
    /// The fewest tokens a block the duplicated-blocks check reports may hold; at least 1.
    pub duplicates_min_tokens: usize,
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
    /// The judged project's Python, which `--python` replaces: a path, absolute or relative to the tree's top level,
    /// or a name without a slash, which PATH finds. None for `python3`. Never blank.
    pub python: Option<String>,
}

/// A command that runs the project's tests, one `[[test]]` entry of gate.toml, which the gate runs and whose reports
/// it judges (tests.rs); `--tests` or `--coverage` replace all of them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TestCommand {
    /// What findings about the command call it.
    pub name: String,
    /// The argv, run in the tree's top level; never through a shell. Never empty.
    pub command: Vec<String>,
    /// The JUnit reports and the coverage reports (Cobertura or LCOV, coverage::new_coverage) it writes, paths inside
    /// the tree from its top level.
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
            cycles_exempt: Globs::new(&[
                "**/tests/**",
                "**/tests.rs",
                "**/test_*.py",
                "**/*_test.py",
                "**/conftest.py",
            ]),
            duplicates_exempt: Globs::new(&[
                "**/tests/**",
                "**/tests.rs",
                "**/test_*.py",
                "**/*_test.py",
                "**/conftest.py",
                "**/*_test.go",
                "**/testdata/**",
                "**/fixtures/**",
            ]),
            duplicates_min_tokens: 50,
            build_config: vec![],
            linters: vec![],
            mutation_budget: 600,
            base_run_budget: 120,
            test_commands: vec![],
            python: None,
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
        if file.python.as_deref().is_some_and(|python| crate::text::py_strip(python).is_empty()) {
            return Err(invalid("`python` must be the path or the name of a Python".into()));
        }
        settings.python = file.python;
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
        if let Some(exempt) = file.dependency_cycles.and_then(|c| c.exempt) {
            settings.cycles_exempt = Globs::new(&exempt);
        }
        if let Some(duplicates) = file.duplicated_blocks {
            if let Some(exempt) = duplicates.exempt {
                settings.duplicates_exempt = Globs::new(&exempt);
            }
            if let Some(n) = duplicates.min_tokens {
                settings.duplicates_min_tokens = usize::try_from(n).ok().filter(|&n| n >= 1).ok_or_else(|| {
                    invalid(format!("`duplicated-blocks.min-tokens` must be a whole number, at least 1, not {n}"))
                })?;
            }
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

    /// The globs gate.toml gives the linter `name`, None when it gives none and the linter reads its own.
    pub fn linter_globs(&self, name: &str) -> Option<&Globs> {
        self.linters.iter().find(|(linter, _)| linter == name).map(|(_, globs)| globs)
    }
}

/// What the work tree at a root is judged by, as [`JudgedBy::for_tree`] finds it: the gate's settings, and the rules
/// of the tools that take theirs from a pyproject.toml, which are ruff, mypy, pymarkdown and mutmut.
#[derive(Clone, Debug)]
pub struct JudgedBy {
    pub settings: Settings,
    /// Where the settings came from when that is not the root's own gate.toml, for the quality file to say: the
    /// file as a code span, or "the defaults (there is no `path`)". None for the root's own, or the defaults for
    /// want of it.
    pub settings_from: Option<String>,
    /// The pyproject.toml that holds the rules, an absolute path to a file; None when there is none, and each tool
    /// runs on its defaults, whatever configuration the tree holds.
    pub rules: Option<PathBuf>,
    /// Where the rules came from when that is not the root's own pyproject.toml, as `settings_from` says it. None
    /// for the root's own, or the defaults for want of it.
    pub rules_from: Option<String>,
}

impl JudgedBy {
    /// What the gate judges the work tree at `root` by.
    ///
    /// The settings are those of the gate.toml at `given`, a path absolute or relative to the current directory,
    /// which `--config` names; without one, those of root's own gate.toml, or the defaults when it has none. The
    /// rules are the pyproject.toml in the directory of that gate.toml, as the path names it and not where a link
    /// leads, so root's own unless `--config` names a gate.toml elsewhere: a CI that judges a change by main's
    /// settings thereby judges it by main's rules too, never by the change's own.
    ///
    /// Errs as [`Settings::read`] does, also for a `given` that is not there and for a gate.toml of root's that is
    /// there but no file, such as a dangling link; and when the pyproject.toml is there but no file.
    pub fn for_tree(root: &Path, given: Option<&Path>) -> Result<JudgedBy, Unjudged> {
        let path = given.map_or_else(|| root.join(SETTINGS_FILE), crate::paths::abs_path);
        let there = given.is_some() || path.symlink_metadata().is_ok();
        let settings = if there { Settings::read(&path)? } else { Settings::default() };
        let rules = path.parent().unwrap_or(Path::new("/")).join(RULES_FILE);
        let ruled = rules.symlink_metadata().is_ok();
        if ruled && !rules.is_file() {
            return Err(Unjudged(format!("no rules: {} is not a file", crate::text::path(rules.as_os_str()))));
        }
        Ok(JudgedBy {
            settings,
            settings_from: from_elsewhere(root, &path, SETTINGS_FILE, there),
            rules_from: from_elsewhere(root, &rules, RULES_FILE, ruled),
            rules: ruled.then_some(rules),
        })
    }
}

/// Where what the gate reads at `path`, an absolute path, came from, as [`JudgedBy`]'s `settings_from` says it: None
/// when `path` is the file `name` at the top level of the work tree at `root`; else `path` as a code span when it is
/// `there`, or the defaults taken for want of it.
fn from_elsewhere(root: &Path, path: &Path, name: &str, there: bool) -> Option<String> {
    if tree_s_own(root, path, name) {
        return None;
    }
    let shown = crate::text::path(path.as_os_str());
    Some(if there { format!("`{shown}`") } else { format!("the defaults (there is no `{shown}`)") })
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
/// [`LINTER_NAMES`], or names clippy with globs that take in no Cargo.toml, or golangci-lint with globs that take in
/// no go.mod.
fn linter_entry(name: String, globs: &[String]) -> Result<(String, Globs), String> {
    if !LINTER_NAMES.contains(&name.as_str()) {
        return Err(format!("unknown key `linters.{name}`"));
    }
    // Clippy lints only the workspaces of the manifests among its files, and golangci-lint the modules: globs that
    // take in none would turn it off as silently as a typo would.
    let (manifest, project) = match name.as_str() {
        "clippy" => ("Cargo.toml", "workspace"),
        "golangci-lint" => ("go.mod", "module"),
        _ => return Ok((name, Globs::new(globs))),
    };
    let last = |g: &String| g.rsplit('/').next().unwrap_or_default().to_string();
    if !globs.is_empty() && !globs.iter().any(|g| Globs::new(&[last(g)]).matches(OsStr::new(manifest))) {
        return Err(format!(
            "`linters.{name}` must take in the {manifest} of each {project} {name} lints, or be [] to turn it off"
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
fn tree_s_own(root: &Path, path: &Path, name: &str) -> bool {
    real_path(path) == real_path(&root.join(name))
}

/// gate.toml as TOML gives it, each key the gate knows and no other.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct File {
    python: Option<String>,
    #[serde(rename = "test-files")]
    test_files: Option<TestFiles>,
    complexity: Option<Exempt>,
    #[serde(rename = "dependency-cycles")]
    dependency_cycles: Option<Exempt>,
    #[serde(rename = "duplicated-blocks")]
    duplicated_blocks: Option<DuplicatedBlocks>,
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

/// A check's table that holds only the globs of the files the check leaves alone, as `[complexity]`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Exempt {
    exempt: Option<Vec<String>>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DuplicatedBlocks {
    exempt: Option<Vec<String>>,
    #[serde(rename = "min-tokens")]
    min_tokens: Option<i64>,
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
    !path.is_absolute() && crate::paths::normpath(path).components().next() != Some(Component::ParentDir)
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
        assert_eq!(names, ["cargo-nextest"]);
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
            ("python = \" \"\n", "<path>: `python` must be the path or the name of a Python"),
            ("python = 3\n", ""),
            ("[test-files]\npyton = []\n", "unknown field `pyton`"),
            ("[linters]\nflake8 = []\n", "<path>: unknown key `linters.flake8`"),
            ("[budgets]\nmutation = 0\n", "<path>: `budgets.mutation` must be a whole number of seconds, at least 1"),
            ("[budgets]\nmutation = true\n", ""),
            ("[duplicated-blocks]\nmin-tokens = 0\n", "<path>: `duplicated-blocks.min-tokens` must be a whole number"),
            ("[duplicated-blocks]\nmin_tokens = 30\n", "unknown field `min_tokens`"),
            ("[build-config]\n\"uv.lock\" = \"x\"\n", "<path>: `build-config.uv.lock` is build configuration already"),
            ("[build-config]\nx = \" \"\n", "<path>: `build-config.x` must be a sentence"),
            ("[linters]\nclippy = [\"**/*.rs\"]\n", "<path>: `linters.clippy` must take in the Cargo.toml"),
            ("[linters]\ngolangci-lint = [\"**/*.go\"]\n", "<path>: `linters.golangci-lint` must take in the go.mod"),
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
    fn the_python_is_the_one_named_or_none() {
        assert_eq!(read("python = \".venv/bin/python\"\n").unwrap().python.as_deref(), Some(".venv/bin/python"));
        assert_eq!(read("").unwrap().python, None);
    }

    #[test]
    fn a_linter_takes_its_globs() {
        let settings = read("[linters]\nclippy = []\nshellcheck = [\"bin/*\"]\n").unwrap();
        assert!(settings.linter_globs("shellcheck").unwrap().matches(OsStr::new("bin/x")));
        assert!(settings.linter_globs("clippy").unwrap().globs().is_empty());
        assert!(settings.linter_globs("ruff").is_none());
    }

    #[test]
    fn duplicated_blocks_takes_its_globs_and_its_fewest_tokens() {
        let settings = read("[duplicated-blocks]\nexempt = [\"gen/**\"]\nmin-tokens = 30\n").unwrap();
        assert!(settings.duplicates_exempt.matches(OsStr::new("gen/a.py")));
        assert!(!settings.duplicates_exempt.matches(OsStr::new("tests/test_a.py")));
        assert_eq!(settings.duplicates_min_tokens, 30);
        assert_eq!(read("").unwrap().duplicates_min_tokens, 50);
    }
}
