//! The surviving-mutants check over a real git repository's Cargo package, through the `gate` binary and its
//! quality.json: with a real cargo-mutants, or a stub `cargo` ahead of PATH where a test needs cargo-mutants to do
//! what the real one cannot be made to. What a stub `cargo` cannot vary either is held by mutants.rs's own tests.
//! cargo runs offline, so it never reaches for the registry, and the passes-at-base check, which the gate runs too,
//! has a budget of a second.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Repo;

mod common;

const LIB: &str = "pub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n";

/// checked's tests notice every mutant; unchecked's call it and check nothing. unchecked's fn is at line 9 of the
/// library with them.
const ADDED: &str = "\npub fn checked(a: u32) -> u32 {\n    a * 2\n}\n\n\
                     pub fn unchecked(a: u32) -> bool {\n    a > 3\n}\n";
const TESTS: &str = "#[test]\nfn checks() {\n    SLEEP\n    assert_eq!(calc::checked(3), 6);\n\
                     \x20   assert_eq!(calc::checked(0), 0);\n}\n\n#[test]\nfn calls() {\n    calc::unchecked(1);\n}\n";

/// The manifest and lock file of the package `name`, at `dir`, a directory with a slash, or the top when empty.
fn package(dir: &str, name: &str) -> Vec<(String, String)> {
    vec![
        (
            format!("{dir}Cargo.toml"),
            format!("[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ),
        (format!("{dir}Cargo.lock"), format!("version = 4\n\n[[package]]\nname = \"{name}\"\nversion = \"0.1.0\"\n")),
        (format!("{dir}src/lib.rs"), LIB.to_string()),
    ]
}

/// A repository whose base holds the package calc at the top, and the packages of `more`, (directory with a slash,
/// name). gate.toml turns clippy off, which would build the packages once more.
fn repo(more: &[(&str, &str)]) -> Repo {
    let mut files = vec![
        ("pyproject.toml".to_string(), "[tool.ruff]\n".to_string()),
        ("gate.toml".into(), "[linters]\nclippy = []\n".into()),
        (".gitignore".into(), "target/\n".into()),
    ];
    files.extend(package("", "calc"));
    for (dir, name) in more {
        files.extend(package(dir, name));
    }
    let files: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (p.as_str(), t.as_bytes())).collect();
    Repo::with(&files)
}

/// Adds checked and unchecked to the library at `dir`, and their tests, sleeping as `sleep`, a statement, says.
fn add_functions(repo: &Repo, dir: &str, crate_name: &str, sleep: &str) {
    repo.put(format!("{dir}src/lib.rs"), format!("{LIB}{ADDED}").as_bytes());
    let tests = TESTS.replace("SLEEP", sleep).replace("calc::", &format!("{crate_name}::"));
    repo.put(format!("{dir}tests/calc.rs"), tests.as_bytes());
}

/// A finding of the check: its path, line, own rule without the check's name, and message.
type Found = (Option<String>, Option<u32>, String, String);

/// The check's findings on the change from HEAD to the working tree of `repo`, every file in it tracked, with a
/// budget of `budget` seconds, the default when None, and `env` set for the gate.
fn findings(repo: &Repo, budget: Option<u64>, env: &[(&str, OsString)]) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let mut command = repo.gate_command("HEAD");
    command.arg("--json").arg(&json).args(["--base-run-budget", "1"]);
    if let Some(budget) = budget {
        command.args(["--mutation-budget", &budget.to_string()]);
    }
    command.env("CARGO_NET_OFFLINE", "true").envs(env.iter().map(|(k, v)| (k, v)));
    let done = command.output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        let Some(rule) = f["rule"].as_str().unwrap().strip_prefix("surviving-mutants ") else { continue };
        let path = f["path"].as_str().map(String::from);
        let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
        out.push((path, line, rule.to_string(), f["message"].as_str().unwrap().to_string()));
    }
    out
}

/// A finding on no path or line, of `rule`, saying `message`.
fn lineless(rule: &str, message: &str) -> Found {
    (None, None, rule.into(), message.into())
}

/// Puts a `cargo` in `repo`'s stub directory that runs the sh `script` for the subcommand `command`, and the real
/// cargo for any other, and gives back PATH with that directory ahead of it.
fn cargo_stub(repo: &Repo, command: &str, script: &str) -> OsString {
    let found = Command::new("sh").args(["-c", "command -v cargo"]).output().unwrap();
    let cargo = String::from_utf8(found.stdout).unwrap();
    let script = format!("[ \"$1\" = {command} ] || exec '{}' \"$@\"\n{script}", cargo.trim());
    let bin = repo.stub("cargo", &script);
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()).into()
}

#[test]
fn a_function_whose_mutants_survive_is_found_and_one_whose_mutants_are_caught_is_not() {
    let repo = repo(&[]);
    add_functions(&repo, "", "calc", "");
    let found = findings(&repo, None, &[]);
    assert_eq!(found.len(), 1, "{found:?}");
    let (path, line, rule, message) = &found[0];
    assert_eq!((path.as_deref(), *line, rule.as_str()), (Some("src/lib.rs"), Some(9), "survived"), "{message}");
    assert!(message.starts_with("5 of 5 mutants of `unchecked` survived its tests: line 10: replace "), "{message}");
}

#[test]
fn the_budget_running_out_is_a_finding() {
    let repo = repo(&[]);
    // The unmutated tests pass at once, and each of checked's mutants makes them wait for cargo-mutants' 60-second
    // timeout: of the 60 seconds, less the 15 the stop may take, the baseline, a build of the package, has 45 however
    // loaded the machine is, and the first such mutant outlasts what is left.
    let wait = "while calc::checked(3) != 6 {\n        std::thread::sleep(std::time::Duration::from_secs(1));\n    }";
    add_functions(&repo, "", "calc", wait);
    let found = findings(&repo, Some(60), &[]);
    let reached: Vec<&String> = found.iter().filter(|f| f.2 == "budget-reached").map(|f| &f.3).collect();
    assert_eq!(reached.len(), 1, "{found:?}");
    let opening = "budget reached after ";
    assert!(reached[0].starts_with(opening), "{}", reached[0]);
    assert!(reached[0].contains(" of the mutants cargo-mutants made of the workspace at `.`"), "{}", reached[0]);
}

#[test]
fn a_change_without_rust_never_starts_cargo() {
    let repo = repo(&[]);
    repo.put("notes.md", b"notes\n");
    let ran = repo.outside.join("ran");
    let bin = repo.stub("cargo", &format!("echo ran > '{}'; exit 1", ran.display()));
    let path: OsString = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()).into();
    assert_eq!(findings(&repo, None, &[("PATH", path)]), vec![]);
    assert!(!ran.exists());
}

#[test]
fn a_workspace_below_the_top_is_mutated_in_its_own_directory() {
    let repo = repo(&[("engine/", "engine")]);
    add_functions(&repo, "engine/", "engine", "");
    let found = findings(&repo, None, &[]);
    let places: Vec<_> = found.iter().map(|(path, line, rule, _)| (path.as_deref(), *line, rule.as_str())).collect();
    assert_eq!(places, vec![(Some("engine/src/lib.rs"), Some(9), "survived")]);
}

#[test]
fn added_lines_with_nothing_to_mutate_give_nothing() {
    let repo = repo(&[]);
    repo.put("src/lib.rs", format!("{LIB}\npub const LIMIT: u32 = 4;\n").as_bytes());
    assert_eq!(findings(&repo, None, &[]), vec![]);
}

#[test]
fn cargo_mutants_runs_in_the_copy_uncoloured_on_the_workspace_s_diff_and_is_stopped_with_sigint() {
    let repo = repo(&[("engine/", "engine")]);
    repo.put("engine/src/lib.rs", format!("{LIB}{ADDED}").as_bytes());
    let record = repo.outside.join("record");
    // The diff is the argument after --in-diff; a SIGINT is recorded, and ends the stub.
    let script = format!(
        "{{ pwd; printf '%s\\n' \"$@\"; echo \"color=$CARGO_TERM_COLOR token=${{JENKINS_TOKEN-unset}}\"; \
         shift 6; cat \"$1\"; }} > '{0}'\ntrap 'echo interrupted >> \"{0}\"; exit 3' INT\nsleep 30 & wait",
        record.display()
    );
    let env = [
        ("PATH", cargo_stub(&repo, "mutants", &script)),
        ("CARGO_TERM_COLOR", "always".into()),
        ("JENKINS_TOKEN", "secret".into()),
    ];
    // 15 of the 18 seconds are the stop's.
    assert_eq!(findings(&repo, Some(18), &env), vec![lineless(
        "budget-reached",
        "budget reached before cargo-mutants had tested the unmutated baseline of the workspace at `engine`: it was \
         stopped at 18 seconds (`--mutation-budget`), so the mutants it did not try may survive too.",
    )]);
    let recorded = std::fs::read_to_string(&record).unwrap();
    let lines: Vec<&str> = recorded.lines().collect();
    let (cwd, argv) = (Path::new(lines[0]), &lines[1..17]);
    assert_eq!(cwd.file_name().unwrap(), "tree");
    let scratch = cwd.parent().unwrap().join("run-0");
    let (dir, diff_file) = (cwd.join("engine"), scratch.join("change.diff"));
    let expected = [
        "mutants", "--in-place", "--no-shuffle", "--dir", dir.to_str().unwrap(), "--in-diff",
        diff_file.to_str().unwrap(), "--test-tool", "nextest", "--timeout", "60", "--output",
        scratch.to_str().unwrap(), "--colors", "never",
    ];
    assert_eq!(argv[..15], expected);
    assert_eq!(argv[15], "color=never token=unset");
    let diff = &lines[17..lines.len() - 1];
    assert_eq!(diff[0], "diff --git a/src/lib.rs b/src/lib.rs");
    assert!(diff.contains(&"+++ b/src/lib.rs"), "{diff:?}");
    assert_eq!(lines.last(), Some(&"interrupted"));
}

#[test]
fn each_workspace_runs_in_turn_until_the_budget_runs_out() {
    let repo = repo(&[("a/", "a"), ("b/", "b"), ("c/", "c"), ("d/", "d")]);
    for dir in ["a/", "b/", "c/", "d/"] {
        repo.put(format!("{dir}src/lib.rs"), format!("{LIB}{ADDED}").as_bytes());
    }
    // d's manifest, and the one at the top, are gone from the working tree, so d's library is still the code it
    // tests, in no workspace.
    for path in ["d/Cargo.toml", "Cargo.toml", "Cargo.lock"] {
        std::fs::remove_file(repo.repo.join(path)).unwrap();
    }
    let not_reached = |at: &str| {
        lineless("not-run", &format!(
            "the budget ran out before cargo-mutants reached the workspace at `{at}`; the change's Rust was not \
             mutation-tested."
        ))
    };
    assert_eq!(findings(&repo, Some(1), &[]), vec![
        lineless(
            "budget-reached",
            "budget reached before cargo-mutants started on the workspace at `a`: the 1 seconds of \
             `--mutation-budget` left too little to run and stop it, so its mutants were not tried.",
        ),
        not_reached("b"),
        not_reached("c"),
        (
            Some("d/src/lib.rs".into()),
            None,
            "not-mutated".into(),
            "was not mutation-tested: no Cargo.toml lies above it.".into(),
        ),
    ]);
}

#[test]
fn a_workspace_cargo_cannot_tell_is_not_mutated() {
    let repo = repo(&[]);
    repo.put("src/lib.rs", format!("{LIB}{ADDED}").as_bytes());
    let path = cargo_stub(&repo, "locate-project", "echo 'error: nope' >&2; exit 1");
    assert_eq!(findings(&repo, None, &[("PATH", path)]), vec![(
        Some("src/lib.rs".into()),
        None,
        "not-mutated".into(),
        "was not mutation-tested: `cargo-mutants` could not find the workspace of `Cargo.toml`: error: nope.".into(),
    )]);
}

#[test]
fn a_workspace_cargo_is_missing_to_tell_names_cargo_mutants() {
    let repo = repo(&[]);
    repo.put("src/lib.rs", format!("{LIB}{ADDED}").as_bytes());
    // PATH holds git and nothing else.
    let found = Command::new("sh").args(["-c", "command -v git"]).output().unwrap();
    let git = String::from_utf8(found.stdout).unwrap();
    let bin = repo.outside.join("git-only");
    std::fs::create_dir_all(&bin).unwrap();
    std::os::unix::fs::symlink(git.trim(), bin.join("git")).unwrap();
    assert_eq!(findings(&repo, None, &[("PATH", bin.into_os_string())]), vec![(
        Some("src/lib.rs".into()),
        None,
        "not-mutated".into(),
        "was not mutation-tested: `cargo-mutants` is not installed: `cargo` is not on PATH.".into(),
    )]);
}

#[test]
fn a_tree_that_cannot_be_copied_is_not_run() {
    let repo = repo(&[]);
    repo.make_uncopyable();
    repo.put("src/lib.rs", format!("{LIB}{ADDED}").as_bytes());
    let found = findings(&repo, None, &[]);
    assert_eq!(found.len(), 1, "{found:?}");
    let (path, line, rule, message) = &found[0];
    assert_eq!((path.as_deref(), *line, rule.as_str()), (None, None, "not-run"), "{message}");
    assert!(message.starts_with("could not copy the tree for cargo-mutants: cannot copy /"), "{message}");
    assert!(message.contains("/vendor/lib/pipe: it is no regular file, link or directory; "), "{message}");
}

#[test]
fn a_cargo_mutants_that_fails_writing_no_outcomes_is_not_run() {
    let repo = repo(&[]);
    repo.put("src/lib.rs", format!("{LIB}{ADDED}").as_bytes());
    let path = cargo_stub(&repo, "mutants", "echo 'it said so' >&2; exit 1");
    let found = findings(&repo, None, &[("PATH", path)]);
    assert_eq!(found.len(), 1, "{found:?}");
    let (_, _, rule, message) = &found[0];
    assert_eq!(rule, "not-run");
    assert!(message.starts_with("cargo-mutants, exiting with status 1, wrote no outcomes the gate can read ("));
    assert!(message.ends_with("): it said so; the change's Rust was not mutation-tested."), "{message}");
}

#[test]
fn no_room_for_cargo_mutants_is_a_finding() {
    let repo = repo(&[]);
    repo.put("src/lib.rs", format!("{LIB}{ADDED}").as_bytes());
    let missing: PathBuf = repo.outside.join("missing");
    assert_eq!(findings(&repo, None, &[("TMPDIR", missing.into())]), vec![lineless(
        "not-run",
        "could not make a directory to run cargo-mutants on a copy of the tree in: No such file or directory (os \
         error 2); nothing ran, so the check judged nothing.",
    )]);
}
