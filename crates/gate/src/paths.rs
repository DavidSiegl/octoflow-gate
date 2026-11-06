//! Paths as Python's os.path resolves them, which gate.py's judgement of paths rests on: `realpath`, which never
//! fails, and `relpath`.

use std::path::{Component, Path, PathBuf};

/// `path`, made absolute from the current directory, with every symbolic link resolved as far as the path exists and
/// `.` and `..` taken out, as Python's `os.path.realpath` gives it: the part past the first missing component is
/// appended as written, lexically normalized.
pub fn real_path(path: &Path) -> PathBuf {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let mut out = PathBuf::from("/");
    let mut parts = absolute.components().peekable();
    while let Some(part) = parts.next() {
        match part {
            Component::RootDir | Component::Prefix(_) | Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::Normal(name) => {
                let next = out.join(name);
                match std::fs::canonicalize(&next) {
                    Ok(resolved) => out = resolved,
                    Err(_) => {
                        out = next;
                        for rest in parts.by_ref() {
                            match rest {
                                Component::ParentDir => {
                                    out.pop();
                                }
                                Component::Normal(name) => out.push(name),
                                _ => {}
                            }
                        }
                    }
                }
            }
        }
    }
    out
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

    #[test]
    fn rel_path_climbs_and_descends() {
        assert_eq!(rel_path(Path::new("/a/b/c"), Path::new("/a")), Path::new("b/c"));
        assert_eq!(rel_path(Path::new("/a"), Path::new("/a/b")), Path::new(".."));
        assert_eq!(rel_path(Path::new("/a/x"), Path::new("/a/b")), Path::new("../x"));
        assert_eq!(rel_path(Path::new("/a"), Path::new("/a")), Path::new("."));
    }

    #[test]
    fn real_path_resolves_links_and_keeps_what_is_missing() {
        let dir = std::env::temp_dir().join(format!("gate-paths-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("real")).unwrap();
        let link = dir.join("link");
        let _ = std::fs::remove_file(&link);
        std::os::unix::fs::symlink(dir.join("real"), &link).unwrap();
        let real = std::fs::canonicalize(dir.join("real")).unwrap();
        assert_eq!(real_path(&link.join("missing/../file")), real.join("file"));
        assert_eq!(real_path(&link.join("../real/./x")), real.join("x"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
