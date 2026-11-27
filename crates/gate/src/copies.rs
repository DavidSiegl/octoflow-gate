//! What the checks that run a project's tools share: the copies of a tree they run them in, so that nothing they
//! build or write lands in the tree the gate judges (a scratch directory of the gate's own, a copy of the working
//! tree's files, and a copy of the base with the change's tests in it); the budget they run within; and the
//! shape and order of their findings.

use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::change::Change;
use crate::process::KILL_WAIT;
use crate::report::Finding;
use crate::scratch_directory::ScratchDirectory;
use crate::{git, text, Unjudged};

/// What a check runs in a copy of a tree, set up before the copy is made: the findings it gives there, in no order.
pub type OnCopy<'a> = Box<dyn FnOnce(&ScratchCopy) -> Vec<Finding> + 'a>;

/// A copy of a tree that a check's runs share, made by [`in_working_tree_copy`] or [`in_base_copy`], which remove it
/// with all it holds once the runs are done. A run may change the copy, so a copy never serves two checks.
pub struct ScratchCopy {
    /// The copy's top.
    pub tree: PathBuf,
    /// A private directory beside the copy, empty when the first run starts, for what the runs write outside the copy:
    /// reports, built tests, configuration. The runs on one copy share it, so each names what it writes there after
    /// its own tool, as `gremlins-0`.
    pub scratch: PathBuf,
}

/// The findings of `runs`, each run in turn on one copy of the files the gate judges in `change`'s working tree
/// (Change::judged), as they are there now: a symbolic link is copied as a link, a submodule, or a directory holding a
/// repository git does not track, as its directory, and a listed file the working tree no longer holds is left out.
/// No copy is made when `runs` is empty.
///
/// The copy is removed afterwards, also when a run panics or the gate is stopped meanwhile. `rule` is the check's own,
/// without its name, for a copy that cannot be made, which is then the one finding, no run having been called: it ends
/// in `unjudged`, a clause saying what was not judged, as "the changed tests were not rerun". A copy that cannot be
/// removed is a `not-removed` finding after the runs'.
pub fn in_working_tree_copy(change: &Change, rule: &str, unjudged: &str, runs: Vec<OnCopy>) -> Vec<Finding> {
    in_copy("working tree", rule, unjudged, runs, |_, tree| copy_working_tree(change, tree).map(|()| vec![]))
}

/// The findings of `runs`, each run in turn on one copy of the base of `change` with each file of the change's that
/// `beside` accepts, a repository path that belongs with the tests rather than with the code they test, as the
/// working tree has it, as [`copy_base_with_tests`] writes them. No copy is made when `runs` is empty.
///
/// Submodules the copy leaves empty are one finding of `rule` before the runs', naming each and why. Otherwise as
/// [`in_working_tree_copy`] says.
pub fn in_base_copy(
    change: &Change,
    beside: impl Fn(&OsStr) -> bool,
    rule: &str,
    unjudged: &str,
    runs: Vec<OnCopy>,
) -> Vec<Finding> {
    in_copy("base", rule, unjudged, runs, |dir, tree| {
        let empty = copy_base_with_tests(change, tree, &dir.join("base.tar"), beside)?;
        Ok(submodules_left_empty(&empty, rule).into_iter().collect())
    })
}

/// The findings of `runs` on a copy of `what`, as "base", that `make(dir, tree)` writes to `tree` in `dir`, a
/// [`ScratchDirectory`], giving its own findings or why it could not, a clause; as [`in_working_tree_copy`] says.
fn in_copy(
    what: &str,
    rule: &str,
    unjudged: &str,
    runs: Vec<OnCopy>,
    make: impl FnOnce(&Path, &Path) -> Result<Vec<Finding>, String>,
) -> Vec<Finding> {
    if runs.is_empty() {
        return vec![];
    }
    let no_room = |e: &dyn std::fmt::Display| {
        let message = format!("could not make a directory to copy the {what} into: {e}; {unjudged}.");
        vec![finding(None, None, rule, message)]
    };
    let dir = match ScratchDirectory::new(&std::env::temp_dir()) {
        Ok(dir) => dir,
        Err(e) => return no_room(&e),
    };
    let copy = ScratchCopy { tree: dir.path().join("tree"), scratch: dir.path().join("scratch") };
    let made = std::fs::create_dir(&copy.scratch).map_err(|e| no_room(&failed("make", &copy.scratch, &e)));
    let copied = made.and_then(|()| {
        make(dir.path(), &copy.tree)
            .map_err(|e| vec![finding(None, None, rule, format!("could not copy the {what}: {e}; {unjudged}."))])
    });
    let mut out = match copied {
        Ok(mut out) => {
            for run in runs {
                out.extend(run(&copy));
            }
            out
        }
        Err(found) => found,
    };
    let path = dir.path().to_path_buf();
    if let Err(e) = dir.remove() {
        let message = format!("could not remove the directory of the copy of the {what}, `{}`: {e}", path.display());
        out.push(finding(None, None, "not-removed", message));
    }
    out
}

/// The finding of `rule` on `empty`, the submodules a copy of the base leaves empty, each with its path and why, as
/// [`copy_base_with_tests`] gives them; None for none.
fn submodules_left_empty(empty: &[(String, String)], rule: &str) -> Option<Finding> {
    let (noun, pronoun) = match empty.len() {
        0 => return None,
        1 => ("a submodule", "it"),
        _ => ("submodules", "one of them"),
    };
    let named: Vec<String> = empty.iter().map(|(path, why)| format!("`{path}`, {why}")).collect();
    let message = format!(
        "the copy of the base leaves {noun} empty, so a changed test that needs {pronoun}, if any does, was not \
         judged: {}.",
        named.join("; ")
    );
    Some(finding(None, None, rule, message))
}

/// The time a check's tool runs may take.
pub struct Budget {
    /// Its length in seconds, as findings name what a run was stopped at: the check's budget, or a share of it.
    pub seconds: u64,
    /// When it runs out.
    pub deadline: Instant,
}

impl Budget {
    /// A budget of `seconds` that starts now.
    pub fn starting_now(seconds: u64) -> Budget {
        Budget { seconds, deadline: Instant::now() + Duration::from_secs(seconds) }
    }

    /// One of `parts` equal shares of what is left of this budget from now: a deadline that share from now, and its
    /// length in whole seconds, rounded. `parts` of 0 is taken as 1.
    pub fn share(&self, parts: u32) -> Budget {
        let now = Instant::now();
        let share = self.deadline.saturating_duration_since(now) / parts.max(1);
        Budget { seconds: share.as_secs_f64().round() as u64, deadline: now + share }
    }

    /// The timeout to give a run that is stopped softly with `grace` (process::Extras::soft_stop), so that it ends by
    /// the deadline, its stop and the wait for what it printed (process::KILL_WAIT) included; None when no time is
    /// left for it.
    pub fn before_soft_stop(&self, grace: Duration) -> Option<Duration> {
        self.deadline.checked_duration_since(Instant::now() + grace + KILL_WAIT).filter(|left| !left.is_zero())
    }
}

/// Puts `findings` in the order a check gives them: by path and line, a finding on no path or line first.
pub fn sort_by_place(findings: &mut [Finding]) {
    findings.sort_by(|a, b| {
        let key = |f: &Finding| (f.path.clone().unwrap_or_default(), f.line.unwrap_or(0));
        key(a).cmp(&key(b))
    });
}

/// A finding of a check beside the linters, as its find gives it: `rule` its own, without the check's name, which
/// the gate puts before it, and its severity "warning", which the gate replaces with what it does with it.
pub fn finding(path: Option<&OsStr>, line: Option<u32>, rule: &str, message: String) -> Finding {
    Finding { path: path.map(OsStr::to_os_string), line, severity: "warning".into(), rule: rule.into(), message }
}

/// Copies the files the gate judges in `change`'s working tree (Change::judged), as they are there now, to the same
/// paths under `dest`, a directory it makes, with its parents. A symbolic link is copied as a link, and a submodule,
/// or a directory holding a repository git does not track, as its directory; a listed file the working tree no longer
/// holds is left out. Errs, a clause, when `dest` exists or a file cannot be copied; `dest` may then hold part of the
/// copy.
fn copy_working_tree(change: &Change, dest: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dest.parent().unwrap_or(Path::new("/"))).map_err(|e| failed("make", dest, &e))?;
    std::fs::create_dir(dest).map_err(|e| failed("make", dest, &e))?;
    for name in &change.judged {
        let (src, dst) = (change.root.join(name), dest.join(name));
        if src.symlink_metadata().is_err() {
            continue;
        }
        if let Some(parent) = dst.parent() {
            std::fs::create_dir_all(parent).map_err(|e| failed("make", parent, &e))?;
        }
        copy(&src, &dst)?;
    }
    Ok(())
}

/// Copies `src` to `dst`, which must not exist: a symbolic link as a link, a directory with all it holds, a file with
/// its permissions. Errs on anything else, as a named pipe, whose reading could block the gate for good.
fn copy(src: &Path, dst: &Path) -> Result<(), String> {
    let meta = src.symlink_metadata().map_err(|e| failed("read", src, &e))?;
    if meta.file_type().is_symlink() {
        let target = std::fs::read_link(src).map_err(|e| failed("read", src, &e))?;
        std::os::unix::fs::symlink(target, dst).map_err(|e| failed("write", dst, &e))
    } else if meta.is_dir() {
        std::fs::create_dir(dst).map_err(|e| failed("make", dst, &e))?;
        for entry in std::fs::read_dir(src).map_err(|e| failed("read", src, &e))? {
            let entry = entry.map_err(|e| failed("read", src, &e))?;
            copy(&entry.path(), &dst.join(entry.file_name()))?;
        }
        Ok(())
    } else if meta.is_file() {
        std::fs::copy(src, dst).map(|_| ()).map_err(|e| failed("copy", src, &e))
    } else {
        Err(format!("cannot copy {}: it is no regular file, link or directory", text::path(src.as_os_str())))
    }
}

/// Why a file operation failed, as a clause: "cannot `what` `path`: the system's words".
pub fn failed(what: &str, path: &Path, e: &std::io::Error) -> String {
    format!("cannot {what} {}: {e}", text::path(path.as_os_str()))
}

/// Writes the files git tracked at `change.base` to `dest`, a directory it makes, through a tar file at `archive`,
/// then replaces each file of the change's that `beside` accepts, a repository path that belongs with the tests
/// rather than with the code they test, with the working tree's: removed at its path at the base, and copied, a
/// symbolic link as a link, to its path in the working tree. A symbolic link is written as it is, wherever it points,
/// and no file is written or removed through one: a link on the way to a replaced file is replaced by a directory, as
/// the working tree has one there. A submodule holds the files of the commit the base records for it, as
/// [`unpack_commit`] writes them, unless `beside` accepts its path: then it holds those of the commit its checkout in
/// the working tree is at, without what that checkout holds that git does not track there, and is left empty when the
/// working tree has no checkout of it.
///
/// Gives back the submodules left empty, as [`unpack_commit`] does, of the base's and of those taken from the working
/// tree. Errs, a clause, when git cannot list or write a commit, `tar` cannot unpack it, `dest` exists or a file cannot
/// be written.
fn copy_base_with_tests(
    change: &Change,
    dest: &Path,
    archive: &Path,
    beside: impl Fn(&OsStr) -> bool,
) -> Result<Vec<(String, String)>, String> {
    std::fs::create_dir(dest).map_err(|e| failed("make", dest, &e))?;
    let mut empty = unpack_commit(&change.root, &change.base, dest, archive, "")?;
    // A path the base tracks has no link above it there; it is a directory when the base records a submodule there.
    for old in change.files.iter().filter_map(|f| f.before.as_ref()).filter(|p| beside(p)) {
        remove_entry(&dest.join(old))?;
        let removed = text::path(old);
        empty.retain(|(name, _)| name != &removed && !name.starts_with(&format!("{removed}/")));
    }
    for new in change.files.iter().filter_map(|f| f.after.as_ref()).filter(|p| beside(p)) {
        let src = change.root.join(new);
        let Ok(meta) = src.symlink_metadata() else { continue };
        let dst = unlinked_path(dest, new)?;
        // A directory the base had where the change has a file or a link: emptied by the removals above, or not.
        remove_entry(&dst)?;
        // git tracks files, links and commits, so a directory, not a link to one, is where the change records a
        // submodule's commit. Its checkout's own `.git` would lead nowhere in the copy: it is written as the files of
        // the commit the checkout is at.
        if meta.is_dir() {
            std::fs::create_dir(&dst).map_err(|e| failed("make", &dst, &e))?;
            let name = text::path(new);
            if src.join(".git").symlink_metadata().is_ok() {
                empty.extend(unpack_commit(&src, "HEAD", &dst, archive, &format!("{name}/"))?);
            } else {
                empty.push((name, NOT_INITIALISED.to_string()));
            }
        } else {
            copy(&src, &dst)?;
        }
    }
    Ok(empty)
}

/// Removes what is at `path`, a directory with all it holds, nothing when there is nothing, and never through a link
/// at `path` itself. Errs, a clause, when it cannot.
pub fn remove_entry(path: &Path) -> Result<(), String> {
    match path.symlink_metadata() {
        Ok(meta) if meta.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(_) => Ok(()),
    }
    .map_err(|e| failed("remove", path, &e))
}

/// Why a submodule is left empty when the working tree has no checkout of it.
const NOT_INITIALISED: &str = "which is not initialised in the working tree";

/// Writes the files git tracked at `commit` in the repository whose working tree's top is `repo` to `dest`, an
/// existing directory, through a tar file at `archive`, which it overwrites; then fills each submodule's directory
/// the same way, from the submodule's own repository in `repo`'s working tree, at the commit that `commit` records for
/// it, its own submodules too. A submodule that is not initialised there, or whose repository lacks that commit, is
/// left an empty directory, as git archive leaves it; one that git archive leaves out, as for export-ignore, stays
/// missing.
///
/// The archive is unpacked by the system's `tar`, which, GNU tar and bsdtar alike, refuses by default a member whose
/// path leads out of `dest`, through a link or otherwise; a tracked link out of the tree is written as the
/// repository's own.
///
/// Gives back those submodules, each by its path with `prefix` before it, and why, a clause such as "which is not
/// initialised in the working tree". Errs, a clause, when git cannot list or write a commit, or `tar` cannot unpack
/// it.
fn unpack_commit(
    repo: &Path,
    commit: &str,
    dest: &Path,
    archive: &Path,
    prefix: &str,
) -> Result<Vec<(String, String)>, String> {
    let mut output = OsString::from("--output=");
    output.push(archive);
    let args = [OsStr::new("archive"), OsStr::new("--format=tar"), &output, OsStr::new(commit)];
    git::output(repo, &args).map_err(|Unjudged(e)| e)?;
    let argv = [OsStr::new("tar"), OsStr::new("-x"), OsStr::new("-f"), archive.as_os_str(), OsStr::new("-C"),
                dest.as_os_str()];
    let timeout = std::time::Duration::from_secs(crate::process::TOOL_TIMEOUT);
    match crate::process::run(&argv, Path::new("."), timeout) {
        Err(e) => return Err(format!("cannot run tar to unpack the base: {e}")),
        Ok(done) if done.stopped || done.status != 0 => {
            return Err(format!("tar could not unpack the base: {}", done.said()))
        }
        Ok(_) => {}
    }
    // Each entry is `<mode> <type> <object>\t<path>`; a submodule's mode is 160000 and its object the commit.
    let listing = git::output(repo, &["ls-tree", "-r", "-z", commit]).map_err(|Unjudged(e)| e)?;
    let mut empty = vec![];
    for entry in listing.split(|&b| b == 0) {
        let Some(tab) = entry.iter().position(|&b| b == b'\t') else { continue };
        let (meta, path) = (&entry[..tab], OsStr::from_bytes(&entry[tab + 1..]));
        // A submodule the commit's .gitattributes marks export-ignore is missing from the archive, as other paths are.
        if !meta.starts_with(b"160000 ") || !dest.join(path).is_dir() {
            continue;
        }
        let recorded = text::lossless(meta.split(|&b| b == b' ').nth(2).unwrap_or_default());
        let name = format!("{prefix}{}", text::path(path));
        let sub = repo.join(path);
        // The submodule's directory is the top of a repository of its own only once it is initialised: before, git
        // finds the superproject above it, and names the directory as a prefix within it.
        let own = git::run(&sub, &["rev-parse", "--show-prefix"]).map_err(|Unjudged(e)| e)?;
        if own.status != 0 || own.stdout.iter().any(|&b| b != b'\n') {
            empty.push((name, NOT_INITIALISED.to_string()));
        } else if git::run(&sub, &["cat-file", "-e", &format!("{recorded}^{{commit}}")])
            .map_err(|Unjudged(e)| e)?
            .status
            != 0
        {
            let short: String = recorded.chars().take(12).collect();
            empty.push((name, format!("whose repository in the working tree does not have the base's commit {short}")));
        } else {
            empty.extend(unpack_commit(&sub, &recorded, &dest.join(path), archive, &format!("{name}/"))?);
        }
    }
    Ok(empty)
}

/// `dest`/`path`, `path` a repository path, after making each directory above it in `dest` a real one: a symbolic
/// link there is replaced by an empty directory, and a missing one made. So a file written or removed at it stays in
/// `dest`. Errs, a clause, when one cannot be.
fn unlinked_path(dest: &Path, path: &OsStr) -> Result<PathBuf, String> {
    let path = Path::new(path);
    let mut at = dest.to_path_buf();
    for part in path.parent().into_iter().flat_map(Path::components) {
        at.push(part);
        let meta = at.symlink_metadata();
        if meta.as_ref().is_ok_and(|m| m.file_type().is_symlink()) {
            std::fs::remove_file(&at).map_err(|e| failed("remove", &at, &e))?;
        }
        if !at.is_dir() {
            std::fs::create_dir(&at).map_err(|e| failed("make", &at, &e))?;
        }
    }
    Ok(at.join(path.file_name().unwrap_or_default()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::ScratchDir;

    #[test]
    fn a_share_of_a_budget_names_its_own_seconds() {
        let budget = Budget { seconds: 300, deadline: Instant::now() + Duration::from_secs(300) };
        let third = budget.share(3);
        assert_eq!(third.seconds, 100);
        assert!(third.deadline <= Instant::now() + Duration::from_secs(100));
        assert_eq!(budget.share(0).seconds, 300);
        let spent = Budget { seconds: 300, deadline: Instant::now() };
        assert_eq!(spent.share(2).seconds, 0);
    }

    #[test]
    fn a_soft_stopped_run_gets_the_time_before_its_stop() {
        let grace = Duration::from_secs(10);
        let ending_in = |after: Duration| Budget { seconds: 0, deadline: Instant::now() + after };
        assert_eq!(ending_in(grace + KILL_WAIT - Duration::from_secs(1)).before_soft_stop(grace), None);
        assert_eq!(ending_in(Duration::ZERO).before_soft_stop(grace), None);
        let left = ending_in(grace + KILL_WAIT + Duration::from_secs(20)).before_soft_stop(grace).unwrap();
        assert!(Duration::from_secs(19) < left && left <= Duration::from_secs(20), "{left:?}");
    }

    use std::cell::RefCell;

    /// A run on the copy that records the copy's top in `seen` and gives one finding of rule `rule`.
    fn recording<'a>(seen: &'a RefCell<Vec<PathBuf>>, rule: &'a str) -> OnCopy<'a> {
        Box::new(move |copy| {
            seen.borrow_mut().push(copy.tree.clone());
            vec![finding(None, None, rule, "m".into())]
        })
    }

    #[test]
    fn the_runs_share_one_private_copy_that_is_removed_afterwards() {
        use std::os::unix::fs::PermissionsExt;
        let seen = RefCell::new(vec![]);
        let make = |dir: &Path, tree: &Path| {
            assert_eq!(dir.metadata().unwrap().permissions().mode() & 0o777, 0o700);
            assert!(dir.join("scratch").is_dir());
            std::fs::create_dir(tree).unwrap();
            Ok(vec![finding(None, None, "made", "m".into())])
        };
        let runs = vec![recording(&seen, "first"), recording(&seen, "second")];
        let found = in_copy("base", "not-run", "nothing was judged", runs, make);
        let rules: Vec<&str> = found.iter().map(|f| f.rule.as_str()).collect();
        assert_eq!(rules, ["made", "first", "second"]);
        let seen = seen.into_inner();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0], seen[1]);
        assert!(!seen[0].parent().unwrap().exists());
    }

    #[test]
    fn a_copy_that_cannot_be_made_is_the_one_finding_and_no_run_is_called() {
        let (seen, dir) = (RefCell::new(vec![]), RefCell::new(None));
        let make = |at: &Path, _: &Path| {
            *dir.borrow_mut() = Some(at.to_path_buf());
            Err("cannot copy x: why".to_string())
        };
        let found = in_copy("base", "not-rerun", "the changed tests were not rerun", vec![recording(&seen, "r")], make);
        assert_eq!(found.len(), 1);
        assert_eq!((found[0].rule.as_str(), found[0].path.as_ref(), found[0].line), ("not-rerun", None, None));
        assert_eq!(found[0].message, "could not copy the base: cannot copy x: why; the changed tests were not rerun.");
        assert!(seen.borrow().is_empty());
        assert!(!dir.into_inner().unwrap().exists());
    }

    #[test]
    fn no_copy_is_made_for_no_run() {
        let made = std::cell::Cell::new(false);
        let found = in_copy("working tree", "not-run", "x", vec![], |_, _| {
            made.set(true);
            Ok(vec![])
        });
        assert!(found.is_empty() && !made.get());
    }

    #[test]
    fn the_copy_is_removed_when_a_run_panics() {
        let dir = RefCell::new(None);
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let make = |at: &Path, tree: &Path| {
                *dir.borrow_mut() = Some(at.to_path_buf());
                std::fs::create_dir(tree).map(|()| vec![]).map_err(|e| e.to_string())
            };
            let run: OnCopy = Box::new(|copy| {
                std::fs::write(copy.tree.join("f"), "x").unwrap();
                panic!("a check's bug")
            });
            in_copy("working tree", "not-run", "x", vec![run], make)
        }));
        assert!(panicked.is_err());
        assert!(!dir.into_inner().unwrap().exists());
    }

    #[test]
    fn submodules_left_empty_are_one_finding_worded_by_their_number() {
        assert!(submodules_left_empty(&[], "not-run").is_none());
        let one = [("lib".to_string(), "which is not initialised in the working tree".to_string())];
        let found = submodules_left_empty(&one, "not-run").unwrap();
        assert_eq!(
            found.message,
            "the copy of the base leaves a submodule empty, so a changed test that needs it, if any does, was not \
             judged: `lib`, which is not initialised in the working tree."
        );
        let two = [one[0].clone(), ("vendor/x".to_string(), "why".to_string())];
        let found = submodules_left_empty(&two, "not-run").unwrap();
        assert!(found.message.starts_with("the copy of the base leaves submodules empty, so a changed test that needs \
                                           one of them,"), "{}", found.message);
        assert!(found.message.ends_with(": `lib`, which is not initialised in the working tree; `vendor/x`, why."));
    }

    #[test]
    fn a_named_pipe_is_not_copied_but_refused() {
        let dir = ScratchDir::new();
        let made = std::process::Command::new("mkfifo").arg(dir.join("pipe")).status().unwrap();
        assert!(made.success());
        let copied = copy(&dir.join("pipe"), &dir.join("copy"));
        let expected = format!("cannot copy {}: it is no regular file, link or directory", text::path(
            dir.join("pipe").as_os_str()
        ));
        assert_eq!(copied, Err(expected));
    }

    #[test]
    fn a_copy_that_cannot_make_its_directory_says_which_and_why() {
        let dir = ScratchDir::new();
        std::fs::create_dir_all(dir.join("src")).unwrap();
        std::fs::create_dir_all(dir.join("dst")).unwrap();
        let copied = copy(&dir.join("src"), &dir.join("dst"));
        let why = copied.unwrap_err();
        let expected = format!("cannot make {}: ", text::path(dir.join("dst").as_os_str()));
        assert!(why.starts_with(&expected) && why.contains("exists"), "{why}");
    }

    #[test]
    fn an_unlinked_path_replaces_links_above_it_with_directories() {
        let dir = ScratchDir::new();
        std::fs::create_dir_all(dir.join("outside")).unwrap();
        std::fs::create_dir_all(dir.join("dest")).unwrap();
        std::os::unix::fs::symlink(dir.join("outside"), dir.join("dest/a")).unwrap();
        let at = unlinked_path(&dir.join("dest"), OsStr::new("a/b/c.rs")).unwrap();
        assert_eq!(at, dir.join("dest/a/b/c.rs"));
        assert!(!dir.join("dest/a").symlink_metadata().unwrap().file_type().is_symlink());
        assert!(dir.join("dest/a/b").is_dir());
    }
}
