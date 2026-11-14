//! Paths as Python's os.path resolves them, which the gate's judgement of paths rests on: `realpath`, which never
//! fails, `abspath`, `normpath` and `relpath`.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Component, Path, PathBuf};

/// `path`, made absolute from the current directory, with every symbolic link resolved and `.` and `..` taken out,
/// as Python's `os.path.realpath` gives it: a link is followed whether or not its target exists, a component that
/// is missing or no link is kept as written, and a link that leads back to itself is kept as written too.
pub fn real_path(path: &Path) -> PathBuf {
    /// What is left to resolve, last first: a name to append, or the mark that every name a link's target put on
    /// the stack has been resolved, so that the resolved path so far is what the link resolves to.
    enum Step {
        Name(OsString),
        LinkDone(PathBuf),
    }
    let names = |path: &Path| -> Vec<Step> {
        let parts = path.components().filter_map(|part| match part {
            Component::Normal(name) => Some(Step::Name(name.to_os_string())),
            Component::ParentDir => Some(Step::Name("..".into())),
            _ => None,
        });
        parts.collect::<Vec<_>>().into_iter().rev().collect()
    };
    // Not normalized: a `..` after a link climbs out of the link's target, so it waits for the link.
    let mut rest = names(&absolute(path));
    let mut out = PathBuf::from("/");
    // What each link met so far resolves to; None while it is being resolved, so that meeting it again is a loop.
    let mut links: HashMap<PathBuf, Option<PathBuf>> = HashMap::new();
    while let Some(step) = rest.pop() {
        let name = match step {
            Step::LinkDone(link) => {
                links.insert(link, Some(out.clone()));
                continue;
            }
            Step::Name(name) if name == ".." => {
                out.pop();
                continue;
            }
            Step::Name(name) => name,
        };
        let next = out.join(&name);
        if !std::fs::symlink_metadata(&next).is_ok_and(|m| m.file_type().is_symlink()) {
            out = next;
            continue;
        }
        if let Some(resolved) = links.get(&next) {
            out = resolved.clone().unwrap_or(next);
            continue;
        }
        let Ok(target) = std::fs::read_link(&next) else {
            out = next;
            continue;
        };
        if target.is_absolute() {
            out = PathBuf::from("/");
        }
        links.insert(next.clone(), None);
        rest.push(Step::LinkDone(next));
        rest.extend(names(&target));
    }
    out
}

/// `path` made absolute from the current directory and normalized as [`normpath`] does, as Python's
/// `os.path.abspath` gives it: no link is resolved, so `..` after a link climbs out of the link's name, not its
/// target. `path` as written, normalized, when the current directory cannot be read.
pub fn abs_path(path: &Path) -> PathBuf {
    normpath(&absolute(path))
}

/// `path` made absolute from the current directory, `..` kept; `path` as written when it is empty or the current
/// directory cannot be read.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// `path` with `.` and empty components taken out and each `..` cancelling the name before it, as Python's
/// `os.path.normpath` gives it, without looking at the file system: `..` at the root of an absolute path is dropped,
/// and at the start of a relative one kept. `.` when nothing is left.
pub fn normpath(path: &Path) -> PathBuf {
    let mut out: Vec<Component> = vec![];
    for part in path.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => match out.last() {
                Some(Component::Normal(_)) => {
                    out.pop();
                }
                Some(Component::RootDir) => {}
                _ => out.push(part),
            },
            _ => out.push(part),
        }
    }
    if out.is_empty() {
        return PathBuf::from(".");
    }
    out.into_iter().collect()
}

/// `path` relative to `start`, both absolute and normalized, as Python's `os.path.relpath` gives it: `..` for each
/// component of `start` the two do not share, then the rest of `path`; `.` when they are one.
pub fn rel_path(path: &Path, start: &Path) -> PathBuf {
    let path: Vec<Component> = path.components().collect();
    let start: Vec<Component> = start.components().collect();
    let shared = path.iter().zip(&start).take_while(|(a, b)| a == b).count();
    let mut out: PathBuf = start[shared..].iter().map(|_| Component::ParentDir).collect();
    out.extend(&path[shared..]);
    if out.as_os_str().is_empty() {
        out.push(".");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

    #[test]
    fn rel_path_climbs_and_descends() {
        assert_eq!(rel_path(Path::new("/a/b/c"), Path::new("/a")), Path::new("b/c"));
        assert_eq!(rel_path(Path::new("/a"), Path::new("/a/b")), Path::new(".."));
        assert_eq!(rel_path(Path::new("/a/x"), Path::new("/a/b")), Path::new("../x"));
        assert_eq!(rel_path(Path::new("/a"), Path::new("/a")), Path::new("."));
    }

    #[test]
    fn real_path_resolves_links_and_keeps_what_is_missing() {
        let dir = ScratchDir::new();
        std::fs::create_dir_all(dir.join("real")).unwrap();
        let link = dir.join("link");
        std::os::unix::fs::symlink(dir.join("real"), &link).unwrap();
        let real = std::fs::canonicalize(dir.join("real")).unwrap();
        assert_eq!(real_path(&link.join("missing/../file")), real.join("file"));
        assert_eq!(real_path(&link.join("../real/./x")), real.join("x"));
    }

    #[test]
    fn real_path_follows_a_link_whose_target_is_missing_and_stops_at_a_loop() {
        let dir = ScratchDir::new();
        std::fs::create_dir_all(dir.join("reports")).unwrap();
        std::os::unix::fs::symlink("reports/q.md", dir.join("q.md")).unwrap();
        std::os::unix::fs::symlink("../nope/x.md", dir.join("reports/far")).unwrap();
        std::os::unix::fs::symlink("loop", dir.join("loop")).unwrap();
        assert_eq!(real_path(&dir.join("q.md")), dir.join("reports/q.md"));
        assert_eq!(real_path(&dir.join("reports/far")), dir.join("nope/x.md"));
        assert_eq!(real_path(&dir.join("loop/x")), dir.join("loop/x"));
    }

    #[test]
    fn real_path_climbs_out_of_a_link_s_target_not_its_name() {
        let dir = ScratchDir::new();
        std::fs::create_dir_all(dir.join("elsewhere/sub")).unwrap();
        std::fs::create_dir_all(dir.join("here")).unwrap();
        std::os::unix::fs::symlink(dir.join("elsewhere/sub"), dir.join("here/link")).unwrap();
        assert_eq!(real_path(&dir.join("here/link/../x")), dir.join("elsewhere/x"));
    }

    #[test]
    fn normpath_is_python_s() {
        for (path, normal) in [
            ("", "."),
            (".", "."),
            ("a/./b/", "a/b"),
            ("a/../..", ".."),
            ("../a/../b", "../b"),
            ("/../a", "/a"),
            ("/a/b/../../..", "/"),
            ("a/..", "."),
        ] {
            assert_eq!(normpath(Path::new(path)), Path::new(normal), "{path:?}");
        }
        let cwd = std::env::current_dir().unwrap();
        assert_eq!(abs_path(Path::new("../x/./y")), cwd.parent().unwrap().join("x/y"));
    }
}
