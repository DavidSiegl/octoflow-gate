//! The `gate` binary with the real golangci-lint, over a Go module of no dependencies in a subdirectory of the
//! repository, with golangci-lint's default linters.
//!
//! No test here has golangci-lint outlive its timeout: that is a constant of ten minutes, and process.rs's own tests
//! hold that a process past its timeout is killed with what it started.

use std::ffi::CString;
use std::io::Read;
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::Repo;

mod common;

const GO_MOD: &str = "module example.com/calc\n\ngo 1.22\n";
const CALC: &str = "package calc\n\nimport \"os\"\n\n// Add adds.\nfunc Add(a, b int) int {\n\treturn a + b\n}\n";
/// A function whose error return no one checks, errcheck's finding at its line 3.
const CLEAN: &str = "\n// Clean removes x.\nfunc Clean() {\n\tos.Remove(\"x\")\n}\n";

fn module_repo() -> Repo {
    Repo::with(&[
        ("pyproject.toml", b"[tool.ruff]\n"),
        ("calc/go.mod", GO_MOD.as_bytes()),
        ("calc/calc.go", format!("{CALC}\nvar _ = os.Getpid\n").as_bytes()),
    ])
}

/// The exit status and standard output of `gate --base HEAD` in `repo`, with `bin` ahead of PATH when given.
fn gate(repo: &Repo, bin: Option<&Path>) -> (i32, String) {
    let mut command = repo.gate_command("HEAD");
    if let Some(bin) = bin {
        command.env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()));
    }
    let done = command.output().unwrap();
    (done.status.code().unwrap(), String::from_utf8(done.stdout).unwrap())
}

#[test]
fn a_finding_on_an_added_line_is_an_issue() {
    let repo = module_repo();
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- 1 findings on lines the change added; each is listed below.\n"), "{out}");
    assert!(out.contains("1. `calc/calc.go:12` error golangci-lint errcheck: Error return value of `os.Remove` is not \
                          checked\n"), "{out}");
}

/// A watch on one file through inotify(7) for its being opened, by any process. Dropping it ends the watch.
struct Opens(std::fs::File);

impl Opens {
    /// Watches `path`, which must exist; opens before the call are not seen. Panics when inotify refuses.
    fn of(path: &Path) -> Opens {
        // SAFETY: inotify_init1 reads no memory, and the descriptor it returns is the File's alone.
        #[allow(unsafe_code, reason = "inotify(7) has no wrapper in std")]
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK | libc::IN_CLOEXEC) };
        assert!(fd >= 0, "inotify_init1: {}", std::io::Error::last_os_error());
        // SAFETY: fd is the open descriptor inotify_init1 returned, owned by nothing else.
        #[allow(unsafe_code, reason = "inotify(7) has no wrapper in std")]
        let watch = unsafe { std::fs::File::from_raw_fd(fd) };
        let name = CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a NUL-terminated string that outlives the call.
        #[allow(unsafe_code, reason = "inotify(7) has no wrapper in std")]
        let added = unsafe { libc::inotify_add_watch(fd, name.as_ptr(), libc::IN_OPEN) };
        assert!(added >= 0, "inotify_add_watch on {}: {}", path.display(), std::io::Error::last_os_error());
        Opens(watch)
    }

    /// Whether the file was opened since the watch began or since this last answered true; it does not wait.
    fn seen(&mut self) -> bool {
        let mut events = [0; 4096];
        match self.0.read(&mut events) {
            Ok(n) => n > 0,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(err) => panic!("reading the lock's inotify events: {err}"),
        }
    }
}

#[test]
fn another_golangci_lint_running_makes_it_wait_not_fail() {
    let repo = module_repo();
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes());
    // golangci-lint takes flock(2) on $TMPDIR/golangci-lint.lock while it runs; the test holds it as another instance
    // would, in a TMPDIR of its own so that the other tests' instances keep out of it.
    let tmp = repo.outside.join("tmp");
    std::fs::create_dir_all(&tmp).unwrap();
    let lock = std::fs::File::create(tmp.join("golangci-lint.lock")).unwrap();
    lock.lock().unwrap();
    // Held for ten seconds from the gate's golangci-lint first trying it, which it does as it starts, opening the file
    // and trying again each second: without --allow-serial-runners it gives up after five seconds, with it it waits
    // (acquireFileLock in golangci-lint's pkg/commands/run.go). The holder answers whether the gate met the lock and
    // outlasted the hold; returning early drops the file, which lets the lock go.
    let mut opens = Opens::of(&tmp.join("golangci-lint.lock"));
    let gate_done = Arc::new(AtomicBool::new(false));
    let holder = std::thread::spawn({
        let gate_done = Arc::clone(&gate_done);
        move || {
            let seen_by = Instant::now() + Duration::from_secs(120);
            while !opens.seen() {
                if gate_done.load(Ordering::SeqCst) || Instant::now() > seen_by {
                    return Err("nothing opened the lock while the gate ran, within two minutes");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            let release = Instant::now() + Duration::from_secs(10);
            while Instant::now() < release {
                if gate_done.load(Ordering::SeqCst) {
                    return Err("the gate was done while the lock its golangci-lint asked for was held");
                }
                std::thread::sleep(Duration::from_millis(20));
            }
            lock.unlock().unwrap();
            Ok(())
        }
    });
    let done = repo.gate_command("HEAD").env("TMPDIR", &tmp).output().unwrap();
    gate_done.store(true, Ordering::SeqCst);
    let held = holder.join().unwrap();
    let out = String::from_utf8(done.stdout).unwrap();
    assert!(!out.contains("parallel golangci-lint is running"), "{out}");
    assert_eq!(done.status.code(), Some(1), "{out}");
    assert!(out.contains("1. `calc/calc.go:12` error golangci-lint errcheck: "), "{out}");
    assert_eq!(held, Ok(()), "{out}");
}

#[test]
fn a_finding_on_an_old_line_counts_as_elsewhere() {
    let repo = module_repo();
    repo.commit(&[("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes())], "a finding");
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}\n// Two is 2.\nfunc Two() int {{\n\treturn 2\n}}\n").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 0, "{out}");
    assert!(out.contains("1 other open issues in the project"), "{out}");
}

#[test]
fn code_that_does_not_compile_fails_the_gate_naming_the_type_checker_s_first_error() {
    let repo = module_repo();
    let two = "\n// Two is 2.\nfunc Two() int {\n\treturn Add(1, 1)\n}\n";
    repo.commit(&[("calc/calc.go", format!("{CALC}{two}").as_bytes())], "a caller");
    repo.put("calc/calc.go", format!("package calc\n{two}").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- `golangci-lint` could not compile the module of `calc/go.mod`: `calc/calc.go:1` \
                          golangci-lint typecheck: ./calc.go:5:9: undefined: Add\n"), "{out}");
    assert!(out.contains("## Issues on changed lines\n\nNone.\n"), "{out}");
}

#[test]
fn a_module_with_no_go_files_of_its_own_is_nothing_to_lint() {
    let repo = module_repo();
    // The top's module holds no package: its code is all in calc/, a module of its own.
    repo.commit(&[("go.mod", b"module example.com/top\n\ngo 1.22\n")], "a module at the top");
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("## Failed conditions\n\n- 1 findings on lines the change added; each is listed below.\n\n"),
            "{out}");
    assert!(out.contains("1. `calc/calc.go:12` error golangci-lint errcheck: "), "{out}");
}

#[test]
fn a_configuration_s_issues_exit_code_drops_no_finding() {
    let repo = module_repo();
    repo.commit(&[("calc/.golangci.yml", b"version: \"2\"\nrun:\n  issues-exit-code: 2\n")], "an exit code");
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("## Failed conditions\n\n- 1 findings on lines the change added; each is listed below.\n\n"),
            "{out}");
    assert!(out.contains("1. `calc/calc.go:12` error golangci-lint errcheck: "), "{out}");
}

#[test]
fn a_module_under_testdata_is_not_linted() {
    let repo = module_repo();
    repo.commit(&[
        ("calc/testdata/broken/go.mod", b"module example.com/broken\n\ngo 1.22\n"),
        ("calc/testdata/broken/b.go", b"package broken\n\nfunc F() int {\n\treturn nope\n}\n"),
    ], "a test's input");
    repo.put("calc/calc.go", format!("{CALC}{CLEAN}").as_bytes());
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- 1 findings on lines the change added; each is listed below.\n"), "{out}");
    assert!(!out.contains("testdata"), "{out}");
}

#[test]
fn a_configuration_golangci_lint_cannot_read_fails_naming_it() {
    let repo = module_repo();
    repo.put("calc/.golangci.yml", b"linters: [\n");
    repo.git(&["add", "calc/.golangci.yml"]);
    let (exit_code, out) = gate(&repo, None);
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- `golangci-lint` failed with exit status 3 in `calc/go.mod`: "), "{out}");
    assert!(out.contains("can't load config"), "{out}");
}

#[test]
fn a_repository_without_go_files_runs_no_golangci_lint() {
    let repo = module_repo();
    repo.git(&["rm", "--quiet", "calc/calc.go"]);
    let ran = repo.outside.join("golangci-lint-ran");
    let bin = repo.stub("golangci-lint", &format!("echo ran > '{}'; exit 3", ran.display()));
    let (exit_code, out) = gate(&repo, Some(&bin));
    assert_eq!(exit_code, 0, "{out}");
    assert!(!ran.exists());
}

#[test]
fn a_golangci_lint_that_prints_no_report_fails_naming_what_it_said() {
    let repo = module_repo();
    let bin = repo.stub("golangci-lint", "echo 'level=error msg=\"Running error: no go files\"' >&2; exit 1");
    let (exit_code, out) = gate(&repo, Some(&bin));
    assert_eq!(exit_code, 1, "{out}");
    assert!(out.contains("- `golangci-lint` failed with exit status 1 in `calc/go.mod`: level=error \
                          msg=\"Running error: no go files\"\n"), "{out}");
}
