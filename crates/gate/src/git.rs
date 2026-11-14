//! git, as the gate asks it about the tree it judges.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;
use std::time::Duration;

use crate::process::{self, Extras, Finished};
use crate::text;
use crate::Unjudged;

/// Seconds a git command may take before the gate gives up judging the change; a command that reads several objects
/// may take this long for each.
const TIMEOUT: u64 = 60;

/// `git -C cwd args`, run as [`process::run`] runs a program, for a command whose failure the caller interprets: its
/// exit status and what it printed, as git printed it. Errs when git cannot be started or runs longer than a minute.
pub fn run<S: AsRef<OsStr>>(cwd: &Path, args: &[S]) -> Result<Finished, Unjudged> {
    run_fed(cwd, args, &[], TIMEOUT)
}

/// [`run`] with `input` on git's standard input, and `seconds` in place of its minute.
fn run_fed<S: AsRef<OsStr>>(cwd: &Path, args: &[S], input: &[u8], seconds: u64) -> Result<Finished, Unjudged> {
    let mut argv: Vec<&OsStr> = vec![OsStr::new("git"), OsStr::new("-C"), cwd.as_os_str()];
    argv.extend(args.iter().map(AsRef::as_ref));
    let extras = Extras { input, ..Extras::default() };
    match process::run_with(&argv, Path::new("."), Duration::from_secs(seconds), &extras) {
        Err(e) => Err(Unjudged(format!("{}: {e}", command(cwd, args)))),
        Ok(done) if done.stopped => {
            Err(Unjudged(format!("{}: did not finish within {seconds} seconds", command(cwd, args))))
        }
        Ok(done) => Ok(done),
    }
}

/// The standard output of `git -C cwd args`, as git printed it. Errs as [`run`] does, and when git exits with a
/// status other than 0.
pub fn output<S: AsRef<OsStr>>(cwd: &Path, args: &[S]) -> Result<Vec<u8>, Unjudged> {
    output_fed(cwd, args, &[], TIMEOUT)
}

/// [`output`] with `input` on git's standard input, and `seconds` in place of its minute.
fn output_fed<S: AsRef<OsStr>>(cwd: &Path, args: &[S], input: &[u8], seconds: u64) -> Result<Vec<u8>, Unjudged> {
    let done = run_fed(cwd, args, input, seconds)?;
    if done.status != 0 {
        let said = text::lossless(&done.stderr);
        return Err(Unjudged(format!("{} failed: {}", command(cwd, args), text::py_strip(&said))));
    }
    Ok(done.stdout)
}

/// The contents of the objects of the repository at `cwd` that `ids`, their full object ids in hex, name, in the
/// order given, all read by one `git cat-file --batch`, which may take a minute for each object, as long as a git
/// command per object would have: a partial clone fetches each missing object from its remote in turn. Errs as
/// [`output`] does, and when the repository has no object of one of the ids.
pub fn objects(cwd: &Path, ids: &[&[u8]]) -> Result<Vec<Vec<u8>>, Unjudged> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let args = ["cat-file", "--batch"];
    let input: Vec<u8> = ids.iter().flat_map(|id| [*id, b"\n"]).flatten().copied().collect();
    let seconds = TIMEOUT.saturating_mul(ids.len() as u64);
    let printed = output_fed(cwd, &args, &input, seconds)?;
    let failed = |why: String| Unjudged(format!("{}: {why}", command(cwd, &args)));
    let mut rest = printed.as_slice();
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        // Each object is printed as `<id> <type> <size>\n<contents>\n`, one git does not have as `<id> missing\n`.
        let end = rest.iter().position(|&b| b == b'\n').unwrap_or(rest.len());
        let header = &rest[..end];
        let size = header.rsplit(|&b| b == b' ').next().and_then(|n| std::str::from_utf8(n).ok()?.parse().ok());
        let contents = size.filter(|_| header.starts_with(id)).and_then(|n: usize| rest.get(end + 1..end + 1 + n));
        let Some(contents) = contents else {
            return Err(failed(format!("printed `{}` for {}", text::lossless(header), text::lossless(id))));
        };
        out.push(contents.to_vec());
        rest = rest.get(end + 2 + contents.len()..).unwrap_or_default();
    }
    Ok(out)
}

/// `git -C cwd args` as a shell would take it, for a message.
fn command<S: AsRef<OsStr>>(cwd: &Path, args: &[S]) -> String {
    let mut words = vec!["git".to_string(), "-C".to_string(), text::lossless(cwd.as_os_str().as_bytes())];
    words.extend(args.iter().map(|a| text::lossless(a.as_ref().as_bytes())));
    words.iter().map(|w| shell_quote(w)).collect::<Vec<_>>().join(" ")
}

/// `word` quoted for a POSIX shell as Python's `shlex.quote` quotes it: as it is when it holds only letters, digits
/// and `_@%+=:,./-`, else in single quotes ([`shell_quote_bytes`]).
pub fn shell_quote(word: &str) -> String {
    String::from_utf8(shell_quote_bytes(word.as_bytes())).expect("quoting adds only ASCII to UTF-8")
}

/// The bytes `word`, such as a path that need not be UTF-8, quoted for a POSIX shell as [`shell_quote`] says, each
/// byte kept as it is.
pub fn shell_quote_bytes(word: &[u8]) -> Vec<u8> {
    let plain = |b: &u8| b.is_ascii_alphanumeric() || b"_@%+=:,./-".contains(b);
    if !word.is_empty() && word.iter().all(plain) {
        return word.to_vec();
    }
    let mut out = b"'".to_vec();
    for &b in word {
        match b {
            b'\'' => out.extend(b"'\"'\"'"),
            b => out.push(b),
        }
    }
    out.push(b'\'');
    out
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
        assert_eq!(shell_quote_bytes(b"/tmp/a\xff b"), b"'/tmp/a\xff b'");
    }

    #[test]
    fn objects_are_read_in_the_order_asked_and_a_missing_one_is_an_error() {
        let dir = std::env::temp_dir().join(format!("gate-git-objects-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        output(&dir, &["init", "--quiet"]).unwrap();
        let mut ids = vec![];
        for contents in ["one\n", "", "two\nlines\n"] {
            let file = dir.join("f");
            std::fs::write(&file, contents).unwrap();
            let id = output(&dir, &[OsStr::new("hash-object"), OsStr::new("-w"), file.as_os_str()]).unwrap();
            ids.push(id.trim_ascii().to_vec());
        }
        let asked: Vec<&[u8]> = [2, 0, 1, 2].iter().map(|&k| ids[k].as_slice()).collect();
        let read = objects(&dir, &asked);
        let missing = objects(&dir, &[ids[0].as_slice(), &[b'0'; 40]]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert_eq!(read.unwrap(), [&b"two\nlines\n"[..], b"one\n", b"", b"two\nlines\n"]);
        let Err(Unjudged(said)) = missing else { panic!("a missing object was read") };
        assert!(said.ends_with(&format!("cat-file --batch: printed `{0} missing` for {0}", "0".repeat(40))), "{said}");
    }

    #[test]
    fn a_failing_command_names_itself_and_git_s_complaint() {
        let Err(Unjudged(said)) = output(Path::new("/"), &["rev-parse", "--verify", "no such rev"]) else {
            panic!("rev-parse succeeded outside a repository")
        };
        assert!(said.starts_with("git -C / rev-parse --verify 'no such rev' failed: "), "{said}");
    }
}
