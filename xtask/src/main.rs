//! The workspace's own tasks, run as `cargo xtask <task>` from anywhere in it (.cargo/config.toml):
//!
//! ```text
//! cargo xtask compare [--repo DIR] [BASE] [-- GATE_ARGS...]
//! ```
//!
//! `compare` judges the change the git working tree at DIR (default the current directory) makes since BASE (default
//! `main`) with gate.py and with the Rust `gate`, each given `--base BASE --out FILE` and GATE_ARGS, and compares
//! their quality.json in the parts the Rust gate has ported so far ([`ported`]). It exits 0 when they agree, 1 when
//! they differ, printing each part that does, and 2 when either gate could not judge the change or the arguments are
//! bad. Both run in this workspace's Python environment, with its dev tools on PATH: gate.py as `uv run --project
//! <workspace> python <workspace>/gate.py`, and `gate` as `uv run --project <workspace> cargo run`. Each gate runs
//! the tree's `[[test]]` commands unless GATE_ARGS gives `--tests` or `--coverage`; `-- --mutation-budget 1
//! --base-run-budget 1` keeps gate.py's mutation and base runs, which the Rust gate does not port yet, short.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::{json, Value};

const USAGE: &str = "usage: cargo xtask compare [--repo DIR] [BASE] [-- GATE_ARGS...]";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    if args.next().as_deref() != Some("compare".as_ref()) {
        eprintln!("{USAGE}");
        return ExitCode::from(2);
    }
    match compare(args.collect()) {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            eprintln!("xtask compare: {e}");
            ExitCode::from(2)
        }
    }
}

/// Runs `compare` with its arguments `args`, as the module comment says: true when the two gates agree, false when
/// they differ; errs with why it could not compare them.
fn compare(args: Vec<OsString>) -> Result<bool, String> {
    let (mut repo, mut base, mut gate_args) = (PathBuf::from("."), "main".to_string(), vec![]);
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--repo") => repo = args.next().ok_or("--repo needs a directory")?.into(),
            Some("--") => gate_args = args.by_ref().collect(),
            Some(rev) if !rev.starts_with('-') => base = rev.to_string(),
            _ => return Err(format!("{}\nunknown argument {arg:?}", USAGE)),
        }
    }
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR")).parent().ok_or("xtask has no workspace")?;
    let out = std::env::temp_dir().join(format!("xtask-compare-{}", std::process::id()));
    std::fs::create_dir_all(&out).map_err(|e| format!("cannot make {}: {e}", out.display()))?;
    let judged = judge_twice(workspace, &repo, &base, &gate_args, &out);
    // The reports are read by now, or no longer needed.
    if let Err(e) = std::fs::remove_dir_all(&out) {
        eprintln!("xtask compare: cannot remove {}: {e}", out.display());
    }
    let (python, rust) = judged?;
    let (python, rust) = (ported(&python, true), ported(&rust, false));
    let mut agree = true;
    for (part, from_python) in python.as_object().into_iter().flatten() {
        let from_rust = &rust[part];
        if from_python != from_rust {
            agree = false;
            println!("{part} differ:\n  gate.py: {from_python}\n  gate:    {from_rust}");
        }
    }
    if agree {
        println!("gate.py and gate agree on the parts ported so far.");
    }
    Ok(agree)
}

/// quality.json of gate.py, then of the Rust gate, judging the tree at `repo` since `base` with `gate_args`, each
/// written to `out`, a directory.
fn judge_twice(
    workspace: &Path,
    repo: &Path,
    base: &str,
    gate_args: &[OsString],
    out: &Path,
) -> Result<(Value, Value), String> {
    // Both in this workspace's Python environment, so that each finds the linters it installs on PATH.
    let in_uv = |program: &OsStr| {
        let mut command = Command::new("uv");
        command.args(["run", "--quiet", "--project"]).arg(workspace).arg(program);
        command
    };
    let mut python = in_uv("python".as_ref());
    python.arg(workspace.join("gate.py"));
    let mut rust = in_uv(&std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()));
    rust.args(["run", "--quiet", "--package", "gate", "--manifest-path"]).arg(workspace.join("Cargo.toml")).arg("--");
    let mut reports = vec![];
    for (name, mut command) in [("gate.py", python), ("gate", rust)] {
        let md = out.join(format!("{name}.md"));
        let done = command
            .current_dir(repo)
            .args(["--base", base, "--out"])
            .arg(&md)
            .args(gate_args)
            .status()
            .map_err(|e| format!("cannot run {name}: {e}"))?;
        if !matches!(done.code(), Some(0 | 1)) {
            return Err(format!("{name} could not judge the change: {done}"));
        }
        let json = out.join(format!("{name}.json"));
        let text = std::fs::read_to_string(&json).map_err(|e| format!("cannot read {}: {e}", json.display()))?;
        reports.push(serde_json::from_str(&text).map_err(|e| format!("{} is not JSON: {e}", json.display()))?);
    }
    let rust = reports.pop().ok_or("no report of gate")?;
    let python = reports.pop().ok_or("no report of gate.py")?;
    Ok((python, rust))
}

/// The linters the Rust gate runs; gate.py's findings of any other are not compared.
const PORTED_LINTERS: [&str; 3] = ["pymarkdown", "shellcheck", "actionlint"];

/// The parts of `report`, a quality.json, that the Rust gate has ported, as an object of the parts by name; those of
/// gate.py's when `from_python`, which leaves out what comes of the parts not ported. The verdict, the count of
/// findings and the condition counting the linters' findings on added lines are left out of both: they count what
/// was not ported too, and the findings themselves are compared.
fn ported(report: &Value, from_python: bool) -> Value {
    let strings = |part: &str| report[part].as_array().cloned().unwrap_or_default();
    let counts_findings = |c: &str| c.ends_with(" findings on lines the change added; each is listed below.");
    let conditions: Vec<Value> = strings("failed_conditions")
        .into_iter()
        .filter(|c| c.as_str().is_some_and(|c| !counts_findings(c)))
        .filter(|c| !from_python || c.as_str().is_some_and(ported_condition))
        .collect();
    let findings: Vec<Value> = strings("findings")
        .into_iter()
        .filter(|f| !from_python || f["check"].as_str().is_some_and(|c| PORTED_LINTERS.contains(&c)))
        .collect();
    let suppressions: Vec<Value> = strings("suppressions")
        .into_iter()
        .filter(|s| !from_python || s["path"].as_str().is_some_and(ported_suppression))
        .collect();
    json!({
        "base": report["base"],
        "head": report["head"],
        "failed_conditions": conditions,
        "findings": findings,
        "failing_tests": report["failing_tests"],
        "coverage": report["coverage"],
        "suppressions": suppressions,
        "build_config": report["build_config"],
    })
}

/// Whether gate.py's failed condition `c` comes of a part the Rust gate has ported: a ported linter that could not
/// run, a test command or failing tests, coverage, files under mutmut's state, or a stray line end. The wording is
/// gate.py's judge_change's and run_test_commands's.
fn ported_condition(c: &str) -> bool {
    PORTED_LINTERS.iter().any(|linter| c.starts_with(&format!("`{linter}` ")))
        || c.starts_with("The test command `")
        || c.ends_with(" tests fail; each is listed below.")
        || c.starts_with("Coverage of new code is ")
        || (c.starts_with("The change adds or edits ") && c.contains(" under `mutants/`, "))
        || c.contains(", which a linter that reads the file ends a line at and git does not.")
}

/// Whether the Rust gate reads the suppressions of the file at `path`: a shell script, a workflow or Markdown. A
/// shell script that is Python by its `#!` is gate.py's alone until Python is ported (gate-standalone/15).
fn ported_suppression(path: &str) -> bool {
    path.ends_with(".sh")
        || path.ends_with(".md")
        || (path.starts_with(".github/workflows/") && (path.ends_with(".yml") || path.ends_with(".yaml")))
}
