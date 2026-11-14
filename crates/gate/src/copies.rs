//! What the checks that run a project's tools share: the copies of a tree they run them in, so that nothing they
//! build or write lands in the tree the gate judges (a scratch directory of the gate's own, a copy of the working
//! tree's tracked files, and a copy of the base with the change's tests in it); the budget they run within; and the
//! shape and order of their findings.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::judge::Change;
use crate::report::Finding;
use crate::{git, process, text, Unjudged};

/// The findings of `run(dir)`, `dir` a [`ScratchDirectory`], which is removed afterwards whatever `run` finds, and when
/// `run` panics or the gate is stopped meanwhile; `purpose` says what it is for, as a clause such as "to run
/// cargo-mutants in". When the directory cannot be made, `run` is not called, and the one finding is of `rule`, of the
/// check whose run it is, saying so; when it cannot be removed, a `not-removed` finding naming it follows `run`'s. The
/// findings' rules are the check's own, without its name.
pub fn in_scratch_directory(rule: &str, purpose: &str, run: impl FnOnce(&Path) -> Vec<Finding>) -> Vec<Finding> {
    let dir = match ScratchDirectory::new(&std::env::temp_dir()) {
        Ok(dir) => dir,
        Err(e) => {
            let why = format!("could not make a directory {purpose}: {e}");
            return vec![finding(None, None, rule, format!("{why}; nothing ran, so the check judged nothing."))];
        }
    };
    let mut out = run(dir.path());
    let path = dir.path().to_path_buf();
    if let Err(e) = dir.remove() {
        let message = format!("could not remove the directory {purpose}, `{}`: {e}", path.display());
        out.push(finding(None, None, "not-removed", message));
    }
    out
}

/// A new empty directory of the gate's own, mode 0700, named `gate-<pid>-<n>` for the gate's process, for a check to
/// run a tool in. It is removed, with all it holds, by [`ScratchDirectory::remove`], when dropped, a panic's unwinding
/// included, and by [`remove_scratch_directories`], which the gate calls when Ctrl-C, SIGTERM or SIGHUP stops it. A
/// gate killed by SIGKILL, or by a signal it does not catch, leaves it, until the next gate to make one in the same
/// directory finds that the gate that made it no longer runs, and removes it.
pub(crate) struct ScratchDirectory {
    path: PathBuf,
    /// The directory, open and exclusively locked (flock(2)) for as long as this value lives: a lock the next gate can
    /// take means that the gate that made the directory has gone, as the kernel frees a lock with its last holder.
    _lock: std::fs::File,
}

/// The paths of the [`ScratchDirectory`]s this process holds now.
static HELD: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(vec![]);

impl ScratchDirectory {
    /// Makes the directory in `parent`, first removing those there that gates no longer running left. Errs when it
    /// cannot be made or locked, having removed what it made.
    pub(crate) fn new(parent: &Path) -> std::io::Result<ScratchDirectory> {
        use std::os::unix::fs::DirBuilderExt;
        use std::sync::atomic::{AtomicU32, Ordering};
        static MADE: AtomicU32 = AtomicU32::new(0);
        remove_abandoned(parent);
        let path = loop {
            let path = parent.join(format!("gate-{}-{}", std::process::id(), MADE.fetch_add(1, Ordering::SeqCst)));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                // Left by an earlier gate of the same process id that still runs, or ran in another pid namespace.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
                Ok(()) => break path,
            }
        };
        let lock = std::fs::File::open(&path);
        match lock.and_then(|lock| lock.try_lock().map(|()| lock).map_err(std::io::Error::from)) {
            Ok(lock) => {
                HELD.lock().unwrap_or_else(|e| e.into_inner()).push(path.clone());
                Ok(ScratchDirectory { path, _lock: lock })
            }
            Err(e) => {
                // An empty directory of the gate's own: nothing to lose when it cannot be removed either.
                let _ = std::fs::remove_dir(&path);
                Err(e)
            }
        }
    }

    /// The directory's path: `parent`/`gate-<pid>-<n>`.
    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    /// Removes the directory and all it holds. Errs when that fails; what is left of it stays.
    pub(crate) fn remove(self) -> std::io::Result<()> {
        std::fs::remove_dir_all(&self.path)
    }
}

impl Drop for ScratchDirectory {
    /// After [`ScratchDirectory::remove`], finds nothing to remove; otherwise removes what it can, as a panicking check
    /// has no one to tell that it could not.
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
        HELD.lock().unwrap_or_else(|e| e.into_inner()).retain(|held| *held != self.path);
    }
}

/// Removes every [`ScratchDirectory`] the gate holds, with all it holds, as it is about to exit without dropping them;
/// one that cannot be removed stays, since the gate has no report left to say so in.
pub(crate) fn remove_scratch_directories() {
    for path in HELD.lock().unwrap_or_else(|e| e.into_inner()).iter() {
        let _ = std::fs::remove_dir_all(path);
    }
}

/// Removes, from `parent`, the [`ScratchDirectory`]s of gates no longer running: a `gate-<pid>-<n>` that this account
/// can open and lock, so that no gate holds it, and whose `<pid>` names no process, so that no gate before 0.14.0,
/// which locked none, is running in it. One that cannot be read, locked or removed stays: another account's, a running
/// gate's, or one to try again next time.
fn remove_abandoned(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = scratch_pid(&name) else { continue };
        let path = entry.path();
        let Ok(dir) = std::fs::File::open(&path) else { continue };
        if dir.try_lock().is_ok() && !process::runs(pid) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// The process id in `name` when it is a [`ScratchDirectory`]'s, `gate-<pid>-<n>`.
fn scratch_pid(name: &OsStr) -> Option<u32> {
    let (pid, n) = name.to_str()?.strip_prefix("gate-")?.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(pid) && digits(n)).then(|| pid.parse().ok()).flatten()
}

/// The time a check's tool runs may take.
pub struct Budget {
    /// Its length in seconds, as findings name what a run was stopped at: the check's budget, or a share of it.
    pub seconds: u64,
    /// When it runs out.
    pub deadline: Instant,
}

impl Budget {
    /// A budget of `seconds` that starts now.
    pub fn starting_now(seconds: u64) -> Budget {
        Budget { seconds, deadline: Instant::now() + Duration::from_secs(seconds) }
    }

    /// One of `parts` equal shares of what is left of this budget from now: a deadline that share from now, and its
    /// length in whole seconds, rounded. `parts` of 0 is taken as 1.
    pub fn share(&self, parts: u32) -> Budget {
        let now = Instant::now();
        let share = self.deadline.saturating_duration_since(now) / parts.max(1);
        Budget { seconds: share.as_secs_f64().round() as u64, deadline: now + share }
    }
}

/// Puts `findings` in the order a check gives them: by path and line, a finding on no path or line first.
pub fn sort_by_place(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        let key = |f: &Finding| (f.path.clone().unwrap_or_default(), f.line.unwrap_or(0));
        key(a).cmp(&key(b))
    });
}

/// A finding of a check beside the linters, as its find gives it: `rule` its own, without the check's name, which
/// the gate puts before it, and its severity "warning", which the gate replaces with what it does with it.
pub fn finding(path: Option<&OsStr>, line: Option<u32>, rule: &str, message: String) -> Finding {
    Finding { path: path.map(OsStr::to_os_string), line, severity: "warning".into(), rule: rule.into(), message }
}

/// Copies the files git tracks in the working tree at `root`, as they are there, to the same paths under `dest`, a
/// directory it makes, with its parents. A symbolic link is copied as a link and a submodule as its directory; a
/// tracked file missing from the working tree is left out. Errs, a clause, when git cannot list the files, `dest`
/// exists or a file cannot be copied; `dest` may then hold part of the copy.
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
/// its permissions. Errs on anything else, as a named pipe, whose reading could block the gate for good.
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
pub fn failed(what: &str, path: &Path, e: &std::io::Error) -> String {
    format!("cannot {what} {}: {e}", text::path(path.as_os_str()))
}

/// Writes the files git tracked at `change.base` to `dest`, a directory it makes, through a tar file at `archive`,
/// then replaces each file of the change's that `beside` accepts, a repository path that belongs with the tests
/// rather than with the code they test, with the working tree's: removed at its path at the base, and copied, a
/// symbolic link as a link, to its path in the working tree. A symbolic link is written as it is, wherever it points,
/// and no file is written or removed through one: a link on the way to a replaced file is replaced by a directory, as
/// the working tree has one there. A submodule holds the files of the commit the base records for it, as
/// [`unpack_commit`] writes them, unless `beside` accepts its path: then it holds those of the commit its checkout in
/// the working tree is at, without what that checkout holds that git does not track there, and is left empty when the
/// working tree has no checkout of it.
///
/// Gives back the submodules left empty, as [`unpack_commit`] does, of the base's and of those taken from the working
/// tree. Errs, a clause, when git cannot list or write a commit, `tar` cannot unpack it, `dest` exists or a file cannot
/// be written.
pub fn copy_base_with_tests(
    change: &Change,
    dest: &Path,
    archive: &Path,
    beside: impl Fn(&OsStr) -> bool,
) -> Result<Vec<(String, String)>, String> {
    std::fs::create_dir(dest).map_err(|e| failed("make", dest, &e))?;
    let mut empty = unpack_commit(&change.root, &change.base, dest, archive, "")?;
    // A path the base tracks has no link above it there; it is a directory when the base records a submodule there.
    for old in change.files.iter().filter_map(|f| f.before.as_ref()).filter(|p| beside(p)) {
        remove_entry(&dest.join(old))?;
        let removed = text::path(old);
        empty.retain(|(name, _)| name != &removed && !name.starts_with(&format!("{removed}/")));
    }
    for new in change.files.iter().filter_map(|f| f.after.as_ref()).filter(|p| beside(p)) {
        let src = change.root.join(new);
        let Ok(meta) = src.symlink_metadata() else { continue };
        let dst = unlinked_path(dest, new)?;
        // A directory the base had where the change has a file or a link: emptied by the removals above, or not.
        remove_entry(&dst)?;
        // git tracks files, links and commits, so a directory, not a link to one, is where the change records a
        // submodule's commit. Its checkout's own `.git` would lead nowhere in the copy: it is written as the files of
        // the commit the checkout is at.
        if meta.is_dir() {
            std::fs::create_dir(&dst).map_err(|e| failed("make", &dst, &e))?;
            let name = text::path(new);
            if src.join(".git").symlink_metadata().is_ok() {
                empty.extend(unpack_commit(&src, "HEAD", &dst, archive, &format!("{name}/"))?);
            } else {
                empty.push((name, NOT_INITIALISED.to_string()));
            }
        } else {
            copy(&src, &dst)?;
        }
    }
    Ok(empty)
}

/// Removes what is at `path`, a directory with all it holds, nothing when there is nothing, and never through a link
/// at `path` itself. Errs, a clause, when it cannot.
fn remove_entry(path: &Path) -> Result<(), String> {
    match path.symlink_metadata() {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(_) => Ok(()),
    }
    .map_err(|e| failed("remove", path, &e))
}

/// Why a submodule is left empty when the working tree has no checkout of it.
const NOT_INITIALISED: &str = "which is not initialised in the working tree";

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
/// it.
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
            empty.push((name, NOT_INITIALISED.to_string()));
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
/// `dest`. Errs, a clause, when one cannot be.
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
    use crate::scratch::ScratchDir;

    #[test]
    fn a_share_of_a_budget_names_its_own_seconds() {
        let budget = Budget { seconds: 300, deadline: Instant::now() + Duration::from_secs(300) };
        let third = budget.share(3);
        assert_eq!(third.seconds, 100);
        assert!(third.deadline <= Instant::now() + Duration::from_secs(100));
        assert_eq!(budget.share(0).seconds, 300);
        let spent = Budget { seconds: 300, deadline: Instant::now() };
        assert_eq!(spent.share(2).seconds, 0);
    }

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
        let parent = ScratchDir::new();
        let left: Vec<PathBuf> = (0..8).map(|n| parent.join(format!("gate-{}-{n}", std::process::id()))).collect();
        for dir in &left {
            std::fs::create_dir_all(dir).unwrap();
        }
        let made = ScratchDirectory::new(&parent).unwrap();
        assert!(made.path().starts_with(&parent) && !left.contains(&made.path().to_path_buf()));
        assert!(left.iter().all(|dir| dir.exists()), "a running gate's directory went");
    }

    #[test]
    fn a_scratch_directory_is_removed_when_its_check_panics() {
        let mut seen = None;
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            in_scratch_directory("not-run", "to test in", |dir| {
                std::fs::write(dir.join("f"), "x").unwrap();
                seen = Some(dir.to_path_buf());
                panic!("a check's bug")
            })
        }));
        assert!(panicked.is_err());
        assert!(!seen.unwrap().exists());
    }

    #[test]
    fn a_scratch_directory_is_removed_when_the_gate_stops() {
        let parent = ScratchDir::new();
        let made = ScratchDirectory::new(&parent).unwrap();
        std::fs::write(made.path().join("f"), "x").unwrap();
        remove_scratch_directories();
        assert!(!made.path().exists());
        made.remove().unwrap_err();
    }

    #[test]
    fn the_scratch_directories_of_gates_no_longer_running_are_removed_and_no_other() {
        let parent = ScratchDir::new();
        let mut exited = std::process::Command::new("true").spawn().unwrap();
        exited.wait().unwrap();
        let gone = parent.join(format!("gate-{}-0", exited.id()));
        let running = parent.join(format!("gate-{}-0", std::process::id()));
        let unlike = ["gate-unit-1-0", "gate-1-x", "gate--0", "other-1-0"].map(|name| parent.join(name));
        for dir in [&gone, &running].into_iter().chain(&unlike) {
            std::fs::create_dir(dir).unwrap();
            std::fs::write(dir.join("f"), "x").unwrap();
        }
        // A gate of 0.14.0 or later, here this process, holds its own: its lock keeps it whatever its pid.
        let held = ScratchDirectory::new(&parent).unwrap();
        let locked_gone = parent.join(format!("gate-{}-1", exited.id()));
        std::fs::rename(held.path(), &locked_gone).unwrap();
        let again = ScratchDirectory::new(&parent).unwrap();
        assert!(!gone.exists());
        assert!(running.exists() && locked_gone.exists() && again.path().exists());
        assert!(unlike.iter().all(|dir| dir.exists()));
    }

    #[test]
    fn a_named_pipe_is_not_copied_but_refused() {
        let dir = ScratchDir::new();
        let made = std::process::Command::new("mkfifo").arg(dir.join("pipe")).status().unwrap();
        assert!(made.success());
        let copied = copy(&dir.join("pipe"), &dir.join("copy"));
        let expected = format!("cannot copy {}: it is no regular file, link or directory", text::path(
            dir.join("pipe").as_os_str()
        ));
        assert_eq!(copied, Err(expected));
    }

    #[test]
    fn a_copy_that_cannot_make_its_directory_says_which_and_why() {
        let dir = ScratchDir::new();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        let copied = copy(&dir.join("src"), &dir.join("dst"));
        let why = copied.unwrap_err();
        let expected = format!("cannot make {}: ", text::path(dir.join("dst").as_os_str()));
        assert!(why.starts_with(&expected) && why.contains("exists"), "{why}");
    }

    #[test]
    fn an_unlinked_path_replaces_links_above_it_with_directories() {
        let dir = ScratchDir::new();
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        std::os::unix::fs::symlink(dir.join("outside"), dir.join("dest/a")).unwrap();
        let at = unlinked_path(&dir.join("dest"), OsStr::new("a/b/c.rs")).unwrap();
        assert_eq!(at, dir.join("dest/a/b/c.rs"));
        assert!(!dir.join("dest/a").symlink_metadata().unwrap().file_type().is_symlink());
        assert!(dir.join("dest/a/b").is_dir());
    }
}
