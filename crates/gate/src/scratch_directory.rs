//! The directories the gate makes to run a project's tools in, and removes on every way out: when a check is done
//! with one, when a panic unwinds past it, when Ctrl-C, SIGTERM or SIGHUP stops the gate (process::exit_interrupted),
//! and, for those a killed gate left, when the next gate makes one.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

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
        loop {
            let path = parent.join(format!("gate-{}-{}", std::process::id(), MADE.fetch_add(1, Ordering::SeqCst)));
            match std::fs::DirBuilder::new().mode(0o700).create(&path) {
                // Left by an earlier gate of the same process id that still runs, or ran in another pid namespace.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
                Ok(()) => {}
            }
            // Until it is locked, another gate's remove_abandoned may hold the lock a moment, which `lock` waits out,
            // or, when it runs in another pid namespace and finds no process of this pid, remove the directory: then
            // the lock is of a directory no longer at `path`, and the next number is tried.
            let locked = std::fs::File::open(&path).and_then(|dir| dir.lock().map(|()| dir));
            match locked.and_then(|dir| at(&dir, &path).map(|there| there.then_some(dir))) {
                Ok(Some(dir)) => {
                    HELD.lock().unwrap_or_else(|e| e.into_inner()).push(path.clone());
                    return Ok(ScratchDirectory { path, _lock: dir });
                }
                Ok(None) => continue,
                Err(e) => {
                    // An empty directory of the gate's own: nothing to lose when it cannot be removed either.
                    let _ = std::fs::remove_dir(&path);
                    return Err(e);
                }
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
/// can open and lock, so that no gate holds it, and whose `<pid>` names no process, so that no gate before 0.15.0,
/// which locked none, is running in it. One that cannot be read, locked or removed stays: another account's, a running
/// gate's, or one to try again next time.
fn remove_abandoned(parent: &Path) {
    let Ok(entries) = std::fs::read_dir(parent) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(pid) = scratch_pid(&name) else { continue };
        let path = entry.path();
        let Ok(dir) = std::fs::File::open(&path) else { continue };
        if dir.try_lock().is_ok() && !runs(pid) {
            let _ = std::fs::remove_dir_all(&path);
        }
    }
}

/// Whether `path` names `dir`, an open directory: false when nothing is at `path`, or another file is. Errs when what
/// is at `path` cannot be read.
fn at(dir: &std::fs::File, path: &Path) -> std::io::Result<bool> {
    use std::os::unix::fs::MetadataExt;
    let open = dir.metadata()?;
    match std::fs::symlink_metadata(path) {
        Ok(there) => Ok(there.dev() == open.dev() && there.ino() == open.ino()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// The process id in `name` when it is a [`ScratchDirectory`]'s, `gate-<pid>-<n>`.
fn scratch_pid(name: &OsStr) -> Option<u32> {
    let (pid, n) = name.to_str()?.strip_prefix("gate-")?.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit());
    (digits(pid) && digits(n)).then(|| pid.parse().ok()).flatten()
}

/// Whether a process of id `pid` runs, in the gate's pid namespace, as a zombie included; true, too, when that cannot
/// be told.
fn runs(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else { return false };
    // SAFETY: kill(2) with signal 0 sends nothing and touches no memory of the gate's; it only checks that pid exists.
    #[allow(unsafe_code, reason = "kill(2) has no safe wrapper in std; it reads no memory")]
    let sent = unsafe { libc::kill(pid, 0) };
    sent == 0 || std::io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

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
    fn an_open_directory_is_at_its_path_until_that_names_another_or_nothing() {
        let parent = ScratchDir::new();
        let path = parent.join("d");
        std::fs::create_dir(&path).unwrap();
        let dir = std::fs::File::open(&path).unwrap();
        assert!(at(&dir, &path).unwrap());
        std::fs::remove_dir(&path).unwrap();
        assert!(!at(&dir, &path).unwrap());
        std::fs::create_dir(&path).unwrap();
        assert!(!at(&dir, &path).unwrap());
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
        // A gate of 0.15.0 or later, here this process, holds its own: its lock keeps it whatever its pid.
        let held = ScratchDirectory::new(&parent).unwrap();
        let locked_gone = parent.join(format!("gate-{}-1", exited.id()));
        std::fs::rename(held.path(), &locked_gone).unwrap();
        let again = ScratchDirectory::new(&parent).unwrap();
        assert!(!gone.exists());
        assert!(running.exists() && locked_gone.exists() && again.path().exists());
        assert!(unlike.iter().all(|dir| dir.exists()));
    }
}
