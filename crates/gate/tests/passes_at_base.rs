//! The passes-at-base check over a real git repository's Cargo package, its tests run with cargo-nextest, through
//! the `gate` binary and its quality.json. Where a test needs cargo-nextest or the copy of the base to go wrong, it
//! puts a stub `cargo` or `tar` ahead of PATH. A budget spent before anything runs, which the `gate` binary's whole
//! seconds cannot give, is passes_at_base.rs's own test. cargo runs offline, so it never reaches for the registry,
//! and the surviving-mutants check, which the gate runs too, has a budget too short to start cargo-mutants.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::Repo;

mod common;

/// double is wrong at the base; add is right, and stays so.
const LIB: &str = "pub mod other;\n\npub fn add(a: u32, b: u32) -> u32 {\n    a + b\n}\n\n\
                   pub fn double(a: u32) -> u32 {\n    a + a + 1\n}\n";
const OTHER: &str = "pub fn one() -> u32 {\n    1\n}\n";
const ADDS: &str = "#[test]\nfn adds() {\n    assert_eq!(calc::add(1, 2), 3);\n}\n";

/// The library with double fixed, as every test's working tree has it, so that the change edits Rust code outside
/// the tests.
fn fixed_lib() -> String {
    LIB.replace("a + a + 1", "a * 2")
}

/// The package calc at the top of a repository whose base holds `more`, (path, contents), too, with the fixed
/// library in the working tree. gate.toml turns clippy off, which would build the package once more.
fn calc(more: &[(&str, &str)]) -> Repo {
    let mut files = vec![
        ("pyproject.toml", "[tool.ruff]\n"),
        ("gate.toml", "[linters]\nclippy = []\n"),
        (".gitignore", "target/\n"),
        ("Cargo.toml", "[package]\nname = \"calc\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("Cargo.lock", "version = 4\n\n[[package]]\nname = \"calc\"\nversion = \"0.1.0\"\n"),
        ("src/lib.rs", LIB),
        ("src/other.rs", OTHER),
    ];
    files.extend(more);
    let files: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (*p, t.as_bytes())).collect();
    let repo = Repo::with(&files);
    repo.put("src/lib.rs", fixed_lib().as_bytes());
    repo
}

/// Commits `files` as the new base, without the fix of double, which stays in the working tree.
fn rebase(repo: &Repo, files: &[(&str, &str)], message: &str) {
    let mut files: Vec<(&str, &[u8])> = files.iter().map(|(p, t)| (*p, t.as_bytes())).collect();
    files.push(("src/lib.rs", LIB.as_bytes()));
    repo.commit(&files, message);
    repo.put("src/lib.rs", fixed_lib().as_bytes());
}

/// A finding of the check: its path, line, own rule without the check's name, and message.
type Found = (Option<String>, Option<u32>, String, String);

/// How the gate is run beside the defaults: its budget, and variables of its environment.
struct Gate {
    budget: u64,
    env: Vec<(&'static str, OsString)>,
}

impl Default for Gate {
    fn default() -> Gate {
        Gate { budget: 120, env: vec![] }
    }
}

/// The check's findings on the change from HEAD to the working tree of `repo`, every file in it tracked, with the
/// gate run as `how` says.
fn findings(repo: &Repo, how: Gate) -> Vec<Found> {
    repo.git(&["add", "-A"]);
    let json = repo.outside.join("quality.json");
    let mut command = repo.gate_command("HEAD");
    command.arg("--json").arg(&json).arg("--base-run-budget").arg(how.budget.to_string());
    command.args(["--mutation-budget", "1"]);
    command.env("CARGO_NET_OFFLINE", "true").envs(how.env);
    let done = command.output().unwrap();
    assert!(matches!(done.status.code(), Some(0 | 1)), "{}", String::from_utf8_lossy(&done.stderr));
    let report: serde_json::Value = serde_json::from_slice(&std::fs::read(&json).unwrap()).unwrap();
    let mut out = vec![];
    for f in report["findings"].as_array().unwrap() {
        let Some(rule) = f["rule"].as_str().unwrap().strip_prefix("passes-at-base ") else { continue };
        let path = f["path"].as_str().map(String::from);
        let line = f["line"].as_u64().map(|n| u32::try_from(n).unwrap());
        out.push((path, line, rule.to_string(), f["message"].as_str().unwrap().to_string()));
    }
    out
}

/// The path, line and rule of each of [`findings`] with the default budget.
fn found(repo: &Repo) -> Vec<(Option<String>, Option<u32>, String)> {
    findings(repo, Gate::default()).into_iter().map(|(path, line, rule, _)| (path, line, rule)).collect()
}

/// A finding as [`found`] gives it, at `line` of `path`.
fn at(path: &str, line: u32, rule: &str) -> (Option<String>, Option<u32>, String) {
    (Some(path.into()), Some(line), rule.into())
}

/// The line of `fn name(` in the working tree's `path`.
fn fn_line(repo: &Repo, path: &str, name: &str) -> u32 {
    let text = std::fs::read_to_string(repo.repo.join(path)).unwrap();
    let n = text.lines().position(|l| l.trim_start().starts_with(&format!("fn {name}("))).unwrap();
    u32::try_from(n + 1).unwrap()
}

/// Adds tests/calc.rs's adds and tests/more.rs's adds_more, each a target of its own, both passing at the base.
fn two_targets(repo: &Repo) {
    repo.put("tests/calc.rs", ADDS.as_bytes());
    repo.put("tests/more.rs", b"#[test]\nfn adds_more() {\n    assert_eq!(calc::add(2, 2), 4);\n}\n");
}

/// Puts a `cargo` in `repo`'s stub directory that runs the sh `script` for `cargo nextest` and the real cargo for
/// anything else, as the cargo locate-project that surviving-mutants runs, and gives back that directory.
fn nextest_stub(repo: &Repo, script: &str) -> PathBuf {
    let found = Command::new("sh").args(["-c", "command -v cargo"]).output().unwrap();
    let cargo = String::from_utf8(found.stdout).unwrap();
    repo.stub("cargo", &format!("[ \"$1\" = nextest ] || exec '{}' \"$@\"\n{script}", cargo.trim()))
}

/// PATH with `bin` ahead of it.
fn path_after(bin: &Path) -> OsString {
    format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()).into()
}

/// The not-run finding for `--test name` when the run at the base stopped before it.
fn not_reached(seconds: u64, name: &str) -> Found {
    let message = format!(
        "the run at the base used its {seconds} seconds (`--base-run-budget`) before it reached `--test {name}` of \
         the package at the top level, so its changed tests were not run there."
    );
    (None, None, "not-run".into(), message)
}

#[test]
fn a_test_that_passes_at_the_base_is_found_at_its_fn_and_one_that_fails_there_is_not() {
    let repo = calc(&[]);
    repo.put(
        "tests/calc.rs",
        format!("{ADDS}\n#[test]\nfn doubles() {{\n    assert_eq!(calc::double(2), 4);\n}}\n").as_bytes(),
    );
    // No blank line before the module: an added line outside it would edit the file's code.
    let tests = "#[cfg(test)]\nmod tests {\n    #[test]\n    fn is_one() {\n        assert_eq!(super::one(), 1);\n\
                 \x20   }\n}\n";
    repo.put("src/other.rs", format!("{OTHER}{tests}").as_bytes());
    assert_eq!(found(&repo), vec![
        at("src/other.rs", fn_line(&repo, "src/other.rs", "is_one"), "passes-at-base"),
        at("tests/calc.rs", fn_line(&repo, "tests/calc.rs", "adds"), "passes-at-base"),
    ]);
}

#[test]
fn a_test_file_that_does_not_compile_at_the_base_counts_as_failing() {
    let repo = calc(&[]);
    repo.put("src/lib.rs", format!("{}\npub fn triple(a: u32) -> u32 {{\n    a * 3\n}}\n", fixed_lib()).as_bytes());
    // a_triple.rs's target runs first, and the one after it is run all the same; cargo's errors are read
    // uncoloured, whatever CARGO_TERM_COLOR the gate runs under.
    repo.put("tests/a_triple.rs", b"#[test]\nfn triples() {\n    assert_eq!(calc::triple(2), 6);\n}\n\n\
                                    #[test]\nfn adds_too() {\n    assert_eq!(calc::add(1, 1), 2);\n}\n");
    repo.put("tests/calc.rs", ADDS.as_bytes());
    let how = Gate { env: vec![("CARGO_TERM_COLOR", "always".into())], ..Gate::default() };
    assert_eq!(findings(&repo, how), vec![(
        Some("tests/calc.rs".into()),
        Some(fn_line(&repo, "tests/calc.rs", "adds")),
        "passes-at-base".into(),
        "`adds` passes without the change, so it does not test it.".into(),
    )]);
}

#[test]
fn a_test_in_an_inline_module_of_a_file_the_change_edits_outside_it_is_not_run() {
    let repo = calc(&[]);
    let tests = "\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn doubles() {\n\
                 \x20       assert_eq!(super::double(2), 4);\n    }\n}\n";
    repo.put("src/lib.rs", format!("{}{tests}", fixed_lib()).as_bytes());
    // tests/calc.rs's test runs at the base all the same.
    repo.put("tests/calc.rs", ADDS.as_bytes());
    let found = findings(&repo, Gate::default());
    assert_eq!(found, vec![
        (
            Some("src/lib.rs".into()),
            Some(fn_line(&repo, "src/lib.rs", "doubles")),
            "not-run".into(),
            "`tests::doubles` is in a file whose code the change also edits outside its `#[cfg(test)]` items, so it \
             cannot be run apart from the code it tests, at the base."
                .into(),
        ),
        (
            Some("tests/calc.rs".into()),
            Some(fn_line(&repo, "tests/calc.rs", "adds")),
            "passes-at-base".into(),
            "`adds` passes without the change, so it does not test it.".into(),
        ),
    ]);
}

#[test]
fn no_room_for_the_run_at_the_base_is_a_finding() {
    let repo = calc(&[]);
    repo.put("tests/calc.rs", ADDS.as_bytes());
    let how = Gate { env: vec![("TMPDIR", repo.outside.join("missing").into())], ..Gate::default() };
    assert_eq!(findings(&repo, how), vec![(
        None,
        None,
        "not-run".into(),
        "could not make a directory to run the changed Rust tests at the base in: No such file or directory (os \
         error 2); nothing ran, so the check judged nothing."
            .into(),
    )]);
}

#[test]
fn a_test_whose_first_attribute_the_change_removes_is_run() {
    let repo = calc(&[]);
    rebase(&repo, &[("tests/calc.rs", &format!("#[ignore]\n{ADDS}"))], "an ignored test");
    repo.put("tests/calc.rs", ADDS.as_bytes());
    assert_eq!(found(&repo), vec![at("tests/calc.rs", fn_line(&repo, "tests/calc.rs", "adds"), "passes-at-base")]);
}

#[test]
fn a_cargo_error_other_than_a_compile_error_is_not_run() {
    let repo = calc(&[]);
    rebase(&repo, &[("Cargo.lock", "version = 4\n")], "a lock file that --locked refuses");
    repo.put("tests/calc.rs", ADDS.as_bytes());
    assert_eq!(found(&repo), vec![(None, None, "not-run".into())]);
}

#[test]
fn a_run_stopped_by_the_budget_is_a_finding() {
    let repo = calc(&[]);
    two_targets(&repo);
    let bin = nextest_stub(&repo, "exec sleep 30");
    let how = Gate { budget: 1, env: vec![("PATH", path_after(&bin))] };
    assert_eq!(findings(&repo, how), vec![
        (
            None,
            None,
            "budget-reached".into(),
            "cargo nextest was stopped at 1 seconds (`--base-run-budget`) while it built or ran `--test calc` of the \
             package at the top level, so none of them was judged."
                .into(),
        ),
        not_reached(1, "more"),
    ]);
}

#[test]
fn a_cargo_nextest_that_cannot_start_is_a_finding_for_each_target() {
    let repo = calc(&[]);
    two_targets(&repo);
    // PATH holds what the gate itself runs, git and tar, and no cargo.
    let bin = repo.outside.join("no-cargo");
    std::fs::create_dir_all(&bin).unwrap();
    for tool in ["git", "tar"] {
        let found = Command::new("sh").args(["-c", &format!("command -v {tool}")]).output().unwrap();
        let at = PathBuf::from(String::from_utf8(found.stdout).unwrap().trim());
        std::os::unix::fs::symlink(at, bin.join(tool)).unwrap();
    }
    let how = Gate { env: vec![("PATH", bin.into())], ..Gate::default() };
    let expected: Vec<Found> = ["calc", "more"]
        .iter()
        .map(|name| {
            let message = format!(
                "could not start cargo nextest to run the changed Rust tests of `--test {name}` of the package at \
                 the top level at the base: No such file or directory (os error 2)"
            );
            (None, None, "not-run".into(), message)
        })
        .collect();
    assert_eq!(findings(&repo, how), expected);
}

#[test]
fn a_cargo_error_writing_no_report_is_a_finding_for_each_target() {
    let repo = calc(&[]);
    two_targets(&repo);
    let bin = nextest_stub(&repo, "echo 'error: failed to run custom build command' >&2; exit 101");
    let found = findings(&repo, Gate { env: vec![("PATH", path_after(&bin))], ..Gate::default() });
    assert_eq!(found.len(), 2, "{found:?}");
    for ((path, line, rule, message), name) in found.iter().zip(["calc", "more"]) {
        assert_eq!((path, line, rule.as_str()), (&None, &None, "not-run"));
        let opening = format!(
            "cargo nextest, running the changed Rust tests of `--test {name}` of the package at the top level at the \
             base, exited with status 101 and wrote no report the gate can read ("
        );
        assert!(message.starts_with(&opening), "{message}");
        assert!(message.ends_with("): error: failed to run custom build command"), "{message}");
    }
}

#[test]
fn a_changed_test_file_that_does_not_parse_is_not_run_and_the_others_are() {
    // src/other.rs held a test module at the base, so it holds tests; tests/0_old.rs, deleted, holds none now.
    let repo = calc(&[]);
    let other = format!("{OTHER}#[cfg(test)]\nmod tests {{}}\n");
    rebase(&repo, &[("src/other.rs", &other), ("tests/0_old.rs", "#[test]\nfn old() {}\n")], "a test module");
    std::fs::remove_file(repo.repo.join("tests/0_old.rs")).unwrap();
    repo.put("src/other.rs", format!("{OTHER}fn broken( {{\n").as_bytes());
    repo.put("tests/a_broken.rs", b"#[test]\nfn broken( {\n");
    repo.put("tests/calc.rs", ADDS.as_bytes());
    let found = findings(&repo, Gate::default());
    let places: Vec<_> = found.iter().map(|(path, line, rule, _)| (path.clone(), *line, rule.clone())).collect();
    assert_eq!(places, vec![
        at("src/other.rs", 4, "not-run"),
        at("tests/a_broken.rs", 2, "not-run"),
        at("tests/calc.rs", fn_line(&repo, "tests/calc.rs", "adds"), "passes-at-base"),
    ]);
    for (_, _, _, message) in &found[..2] {
        assert!(message.starts_with("does not parse as Rust ("), "{message}");
        assert!(message.ends_with("), so its tests were not run at the base."), "{message}");
    }
}

#[test]
fn a_file_under_a_tests_directory_is_the_change_s_in_the_run_at_the_base() {
    let repo = calc(&[]);
    rebase(&repo, &[("tests/data.txt", "one\n")], "the tests' data");
    repo.put("tests/data.txt", b"two\n");
    repo.put("tests/calc.rs", b"#[test]\nfn reads() {\n    assert_eq!(include_str!(\"data.txt\"), \"two\\n\");\n}\n");
    assert_eq!(found(&repo), vec![at("tests/calc.rs", 2, "passes-at-base")]);
}

#[test]
fn a_base_that_cannot_be_copied_is_a_finding() {
    let repo = calc(&[]);
    repo.put("tests/calc.rs", ADDS.as_bytes());
    let bin = repo.stub("tar", "echo 'tar: no space' >&2; exit 2");
    let found = findings(&repo, Gate { env: vec![("PATH", path_after(&bin))], ..Gate::default() });
    assert_eq!(found, vec![(
        None,
        None,
        "not-run".into(),
        "could not copy the base to run the changed Rust tests in: tar could not unpack the base: tar: no space; \
         they were not run there."
            .into(),
    )]);
}

/// Adds a submodule at each of `names` to `repo`, all of one repository outside it whose one file is notes.txt, and
/// commits them with `more`, (path, contents), as the new base.
fn add_submodules(repo: &Repo, names: &[&str], more: &[(&str, &str)]) {
    let lib = repo.outside.join("lib");
    std::fs::create_dir_all(&lib).unwrap();
    let git = |args: &[&str]| {
        let done = Command::new("git").current_dir(&lib).args(args).output().unwrap();
        assert!(done.status.success(), "{}", String::from_utf8_lossy(&done.stderr));
    };
    git(&["init", "--quiet", "-b", "main"]);
    std::fs::write(lib.join("notes.txt"), "notes\n").unwrap();
    git(&["add", "notes.txt"]);
    git(&["-c", "user.name=t", "-c", "user.email=t@example.com", "-c", "commit.gpgsign=false", "commit", "--quiet",
          "-m", "lib"]);
    for name in names {
        let lib = lib.to_str().unwrap();
        repo.git(&["-c", "protocol.file.allow=always", "submodule", "--quiet", "add", lib, name]);
    }
    rebase(repo, more, "submodules");
}

#[test]
fn empty_submodules_are_one_finding() {
    let repo = calc(&[]);
    add_submodules(&repo, &["vendor/a", "vendor/b"], &[]);
    repo.git(&["submodule", "--quiet", "deinit", "--force", "--all"]);
    repo.put("tests/calc.rs", ADDS.as_bytes());
    assert_eq!(findings(&repo, Gate::default())[0], (
        None,
        None,
        "not-run".into(),
        "the copy of the base leaves submodules empty, so a changed Rust test that needs one, if any does, was not \
         judged: `vendor/a`, which is not initialised in the working tree; `vendor/b`, which is not initialised in \
         the working tree."
            .into(),
    ));
}

#[test]
fn a_test_reading_from_a_submodule_is_judged_at_the_base() {
    let repo = calc(&[]);
    add_submodules(&repo, &["vendor/lib"], &[]);
    let reads = "#[test]\nfn reads() {\n    assert_eq!(include_str!(\"../vendor/lib/notes.txt\"), \"notes\\n\");\n}\n";
    repo.put("tests/calc.rs", reads.as_bytes());
    assert_eq!(found(&repo), vec![at("tests/calc.rs", 2, "passes-at-base")]);
}

#[test]
fn a_submodule_without_the_base_s_commit_is_one_finding() {
    let repo = calc(&[]);
    add_submodules(&repo, &["vendor/lib"], &[]);
    // The base records a commit that the submodule's repository in the working tree never had.
    repo.git(&["update-index", "--cacheinfo", &format!("160000,{},vendor/lib", "1".repeat(40))]);
    repo.git(&["commit", "--quiet", "-m", "unknown submodule commit"]);
    repo.put("tests/calc.rs", ADDS.as_bytes());
    assert_eq!(findings(&repo, Gate::default())[0], (
        None,
        None,
        "not-run".into(),
        "the copy of the base leaves submodules empty, so a changed Rust test that needs one, if any does, was not \
         judged: `vendor/lib`, whose repository in the working tree does not have the base's commit 111111111111."
            .into(),
    ));
}

#[test]
fn a_submodule_marked_export_ignore_stays_out_of_the_copy() {
    let repo = calc(&[]);
    add_submodules(&repo, &["vendor/lib"], &[(".gitattributes", "vendor/lib export-ignore\n")]);
    repo.git(&["submodule", "--quiet", "deinit", "--force", "--all"]);
    repo.put("tests/calc.rs", ADDS.as_bytes());
    assert_eq!(found(&repo), vec![at("tests/calc.rs", 2, "passes-at-base")]);
}

#[test]
fn a_directory_of_the_tests_the_change_makes_a_file_is_replaced_in_the_copy() {
    let repo = calc(&[]);
    rebase(&repo, &[("tests/data/one.txt", "one\n")], "the tests' data");
    std::fs::remove_dir_all(repo.repo.join("tests/data")).unwrap();
    repo.put("tests/data", b"two\n");
    repo.put("tests/calc.rs", b"#[test]\nfn reads() {\n    assert_eq!(include_str!(\"data\"), \"two\\n\");\n}\n");
    assert_eq!(found(&repo), vec![at("tests/calc.rs", 2, "passes-at-base")]);
}

#[test]
fn a_test_cargo_builds_no_target_of_is_not_run() {
    let repo = calc(&[]);
    repo.put("tests/common/mod.rs", ADDS.replace("fn adds", "fn shared").as_bytes());
    assert_eq!(findings(&repo, Gate::default()), vec![(
        Some("tests/common/mod.rs".into()),
        Some(2),
        "not-run".into(),
        "`shared` was not run at the base: no `tests/common/main.rs` makes a target of its directory.".into(),
    )]);
}

#[test]
fn a_package_below_the_top_runs_its_tests_in_its_own_directory() {
    let repo = calc(&[]);
    let engine = [
        ("engine/Cargo.toml", "[package]\nname = \"engine\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("engine/Cargo.lock", "version = 4\n\n[[package]]\nname = \"engine\"\nversion = \"0.1.0\"\n"),
        ("engine/src/lib.rs", "pub fn one() -> u32 {\n    1\n}\n"),
    ];
    rebase(&repo, &engine, "a package below the top");
    // Its tests' data, under its own tests directory, is the change's too.
    rebase(&repo, &[("engine/tests/data.txt", "one\n")], "the package's tests' data");
    repo.put("engine/tests/data.txt", b"two\n");
    repo.put("engine/tests/one.rs", b"#[test]\nfn is_one() {\n    assert_eq!(engine::one(), 1);\n\
                                      \x20   assert_eq!(include_str!(\"data.txt\"), \"two\\n\");\n}\n");
    assert_eq!(found(&repo), vec![at("engine/tests/one.rs", 2, "passes-at-base")]);
}

#[test]
fn a_test_is_changed_by_a_line_the_change_adds_or_removes_from_its_attributes_to_its_end() {
    let test = "#[test]\nfn adds() { assert_eq!(calc::add(1, 2), 3); }\n";
    let repo = calc(&[]);
    for (name, base, after, judged) in [
        // No two in a row share a base, which each commits.
        ("its attribute edited", test.to_string(), test.replace("#[test]", "#[test] // run"), true),
        ("a line removed right after it", format!("{test}// tail\n"), test.to_string(), true),
        ("its last line edited", test.to_string(), test.replace("3); }", "3);  }"), true),
        ("a line removed above the line above it", format!("// a\n// b\n{test}"), format!("// b\n{test}"), false),
    ] {
        rebase(&repo, &[("tests/calc.rs", &base)], name);
        repo.put("tests/calc.rs", after.as_bytes());
        let line = fn_line(&repo, "tests/calc.rs", "adds");
        let expected = if judged { vec![at("tests/calc.rs", line, "passes-at-base")] } else { vec![] };
        assert_eq!(found(&repo), expected, "{name}");
    }
}

#[test]
fn a_change_to_the_tests_alone_runs_nothing() {
    let repo = calc(&[]);
    repo.put("src/lib.rs", LIB.as_bytes());
    repo.put("tests/calc.rs", ADDS.as_bytes());
    assert_eq!(found(&repo), vec![]);
}
