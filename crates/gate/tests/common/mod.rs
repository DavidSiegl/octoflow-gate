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
    /// The repository with `files`, (path, contents), committed as "base".
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
        self.gate_command(base).args(args).output().unwrap()
    }

    /// `gate --base base`, to run in the repository, for a caller to add to, without the profile of the nextest run
    /// that runs this test.
    pub fn gate_command(&self, base: &str) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_gate"));
        command.current_dir(&self.repo).arg("--base").arg(base);
        // nextest tells each test its profile in NEXTEST_PROFILE, and a cargo-nextest the gate starts, as
        // cargo-mutants' test tool, would read it: under CI's `--profile ci` it then asks the test repository for a
        // profile it lacks.
        command.env_remove("NEXTEST_PROFILE");
        command
    }

    /// Commits `files`, (path, contents), and whatever else the work tree changes, as `message`.
    pub fn commit(&self, files: &[(&str, &[u8])], message: &str) {
        for (path, contents) in files {
            self.put(path, contents);
        }
        self.git(&["add", "-A"]);
        self.git(&["commit", "--quiet", "-m", message]);
    }

    /// Commits a submodule at vendor/lib and puts a named pipe in its working tree. A copy of the tracked files
    /// (copies::copy_tracked) takes a submodule's directory whole, so it meets the pipe, which git does not track,
    /// and refuses it: the tree cannot be copied, though git reads its change.
    pub fn make_uncopyable(&self) {
        let lib = self.outside.join("lib");
        std::fs::create_dir_all(&lib).unwrap();
        let git = |args: &[&str]| {
            let done = Command::new("git").current_dir(&lib).args(args).output().unwrap();
            assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
        };
        git(&["init", "--quiet", "-b", "main"]);
        std::fs::write(lib.join("notes.txt"), "notes\n").unwrap();
        git(&["add", "notes.txt"]);
        let me = ["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"];
        git(&[&me[..], &["commit", "--quiet", "-m", "lib"]].concat());
        let lib = lib.to_str().unwrap();
        self.git(&["-c", "protocol.file.allow=always", "submodule", "--quiet", "add", lib, "vendor/lib"]);
        self.commit(&[], "a submodule");
        assert!(Command::new("mkfifo").arg(self.repo.join("vendor/lib/pipe")).status().unwrap().success());
    }

    /// Puts an executable `name` that runs the sh `script` in a directory outside the repository, and gives back
    /// that directory, to put ahead of PATH.
    pub fn stub(&self, name: &str, script: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let bin = self.outside.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        std::fs::write(bin.join(name), format!("#!/bin/sh\n{script}\n")).unwrap();
        std::fs::set_permissions(bin.join(name), std::fs::Permissions::from_mode(0o755)).unwrap();
        bin
    }

    /// Puts stubs of ruff, mypy and pymarkdown outside the repository, and gives back a PATH that finds them ahead of
    /// the real ones. Each writes its arguments, a line each, where [`Repo::argv`] reads them, then prints what
    /// [`Repo::says`] gave it and exits 1, or, given nothing, prints nothing and exits 0.
    pub fn stub_linters(&self) -> String {
        let stubs = self.outside.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        let mut bin = PathBuf::new();
        for linter in ["ruff", "mypy", "pymarkdown"] {
            let script = format!(
                "d='{}'\nprintf '%s\\n' \"$@\" > \"$d.argv\"\n[ -f \"$d.out\" ] || exit 0\ncat \"$d.out\"\nexit 1",
                stubs.join(linter).display()
            );
            bin = self.stub(linter, &script);
        }
        format!("{}:{}", bin.display(), std::env::var("PATH").unwrap())
    }

    /// Has the stub of `linter` ([`Repo::stub_linters`]) print `printed` and exit 1 from now on, whether the stubs
    /// are made yet or not.
    pub fn says(&self, linter: &str, printed: &[u8]) {
        let stubs = self.outside.join("stubs");
        std::fs::create_dir_all(&stubs).unwrap();
        std::fs::write(stubs.join(format!("{linter}.out")), printed).unwrap();
    }

    /// The arguments the stub of `linter` ([`Repo::stub_linters`]) last ran with; panics when it has not run.
    pub fn argv(&self, linter: &str) -> Vec<String> {
        let written = std::fs::read_to_string(self.outside.join("stubs").join(format!("{linter}.argv"))).unwrap();
        written.lines().map(String::from).collect()
    }
}

/// A gate.toml, as [`Repo::with`] takes a file, that turns ruff and mypy off, for a repository of Python whose tests
/// are of something else: a pyproject.toml without mypy's `files` has mypy fail, and with it the gate.
pub const NO_PYTHON_LINTERS: (&str, &[u8]) = ("gate.toml", b"[linters]\nruff = []\nmypy = []\n");

/// The Python of the workspace's own environment, which has the pytest, pytest-randomly, pytest-cov and mutmut that
/// the checks run with a judged project's Python. uv names it, and makes the environment first where the tree has
/// none, as in the copy of the tracked files that cargo-mutants runs these tests in.
pub fn python() -> PathBuf {
    let workspace = concat!(env!("CARGO_MANIFEST_DIR"), "/../..");
    let named = Command::new("uv")
        .current_dir(workspace)
        .args(["run", "--frozen", "python", "-c", "import sys; print(sys.executable)"])
        .output()
        .unwrap();
    assert!(named.status.success(), "uv failed: {}", String::from_utf8_lossy(&named.stderr));
    PathBuf::from(String::from_utf8(named.stdout).unwrap().trim_end())
}

/// A program outside `repo` that runs the sh `script`, whatever it is asked, to be the gate's `--python`.
pub fn fake_python(repo: &Repo, script: &str) -> PathBuf {
    repo.stub("python", script).join("python")
}

/// A `--python` that is the workspace's Python, but first runs the sh `script`, which has the arguments in `"$@"`.
pub fn python_after(repo: &Repo, script: &str) -> PathBuf {
    fake_python(repo, &format!("{script}\nexec '{}' \"$@\"", python().display()))
}

/// A `--python` that is the workspace's Python, and the file it writes its arguments to, a line for each time the
/// gate runs it; the file is missing until then.
pub fn logging_python(repo: &Repo) -> (PathBuf, PathBuf) {
    let log = repo.outside.join("python-runs");
    (python_after(repo, &format!("printf '%s\\n' \"$*\" >> '{}'", log.display())), log)
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
