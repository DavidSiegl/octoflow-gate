//! Running the programs the gate judges a change with, git, a linter or a test command: each in a process group of
//! its own, killed with all it started when it overruns or when Ctrl-C stops the gate, and the gate's own exit on
//! Ctrl-C.

use std::ffi::{OsStr, OsString};
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Seconds a linter or a test command may run before the gate stops it, and judges it as not having finished.
pub const TOOL_TIMEOUT: u64 = 600;

/// The most the gate waits, after a process has exited or been killed, for the rest of what its group printed.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// How often the gate looks whether a process has exited, so the most it waits on past the exit.
const POLL: Duration = Duration::from_millis(50);

/// The gate's exit status when Ctrl-C stopped it, having written no report: 128 + SIGINT, as a shell reports it.
pub const EXIT_INTERRUPTED: i32 = 130;

/// What a process printed, and how it ended.
#[derive(Debug)]
pub struct Finished {
    /// Its exit status, or the negated signal that ended it, as Python's returncode; -SIGKILL when the gate killed it
    /// and could not reap it within [`KILL_WAIT`].
    pub status: i32,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    /// True when it was killed for running longer than its timeout.
    pub stopped: bool,
}

/// The environment for every process the gate starts, git, a linter or a test command: the gate's own, without the
/// variables whose names end in `_TOKEN`, `_PASSWORD`, `_SECRET` or `_API_KEY`, without UPDATE_GOLDEN, and without
/// git's switches for how it reads every pathspec.
///
/// So a test the change brings never inherits a CI's or a service's tokens when the gate runs where they are set; a
/// golden test (tests/golden.py) compares with its file instead of rewriting it, as it does under UPDATE_GOLDEN=1;
/// and the gate's pathspecs, written in git's default syntax, mean what they say: each switch changes what they
/// match without an error. gate.py's tool_env, whose comment tells how.
pub fn tool_env() -> Vec<(OsString, OsString)> {
    const LEFT_OUT: [&str; 5] =
        ["UPDATE_GOLDEN", "GIT_LITERAL_PATHSPECS", "GIT_GLOB_PATHSPECS", "GIT_NOGLOB_PATHSPECS", "GIT_ICASE_PATHSPECS"];
    const CREDENTIAL_NAME_ENDINGS: [&str; 4] = ["_TOKEN", "_PASSWORD", "_SECRET", "_API_KEY"];
    std::env::vars_os()
        .filter(|(name, _)| {
            let name = name.to_string_lossy();
            !LEFT_OUT.contains(&name.as_ref()) && !CREDENTIAL_NAME_ENDINGS.iter().any(|end| name.ends_with(end))
        })
        .collect()
}

/// `argv` run in `cwd` with [`tool_env`] as its whole environment and no input, in a process group of its own,
/// killed after `timeout`; everything it printed, and how it ended.
///
/// Once it has exited, or been killed, every process left in its group, such as a server it started, is killed too,
/// and what was printed is read until no process holds the output open, for at most [`KILL_WAIT`] more: only one
/// that left the group can still hold it, and what that prints later is lost. So the call returns within `timeout`
/// and [`KILL_WAIT`]. When Ctrl-C reaches the gate meanwhile, the group is killed and the gate exits through
/// [`exit_interrupted`]. Errs when `argv` cannot be started.
///
/// Unlike gate.py, which starts a session, the group stays in the gate's session, so a tool that opens the terminal
/// still can; neither gets the terminal's Ctrl-C, which is the gate's to pass on.
pub fn run(argv: &[&OsStr], cwd: &Path, timeout: Duration) -> std::io::Result<Finished> {
    let (mut child, group) = start(argv, cwd, Stdio::piped(), Stdio::piped())?;
    let stdout = Printed::read(child.stdout.take());
    let stderr = Printed::read(child.stderr.take());
    // Read on their threads while the process runs, so that a full pipe never blocks it.
    let mut status = wait(&mut child, &group, Some(Instant::now() + timeout))?;
    let stopped = status.is_none();
    group.kill();
    let end = Instant::now() + KILL_WAIT;
    while !(stdout.ended() && stderr.ended()) && Instant::now() < end {
        std::thread::sleep(POLL);
    }
    // Killed a moment ago, the process may not have been reaped yet; what is left of the wait is its to exit in.
    while status.is_none() && Instant::now() < end {
        status = child.try_wait()?;
        if status.is_none() {
            std::thread::sleep(POLL);
        }
    }
    Ok(Finished { status: status_code(status), stdout: stdout.take(), stderr: stderr.take(), stopped })
}

/// `argv` run in `cwd` as [`run`] runs it, but with no time limit and what it prints going to the gate's standard
/// error; its exit status, as [`Finished::status`] gives one. Errs when `argv` cannot be started.
pub fn run_to_stderr(argv: &[&OsStr], cwd: &Path) -> std::io::Result<i32> {
    use std::os::fd::AsFd;
    let stdout = Stdio::from(std::io::stderr().as_fd().try_clone_to_owned()?);
    let (mut child, group) = start(argv, cwd, stdout, Stdio::inherit())?;
    let status = wait(&mut child, &group, None)?;
    group.kill();
    Ok(status_code(status))
}

/// `argv` started in `cwd` with [`tool_env`] as its whole environment, no input and the given output, in a process
/// group of its own, which the second value kills when dropped.
fn start(argv: &[&OsStr], cwd: &Path, stdout: Stdio, stderr: Stdio) -> std::io::Result<(Child, Group)> {
    use std::os::unix::process::CommandExt;
    let (program, args) = argv.split_first().ok_or_else(|| std::io::Error::other("no program to run"))?;
    let child = Command::new(program)
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(tool_env())
        .stdin(Stdio::null())
        .stdout(stdout)
        .stderr(stderr)
        .process_group(0)
        .spawn()?;
    let group = Group(child.id());
    Ok((child, group))
}

/// How `child` exited, looking every [`POLL`]; None when it was still running at `deadline`. When Ctrl-C reaches
/// the gate meanwhile, kills `group` and exits through [`exit_interrupted`].
fn wait(child: &mut Child, group: &Group, deadline: Option<Instant>) -> std::io::Result<Option<ExitStatus>> {
    loop {
        if interrupted() {
            group.kill();
            exit_interrupted();
        }
        if let Some(status) = child.try_wait()? {
            return Ok(Some(status));
        }
        let left = deadline.map(|d| d.saturating_duration_since(Instant::now()));
        if left == Some(Duration::ZERO) {
            return Ok(None);
        }
        std::thread::sleep(left.map_or(POLL, |left| POLL.min(left)));
    }
}

/// An exit status as [`Finished::status`] gives it.
fn status_code(status: Option<ExitStatus>) -> i32 {
    use std::os::unix::process::ExitStatusExt;
    status.map_or(-libc::SIGKILL, |s| s.code().unwrap_or_else(|| -s.signal().unwrap_or(libc::SIGKILL)))
}

/// A started process's group, killed when dropped, so that no exit from [`run`], an error's included, leaves a
/// process of it running.
struct Group(u32);

impl Group {
    /// Kills every process left in the group; one already gone, or a group of zombies that macOS refuses, is no error.
    fn kill(&self) {
        let Ok(pid) = libc::pid_t::try_from(self.0) else { return };
        // SAFETY: kill(2) on a process group the gate started; it touches no memory of the gate's.
        #[allow(unsafe_code, reason = "kill(2) has no safe wrapper in std; it reads no memory")]
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

impl Drop for Group {
    fn drop(&mut self) {
        self.kill();
    }
}

/// What a process printed on one stream, read on a thread of its own until no process holds the stream open.
struct Printed {
    bytes: Arc<Mutex<Vec<u8>>>,
    done: Arc<AtomicBool>,
}

impl Printed {
    fn read(stream: Option<impl Read + Send + 'static>) -> Printed {
        let printed = Printed { bytes: Arc::default(), done: Arc::new(AtomicBool::new(stream.is_none())) };
        if let Some(mut stream) = stream {
            let (bytes, done) = (Arc::clone(&printed.bytes), Arc::clone(&printed.done));
            std::thread::spawn(move || {
                let mut chunk = [0u8; 65536];
                while let Ok(n @ 1..) = stream.read(&mut chunk) {
                    bytes.lock().unwrap_or_else(|e| e.into_inner()).extend_from_slice(&chunk[..n]);
                }
                done.store(true, Ordering::SeqCst);
            });
        }
        printed
    }

    fn ended(&self) -> bool {
        self.done.load(Ordering::SeqCst)
    }

    /// What was read so far; a thread still reading keeps the rest.
    fn take(&self) -> Vec<u8> {
        std::mem::take(&mut *self.bytes.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// True once Ctrl-C has reached the gate since [`catch_interrupt`].
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Makes Ctrl-C stop the gate, through [`run`] and [`interrupted`], unless the gate was started with SIGINT ignored,
/// as a command a Jenkins `sh` step runs is: then it stays ignored, for the gate and every process it starts, as for
/// gate.py. Call it once, at the start.
pub fn catch_interrupt() {
    extern "C" fn note(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    // SAFETY: sigaction(2) with a zeroed struct read back, and then a handler that only stores to an atomic, which is
    // async-signal-safe.
    #[allow(unsafe_code, reason = "sigaction(2) has no safe wrapper in std")]
    unsafe {
        let mut was: libc::sigaction = std::mem::zeroed();
        if libc::sigaction(libc::SIGINT, std::ptr::null(), &mut was) != 0 || was.sa_sigaction == libc::SIG_IGN {
            return;
        }
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = note as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::sigemptyset(&mut action.sa_mask);
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
}

/// Whether Ctrl-C has reached the gate, which must then stop through [`exit_interrupted`] before it writes a report.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Ends the gate as Ctrl-C does, saying so on standard error, with [`EXIT_INTERRUPTED`]; no report is written past
/// this. The one place the gate exits other than from main, so that a Ctrl-C deep in a check needs no path back up.
pub fn exit_interrupted() -> ! {
    eprintln!("interrupted; the report was not written whole");
    std::process::exit(EXIT_INTERRUPTED)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sh(script: &str, timeout: Duration) -> Finished {
        let argv: Vec<&OsStr> = ["sh", "-c", script].into_iter().map(OsStr::new).collect();
        run(&argv, Path::new("."), timeout).unwrap()
    }

    #[test]
    fn it_keeps_both_streams_and_the_status() {
        let done = sh("printf 'a\\377'; printf e >&2; exit 3", Duration::from_secs(10));
        assert_eq!((done.status, done.stdout, done.stderr, done.stopped), (3, b"a\xff".to_vec(), b"e".to_vec(), false));
    }

    #[test]
    fn it_stops_a_process_at_its_timeout_with_what_it_started() {
        let start = Instant::now();
        let done = sh("echo early; sleep 30 & sleep 30", Duration::from_millis(300));
        assert!(done.stopped);
        assert_eq!(done.stdout, b"early\n");
        // The background sleep held stdout too: killed with the group, it let the read end at once.
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
    }

    #[test]
    fn it_kills_what_is_left_of_the_group_after_the_exit() {
        let start = Instant::now();
        let done = sh("sleep 30 & echo started", Duration::from_secs(10));
        assert_eq!((done.status, done.stopped), (0, false));
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
    }

    #[test]
    fn credentials_stay_out_of_the_environment() {
        std::env::set_var("GATE_TEST_API_KEY", "secret");
        std::env::set_var("GATE_TEST_TOKENIZER", "kept");
        let names: Vec<OsString> = tool_env().into_iter().map(|(name, _)| name).collect();
        assert!(!names.contains(&OsString::from("GATE_TEST_API_KEY")));
        assert!(names.contains(&OsString::from("GATE_TEST_TOKENIZER")));
    }
}
