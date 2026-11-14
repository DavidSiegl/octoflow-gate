//! golangci-lint as one of the gate's linters (linters::LINTERS), over every Go module that a go.mod among the files
//! it is given makes, its tests included.
//!
//! Which linters it runs, and with what settings, is each module's own choice, the `.golangci.yml` (or `.yaml`,
//! `.toml`, `.json`) golangci-lint would find from the module's directory up, as clippy's lints are a workspace's: the
//! pyproject.toml the gate is given holds none of golangci-lint's. The gate names that configuration itself, the
//! nearest no higher than the tree's top, so that a file above the tree or in the home directory, which no change
//! shows, does not change how a change is judged ([`config_flags`]). The gate asks for every finding, none held back
//! as a repeat (`--max-issues-per-linter 0 --max-same-issues 0`), and keeps those on added lines. Code that does not
//! compile is a failed condition, as clippy's is, naming the type checker's first error: a change that deletes a
//! function an old line calls breaks the build on no line it added, and golangci-lint places such an error at the
//! package clause rather than where it is.
//!
//! It runs `golangci-lint run ./...` in each module's directory, within process::TOOL_TIMEOUT, which builds what the
//! packages need with the go tool, from its build and module caches, and may download the modules they use. Another
//! golangci-lint running with the same TMPDIR, where golangci-lint keeps its lock, as another gate's does on a CI
//! agent whose executors share one, makes it wait for that one's lock (`--allow-serial-runners`), its wait counted in
//! the timeout: golangci-lint otherwise exits with "parallel golangci-lint is running", a failed condition that says
//! nothing about the change.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use serde_json::Value;

use crate::linters::{tool_process, Tracked};
use crate::report::Finding;
use crate::text;

/// The linter golangci-lint reports a package that does not compile under, as it reports a lint under its own.
const TYPECHECK: &str = "typecheck";

/// golangci-lint's exit status when `./...` matches no Go file, as in a module whose code all lies in modules nested
/// in it (`NoGoFiles` in golangci-lint 2's `pkg/exitcodes`): nothing to lint, so nothing found.
const NO_GO_FILES: i32 = 5;

/// golangci-lint's findings over `files`, the tracked files it reads, in the tree whose top level is `root`, and the
/// failed conditions it caused, as linters::Linter::run says, `name` being the linter's. Of `files` the go.mod files
/// say which modules it lints, those the go tool would build ([`linted_module`]); without a Go source among them
/// there is nothing to lint, and golangci-lint does not run. A module with no Go file of its own, its code all in
/// modules nested in it, gives nothing.
pub fn run(name: &str, root: &Path, files: &[OsString], tracked: &Tracked) -> (Vec<Finding>, Vec<String>) {
    if !files.iter().any(|f| f.as_bytes().ends_with(b".go")) {
        return (vec![], vec![]);
    }
    let mut manifests: Vec<&OsString> = files.iter().filter(|f| linted_module(f)).collect();
    manifests.sort();
    let (mut found, mut problems) = (vec![], vec![]);
    for manifest in manifests {
        let (lints, problem) = lint_module(name, root, Path::new(manifest), tracked);
        found.extend(lints);
        problems.extend(problem);
    }
    (found, problems)
}

/// Whether `path`, from the tree's top, is a go.mod whose module golangci-lint lints: one outside the directories the
/// go tool leaves out of `./...`, as a module kept under `testdata` as a test's input is (lang_go::is_go).
fn linted_module(path: &OsStr) -> bool {
    let path = path.as_bytes();
    let dir = path.strip_suffix(b"go.mod").filter(|dir| dir.is_empty() || dir.ends_with(b"/"));
    dir.is_some_and(|dir| lang_go::is_go(&[dir, b"go.mod.go"].concat()))
}

/// The findings golangci-lint gives in the module whose go.mod is `manifest`, relative to `root`, and the failed
/// condition, None when its code compiled and golangci-lint ran as it should.
fn lint_module(name: &str, root: &Path, manifest: &Path, tracked: &Tracked) -> (Vec<Finding>, Option<String>) {
    let module = manifest.parent().unwrap_or(Path::new(""));
    let config = config_flags(root, module);
    let mut argv = [
        "golangci-lint",
        "run",
        "--allow-serial-runners",
        "--output.json.path",
        "stdout",
        "--show-stats=false",
        "--max-issues-per-linter",
        "0",
        "--max-same-issues",
        "0",
        "--issues-exit-code",
        "1",
        "--path-mode",
        "abs",
    ]
    .map(OsStr::new)
    .to_vec();
    argv.extend(config.iter().map(OsString::as_os_str));
    argv.push(OsStr::new("./..."));
    let done = match tool_process(name, &argv, &root.join(module)) {
        Err(problem) => return (vec![], Some(problem)),
        Ok(done) => done,
    };
    let named = text::path(manifest.as_os_str());
    // Exit status 1 means it found something, whatever the module's configuration asks for (--issues-exit-code),
    // NO_GO_FILES that there was nothing to lint, and any other but 0 that it failed; the JSON is its standard output's
    // last line, after anything a linter printed.
    let report = done.stdout.split(|&b| b == b'\n').rev().find_map(|line| serde_json::from_slice::<Value>(line).ok());
    let issues = report.as_ref().and_then(|r| r.get("Issues")).and_then(Value::as_array);
    let (exit, issues) = match (done.status, issues) {
        (0 | 1, Some(issues)) => (done.status, issues),
        (0, None) if done.stdout.trim_ascii().is_empty() => return (vec![], None),
        (NO_GO_FILES, _) => return (vec![], None),
        (exit, _) => {
            let said = done.said();
            return (vec![], Some(format!("`{name}` failed with exit status {exit} in `{named}`: {said}")));
        }
    };
    let (mut lints, mut compile_errors) = (vec![], vec![]);
    for issue in issues {
        match issue_finding(issue, root, tracked) {
            Some((finding, true)) => compile_errors.push(finding),
            Some((finding, false)) => lints.push(finding),
            None => {}
        }
    }
    if let Some(error) = compile_errors.first() {
        let at = format!("{}:{}", text::path(error.path.as_deref().unwrap_or_default()), error.line.unwrap_or(0));
        let what = format!("`{at}` {}: {}", error.rule, error.message);
        return (lints, Some(format!("`{name}` could not compile the module of `{named}`: {what}")));
    }
    if exit == 1 && lints.is_empty() {
        let said = done.said();
        return (vec![], Some(format!("`{name}` reported findings, but none the gate could read: {said}")));
    }
    (lints, None)
}

/// golangci-lint's names for its configuration, in the order it prefers them within one directory, as
/// `golangci-lint config path` shows of golangci-lint 2 (viper's `SupportedExts`).
const CONFIG_NAMES: [&str; 4] = [".golangci.json", ".golangci.toml", ".golangci.yaml", ".golangci.yml"];

/// The flags that give golangci-lint, run in `module`, a directory relative to `root`, the tree's top level, the
/// configuration it would find itself from the module's directory up, but none from above the tree or the home
/// directory, where no change shows it: `--config` with the nearest's path under `root`, or `--no-config`.
fn config_flags(root: &Path, module: &Path) -> Vec<OsString> {
    let nearest = module.ancestors().flat_map(|dir| CONFIG_NAMES.map(|n| root.join(dir).join(n))).find(|p| p.is_file());
    match nearest {
        Some(path) => vec!["--config".into(), path.into()],
        None => vec!["--no-config".into()],
    }
}

/// The finding `issue`, one of golangci-lint's JSON `Issues`, gives, an error with the linter's name as its rule, and
/// whether it is the type checker's, which says the code does not compile; None when it names no tracked file, or is
/// not of the shape golangci-lint 2 writes.
///
/// A type checker's message is its first line naming a place in the code, as `./calc.go:15:28: undefined: nope`, past
/// the line naming the package that golangci-lint puts first.
fn issue_finding(issue: &Value, root: &Path, tracked: &Tracked) -> Option<(Finding, bool)> {
    let linter = issue.get("FromLinter")?.as_str()?;
    let place = issue.get("Pos")?;
    let path = tracked.name_of(place.get("Filename")?.as_str()?.as_bytes(), root)?;
    let said = issue.get("Text")?.as_str()?;
    let compile_error = linter == TYPECHECK;
    let message = match compile_error {
        true => said.split('\n').map(text::py_strip).find(|l| !l.is_empty() && !l.starts_with(": #")).unwrap_or(said),
        false => said,
    };
    let finding = Finding {
        path: Some(path),
        line: u32::try_from(place.get("Line")?.as_u64()?).ok().filter(|&n| n > 0),
        severity: Some(issue.get("Severity").and_then(Value::as_str).unwrap_or_default())
            .filter(|s| !s.is_empty())
            .unwrap_or("error")
            .to_string(),
        rule: format!("golangci-lint {linter}"),
        message: message.to_string(),
    };
    Some((finding, compile_error))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::scratch::ScratchDir;

    #[test]
    fn the_configuration_is_the_nearest_inside_the_tree_in_golangci_lint_s_order() {
        let scratch = ScratchDir::new();
        let root = scratch.join("top");
        let module = Path::new("m/sub");
        std::fs::create_dir_all(root.join(module)).unwrap();
        let put = |path: &Path| std::fs::write(path, "version: \"2\"\n").unwrap();
        let flags = || config_flags(&root, module);
        put(&scratch.join(".golangci.yml"));
        assert_eq!(flags(), ["--no-config"]);
        put(&root.join(".golangci.yml"));
        assert_eq!(flags(), [OsString::from("--config"), root.join(".golangci.yml").into()]);
        put(&root.join("m/sub/.golangci.yml"));
        put(&root.join("m/.golangci.json"));
        assert_eq!(flags(), [OsString::from("--config"), root.join("m/sub/.golangci.yml").into()]);
        for name in [".golangci.yaml", ".golangci.toml", ".golangci.json"] {
            put(&root.join("m/sub").join(name));
            assert_eq!(flags(), [OsString::from("--config"), root.join("m/sub").join(name).into()]);
        }
    }

    #[test]
    fn a_module_is_linted_where_the_go_tool_builds_it() {
        for (path, linted) in [
            ("go.mod", true),
            ("tools/go.mod", true),
            ("testdata/go.mod", false),
            ("a/testdata/m/go.mod", false),
            ("vendor/x/go.mod", false),
            (".hidden/go.mod", false),
            ("_old/go.mod", false),
            ("notgo.mod", false),
            ("go.mod/x", false),
        ] {
            assert_eq!(linted_module(OsStr::new(path)), linted, "{path}");
        }
    }

    #[test]
    fn an_issue_is_read_with_its_linter_and_a_type_error_by_its_place_in_the_code() {
        let tracked = Tracked::new(vec!["m/calc.go".into()]);
        let issue = |linter: &str, text: &str, file: &str, line: u64| {
            json!({"FromLinter": linter, "Text": text, "Severity": "", "SourceLines": ["x"],
                   "Pos": {"Filename": file, "Offset": 0, "Line": line, "Column": 0}})
        };
        let read = |issue: Value| issue_finding(&issue, Path::new("/top"), &tracked);
        let (found, compile_error) = read(issue("errcheck", "Error return value is not checked", "/top/m/calc.go", 12))
            .unwrap();
        assert!(!compile_error);
        assert_eq!(
            found,
            Finding {
                path: Some("m/calc.go".into()),
                line: Some(12),
                severity: "error".into(),
                rule: "golangci-lint errcheck".into(),
                message: "Error return value is not checked".into(),
            }
        );
        let typecheck = issue("typecheck", ": # example.com/m [example.com/m.test]\n./calc.go:15:28: undefined: nope",
                              "/top/m/calc.go", 1);
        let (found, compile_error) = read(typecheck).unwrap();
        assert!(compile_error);
        assert_eq!(found.message, "./calc.go:15:28: undefined: nope");
        let mut graded = issue("revive", "m", "/top/m/calc.go", 3);
        graded["Severity"] = json!("warning");
        assert_eq!(read(graded).unwrap().0.severity, "warning");
        assert_eq!(read(issue("errcheck", "m", "/top/other.go", 3)), None);
        assert_eq!(read(json!({"FromLinter": "errcheck"})), None);
    }
}
