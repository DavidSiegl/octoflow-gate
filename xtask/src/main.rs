//! The workspace's own tasks, run as `cargo xtask <task>` from anywhere in it (.cargo/config.toml):
//!
//! ```text
//! cargo xtask compare [--repo DIR] [BASE] [-- GATE_ARGS...]
//! cargo xtask fixture DIR
//! ```
//!
//! `compare` judges the change the git working tree at DIR (default the current directory) makes since BASE (default
//! `main`) with gate.py and with the Rust `gate`, each given `--base BASE --out FILE` and GATE_ARGS, and compares
//! their quality.json and their quality file in the parts the Rust gate has ported so far ([`ported`],
//! [`ported_markdown`]). It exits 0 when they agree, 1 when they differ, printing each part that does, and 2 when
//! either gate could not judge the change or the arguments are bad. Both run in this workspace's Python environment,
//! with its dev tools on PATH: gate.py as `uv run --project <workspace> python <workspace>/gate.py`, reading Rust with
//! the gate-rust helper built from the workspace's gate-rust/, and `gate` as `uv run --project <workspace> cargo run`,
//! with that environment's Python as the judged project's (`--python python`), as it is gate.py's.
//! Each gate runs the tree's `[[test]]` commands unless GATE_ARGS gives `--tests` or `--coverage`. A change between
//! two commits is compared with the later one checked out.
//!
//! `fixture` makes DIR, which must not exist, a git repository whose working tree holds a change since its branch
//! `main` that gives every part ported so far something to report ([`fixture`]), for `cargo xtask compare --repo
//! DIR`. It exits 0 once DIR is made, and 2 when it cannot make it.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::{json, Value};

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
    let (json_python, json_rust) = (ported(&python.json, true), ported(&rust.json, false));
    for (part, from_python) in json_python.as_object().into_iter().flatten() {
        let from_rust = &json_rust[part];
        if from_python != from_rust {
            agree = false;
            println!("{part} differ:\n  gate.py: {from_python}\n  gate:    {from_rust}");
        }
    }
    let (md_python, md_rust) = (ported_markdown(&python, true), ported_markdown(&rust, false));
    if let Some(k) = (0..md_python.len().max(md_rust.len())).find(|&k| md_python.get(k) != md_rust.get(k)) {
        agree = false;
        // Five lines from the first that differs, of each.
        let from = |md: &[String]| -> String {
            md.get(k..).unwrap_or_default().iter().take(5).map(|l| format!("\n    {l}")).collect()
        };
        let (from_python, from_rust) = (from(&md_python), from(&md_rust));
        println!("the quality files differ from ported line {}:\n  gate.py:{from_python}\n  gate:{from_rust}", k + 1);
    }
    if agree {
        println!("gate.py and gate agree on the parts ported so far.");
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

/// The linters the Rust gate runs; gate.py's findings of any other are not compared.
const PORTED_LINTERS: [&str; 4] = ["pymarkdown", "shellcheck", "actionlint", "clippy"];

/// The parts of `report`, a quality.json, that the Rust gate has ported, as an object of the parts by name; those of
/// gate.py's when `from_python`, which leaves out what comes of the parts not ported. The verdict and the count of
/// findings are left out of both: they count what was not ported too, and the findings themselves are compared.
fn ported(report: &Value, from_python: bool) -> Value {
    let kept = |part: &str, keep: Keep| -> Vec<Value> {
        let entries = report[part].as_array().into_iter().flatten();
        entries.filter(|e| keep(e, from_python)).cloned().collect()
    };
    json!({
        "base": report["base"],
        "head": report["head"],
        "failed_conditions": kept("failed_conditions", ported_condition),
        "findings": kept("findings", ported_finding),
        "failing_tests": report["failing_tests"],
        "coverage": report["coverage"],
        "suppressions": report["suppressions"],
        "build_config": report["build_config"],
    })
}

/// Whether an entry of one of quality.json's lists is compared, given whether it is gate.py's.
type Keep = fn(&Value, bool) -> bool;

/// The lines of `reports`' quality file that come of the parts the Rust gate has ported, gate.py's when
/// `from_python`. The verdict's line is left out. Of the sections that list the failed conditions and the findings,
/// only the items are kept, without their numbers, and only those whose entry in quality.json [`ported`] keeps; a
/// section none of whose items are kept is left out whole. Their other lines are left out, for they count what was
/// not ported too; whether a list is empty, and how much of it is not listed, quality.json's comparison already
/// holds. The two files agree only while gate.py's quality file lists every finding it has, fewer than
/// `QUALITY_ISSUE_LIMIT`: beyond, the findings it leaves unlisted can differ from the Rust gate's.
fn ported_markdown(reports: &Reports, from_python: bool) -> Vec<String> {
    let entries = |part: &str| reports.json[part].as_array().cloned().unwrap_or_default();
    let (issues, to_weigh): (Vec<Value>, Vec<Value>) =
        entries("findings").into_iter().partition(|f| f["fails"].as_bool() == Some(true));
    let mut out = vec![];
    for (k, section) in reports.markdown.split("\n## ").enumerate() {
        let mut lines = section.lines();
        let title = lines.next().unwrap_or_default();
        if k == 0 {
            out.extend(lines.map(String::from));
            continue;
        }
        // Each item of a list stands for the next entry of the quality.json list it comes from, in order.
        let (listed, keep): (Vec<Value>, Keep) = match title {
            "Failed conditions" => (entries("failed_conditions"), ported_condition),
            "Issues on changed lines" => (issues.clone(), ported_finding),
            "Findings to weigh" => (to_weigh.clone(), ported_finding),
            _ => {
                out.push(format!("## {title}"));
                out.extend(lines.map(String::from));
                continue;
            }
        };
        let mut listed = listed.iter();
        let mut items = vec![];
        for item in lines.filter_map(list_item) {
            if listed.next().is_some_and(|entry| keep(entry, from_python)) {
                items.push(item.to_string());
            }
        }
        if !items.is_empty() {
            out.push(format!("## {title}"));
            out.extend(items);
        }
    }
    out
}

/// `line` without its bullet, `- `, or its number, as `12. `; None when it is no item of a list.
fn list_item(line: &str) -> Option<&str> {
    line.strip_prefix("- ").or_else(|| {
        let (number, item) = line.split_once(". ")?;
        number.parse::<usize>().is_ok().then_some(item)
    })
}

/// Whether quality.json's failed condition `c` is compared, given whether it is gate.py's: not the count of the
/// linters' findings on added lines, nor of a check's findings, which count what was not ported too, and of
/// gate.py's, only those that [`ported_condition_of_python`] keeps.
fn ported_condition(c: &Value, from_python: bool) -> bool {
    let c = c.as_str().unwrap_or_default();
    let counts_findings = c.ends_with(" findings on lines the change added; each is listed below.")
        || (c.contains(" findings of the `") && c.ends_with("` check; each is listed below."));
    !counts_findings && (!from_python || ported_condition_of_python(c))
}

/// Whether gate.py's failed condition `c` comes of a part the Rust gate has ported: a ported linter that could not
/// run, a test command or failing tests, coverage, files under mutmut's state, or a stray line end. The wording is
/// gate.py's judge_change's and run_test_commands's.
fn ported_condition_of_python(c: &str) -> bool {
    PORTED_LINTERS.iter().any(|linter| c.starts_with(&format!("`{linter}` ")))
        || c.starts_with("The test command `")
        || c.ends_with(" tests fail; each is listed below.")
        || c.starts_with("Coverage of new code is ")
        || (c.starts_with("The change adds or edits ") && c.contains(" under `mutants/`, "))
        || c.contains(", which a linter that reads the file ends a line at and git does not.")
}

/// The checks the Rust gate runs whole, over each language gate.py runs them over (gate::parser_checks,
/// gate::flaky_tests, gate::passes_at_base).
const WHOLE_CHECKS: [&str; 5] =
    ["test-integrity", "cognitive-complexity", "vague-names", "flaky-tests", "passes-at-base"];

/// The check the Rust gate runs over Rust alone (gate::mutants), its Python half coming with gate-standalone/16.
const RUST_ONLY_CHECK: &str = "surviving-mutants";

/// Whether quality.json's finding `f` is compared, given whether it is gate.py's: of gate.py's, only a ported
/// linter's, one of the [`WHOLE_CHECKS`], and of the [`RUST_ONLY_CHECK`] one on a .rs file, or on no file when it is
/// of its Rust half ([`rust_s_mutation_run`]).
fn ported_finding(f: &Value, from_python: bool) -> bool {
    let check = f["check"].as_str().unwrap_or_default();
    let path = f["path"].as_str().unwrap_or_default();
    let message = f["message"].as_str().unwrap_or_default();
    let of_rust = if f["path"].is_null() { rust_s_mutation_run(message) } else { path.ends_with(".rs") };
    !from_python
        || PORTED_LINTERS.contains(&check)
        || WHOLE_CHECKS.contains(&check)
        || (check == RUST_ONLY_CHECK && of_rust)
}

/// Whether `message`, of a surviving-mutants finding on no file, is the Rust half's, by gate.py's words in
/// `_cargo_mutants_on_copy` and what it calls; the Python half's speak of mutmut and "the change's functions".
fn rust_s_mutation_run(message: &str) -> bool {
    message.contains("cargo-mutants") || message.ends_with("the change's Rust was not mutation-tested.")
}
