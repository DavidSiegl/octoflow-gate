//! Running the programs the gate judges a change with, git, a linter or a test command: each in a session of its
//! own, killed with all it started when it overruns or when Ctrl-C or SIGTERM stops the gate, and the gate's own exit
//! on those.

use std::ffi::{OsStr, OsString};
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Seconds a linter or a test command may run before the gate stops it, and judges it as not having finished.
pub const TOOL_TIMEOUT: u64 = 600;

/// The most the gate waits, after a process has exited or been killed, for the rest of what its group printed and for
/// the process to be reaped.
const KILL_WAIT: Duration = Duration::from_secs(5);

/// How often the gate looks whether a process has exited, so the most it waits on past the exit.
const POLL: Duration = Duration::from_millis(50);

/// The gate's exit status when Ctrl-C or SIGTERM stopped it, having written no report: 128 + SIGINT, as a shell
/// reports Ctrl-C; SIGTERM gives the same, so that a caller has one status to tell a stop by.
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

impl Finished {
    /// What a tool that went wrong, such as cargo, said about it, in a line cut as text::clip cuts one: the
    /// exception of a pytest internal error, else the exception that ended it when it printed a traceback, else the
    /// first line of its standard error, else the last line of its output; "nothing" when it printed nothing. Only
    /// `\n` ends a line, so a path in it stays whole.
    pub fn said(&self) -> String {
        let lines = |printed: &[u8]| -> Vec<String> {
            let printed = crate::text::lossless(printed);
            printed.split('\n').map(crate::text::py_strip).filter(|l| !l.is_empty()).map(String::from).collect()
        };
        let (err, out) = (lines(&self.stderr), lines(&self.stdout));
        let internal = out.iter().filter_map(|l| l.strip_prefix("INTERNALERROR>")).next_back();
        let internal = internal.map(crate::text::py_strip);
        let traceback = self.stderr.windows(9).any(|w| w == b"Traceback");
        let said = match (internal, err.first(), err.last(), out.last()) {
            (Some(internal), ..) => internal,
            (None, Some(_), Some(last), _) if traceback => last,
            (None, Some(first), ..) => first,
            (None, None, _, Some(last)) => last,
            _ => "nothing",
        };
        crate::text::clip(said)
    }
}

/// The environment for every process the gate starts, git, a linter or a test command: the gate's own, without the
/// variables whose names end in `_TOKEN`, `_PASSWORD`, `_SECRET` or `_API_KEY`, without UPDATE_GOLDEN, and without
/// git's switches for how it reads every pathspec.
///
/// So a test the change brings never inherits a CI's or a service's tokens when the gate runs where they are set. A
/// golden test that rewrites its file under UPDATE_GOLDEN=1 instead of comparing with it could not fail in the
/// flaky-tests reruns, the passes-at-base run or a mutation run. And the gate's pathspecs are written in git's default
/// syntax, which each switch breaks without an error: under GIT_LITERAL_PATHSPECS the linters' globs and
/// judge::Change::from_tree's exclude match nothing, under GIT_NOGLOB_PATHSPECS the globs match nothing, under
/// GIT_GLOB_PATHSPECS `*.py` matches only the top level's files, and under GIT_ICASE_PATHSPECS the exclude leaves out
/// a `Mutants/` that is not mutmut's.
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
/// killed after `timeout`; everything it printed, and how it ended. [`run_with`] with [`Extras::default`].
pub fn run(argv: &[&OsStr], cwd: &Path, timeout: Duration) -> std::io::Result<Finished> {
    run_with(argv, cwd, timeout, &Extras::default())
}

/// What a process [`run_with`] runs takes beside what [`run`] gives every process.
#[derive(Clone, Copy, Debug, Default)]
pub struct Extras<'a> {
    /// Variables set on top of [`tool_env`], replacing any of the same name.
    pub env: &'a [(&'a str, &'a OsStr)],
    /// When given, a process that overruns its timeout is first sent SIGINT, as Ctrl-C would, and given this long to
    /// exit before it is killed; it starts with SIGINT's default disposition even where the gate ignores SIGINT, as a
    /// command a Jenkins `sh` step runs does, so that the signal reaches it. For a tool that writes what it has done
    /// so far when interrupted, as cargo-mutants does.
    pub soft_stop: Option<Duration>,
    /// What the process reads on its standard input, which ends after it; empty for none.
    pub input: &'a [u8],
}

/// `argv` run in `cwd` with [`tool_env`] and `extras.env` as its whole environment and `extras.input` as its input,
/// in a process group of its own, stopped after `timeout`, as `extras.soft_stop` says; everything it printed, and how
/// it ended.
///
/// Once it has exited, or been killed, every process left in its group, such as a server it started, is killed too, and
/// what was printed is read until no process holds the output open, and the input written until none holds it open, for
/// at most [`KILL_WAIT`] more: only one that left the group can still hold them, and what that prints later is lost. So
/// the call returns within `timeout`, the soft stop's grace and [`KILL_WAIT`]. When Ctrl-C or SIGTERM reaches the gate
/// meanwhile, the group is killed and the gate exits through [`exit_interrupted`]. Errs when `argv` cannot be started,
/// and when its input cannot be written for a reason other than the process ending before it read it all.
///
/// The process starts a session of its own, so it has no controlling terminal: the terminal's Ctrl-C is the gate's
/// to pass on, and a tool that would read the terminal, or write to it under `stty tostop`, fails rather than
/// stopping the gate with it.
pub fn run_with(argv: &[&OsStr], cwd: &Path, timeout: Duration, extras: &Extras) -> std::io::Result<Finished> {
    let (mut child, group) = start(argv, cwd, Stdio::piped(), Stdio::piped(), extras)?;
    let stdout = Printed::read(child.stdout.take());
    let stderr = Printed::read(child.stderr.take());
    // Written on a thread of its own, so that a process that prints before it has read all its input never blocks
    // on a full pipe. One that exits or is killed before reading it all fails the write with a broken pipe, which
    // ends the thread; how the process ended says what went wrong.
    let writer = child.stdin.take().map(|mut stdin| {
        let input = extras.input.to_vec();
        std::thread::spawn(move || stdin.write_all(&input))
    });
    // Read on their threads while the process runs, so that a full pipe never blocks it.
    let mut status = wait(&mut child, &group, Some(Instant::now() + timeout))?;
    let stopped = status.is_none();
    if let (true, Some(grace), Ok(pid)) = (stopped, extras.soft_stop, libc::pid_t::try_from(child.id())) {
        // SAFETY: kill(2) on the process the gate started, not yet reaped, so its pid is still its own.
        #[allow(unsafe_code, reason = "kill(2) has no safe wrapper in std; it reads no memory")]
        // A failed kill means the process already exited; the wait below finds that out, so its result is ignored.
        unsafe {
            libc::kill(pid, libc::SIGINT);
        }
        status = wait(&mut child, &group, Some(Instant::now() + grace))?;
    }
    group.kill();
    let end = Instant::now() + KILL_WAIT;
    let written = || writer.as_ref().is_none_or(std::thread::JoinHandle::is_finished);
    while !(stdout.ended() && stderr.ended() && written()) && Instant::now() < end {
        std::thread::sleep(POLL);
    }
    // Killed a moment ago, the process may not have been reaped yet; what is left of the wait is its to exit in.
    while status.is_none() && Instant::now() < end {
        status = child.try_wait()?;
        if status.is_none() {
            std::thread::sleep(POLL);
        }
    }
    // A writer still running is writing to a process that left the group; it is left to end with the gate.
    if let Some(writer) = writer.filter(std::thread::JoinHandle::is_finished) {
        let written = writer.join().map_err(|_| std::io::Error::other("the thread writing the input panicked"))?;
        written.or_else(|e| if e.kind() == std::io::ErrorKind::BrokenPipe { Ok(()) } else { Err(e) })?;
    }
    Ok(Finished { status: status_code(status), stdout: stdout.take(), stderr: stderr.take(), stopped })
}

/// `argv` run in `cwd` as [`run`] runs it, but with no time limit and what it prints going to the gate's standard
/// error; its exit status, as [`Finished::status`] gives one. Errs when `argv` cannot be started.
pub fn run_to_stderr(argv: &[&OsStr], cwd: &Path) -> std::io::Result<i32> {
    use std::os::fd::AsFd;
    let stdout = Stdio::from(std::io::stderr().as_fd().try_clone_to_owned()?);
    let (mut child, group) = start(argv, cwd, stdout, Stdio::inherit(), &Extras::default())?;
    let status = wait(&mut child, &group, None)?;
    group.kill();
    Ok(status_code(status))
}

/// `argv` started in `cwd` with [`tool_env`] and `extras.env` as its whole environment, a pipe for its input when
/// `extras.input` holds any and none otherwise, and the given output, in a session, and so a process group, of its
/// own, which the second value kills when dropped; with SIGINT's default disposition when `extras` has a soft stop.
fn start(argv: &[&OsStr], cwd: &Path, stdout: Stdio, stderr: Stdio, extras: &Extras) -> std::io::Result<(Child, Group)> {
    use std::os::unix::process::CommandExt;
    let (program, args) = argv.split_first().ok_or_else(|| std::io::Error::other("no program to run"))?;
    let mut command = Command::new(program);
    command
        .args(args)
        .current_dir(cwd)
        .env_clear()
        .envs(tool_env())
        .envs(extras.env.iter().copied())
        .stdin(if extras.input.is_empty() { Stdio::null() } else { Stdio::piped() })
        .stdout(stdout)
        .stderr(stderr);
    let default_sigint = extras.soft_stop.is_some();
    // SAFETY: setsid(2) and signal(2) are async-signal-safe, so they may run between fork and exec; they touch no
    // memory. An ignored signal stays ignored across exec, a handled one is reset to the default.
    #[allow(unsafe_code, reason = "setsid(2) and signal(2) between fork and exec have no safe wrapper in std")]
    unsafe {
        command.pre_exec(move || {
            if libc::setsid() == -1
                || (default_sigint && libc::signal(libc::SIGINT, libc::SIG_DFL) == libc::SIG_ERR)
            {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    let child = command.spawn()?;
    let group = Group(child.id());
    Ok((child, group))
}

/// How `child` exited, looking every [`POLL`]; None when it was still running at `deadline`. When Ctrl-C or SIGTERM
/// reaches the gate meanwhile, kills `group`, reaps `child` within [`KILL_WAIT`] and exits through
/// [`exit_interrupted`]: unreaped, `child` would stay a zombie of `group` after the gate's exit until whatever adopts
/// it reaps it. One that outlives the wait, as in an uninterruptible sleep, is left to its adopter.
fn wait(child: &mut Child, group: &Group, deadline: Option<Instant>) -> std::io::Result<Option<ExitStatus>> {
    loop {
        if interrupted() {
            group.kill();
            let end = Instant::now() + KILL_WAIT;
            // An error, as a child already reaped, ends the wait as its exit would; the gate is exiting either way.
            while matches!(child.try_wait(), Ok(None)) && Instant::now() < end {
                std::thread::sleep(POLL);
            }
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

/// True once Ctrl-C or SIGTERM has reached the gate since [`catch_interrupt`].
static INTERRUPTED: AtomicBool = AtomicBool::new(false);

/// Makes Ctrl-C (SIGINT) and SIGTERM, as `timeout` sends, stop the gate, through [`run`] and [`interrupted`], with
/// what it started killed, unless the gate was started with that signal ignored, as a command a Jenkins `sh` step runs
/// is SIGINT: then it stays ignored, for the gate and every process it starts. Call it once, at the start. Errs when a
/// handler cannot be installed, as the signal would then leave the processes the gate starts running.
pub fn catch_interrupt() -> std::io::Result<()> {
    extern "C" fn note(_: libc::c_int) {
        INTERRUPTED.store(true, Ordering::SeqCst);
    }
    for signal in [libc::SIGINT, libc::SIGTERM] {
        // SAFETY: sigaction(2) with a zeroed struct read back, and then a handler that only stores to an atomic,
        // which is async-signal-safe.
        #[allow(unsafe_code, reason = "sigaction(2) has no safe wrapper in std")]
        unsafe {
            let mut was: libc::sigaction = std::mem::zeroed();
            if libc::sigaction(signal, std::ptr::null(), &mut was) != 0 {
                return Err(std::io::Error::last_os_error());
            }
            if was.sa_sigaction == libc::SIG_IGN {
                continue;
            }
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = note as extern "C" fn(libc::c_int) as libc::sighandler_t;
            if libc::sigemptyset(&mut action.sa_mask) != 0
                || libc::sigaction(signal, &action, std::ptr::null_mut()) != 0
            {
                return Err(std::io::Error::last_os_error());
            }
        }
    }
    Ok(())
}

/// Whether Ctrl-C or SIGTERM has reached the gate, which must then stop through [`exit_interrupted`] before it writes
/// a report.
pub fn interrupted() -> bool {
    INTERRUPTED.load(Ordering::SeqCst)
}

/// Ends the gate as Ctrl-C or SIGTERM does, saying so on standard error, with [`EXIT_INTERRUPTED`]; no report is
/// written past this. The one place the gate exits other than from main, so that a stop deep in a check needs no path
/// back up.
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
    fn the_input_is_read_whole_however_long_and_none_is_an_empty_one() {
        let argv: Vec<&OsStr> = ["sh", "-c", "cat; echo end"].into_iter().map(OsStr::new).collect();
        // More than a pipe holds, which cat prints back before it has read the rest.
        let input: Vec<u8> = (0..1 << 20).map(|n: u32| n.to_le_bytes()[0]).collect();
        let extras = Extras { input: &input, ..Extras::default() };
        let done = run_with(&argv, Path::new("."), Duration::from_secs(10), &extras).unwrap();
        assert_eq!((done.status, done.stdout.len()), (0, input.len() + 4));
        assert!(done.stdout.starts_with(&input));
        assert_eq!(sh("cat; echo end", Duration::from_secs(10)).stdout, b"end\n");
    }

    #[test]
    fn a_process_that_ends_before_reading_its_input_is_judged_by_how_it_ended() {
        let argv: Vec<&OsStr> = ["sh", "-c", "exit 4"].into_iter().map(OsStr::new).collect();
        let input = vec![b'x'; 1 << 20];
        let extras = Extras { input: &input, ..Extras::default() };
        let done = run_with(&argv, Path::new("."), Duration::from_secs(10), &extras).unwrap();
        assert_eq!(done.status, 4);
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
    fn a_soft_stop_interrupts_first_and_kills_after_the_grace() {
        let argv: Vec<&OsStr> = ["sh", "-c", "trap 'echo interrupted; exit 7' INT; echo $GATE_TEST_EXTRA; \
                                  while :; do sleep 0.05; done"]
            .into_iter()
            .map(OsStr::new)
            .collect();
        let env = [("GATE_TEST_EXTRA", OsStr::new("set"))];
        let extras = Extras { env: &env, soft_stop: Some(Duration::from_secs(5)), ..Extras::default() };
        let done = run_with(&argv, Path::new("."), Duration::from_millis(300), &extras).unwrap();
        assert_eq!((done.status, done.stopped), (7, true));
        assert_eq!(done.stdout, b"set\ninterrupted\n");
        let deaf: Vec<&OsStr> = ["sh", "-c", "trap '' INT; sleep 30"].into_iter().map(OsStr::new).collect();
        let start = Instant::now();
        let extras = Extras { soft_stop: Some(Duration::from_millis(200)), ..Extras::default() };
        let done = run_with(&deaf, Path::new("."), Duration::from_millis(200), &extras).unwrap();
        assert_eq!((done.status, done.stopped), (-libc::SIGKILL, true));
        assert!(start.elapsed() < Duration::from_secs(3), "{:?}", start.elapsed());
    }

    #[test]
    fn what_a_tool_said_is_its_error_s_first_line_or_its_exception() {
        let said = |stdout: &str, stderr: &str| {
            Finished { status: 1, stdout: stdout.into(), stderr: stderr.into(), stopped: false }.said()
        };
        assert_eq!(said("", ""), "nothing");
        assert_eq!(said("a\n b \n\n", ""), "b");
        assert_eq!(said("out", "\n first\nsecond\n"), "first");
        assert_eq!(said("", "Traceback (most recent call last):\n  x\nValueError: no\n"), "ValueError: no");
        assert_eq!(said("INTERNALERROR> a\nINTERNALERROR>  KeyError: k\n", "err"), "KeyError: k");
        let long = said("", &"é".repeat(200));
        assert_eq!((long.chars().count(), long.ends_with("é…")), (160, true));
    }

    #[test]
    fn it_runs_in_a_session_of_its_own() {
        let done = sh("ps -o pid= -o sess= -p $$", Duration::from_secs(10));
        let ids: Vec<&str> = std::str::from_utf8(&done.stdout).unwrap().split_whitespace().collect();
        assert_eq!(ids.len(), 2, "{ids:?}");
        assert_eq!(ids[0], ids[1]);
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
