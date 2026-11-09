//! The copies of a tree the gate runs a project's tools in, so that nothing they build or write lands in the tree it
//! judges: a scratch directory of the gate's own, a copy of the working tree's tracked files, and a copy of the base
//! with the change's tests in it.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::judge::Change;
use crate::report::Finding;
use crate::{git, text, Unjudged};

/// The findings of `run(dir)`, `dir` a new empty directory only the gate's account can enter, which is removed
/// afterwards whatever `run` finds; `purpose` says what it is for, as a clause such as "to run cargo-mutants in".
/// When the directory cannot be made, `run` is not called, and the one finding is of `rule`, of the check whose run it
/// is, saying so; when it cannot be removed, a `not-removed` finding naming it follows `run`'s. The findings' rules
/// are the check's own, without its name. gate.py's `_in_scratch_directory`.
pub fn in_scratch_directory(rule: &str, purpose: &str, run: impl FnOnce(&Path) -> Vec<Finding>) -> Vec<Finding> {
    let dir = match scratch_directory(&std::env::temp_dir()) {
        Ok(dir) => dir,
        Err(e) => {
            let why = format!("could not make a directory {purpose}: {e}");
            return vec![finding(None, None, rule, format!("{why}; nothing ran, so the check judged nothing."))];
        }
    };
    let mut out = run(&dir);
    if let Err(e) = std::fs::remove_dir_all(&dir) {
        let message = format!("could not remove the directory {purpose}, `{}`: {e}", dir.display());
        out.push(finding(None, None, "not-removed", message));
    }
    out
}

/// A finding of a check beside the linters, as its find gives it: `rule` its own, without the check's name, which
/// the gate puts before it, and its severity "warning", which the gate replaces with what it does with it.
pub fn finding(path: Option<&OsStr>, line: Option<u32>, rule: &str, message: String) -> Finding {
    Finding { path: path.map(OsStr::to_os_string), line, severity: "warning".into(), rule: rule.into(), message }
}

/// A new directory, mode 0700, in `parent`, named for the gate's process.
fn scratch_directory(parent: &Path) -> std::io::Result<PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    use std::sync::atomic::{AtomicU32, Ordering};
    static MADE: AtomicU32 = AtomicU32::new(0);
    loop {
        let name = format!("gate-{}-{}", std::process::id(), MADE.fetch_add(1, Ordering::SeqCst));
        let dir = parent.join(name);
        match std::fs::DirBuilder::new().mode(0o700).create(&dir) {
            // Left by an earlier gate of the same process id.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            made => return made.map(|()| dir),
        }
    }
}

/// Copies the files git tracks in the working tree at `root`, as they are there, to the same paths under `dest`, a
/// directory it makes, with its parents. A symbolic link is copied as a link and a submodule as its directory; a
/// tracked file missing from the working tree is left out. Errs, a clause, when git cannot list the files, `dest`
/// exists or a file cannot be copied; `dest` may then hold part of the copy. gate.py's `_copy_tracked`.
pub fn copy_tracked(root: &Path, dest: &Path) -> Result<(), String> {
    let listed = git::output(root, &["ls-files", "-z"]).map_err(|Unjudged(e)| e)?;
    let mut names: Vec<&[u8]> = listed.split(|&b| b == 0).filter(|n| !n.is_empty()).collect();
    names.sort();
    names.dedup();
    std::fs::create_dir_all(dest.parent().unwrap_or(Path::new("/"))).map_err(|e| failed("make", dest, &e))?;
    std::fs::create_dir(dest).map_err(|e| failed("make", dest, &e))?;
    for name in names {
        let (src, dst) = (root.join(OsStr::from_bytes(name)), dest.join(OsStr::from_bytes(name)));
        if src.symlink_metadata().is_err() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| failed("make", parent, &e))?;
        }
        copy(&src, &dst)?;
    }
    Ok(())
}

/// Copies `src` to `dst`, which must not exist: a symbolic link as a link, a directory with all it holds, a file with
/// its permissions. Errs on anything else, as a named pipe, whose reading could block the gate for good, as gate.py's
/// `shutil.copy2` refuses one.
fn copy(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = src.symlink_metadata().map_err(|e| failed("read", src, &e))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(src).map_err(|e| failed("read", src, &e))?;
        std::os::unix::fs::symlink(target, dst).map_err(|e| failed("write", dst, &e))
    } else if meta.is_dir() {
        std::fs::create_dir(dst).map_err(|e| failed("make", dst, &e))?;
        for entry in std::fs::read_dir(src).map_err(|e| failed("read", src, &e))? {
            let entry = entry.map_err(|e| failed("read", src, &e))?;
            copy(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else if meta.is_file() {
        std::fs::copy(src, dst).map(|_| ()).map_err(|e| failed("copy", src, &e))
    } else {
        Err(format!("cannot copy {}: it is no regular file, link or directory", text::path(src.as_os_str())))
    }
}

/// Why a file operation failed, as a clause: "cannot `what` `path`: the system's words".
fn failed(what: &str, path: &Path, e: &std::io::Error) -> String {
    format!("cannot {what} {}: {e}", text::path(path.as_os_str()))
}

/// Writes the files git tracked at `change.base` to `dest`, a directory it makes, through a tar file at `archive`,
/// then replaces each file of the change's that `beside` accepts, a repository path that belongs with the tests
/// rather than with the code they test, with the working tree's: removed at its path at the base, and copied, a
/// symbolic link as a link, to its path in the working tree. A symbolic link is written as it is, wherever it points,
/// and no file is written or removed through one: a link on the way to a replaced file is replaced by a directory, as
/// the working tree has one there. A submodule holds the files of the commit the base records for it, as
/// [`unpack_commit`] writes them.
///
/// Gives back the submodules left empty, as [`unpack_commit`] does. Errs, a clause, when git cannot list or write a
/// commit, `tar` cannot unpack it, `dest` exists or a file cannot be written. gate.py's `_copy_base_with_tests`.
pub fn copy_base_with_tests(
    change: &Change,
    dest: &Path,
    archive: &Path,
    beside: impl Fn(&OsStr) -> bool,
) -> Result<Vec<(String, String)>, String> {
    std::fs::create_dir(dest).map_err(|e| failed("make", dest, &e))?;
    let empty = unpack_commit(&change.root, &change.base, dest, archive, "")?;
    // A path the base tracks has no link above it there.
    for old in change.files.iter().filter_map(|f| f.before.as_ref()).filter(|p| beside(p)) {
        let path = dest.join(old);
        if path.symlink_metadata().is_ok() {
            std::fs::remove_file(&path).map_err(|e| failed("remove", &path, &e))?;
        }
    }
    for new in change.files.iter().filter_map(|f| f.after.as_ref()).filter(|p| beside(p)) {
        let src = change.root.join(new);
        if src.symlink_metadata().is_err() {
            continue;
        }
        let dst = unlinked_path(dest, new)?;
        // A directory the base had where the change has a file or a link: emptied by the removals above, or not.
        match dst.symlink_metadata() {
            Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(&dst),
            Ok(_) => std::fs::remove_file(&dst),
            Err(_) => Ok(()),
        }
        .map_err(|e| failed("remove", &dst, &e))?;
        copy(&src, &dst)?;
    }
    Ok(empty)
}

/// Writes the files git tracked at `commit` in the repository whose working tree's top is `repo` to `dest`, an
/// existing directory, through a tar file at `archive`, which it overwrites; then fills each submodule's directory
/// the same way, from the submodule's own repository in `repo`'s working tree, at the commit that `commit` records for
/// it, its own submodules too. A submodule that is not initialised there, or whose repository lacks that commit, is
/// left an empty directory, as git archive leaves it; one that git archive leaves out, as for export-ignore, stays
/// missing.
///
/// The archive is unpacked by the system's `tar`, which, GNU tar and bsdtar alike, refuses by default a member whose
/// path leads out of `dest`, through a link or otherwise; a tracked link out of the tree is written as the
/// repository's own.
///
/// Gives back those submodules, each by its path with `prefix` before it, and why, a clause such as "which is not
/// initialised in the working tree". Errs, a clause, when git cannot list or write a commit, or `tar` cannot unpack
/// it. gate.py's `_unpack_commit`.
fn unpack_commit(
    repo: &Path,
    commit: &str,
    dest: &Path,
    archive: &Path,
    prefix: &str,
) -> Result<Vec<(String, String)>, String> {
    let mut output = OsString::from("--output=");
    output.push(archive);
    let args = [OsStr::new("archive"), OsStr::new("--format=tar"), &output, OsStr::new(commit)];
    git::output(repo, &args).map_err(|Unjudged(e)| e)?;
    let argv = [OsStr::new("tar"), OsStr::new("-x"), OsStr::new("-f"), archive.as_os_str(), OsStr::new("-C"),
                dest.as_os_str()];
    let timeout = std::time::Duration::from_secs(crate::process::TOOL_TIMEOUT);
    match crate::process::run(&argv, Path::new("."), timeout) {
        Err(e) => return Err(format!("cannot run tar to unpack the base: {e}")),
        Ok(done) if done.stopped || done.status != 0 => {
            return Err(format!("tar could not unpack the base: {}", done.said()))
        }
        Ok(_) => {}
    }
    // Each entry is `<mode> <type> <object>\t<path>`; a submodule's mode is 160000 and its object the commit.
    let listing = git::output(repo, &["ls-tree", "-r", "-z", commit]).map_err(|Unjudged(e)| e)?;
    let mut empty = vec![];
    for entry in listing.split(|&b| b == 0) {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else { continue };
        let (meta, path) = (&entry[..tab], OsStr::from_bytes(&entry[tab + 1..]));
        // A submodule the commit's .gitattributes marks export-ignore is missing from the archive, as other paths are.
        if !meta.starts_with(b"160000 ") || !dest.join(path).is_dir() {
            continue;
        }
        let recorded = text::lossless(meta.split(|&b| b == b' ').nth(2).unwrap_or_default());
        let name = format!("{prefix}{}", text::path(path));
        let sub = repo.join(path);
        // The submodule's directory is the top of a repository of its own only once it is initialised: before, git
        // finds the superproject above it, and names the directory as a prefix within it.
        let own = git::run(&sub, &["rev-parse", "--show-prefix"]).map_err(|Unjudged(e)| e)?;
        if own.status != 0 || own.stdout.iter().any(|&b| b != b'\n') {
            empty.push((name, "which is not initialised in the working tree".to_string()));
        } else if git::run(&sub, &["cat-file", "-e", &format!("{recorded}^{{commit}}")])
            .map_err(|Unjudged(e)| e)?
            .status
            != 0
        {
            let short: String = recorded.chars().take(12).collect();
            empty.push((name, format!("whose repository in the working tree does not have the base's commit {short}")));
        } else {
            empty.extend(unpack_commit(&sub, &recorded, &dest.join(path), archive, &format!("{name}/"))?);
        }
    }
    Ok(empty)
}

/// `dest`/`path`, `path` a repository path, after making each directory above it in `dest` a real one: a symbolic
/// link there is replaced by an empty directory, and a missing one made. So a file written or removed at it stays in
/// `dest`. Errs, a clause, when one cannot be. gate.py's `_unlinked_path`.
fn unlinked_path(dest: &Path, path: &OsStr) -> Result<PathBuf, String> {
    let path = Path::new(path);
    let mut at = dest.to_path_buf();
    for part in path.parent().into_iter().flat_map(Path::components) {
        at.push(part);
        let meta = at.symlink_metadata();
        if meta.as_ref().is_ok_and(|m| m.file_type().is_symlink()) {
            std::fs::remove_file(&at).map_err(|e| failed("remove", &at, &e))?;
        }
        if !at.is_dir() {
            std::fs::create_dir(&at).map_err(|e| failed("make", &at, &e))?;
        }
    }
    Ok(at.join(path.file_name().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_scratch_directory_is_private_and_removed_afterwards() {
        use std::os::unix::fs::PermissionsExt;
        let mut seen = None;
        let found = in_scratch_directory("not-run", "to test in", |dir| {
            assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
            std::fs::write(dir.join("f"), "x").unwrap();
            seen = Some(dir.to_path_buf());
            vec![finding(None, None, "r", "m".into())]
        });
        assert_eq!(found.iter().map(|f| f.rule.as_str()).collect::<Vec<_>>(), ["r"]);
        assert!(!seen.unwrap().exists());
    }

    #[test]
    fn a_scratch_directory_passes_over_those_an_earlier_gate_of_the_process_id_left() {
        // nextest runs each test in a process of its own, so this one's scratch directories are numbered from 0.
        let parent = std::env::temp_dir().join(format!("gate-left-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&parent);
        let left: Vec<PathBuf> = (0..8).map(|n| parent.join(format!("gate-{}-{n}", std::process::id()))).collect();
        for dir in &left {
            std::fs::create_dir_all(dir).unwrap();
        }
        let made = scratch_directory(&parent);
        std::fs::remove_dir_all(&parent).unwrap();
        let made = made.unwrap();
        assert!(made.starts_with(&parent) && !left.contains(&made), "{}", made.display());
    }

    #[test]
    fn a_named_pipe_is_not_copied_but_refused() {
        let dir = std::env::temp_dir().join(format!("gate-copy-pipe-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let made = std::process::Command::new("mkfifo").arg(dir.join("pipe")).status().unwrap();
        assert!(made.success());
        let copied = copy(&dir.join("pipe"), &dir.join("copy"));
        std::fs::remove_dir_all(&dir).unwrap();
        let expected = format!("cannot copy {}: it is no regular file, link or directory", text::path(
            dir.join("pipe").as_os_str()
        ));
        assert_eq!(copied, Err(expected));
    }

    #[test]
    fn a_copy_that_cannot_make_its_directory_says_which_and_why() {
        let dir = std::env::temp_dir().join(format!("gate-copy-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        let copied = copy(&dir.join("src"), &dir.join("dst"));
        std::fs::remove_dir_all(&dir).unwrap();
        let why = copied.unwrap_err();
        let expected = format!("cannot make {}: ", text::path(dir.join("dst").as_os_str()));
        assert!(why.starts_with(&expected) && why.contains("exists"), "{why}");
    }

    #[test]
    fn an_unlinked_path_replaces_links_above_it_with_directories() {
        let dir = std::env::temp_dir().join(format!("gate-unlinked-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        std::os::unix::fs::symlink(dir.join("outside"), dir.join("dest/a")).unwrap();
        let at = unlinked_path(&dir.join("dest"), OsStr::new("a/b/c.rs")).unwrap();
        assert_eq!(at, dir.join("dest/a/b/c.rs"));
        assert!(!dir.join("dest/a").symlink_metadata().unwrap().file_type().is_symlink());
        assert!(dir.join("dest/a/b").is_dir());
        std::fs::remove_dir_all(dir).unwrap();
    }
}
