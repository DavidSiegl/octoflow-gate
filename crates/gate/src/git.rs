//! git, as the gate asks it about the tree it judges.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::process::{self, Extras, Finished};
use crate::scratch_directory::ScratchDirectory;
use crate::text;
use crate::Unjudged;

/// Seconds a git command may take before the gate gives up judging the change; a command that reads several objects
/// may take this long for each.
const TIMEOUT: u64 = 60;

/// `git -C cwd args`, run as [`process::run`] runs a program, for a command whose failure the caller interprets: its
/// exit status and what it printed, as git printed it. Errs when git cannot be started or runs longer than a minute.
pub fn run<S: AsRef<OsStr>>(cwd: &Path, args: &[S]) -> Result<Finished, Unjudged> {
    run_fed(cwd, args, &Fed::default())
}

/// What a git command [`run_fed`] runs takes beside its arguments.
struct Fed<'a> {
    /// What git reads on its standard input; empty for nothing.
    input: &'a [u8],
    /// The index git reads and writes in place of the repository's own; None for the repository's own.
    index: Option<&'a OsStr>,
    /// Seconds the command may take.
    seconds: u64,
}

impl Default for Fed<'_> {
    fn default() -> Self {
        Fed { input: &[], index: None, seconds: TIMEOUT }
    }
}

/// [`run`] with what `fed` says in place of its nothing on standard input, the repository's index and a minute.
fn run_fed<S: AsRef<OsStr>>(cwd: &Path, args: &[S], fed: &Fed) -> Result<Finished, Unjudged> {
    let mut argv: Vec<&OsStr> = vec![OsStr::new("git"), OsStr::new("-C"), cwd.as_os_str()];
    argv.extend(args.iter().map(AsRef::as_ref));
    let env: Vec<(&str, &OsStr)> = fed.index.map(|index| ("GIT_INDEX_FILE", index)).into_iter().collect();
    let (input, seconds) = (fed.input, fed.seconds);
    let extras = Extras { input, env: &env, ..Extras::default() };
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
    output_fed(cwd, args, &Fed::default())
}

/// [`output`] as [`run_fed`] runs git.
fn output_fed<S: AsRef<OsStr>>(cwd: &Path, args: &[S], fed: &Fed) -> Result<Vec<u8>, Unjudged> {
    let done = run_fed(cwd, args, fed)?;
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
    let printed = output_fed(cwd, &args, &Fed { input: &input, seconds, ..Fed::default() })?;
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

/// The files of the working tree at `root`, a repository's top level, that the gate judges, by their paths from
/// `root` in byte order, each once: those git tracks, whether or not the working tree still holds them, and those it
/// neither tracks nor ignores ([`untracked_files`]), so that a change judged before it is committed is judged whole.
/// Errs as [`output`] does.
pub fn judged_files(root: &Path) -> Result<Vec<OsString>, Unjudged> {
    let listed = output(root, &["ls-files", "-z", "--cached", "--others", "--exclude-standard"])?;
    let mut out = names(&listed);
    out.sort();
    out.dedup();
    Ok(out)
}

/// The files of the working tree at `root`, a repository's top level, that git neither tracks nor ignores, by their
/// paths from `root`, in git's order: the files git would add for `git add --all`, but that a directory holding a
/// repository of its own is the one path `<dir>/`, for all it holds. Errs as [`output`] does.
pub fn untracked_files(root: &Path) -> Result<Vec<OsString>, Unjudged> {
    Ok(names(&output(root, &["ls-files", "-z", "--others", "--exclude-standard"])?))
}

/// The paths of a `git ls-files -z`'s output, `listed`, in its order.
fn names(listed: &[u8]) -> Vec<OsString> {
    listed.split(|&b| b == 0).filter(|n| !n.is_empty()).map(|n| OsString::from_vec(n.to_vec())).collect()
}

/// The working tree of a repository as git sees it once each of its [`untracked_files`] is added with `git add
/// --intent-to-add`: a command [`UntrackedAdded::output`] runs, such as `git diff`, takes those files for added ones,
/// beside what git tracks, and no others. A directory holding a repository of its own stays untracked. The
/// repository's own index is never written: the commands read a copy of it, which goes when this value does.
pub struct UntrackedAdded {
    root: PathBuf,
    /// The copy of the repository's index with the untracked files added, and the directory of the gate's own that
    /// holds it; None when the working tree has no file to add, and the commands read the repository's own.
    index: Option<(OsString, ScratchDirectory)>,
}

impl UntrackedAdded {
    /// The working tree of the repository whose top level is `root`, as it is now. Errs when git cannot find or add
    /// the files, or, when there are some, the index cannot be copied to a directory of the gate's own in the
    /// system's temporary one.
    pub fn new(root: &Path) -> Result<UntrackedAdded, Unjudged> {
        let added: Vec<OsString> =
            untracked_files(root)?.into_iter().filter(|n| !n.as_bytes().ends_with(b"/")).collect();
        if added.is_empty() {
            return Ok(UntrackedAdded { root: root.to_path_buf(), index: None });
        }
        let dir = ScratchDirectory::new(&std::env::temp_dir())
            .map_err(|e| Unjudged(format!("could not make a directory for a copy of git's index: {e}")))?;
        let index = dir.path().join("index").into_os_string();
        let own = root.join(OsString::from_vec(first_line(output(root, &["rev-parse", "--git-path", "index"])?)));
        copy_index(&own, Path::new(&index))?;
        let input: Vec<u8> = added.iter().flat_map(|n| [n.as_bytes(), b"\0"]).flatten().copied().collect();
        // --sparse: outside a sparse checkout's cone git lists untracked files but refuses to add them without it.
        let args = ["--literal-pathspecs", "add", "--intent-to-add", "--sparse", "--pathspec-from-file=-",
                    "--pathspec-file-nul"];
        output_fed(root, &args, &Fed { input: &input, index: Some(&index), ..Fed::default() })?;
        Ok(UntrackedAdded { root: root.to_path_buf(), index: Some((index, dir)) })
    }

    /// The standard output of `git -C root args`, `root` the top level [`UntrackedAdded::new`] was given, run on its
    /// working tree with the untracked files added. Errs as [`output`] does.
    pub fn output<S: AsRef<OsStr>>(&self, args: &[S]) -> Result<Vec<u8>, Unjudged> {
        let index = self.index.as_ref().map(|(index, _)| index.as_os_str());
        output_fed(&self.root, args, &Fed { index, ..Fed::default() })
    }
}

/// Copies the index file at `own` to `copy` with its modification time, by which git tells a file it must read again
/// from one whose recorded state it may trust (https://git-scm.com/docs/racy-git); none at `own`, as in a repository
/// with nothing added yet, copies nothing, which git reads as an empty index. Errs when the copy cannot be made.
fn copy_index(own: &Path, copy: &Path) -> Result<(), Unjudged> {
    let failed = |e: std::io::Error| {
        Unjudged(format!("could not copy git's index {}: {e}", text::path(own.as_os_str())))
    };
    let modified = match std::fs::metadata(own) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        meta => meta.and_then(|m| m.modified()).map_err(failed)?,
    };
    std::fs::copy(own, copy).map_err(failed)?;
    let file = std::fs::File::options().write(true).open(copy).map_err(failed)?;
    file.set_modified(modified).map_err(failed)
}

/// What git printed, `printed`, up to the newline that ends its one line.
pub fn first_line(mut printed: Vec<u8>) -> Vec<u8> {
    if let Some(end) = printed.iter().position(|&b| b == b'\n') {
        printed.truncate(end);
    }
    printed
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
    fn untracked_files_are_judged_and_diffed_as_added_but_for_ignored_ones_and_a_repository_of_their_own() {
        let scratch = ScratchDirectory::new(&std::env::temp_dir()).unwrap();
        let dir = scratch.path().to_path_buf();
        std::fs::create_dir_all(dir.join("nested")).unwrap();
        let git = |cwd: &Path, args: &[&str]| output(cwd, args).unwrap();
        git(&dir, &["init", "--quiet"]);
        std::fs::write(dir.join(".gitignore"), "ignored.txt\n").unwrap();
        std::fs::write(dir.join("old.txt"), "one\ntwo\nthree\nfour\n").unwrap();
        git(&dir, &["add", "-A"]);
        git(&dir, &[&COMMITTER[..], &["commit", "--quiet", "-m", "base"]].concat());
        std::fs::rename(dir.join("old.txt"), dir.join("moved.txt")).unwrap();
        std::fs::write(dir.join("ignored.txt"), "").unwrap();
        std::fs::write(dir.join("new.txt"), "new\n").unwrap();
        git(&dir.join("nested"), &["init", "--quiet"]);
        std::fs::write(dir.join("nested/inner.txt"), "").unwrap();
        let index = std::fs::read(dir.join(".git/index")).unwrap();

        let judged = judged_files(&dir).unwrap();
        let tree = UntrackedAdded::new(&dir).unwrap();
        let diffed = tree.output(&["diff", "--name-status", "-M", "HEAD"]).unwrap();
        let after = std::fs::read(dir.join(".git/index")).unwrap();
        let names: Vec<&str> = judged.iter().map(|n| n.to_str().unwrap()).collect();
        assert_eq!(names, [".gitignore", "moved.txt", "nested/", "new.txt", "old.txt"]);
        assert_eq!(String::from_utf8(diffed).unwrap(), "R100\told.txt\tmoved.txt\nA\tnew.txt\n");
        assert_eq!(after, index, "the repository's own index changed");
    }

    /// The `git -c` arguments a test's commit is made with, whatever the user's configuration.
    const COMMITTER: [&str; 6] = ["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false"];

    #[test]
    fn an_untracked_file_outside_a_sparse_checkout_s_cone_is_diffed_as_added() {
        let scratch = ScratchDirectory::new(&std::env::temp_dir()).unwrap();
        let dir = scratch.path();
        let git = |args: &[&str]| output(dir, args).unwrap();
        git(&["init", "--quiet"]);
        for path in ["a/kept.txt", "b/out.txt"] {
            std::fs::create_dir_all(dir.join(path).parent().unwrap()).unwrap();
            std::fs::write(dir.join(path), "").unwrap();
        }
        git(&["add", "-A"]);
        git(&[&COMMITTER[..], &["commit", "--quiet", "-m", "base"]].concat());
        git(&["sparse-checkout", "set", "--cone", "a"]);
        std::fs::create_dir_all(dir.join("b")).unwrap();
        std::fs::write(dir.join("b/new.txt"), "new\n").unwrap();

        let tree = UntrackedAdded::new(dir).unwrap();
        let diffed = tree.output(&["diff", "--name-status", "HEAD"]).unwrap();
        assert_eq!(String::from_utf8(diffed).unwrap(), "A\tb/new.txt\n");
    }

    #[test]
    fn a_failing_command_names_itself_and_git_s_complaint() {
        let Err(Unjudged(said)) = output(Path::new("/"), &["rev-parse", "--verify", "no such rev"]) else {
            panic!("rev-parse succeeded outside a repository")
        };
        assert!(said.starts_with("git -C / rev-parse --verify 'no such rev' failed: "), "{said}");
    }
}
