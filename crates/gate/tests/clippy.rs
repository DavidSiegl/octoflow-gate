//! The `gate` binary with the real cargo clippy, over a crate of no dependencies in a subdirectory of the
//! repository, as gate.py's TestClippy runs gate.py (tests/test_gate.py). cargo runs offline, so it never reaches
//! for the registry.
//!
//! TestClippy's test of a cargo past the timeout is not ported: the Rust gate's timeout is a constant of ten minutes,
//! and process.rs's own tests hold that a process past its timeout is killed with what it started.

use std::path::Path;

use common::Repo;

mod common;

const LIB: &str = "pub fn one() -> i32 {\n    1\n}\n";
const TWO: &str = "pub fn two() -> i32 {\n    return 2;\n}\n";

fn crate_repo() -> Repo {
    Repo::with(&[
        ("pyproject.toml", b"[tool.ruff]\n"),
        ("crate/Cargo.toml", b"[package]\nname = \"tiny\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("crate/src/lib.rs", LIB.as_bytes()),
    ])
}

/// The exit status and standard output of `gate --base HEAD` in `repo`, with `bin` ahead of PATH when given.
fn gate(repo: &Repo, bin: Option<&Path>) -> (i32, String) {
    let mut command = repo.gate_command("HEAD");
    command.env("CARGO_NET_OFFLINE", "true");
    if let Some(bin) = bin {
        command.env("PATH", format!("{}:{}", bin.display(), std::env::var("PATH").unwrap()));
    }
    let done = command.output().unwrap();
    (done.status.code().unwrap(), String::from_utf8(done.stdout).unwrap())
}

#[test]
fn a_lint_on_an_added_line_is_an_issue_listed_once() {
    let repo = crate_repo();
    // --all-targets checks the library twice, as itself and as its tests, and clippy reports the lint for each.
    repo.put("crate/src/lib.rs", format!("{LIB}{TWO}").as_bytes());
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("- 1 findings on lines the change added; each is listed below.\n"), "{out}");
    assert!(out.contains("1. `crate/src/lib.rs:5` warning clippy clippy::needless_return: unneeded `return` \
                          statement\n"), "{out}");
    assert_eq!(out.matches("needless_return").count(), 1, "{out}");
}

#[test]
fn a_lint_on_an_old_line_counts_as_elsewhere() {
    let repo = crate_repo();
    repo.commit(&[("crate/src/lib.rs", TWO.as_bytes())], "a lint");
    repo.put("crate/src/lib.rs", format!("{TWO}{LIB}").as_bytes());
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 0, "{out}");
    assert!(out.contains("1 other open issues in the project"), "{out}");
}

#[test]
fn a_compile_error_on_an_old_line_fails_the_gate_naming_the_first_error() {
    let repo = crate_repo();
    let two = "pub fn two() -> i32 {\n    one() + 1\n}\n";
    repo.commit(&[("crate/src/lib.rs", format!("{LIB}{two}").as_bytes())], "a caller");
    repo.put("crate/src/lib.rs", two.as_bytes());
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("- `clippy` could not compile the workspace of `crate/Cargo.toml`: `crate/src/lib.rs:2` \
                          error clippy E0425: cannot find function `one` in this scope\n"), "{out}");
    assert!(out.contains("## Issues on changed lines\n\nNone.\n"), "{out}");
}

#[test]
fn a_lint_at_deny_in_one_crate_hides_neither_a_compile_error_in_the_next_nor_its_own_lints() {
    let repo = crate_repo();
    // b uses a, so cargo checks a first; a lint at deny there would fail a's build and leave b unchecked.
    repo.commit(&[
        ("crate/Cargo.toml", b"[workspace]\nmembers = [\"a\", \"b\"]\nresolver = \"2\"\n"),
        ("crate/a/Cargo.toml", b"[package]\nname = \"a\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                                 [lints.clippy]\nneedless_return = \"deny\"\n"),
        ("crate/a/src/lib.rs", TWO.as_bytes()),
        ("crate/b/Cargo.toml", b"[package]\nname = \"b\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                                 [dependencies]\na = { path = \"../a\" }\n"),
        ("crate/b/src/lib.rs", b"pub fn three() -> i32 {\n    a::two() + 1\n}\n"),
    ], "a workspace");
    repo.git(&["rm", "--quiet", "crate/src/lib.rs"]);
    repo.put("crate/b/src/lib.rs", b"pub fn three() -> i32 {\n    a::two() + one\n}\n");
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("- `clippy` could not compile the workspace of `crate/Cargo.toml`: `crate/b/src/lib.rs:2` \
                          error clippy E0425: cannot find value `one` in this scope\n"), "{out}");
    assert!(out.contains("1 other open issues in the project"), "{out}");
}

#[test]
fn a_crate_under_mutmut_s_state_is_not_linted() {
    let repo = crate_repo();
    repo.commit(&[
        ("mutants/broken/Cargo.toml", b"[package]\nname = \"broken\"\nversion = \"0.1.0\"\nedition = \"2021\"\n"),
        ("mutants/broken/src/lib.rs", b"pub fn f() -> i32 {\n    nope\n}\n"),
    ], "mutmut's state, committed");
    repo.put("crate/src/lib.rs", format!("{LIB}pub fn two() -> i32 {{\n    2\n}}\n").as_bytes());
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 0, "{out}");
    assert!(!out.contains("mutants/"), "{out}");
}

#[test]
fn a_tracked_cargo_lock_is_held_to_not_rewritten() {
    let repo = crate_repo();
    let lock = "# This file is automatically @generated by Cargo.\nversion = 4\n\n[[package]]\nname = \"tiny\"\n\
                version = \"0.1.0\"\n";
    repo.commit(&[("crate/Cargo.lock", lock.as_bytes())], "a lock");
    repo.put("crate/Cargo.toml", b"[package]\nname = \"tiny\"\nversion = \"0.2.0\"\nedition = \"2021\"\n");
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("- `clippy` failed with exit status 101 in `crate/Cargo.toml`: error: cannot update the \
                          lock file"), "{out}");
    assert_eq!(std::fs::read_to_string(repo.repo.join("crate/Cargo.lock")).unwrap(), lock);
}

#[test]
fn a_repository_without_rust_files_runs_no_cargo() {
    let repo = crate_repo();
    repo.git(&["rm", "--quiet", "crate/src/lib.rs"]);
    let ran = repo.outside.join("cargo-ran");
    let bin = repo.stub("cargo", &format!("echo ran > '{}'; exit 1", ran.display()));
    let (status, out) = gate(&repo, Some(&bin));
    assert_eq!(status, 0, "{out}");
    assert!(!ran.exists());
}

#[test]
fn a_cargo_without_clippy_fails_naming_it() {
    let repo = crate_repo();
    let bin = repo.stub("cargo", "echo \"error: no such command: \\`clippy\\`\" >&2; exit 101");
    let (status, out) = gate(&repo, Some(&bin));
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("- `clippy` is not installed: `cargo clippy --version` failed with exit status 101: error: \
                          no such command: `clippy`\n"), "{out}");
}

#[test]
fn an_added_allow_attribute_is_listed_as_a_suppression_in_a_rust_file_alone() {
    let repo = crate_repo();
    let lib = format!(
        "#![cfg_attr(not(test), allow(clippy::all))]\n{LIB}#[allow(clippy::needless_return)]\n{TWO}\
         #[cfg_attr(\n    not(test),\n    allow(clippy::needless_return)\n)]\npub fn three() {{}}\n\
         #[test] fn parses() {{ \"1\".parse::<i32>().expect(\"ok\"); }}\n"
    );
    repo.put("crate/src/lib.rs", lib.as_bytes());
    repo.put("crate/notes.txt", b"Write `#[allow(lint, reason = \"...\")]`.\n");
    repo.git(&["add", "crate/notes.txt"]);
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 0, "{out}");
    assert!(out.ends_with("## Suppressions added\n\n\
                           - `crate/src/lib.rs:1` `#![cfg_attr(not(test), allow(clippy::all))]`\n\
                           - `crate/src/lib.rs:5` `#[allow(clippy::needless_return)]`\n\
                           - `crate/src/lib.rs:11` `allow(clippy::needless_return)`\n"), "{out}");
}

#[test]
fn a_build_that_fails_without_a_compile_error_fails_naming_cargo_s_error() {
    let repo = crate_repo();
    repo.put("crate/build.rs", b"fn main() {\n    panic!(\"no\");\n}\n");
    repo.git(&["add", "crate/build.rs"]);
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("- `clippy` failed with exit status 101 in `crate/Cargo.toml`: error: failed to run custom \
                          build command for `tiny v0.1.0"), "{out}");
}

#[test]
fn a_manifest_cargo_cannot_read_fails_naming_it_and_the_rest_are_linted() {
    let repo = crate_repo();
    repo.put("broken/Cargo.toml", b"[package\n");
    repo.git(&["add", "broken/Cargo.toml"]);
    repo.put("crate/src/lib.rs", format!("{LIB}{TWO}").as_bytes());
    let (status, out) = gate(&repo, None);
    assert_eq!(status, 1, "{out}");
    assert!(out.contains("- `clippy` could not find the workspace of `broken/Cargo.toml`: error: "), "{out}");
    assert!(out.contains("`crate/src/lib.rs:5` warning clippy clippy::needless_return"), "{out}");
}
