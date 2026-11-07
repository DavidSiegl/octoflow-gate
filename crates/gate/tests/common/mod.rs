//! What the integration tests of the gate share: a git repository of their own to judge.

#![allow(dead_code, reason = "each test crate that includes this module uses only part of it")]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

/// A git repository on branch main in a fresh directory, beside an empty directory outside it; both are removed when
/// it is dropped.
pub struct Repo {
    dir: PathBuf,
    /// The repository's top level.
    pub repo: PathBuf,
    /// The empty directory outside it.
    pub outside: PathBuf,
}

impl Repo {
    /// The repository with `files`, (path, contents), committed as "base", as TestGate's BASE_FILES are.
    pub fn with(files: &[(&str, &[u8])]) -> Repo {
        static MADE: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir()
            .join(format!("gate-test-{}-{}", std::process::id(), MADE.fetch_add(1, Ordering::SeqCst)));
        let (repo, outside) = (dir.join("repo"), dir.join("outside"));
        std::fs::create_dir_all(&outside).unwrap();
        let made = Repo { dir, repo, outside };
        made.git(&["init", "--quiet", "-b", "main", "."]);
        for (key, setting) in [("user.name", "t"), ("user.email", "t@example.com"), ("commit.gpgsign", "false")] {
            made.git(&["config", key, setting]);
        }
        for (path, contents) in files {
            made.put(path, contents);
        }
        made.git(&["add", "-A"]);
        made.git(&["commit", "--quiet", "-m", "base"]);
        made
    }

    /// git's standard output for `args` run in the repository; panics when git fails.
    pub fn git<S: AsRef<OsStr>>(&self, args: &[S]) -> String {
        std::fs::create_dir_all(&self.repo).unwrap();
        let done = Command::new("git").current_dir(&self.repo).args(args).output().unwrap();
        assert!(done.status.success(), "git failed: {}", String::from_utf8_lossy(&done.stderr));
        String::from_utf8(done.stdout).unwrap()
    }

    pub fn put(&self, path: impl AsRef<Path>, contents: &[u8]) {
        let path = self.repo.join(path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }

    /// `gate --base base args`, run in the repository.
    pub fn gate(&self, base: &str, args: &[&OsStr]) -> Output {
        Command::new(env!("CARGO_BIN_EXE_gate")).current_dir(&self.repo).arg("--base").arg(base).args(args).output()
            .unwrap()
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
