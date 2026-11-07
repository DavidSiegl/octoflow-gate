//! The files a change edits that alter how the project is built or judged, which the quality file names for the
//! reviewer.

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::LazyLock;

use regex::bytes::Regex;

use crate::diff::ChangedLines;
use crate::report::{BUILD_CONFIG_FILES, SETTINGS_FILE};
use crate::settings::Settings;

/// The files clippy takes its rules or its toolchain from by name, wherever they are, and the names of cargo's
/// configuration in a `.cargo` directory or in CARGO_HOME.
const CLIPPY_CONFIG_NAMES: [&str; 5] =
    ["Cargo.toml", "clippy.toml", ".clippy.toml", "rust-toolchain", "rust-toolchain.toml"];
const CARGO_CONFIG_NAMES: [&str; 2] = ["config", "config.toml"];

/// The build configuration files `diff` changes, deletes or renames away, `changed` being its lines as
/// diff::changed_lines gives them: of BUILD_CONFIG_FILES, the gate.toml at the top level, and the files `settings`
/// adds, in that order.
pub fn edited_config(diff: &[u8], changed: &ChangedLines, settings: &Settings) -> Vec<OsString> {
    let touched = touched_paths(diff, changed);
    let added = settings.build_config.iter().map(|(path, _)| path.as_str());
    BUILD_CONFIG_FILES
        .into_iter()
        .chain([SETTINGS_FILE])
        .chain(added)
        .map(OsString::from)
        .filter(|f| touched.contains(f))
        .collect()
}

/// The files clippy takes its rules or its toolchain from that `diff` changes, deletes or renames away, in path
/// order. `cargo_home` is the directory, relative to the tree's top and "" for the top itself, of the CARGO_HOME
/// clippy runs with, as [`cargo_home_in`] gives it; None when that is outside the tree, so that no file of the tree
/// is cargo's configuration there.
pub fn clippy_config_files(diff: &[u8], changed: &ChangedLines, cargo_home: Option<&Path>) -> Vec<OsString> {
    let configures_clippy = |path: &OsString| {
        let path = Path::new(path);
        let name = path.file_name().unwrap_or_default();
        let dir = path.parent().unwrap_or(Path::new(""));
        CLIPPY_CONFIG_NAMES.iter().any(|n| name == OsStr::new(n))
            || (CARGO_CONFIG_NAMES.iter().any(|n| name == OsStr::new(n))
                && (dir.file_name() == Some(OsStr::new(".cargo")) || Some(dir) == cargo_home))
    };
    touched_paths(diff, changed).into_iter().filter(configures_clippy).collect()
}

/// The CARGO_HOME of `env`, the environment clippy runs with, relative to `root`, the tree's top, "" when it is
/// `root` itself; None when it is unset, empty or outside `root`. A relative CARGO_HOME is taken from `root`, though
/// cargo takes it from each workspace it runs in.
pub fn cargo_home_in(root: &Path, env: &[(OsString, OsString)]) -> Option<std::path::PathBuf> {
    let home = &env.iter().find(|(name, _)| name == "CARGO_HOME")?.1;
    if home.is_empty() {
        return None;
    }
    let real_root = crate::paths::real_path(root);
    let where_ = crate::paths::rel_path(&crate::paths::real_path(&root.join(home)), &real_root);
    match where_.components().next() {
        Some(std::path::Component::ParentDir) => None,
        _ if where_ == Path::new(".") => Some(Default::default()),
        _ => Some(where_),
    }
}

/// The paths `diff`, of `changed` as diff::changed_lines gives it, changes, deletes or renames away, as its `--- a/`
/// and `rename from ` lines name them, unquoted as gate.py leaves them.
fn touched_paths(diff: &[u8], changed: &ChangedLines) -> BTreeSet<OsString> {
    static FROM: LazyLock<Regex> =
        LazyLock::new(|| Regex::new(r"(?m-u)^(?:--- a/|rename from )([^\n]+?)\t?$").expect("a valid regex"));
    let from = FROM.captures_iter(diff).map(|m| OsStr::from_bytes(&m[1]).to_owned());
    changed.keys().cloned().chain(from).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIFF: &[u8] = b"diff --git a/uv.lock b/uv.lock\ndeleted file mode 100644\n--- a/uv.lock\n+++ /dev/null\n\
        diff --git a/.cargo/config.toml b/x\nrename from .cargo/config.toml\nrename to x\n\
        diff --git a/gate.toml b/gate.toml\n--- a/gate.toml\n+++ b/gate.toml\n@@ -1 +1 @@\n-a\n+b\n";

    #[test]
    fn edited_build_configuration_is_named_in_the_gate_s_order() {
        let changed = crate::diff::changed_lines(DIFF);
        let mut settings = Settings::default();
        settings.build_config.push(("x".into(), "Moved.".into()));
        assert_eq!(edited_config(DIFF, &changed, &settings), ["uv.lock", "gate.toml", "x"]);
    }

    #[test]
    fn clippy_s_configuration_is_named_by_name_and_place() {
        let changed = crate::diff::changed_lines(DIFF);
        assert_eq!(clippy_config_files(DIFF, &changed, None), [".cargo/config.toml"]);
        assert!(clippy_config_files(b"--- a/config\n", &Default::default(), None).is_empty());
        assert_eq!(clippy_config_files(b"--- a/config\n", &Default::default(), Some(Path::new(""))), ["config"]);
        assert!(clippy_config_files(b"--- a/.cargo/notes.md\n", &Default::default(), None).is_empty());
    }

    #[test]
    fn cargo_home_is_found_relative_to_the_tree_or_not_at_all() {
        let root = std::env::temp_dir().join(format!("gate-cargo-home-{}", std::process::id()));
        std::fs::create_dir_all(root.join("h")).unwrap();
        let env = |home: &str| [("PATH".into(), "/bin".into()), ("CARGO_HOME".into(), OsString::from(home))];
        assert_eq!(cargo_home_in(&root, &env("h")), Some("h".into()));
        assert_eq!(cargo_home_in(&root, &env(root.join("h").to_str().unwrap())), Some("h".into()));
        assert_eq!(cargo_home_in(&root, &env(".")), Some("".into()));
        assert_eq!(cargo_home_in(&root, &env("..")), None);
        assert_eq!(cargo_home_in(&root, &env("")), None);
        assert_eq!(cargo_home_in(&root, &[]), None);
        std::fs::remove_dir_all(&root).unwrap();
    }
}
