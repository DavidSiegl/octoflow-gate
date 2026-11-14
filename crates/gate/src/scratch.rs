//! Test support: a directory a test writes its files in, which goes when the test ends, failed or not. A test that
//! removed its files on its last line kept them when an assertion failed, and a cargo-mutants run fails tests by the
//! thousand. lib.rs and main.rs each declare this module under `cfg(test)`, the binary's tests not seeing the
//! library's test-only items.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

/// A new empty directory under the system's temporary one, removed with all it holds when dropped, a panicking test's
/// included. Derefs to its path, absolute and through no symbolic link.
pub struct ScratchDir(PathBuf);

impl ScratchDir {
    /// Makes the directory, named `gate-unit-<pid>-<n>`, a name no other in this process has. Panics when it cannot
    /// make it.
    pub fn new() -> ScratchDir {
        // cargo test runs a binary's tests on threads of one process, so the process id alone would not tell apart
        // two tests' directories.
        static MADE: AtomicU32 = AtomicU32::new(0);
        loop {
            let n = MADE.fetch_add(1, Ordering::SeqCst);
            let dir = std::env::temp_dir().join(format!("gate-unit-{}-{n}", std::process::id()));
            match std::fs::create_dir(&dir) {
                // Left by a run of an earlier process with the same id.
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot make {}: {e}", dir.display()),
                Ok(()) => match std::fs::canonicalize(&dir) {
                    Ok(real) => return ScratchDir(real),
                    Err(e) => panic!("cannot resolve {}: {e}", dir.display()),
                },
            }
        }
    }
}

impl std::ops::Deref for ScratchDir {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for ScratchDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for ScratchDir {
    /// Panics when the directory cannot be removed, unless the test is panicking already: a second panic would abort
    /// the test binary and hide the first.
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0) {
            if !std::thread::panicking() {
                panic!("cannot remove {}: {e}", self.0.display());
            }
        }
    }
}
