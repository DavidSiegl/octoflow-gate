//! The workspace's own tasks, run as `cargo xtask <task>` from anywhere in it (.cargo/config.toml):
//!
//! ```text
//! cargo xtask compare [--repo DIR] [BASE] [-- GATE_ARGS...]
//! cargo xtask fixture DIR
//! ```
//!
//! `compare` judges the change the git working tree at DIR (default the current directory) makes since BASE (default
//! `main`) with gate.py and with the Rust `gate`, each given `--base BASE --out FILE` and GATE_ARGS, and compares
//! their quality.json, part by part, and their quality file, line by line. It exits 0 when they agree, 1 when they
//! differ, printing each part that does and the quality files from their first line that does, and 2 when
//! either gate could not judge the change or the arguments are bad. Both run in this workspace's Python environment,
//! with its dev tools on PATH: gate.py as `uv run --project <workspace> python <workspace>/gate.py`, reading Rust with
//! the gate-rust helper built from the workspace's gate-rust/, and `gate` as `uv run --project <workspace> cargo run`,
//! with that environment's Python as the judged project's (`--python python`), as it is gate.py's.
//! Each gate runs the tree's `[[test]]` commands unless GATE_ARGS gives `--tests` or `--coverage`. A change between
//! two commits is compared with the later one checked out.
//!
//! `fixture` makes DIR, which must not exist, a git repository whose working tree holds a change since its branch
//! `main` that gives every linter and check something to report ([`fixture`]), for `cargo xtask compare --repo
//! DIR`. It exits 0 once DIR is made, and 2 when it cannot make it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::Value;

mod fixture;

const USAGE: &str = "usage: cargo xtask compare [--repo DIR] [BASE] [-- GATE_ARGS...]\n       cargo xtask fixture DIR";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let task = args.next();
    let done = match task.as_ref().and_then(|t| t.to_str()) {
        Some("compare") => compare(args.collect()),
        Some("fixture") => match (args.next(), args.next()) {
            (Some(dir), None) => fixture::make(Path::new(&dir)).map(|()| true),
            _ => Err(USAGE.into()),
        },
        _ => Err(USAGE.into()),
    };
    match done {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::from(1),
        Err(e) => {
            let task = task.map(|t| format!(" {}", t.to_string_lossy())).unwrap_or_default();
            eprintln!("xtask{task}: {e}");
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
    let mut agree = true;
    let parts = |report: &Value| report.as_object().cloned().unwrap_or_default();
    let (json_python, json_rust) = (parts(&python.json), parts(&rust.json));
    for part in json_python.keys().chain(json_rust.keys().filter(|part| !json_python.contains_key(*part))) {
        let (from_python, from_rust) = (&python.json[part], &rust.json[part]);
        if from_python != from_rust {
            agree = false;
            println!("{part} differ:\n  gate.py: {from_python}\n  gate:    {from_rust}");
        }
    }
    let lines = |reports: &'_ Reports| -> Vec<String> { reports.markdown.split('\n').map(String::from).collect() };
    let (md_python, md_rust) = (lines(&python), lines(&rust));
    if let Some(k) = (0..md_python.len().max(md_rust.len())).find(|&k| md_python.get(k) != md_rust.get(k)) {
        agree = false;
        // Five lines from the first that differs, of each.
        let from = |md: &[String]| -> String {
            md.get(k..).unwrap_or_default().iter().take(5).map(|l| format!("\n    {l}")).collect()
        };
        let (from_python, from_rust) = (from(&md_python), from(&md_rust));
        println!("the quality files differ from line {}:\n  gate.py:{from_python}\n  gate:{from_rust}", k + 1);
    }
    if agree {
        println!("gate.py and gate agree.");
    }
    Ok(agree)
}

/// One gate's reports of a change: its quality.json, parsed, and its quality file, the Markdown.
struct Reports {
    json: Value,
    markdown: String,
}

/// The reports of gate.py, then of the Rust gate, judging the tree at `repo` since `base` with `gate_args`, each
/// written to `out`, a directory.
fn judge_twice(
    workspace: &Path,
    repo: &Path,
    base: &str,
    gate_args: &[OsString],
    out: &Path,
) -> Result<(Reports, Reports), String> {
    // Both in this workspace's Python environment, so that each finds the linters it installs on PATH.
    let in_uv = |program: &OsStr| {
        let mut command = Command::new("uv");
        command.args(["run", "--quiet", "--project"]).arg(workspace).arg(program);
        command
    };
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let mut python = in_uv("python".as_ref());
    python.arg(workspace.join("gate.py")).arg("--rust-syntax").arg(gate_rust(workspace, &cargo)?);
    let mut rust = in_uv(&cargo);
    rust.args(["run", "--quiet", "--package", "gate", "--manifest-path"]).arg(workspace.join("Cargo.toml")).arg("--");
    // The Python that runs gate.py, which reruns a tree's tests with its own interpreter, whatever the tree's
    // gate.toml names.
    rust.args(["--python", "python"]);
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
        let read =
            |path: &Path| std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()));
        let json_path = out.join(format!("{name}.json"));
        let json = serde_json::from_str(&read(&json_path)?)
            .map_err(|e| format!("{} is not JSON: {e}", json_path.display()))?;
        reports.push(Reports { json, markdown: read(&md)? });
    }
    let rust = reports.pop().ok_or("no report of gate")?;
    let python = reports.pop().ok_or("no report of gate.py")?;
    Ok((python, rust))
}

/// The path of the gate-rust helper, with which gate.py reads Rust, built from the workspace's gate-rust/ with `cargo`:
/// a debug build, wherever CARGO_TARGET_DIR puts it, as tests/test_gate.py's gate_rust builds it. Errs when it does
/// not build.
fn gate_rust(workspace: &Path, cargo: &OsStr) -> Result<PathBuf, String> {
    let built = Command::new(cargo)
        .args(["build", "--locked", "--quiet", "--message-format=json", "--manifest-path"])
        .arg(workspace.join("gate-rust/Cargo.toml"))
        .output()
        .map_err(|e| format!("cannot run cargo to build the gate-rust helper: {e}"))?;
    if !built.status.success() {
        let why = String::from_utf8_lossy(&built.stderr);
        return Err(format!("cargo could not build the gate-rust helper: {why}"));
    }
    // The executable's path from cargo's own report, wherever CARGO_TARGET_DIR puts it.
    let messages = built.stdout.split(|&b| b == b'\n').filter_map(|line| serde_json::from_slice::<Value>(line).ok());
    let mut executables = messages
        .filter(|m| m["reason"] == "compiler-artifact")
        .filter_map(|m| m["executable"].as_str().map(PathBuf::from));
    executables.next().ok_or_else(|| "cargo built no gate-rust helper executable".into())
}
