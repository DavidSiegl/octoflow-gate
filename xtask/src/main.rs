//! The workspace's own tasks, run as `cargo xtask <task>` from anywhere in it (.cargo/config.toml):
//!
//! ```text
//! cargo xtask compare [--repo DIR] [BASE] [-- GATE_ARGS...]
//! ```
//!
//! `compare` judges the change the git working tree at DIR (default the current directory) makes since BASE (default
//! `main`) with gate.py and with the Rust `gate`, each given `--base BASE --out FILE` and GATE_ARGS, and compares
//! their quality.json and their quality file in the parts the Rust gate has ported so far ([`ported`],
//! [`ported_markdown`]). It exits 0 when they agree, 1 when they differ, printing each part that does, and 2 when
//! either gate could not judge the change or the arguments are bad. Both run in this workspace's Python environment,
//! with its dev tools on PATH: gate.py as `uv run --project <workspace> python <workspace>/gate.py`, and `gate` as
//! `uv run --project <workspace> cargo run`. Each gate runs the tree's `[[test]]` commands unless GATE_ARGS gives
//! `--tests` or `--coverage`; `-- --mutation-budget 1 --base-run-budget 1` keeps gate.py's mutation and base runs,
//! which the Rust gate does not port yet, short. A change between two commits is compared with the later one checked
//! out.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde_json::{json, Value};

const USAGE: &str = "usage: cargo xtask compare [--repo DIR] [BASE] [-- GATE_ARGS...]";

fn main() -> ExitCode {
    let mut args = std::env::args_os().skip(1);
    let task = args.next();
    let done = match task.as_ref().and_then(|t| t.to_str()) {
        Some("compare") => compare(args.collect()),
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

/// The linters the Rust gate runs; gate.py's findings of any other are not compared.
const PORTED_LINTERS: [&str; 3] = ["pymarkdown", "shellcheck", "actionlint"];

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
        "suppressions": kept("suppressions", ported_suppression),
        "build_config": report["build_config"],
    })
}

/// Whether an entry of one of quality.json's lists is compared, given whether it is gate.py's.
type Keep = fn(&Value, bool) -> bool;

/// The lines of `reports`' quality file that come of the parts the Rust gate has ported, gate.py's when
/// `from_python`. The verdict's line is left out. Of the sections that list the failed conditions, the findings and
/// the suppressions, only the items are kept, without their numbers, and only those whose entry in quality.json
/// [`ported`] keeps; a section none of whose items are kept is left out whole. Their other lines are left out, for
/// they count what was not ported too; whether a list is empty, and how much of it is not listed, quality.json's
/// comparison already holds. The two files agree only while gate.py's quality file lists every finding it has, fewer
/// than `QUALITY_ISSUE_LIMIT`: beyond, the findings it leaves unlisted can differ from the Rust gate's.
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
            "Suppressions added" => (entries("suppressions"), ported_suppression),
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
/// linters' findings on added lines, which counts what was not ported too, and of gate.py's, only those that
/// [`ported_condition_of_python`] keeps.
fn ported_condition(c: &Value, from_python: bool) -> bool {
    let c = c.as_str().unwrap_or_default();
    !c.ends_with(" findings on lines the change added; each is listed below.")
        && (!from_python || ported_condition_of_python(c))
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

/// Whether quality.json's finding `f` is compared, given whether it is gate.py's: of gate.py's, only a ported
/// linter's.
fn ported_finding(f: &Value, from_python: bool) -> bool {
    !from_python || f["check"].as_str().is_some_and(|c| PORTED_LINTERS.contains(&c))
}

/// Whether quality.json's suppression `s` is compared, given whether it is gate.py's: of gate.py's, only one in a
/// file the Rust gate reads the suppressions of, a shell script, a workflow or Markdown. A shell script that is
/// Python by its `#!` is gate.py's alone until Python is ported (gate-standalone/15).
fn ported_suppression(s: &Value, from_python: bool) -> bool {
    let path = s["path"].as_str().unwrap_or_default();
    !from_python
        || path.ends_with(".sh")
        || path.ends_with(".md")
        || (path.starts_with(".github/workflows/") && (path.ends_with(".yml") || path.ends_with(".yaml")))
}
