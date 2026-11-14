//! cargo clippy as one of the gate's linters (linters::LINTERS), over every Cargo workspace that a manifest among the
//! files it is given belongs to, all its targets checked, tests and examples included.
//!
//! Which lints are on is each workspace's own choice, the `[lints]` tables of its manifests and its clippy.toml: the
//! pyproject.toml the gate is given holds none of clippy's. The gate caps every lint of the workspace's own crates
//! at warn (rustc's `--cap-lints`), so that a lint at deny or forbid, which would fail its crate's build and stop
//! cargo before the crates after it, is a finding like the rest: every finding is a warning, and the gate keeps
//! those on added lines. The cap also lets an allow override a forbid, which rustc otherwise refuses (E0453), so
//! such an allow silences its lint here; it is listed under the suppressions added only where its added line holds
//! the allow itself, not a lint added to an existing allow wrapped over lines. That gap is left open: this repository
//! sets no lint to forbid, and a test command that builds uncapped, as `cargo llvm-cov nextest` does, still fails such
//! a workspace with E0453. A build that still fails is a failed condition, naming the compiler's first error in
//! a tracked file, wherever it is, else cargo's own: a change that deletes a function an old line calls breaks the
//! build on no line it added.
//!
//! The cargo commands it runs, each within process::TOOL_TIMEOUT: `cargo clippy --version`, whose failure is clippy
//! missing; `cargo locate-project` for each Cargo.toml among the files it is given; and `cargo clippy` in each
//! workspace, which builds what it needs to (target/ in the workspace) and may fetch the crates it uses. That last
//! runs with `--locked` when git tracks the workspace's Cargo.lock, so that it judges the versions the change commits
//! and never rewrites the file; without one, cargo writes an untracked Cargo.lock beside the manifest (a workspace that
//! commits no lock file has cargo resolve one afresh, which `--locked` refuses). passes-at-base follows the same rule.

use std::collections::{BTreeSet, HashSet};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::linters::{tool_process, Tracked};
use crate::paths::{normpath, rel_path};
use crate::report::Finding;
use crate::text;

/// clippy's findings over `files`, the tracked files it reads, in the tree whose top level is `root`, and the failed
/// conditions it caused, as linters::Linter::run says, `name` being the linter's. Of `files` only the manifests play
/// a part, by which clippy reads crates; without a Rust source among them there is nothing to lint, and cargo does
/// not run. A finding --all-targets reports twice, for a library checked as itself and as its tests, is kept once.
pub fn run(name: &str, root: &Path, files: &[OsString], tracked: &Tracked) -> (Vec<Finding>, Vec<String>) {
    if !files.iter().any(|f| f.as_bytes().ends_with(b".rs")) {
        return (vec![], vec![]);
    }
    let version = ["cargo", "clippy", "--version"].map(OsStr::new);
    match tool_process(name, &version, root) {
        Err(problem) => return (vec![], vec![problem]),
        Ok(done) if done.status != 0 => {
            let failed = format!("`cargo clippy --version` failed with exit status {}", done.status);
            return (vec![], vec![format!("`{name}` is not installed: {failed}: {}", done.said())]);
        }
        Ok(_) => {}
    }
    let (manifests, mut problems) = workspaces(name, root, files);
    let mut found = vec![];
    for manifest in manifests {
        let (lints, problem) = lint_workspace(name, root, &manifest, tracked);
        found.extend(lints);
        problems.extend(problem);
    }
    let mut seen = HashSet::new();
    found.retain(|f| seen.insert(f.clone()));
    (found, problems)
}

/// The root manifest of each Cargo workspace that a Cargo.toml among `files`, paths relative to `root`, the tree's
/// top level, belongs to, relative to `root`, in path order; and a failed condition, naming the linter `name`, for
/// each manifest whose workspace cargo cannot tell.
pub(crate) fn workspaces(name: &str, root: &Path, files: &[OsString]) -> (Vec<PathBuf>, Vec<String>) {
    let mut manifests: Vec<&OsString> =
        files.iter().filter(|f| Path::new(f).file_name() == Some(OsStr::new("Cargo.toml"))).collect();
    manifests.sort();
    let (mut roots, mut problems) = (BTreeSet::new(), vec![]);
    for manifest in manifests {
        let mut argv = ["cargo", "locate-project", "--workspace", "--message-format", "plain", "--manifest-path"]
            .map(OsStr::new)
            .to_vec();
        argv.push(manifest);
        match tool_process(name, &argv, root) {
            Err(problem) => problems.push(problem),
            Ok(done) if done.status != 0 => problems
                .push(format!("`{name}` could not find the workspace of `{}`: {}", text::path(manifest), done.said())),
            Ok(done) => {
                let printed = OsStr::from_bytes(done.stdout.trim_ascii());
                roots.insert(rel_path(&normpath(&root.join(printed)), &normpath(root)));
            }
        }
    }
    (roots.into_iter().collect(), problems)
}

/// The lints clippy finds in the workspace whose root manifest is `manifest`, relative to `root`, and the failed
/// condition, None when it compiled and clippy ran as it should.
fn lint_workspace(name: &str, root: &Path, manifest: &Path, tracked: &Tracked) -> (Vec<Finding>, Option<String>) {
    let workspace = manifest.parent().unwrap_or(Path::new(""));
    let mut argv =
        ["cargo", "clippy", "--workspace", "--all-targets", "--message-format=json"].map(OsStr::new).to_vec();
    let lock = workspace.join("Cargo.lock");
    if tracked.listed.iter().any(|p| p == lock.as_os_str()) {
        argv.push(OsStr::new("--locked"));
    }
    argv.extend(["--", "--cap-lints", "warn"].map(OsStr::new));
    let done = match tool_process(name, &argv, &root.join(workspace)) {
        Err(problem) => return (vec![], Some(problem)),
        Ok(done) => done,
    };
    let found: Vec<Finding> =
        done.stdout.split(|&b| b == b'\n').filter_map(|line| diagnostic(line, workspace, root, tracked)).collect();
    // Capped at warn, a lint never fails the build, so each error is the compiler's.
    let (errors, lints): (Vec<Finding>, Vec<Finding>) = found.into_iter().partition(|f| f.severity == "error");
    if done.status == 0 {
        return (lints, None);
    }
    let manifest = text::path(manifest.as_os_str());
    if let Some(error) = errors.first() {
        let at = format!("{}:{}", text::path(error.path.as_deref().unwrap_or_default()), error.line.unwrap_or(0));
        let what = format!("`{at}` error {}: {}", error.rule, error.message);
        return (lints, Some(format!("`{name}` could not compile the workspace of `{manifest}`: {what}")));
    }
    let stderr = text::lossless(&done.stderr);
    let said = match stderr.split('\n').find(|l| l.starts_with("error")) {
        Some(line) => text::py_strip(line).to_string(),
        None => done.said(),
    };
    let said: String = said.chars().take(200).collect();
    (lints, Some(format!("`{name}` failed with exit status {} in `{manifest}`: {said}", done.status)))
}

/// The finding in `line`, one line of `cargo --message-format=json`'s output, at the compiler's level, "error" or
/// "warning", and with the lint's name or the error's code as its rule; None for any other line, such as a note, a
/// summary with no place in the code, or a finding in a file git does not track.
///
/// `workspace` is the workspace's directory relative to `root`, which the paths cargo prints are relative to. A
/// finding inside a macro's definition is placed where the macro was used, at the first expansion that is in a
/// tracked file. Anything that is not a record of the shape
/// <https://doc.rust-lang.org/cargo/reference/external-tools.html> gives, such as a line a build script printed,
/// holds no finding.
fn diagnostic(line: &[u8], workspace: &Path, root: &Path, tracked: &Tracked) -> Option<Finding> {
    let record: Value = serde_json::from_slice(line).ok()?;
    if record.get("reason")?.as_str()? != "compiler-message" {
        return None;
    }
    let message = record.get("message")?;
    let level = message.get("level")?.as_str()?;
    if level != "error" && level != "warning" {
        return None;
    }
    let spans = message.get("spans")?.as_array()?;
    let mut span = spans.iter().find(|s| s.get("is_primary").and_then(Value::as_bool) == Some(true));
    let path = loop {
        let file = span?.get("file_name")?.as_str()?;
        if let Some(path) = tracked.name_of(workspace.join(file).as_os_str().as_bytes(), root) {
            break path;
        }
        span = span?.get("expansion").and_then(|e| e.get("span")).filter(|s| !s.is_null());
    };
    let code = message.get("code").and_then(|c| c.get("code")).and_then(Value::as_str);
    Some(Finding {
        path: Some(path),
        line: u32::try_from(span?.get("line_start")?.as_u64()?).ok(),
        severity: level.to_string(),
        rule: code.map_or_else(|| "clippy".to_string(), |code| format!("clippy {code}")),
        message: message.get("message")?.as_str()?.to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tracked(names: &[&str]) -> Tracked {
        Tracked::new(names.iter().map(OsString::from).collect())
    }

    #[test]
    fn a_diagnostic_is_read_from_its_primary_span_or_the_expansion_in_a_tracked_file() {
        let tracked = tracked(&["crate/src/lib.rs"]);
        let read = |line: &str| diagnostic(line.as_bytes(), Path::new("crate"), Path::new("/top"), &tracked);
        let lint = r#"{"reason":"compiler-message","message":{"level":"warning","message":"unneeded `return`",
            "code":{"code":"clippy::needless_return"},
            "spans":[{"is_primary":false,"file_name":"src/lib.rs","line_start":1},
                     {"is_primary":true,"file_name":"src/lib.rs","line_start":5}]}}"#;
        let found = read(&lint.replace('\n', "")).unwrap();
        assert_eq!(
            (found.path.unwrap(), found.line, found.severity.as_str(), found.rule.as_str()),
            (OsString::from("crate/src/lib.rs"), Some(5), "warning", "clippy clippy::needless_return")
        );
        assert_eq!(found.message, "unneeded `return`");
        let in_macro = r#"{"reason":"compiler-message","message":{"level":"error","message":"m","code":null,"spans":[
            {"is_primary":true,"file_name":"/rustlib/core/src/macros.rs","line_start":9,
             "expansion":{"span":{"file_name":"src/lib.rs","line_start":3}}}]}}"#;
        let found = read(&in_macro.replace('\n', "")).unwrap();
        assert_eq!((found.line, found.rule.as_str()), (Some(3), "clippy"));
        let untracked = r#"{"reason":"compiler-message","message":{"level":"warning","message":"m",
            "spans":[{"is_primary":true,"file_name":"build.rs","line_start":1}]}}"#;
        assert_eq!(read(&untracked.replace('\n', "")), None);
        let note = r#"{"reason":"compiler-message","message":{"level":"note","message":"m",
            "spans":[{"is_primary":true,"file_name":"src/lib.rs","line_start":1}]}}"#;
        assert_eq!(read(&note.replace('\n', "")), None);
        for other in [r#"{"reason":"build-finished","success":true}"#, "warning: printed by a build script", "[]"] {
            assert_eq!(read(other), None, "{other}");
        }
    }
}
