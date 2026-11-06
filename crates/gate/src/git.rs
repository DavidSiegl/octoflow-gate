//! git, as the gate asks it about the tree it judges.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use crate::process::{self, Finished};
use crate::text;
use crate::Unjudged;

/// Seconds a git command may take before the gate gives up judging the change.
const TIMEOUT: u64 = 60;

/// `git -C cwd args`, run as [`process::run`] runs a program, for a command whose failure the caller interprets: its
/// exit status and what it printed, as git printed it. Errs when git cannot be started or runs longer than a minute.
pub fn run(cwd: &Path, args: &[&str]) -> Result<Finished, Unjudged> {
    let mut argv: Vec<&OsStr> = vec![OsStr::new("git"), OsStr::new("-C"), cwd.as_os_str()];
    argv.extend(args.iter().map(OsStr::new));
    match process::run(&argv, Path::new("."), Duration::from_secs(TIMEOUT)) {
        Err(e) => Err(Unjudged(format!("{}: {e}", command(cwd, args)))),
        Ok(done) if done.stopped => {
            Err(Unjudged(format!("{}: did not finish within {TIMEOUT} seconds", command(cwd, args))))
        }
        Ok(done) => Ok(done),
    }
}

/// The standard output of `git -C cwd args`, as git printed it. Errs as [`run`] does, and when git exits with a
/// status other than 0.
pub fn output(cwd: &Path, args: &[&str]) -> Result<Vec<u8>, Unjudged> {
    let done = run(cwd, args)?;
    if done.status != 0 {
        let said = text::lossless(&done.stderr);
        return Err(Unjudged(format!("{} failed: {}", command(cwd, args), text::py_strip(&said))));
    }
    Ok(done.stdout)
}

/// `git -C cwd args` as a shell would take it, for a message.
fn command(cwd: &Path, args: &[&str]) -> String {
    let mut words = vec!["git".to_string(), "-C".to_string(), text::lossless(cwd.as_os_str().as_bytes())];
    words.extend(args.iter().map(|a| a.to_string()));
    words.iter().map(|w| shell_quote(w)).collect::<Vec<_>>().join(" ")
}

/// `word` quoted for a POSIX shell as Python's `shlex.quote` quotes it: as it is when it holds only letters, digits
/// and `_@%+=:,./-`, else in single quotes.
pub fn shell_quote(word: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "_@%+=:,./-".contains(c);
    if !word.is_empty() && word.chars().all(plain) {
        return word.to_string();
    }
    format!("'{}'", word.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_quote_is_shlex_s() {
        assert_eq!(shell_quote("a/b.py"), "a/b.py");
        assert_eq!(shell_quote(""), "''");
        assert_eq!(shell_quote("a b"), "'a b'");
        assert_eq!(shell_quote("it's"), "'it'\"'\"'s'");
    }

    #[test]
    fn a_failing_command_names_itself_and_git_s_complaint() {
        let Err(Unjudged(said)) = output(Path::new("/"), &["rev-parse", "--verify", "no such rev"]) else {
            panic!("rev-parse succeeded outside a repository")
        };
        assert!(said.starts_with("git -C / rev-parse --verify 'no such rev' failed: "), "{said}");
    }
}
