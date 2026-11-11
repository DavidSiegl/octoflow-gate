//! The report's two renderings held to the golden files under tests/golden/, byte for byte: each report here is
//! built to be the one the files of the same name show.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use gate::config_edits::{tool_config, touched_paths, ToolConfig};
use gate::coverage::NewCoverage;
use gate::report::{Finding, QualityReport, GATE_VERSION, QUALITY_ISSUE_LIMIT};

/// Passes when `report`'s quality file and quality.json are tests/golden/<name>.txt and .json, the JSON's
/// gate_version written `<version>` there.
fn assert_golden(name: &str, report: &QualityReport) {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/golden");
    let read = |ext: &str| std::fs::read_to_string(dir.join(format!("{name}.{ext}"))).unwrap();
    assert_eq!(report.markdown(), read("txt"), "{name}.txt");
    let version = format!("\"gate_version\": \"{GATE_VERSION}\"");
    let json = report.json().replace(&version, "\"gate_version\": \"<version>\"");
    assert_eq!(json, read("json"), "{name}.json");
}

fn report() -> QualityReport {
    QualityReport { head: "<head>".into(), base: "<base>".into(), ..Default::default() }
}

fn issues(n: u32) -> Vec<Finding> {
    (1..=n)
        .map(|i| Finding {
            path: Some("f.py".into()),
            line: Some(i),
            severity: "HIGH".into(),
            rule: "r".into(),
            message: format!("m{i}"),
        })
        .collect()
}

/// The configuration of clippy's that a change editing `files`, each a file clippy reads, edits, as
/// config_edits::tool_config gives it.
fn clippy(files: Vec<String>) -> ToolConfig {
    let diff: String = files.iter().map(|f| format!("--- a/{f}\n")).collect();
    let mut found = tool_config(&touched_paths(diff.as_bytes(), &Default::default()), None);
    assert_eq!(found.len(), 1, "{files:?} are clippy's alone");
    found.remove(0)
}

fn finding(path: Option<&str>, line: Option<u32>, rule: &str) -> Finding {
    Finding { path: path.map(OsString::from), line, severity: "warning".into(), rule: rule.into(), message: "m".into() }
}

fn weigh(path: &str, line: u32) -> Finding {
    finding(Some(path), Some(line), "flaky test_x")
}

#[test]
fn an_ok_report_with_nothing_to_answer() {
    assert_golden("quality_report_ok", &report());
}

#[test]
fn an_ok_report_with_findings_to_weigh() {
    let r = QualityReport {
        failing_tests: Some(vec![]),
        coverage: Some(NewCoverage { lines: 20, covered_lines: 17, ..Default::default() }),
        to_weigh: vec![
            weigh("t.py", 3),
            finding(Some("t.py"), None, "flaky test_x"),
            finding(None, None, "mutants budget"),
        ],
        edited_config: vec!["pyproject.toml".into()],
        rules_from: Some("`main`'s `pyproject.toml`".into()),
        ..report()
    };
    assert_golden("quality_report_ok_to_weigh", &r);
}

#[test]
fn an_error_report_with_every_section() {
    let r = QualityReport {
        conditions: vec![
            "2 findings on lines the change added; each is listed below.".into(),
            "`ruff` is not installed.".into(),
        ],
        issues: issues(2),
        to_weigh: vec![weigh("t.py", 9)],
        elsewhere: 3,
        failing_tests: Some(vec![
            ("t.T.test_x".into(), "AssertionError: 3 != 2\ntraceback".into()),
            ("t.T.test_y".into(), String::new()),
        ]),
        coverage: Some(NewCoverage {
            lines: 20,
            covered_lines: 10,
            conditions: 4,
            covered_conditions: 1,
            unmeasured: vec![OsString::from_vec(b"m\xff.py".to_vec())],
        }),
        suppressions: vec![("f.py".into(), 3, "x = 1  # noqa: F401".into()), ("g\rh.py".into(), 1, "# noqa".into())],
        edited_config: ["pyproject.toml", "uv.lock", "gate.toml", "Jenkinsfile"].map(OsString::from).into(),
        settings_from: Some("`main.toml`".into()),
        config_notes: vec![("Jenkinsfile".into(), "The quality job reads it from `main`.".into())],
        tool_config: vec![clippy((0..7).map(|k| format!("c{k}/Cargo.toml")).collect())],
        ..report()
    };
    assert_golden("quality_report_error", &r);
}

#[test]
fn an_error_report_whose_findings_to_weigh_are_cut() {
    let n = QUALITY_ISSUE_LIMIT as u32 - 1;
    let r = QualityReport {
        conditions: vec![format!("{n} findings on lines the change added; each is listed below.")],
        issues: issues(n),
        to_weigh: (1..4).map(|line| weigh("t.py", line)).collect(),
        tool_config: vec![clippy(vec!["clippy.toml".into(), "ui/Cargo.toml".into()])],
        ..report()
    };
    assert_golden("quality_report_error_to_weigh_cut", &r);
}

#[test]
fn an_error_report_over_the_limit() {
    let limit = QUALITY_ISSUE_LIMIT;
    let r = QualityReport {
        conditions: vec![
            format!("{} findings on lines the change added; each is listed below.", limit + 2),
            format!("{} tests fail; each is listed below.", limit + 1),
        ],
        issues: issues(limit as u32 + 2),
        to_weigh: vec![weigh("t.py", 1), weigh("t.py", 2)],
        failing_tests: Some((0..=limit).map(|k| (format!("t.T.test_{k}"), String::new())).collect()),
        ..report()
    };
    assert_golden("quality_report_error_over_limit", &r);
}

/// gate_error's report, built here and so held to the file whole: gate_cli.rs gets it from a failing change end to
/// end, with one finding more, which it takes out.
#[test]
fn the_report_of_a_failing_change_is_gate_error_s() {
    let error = |path: &str, line: u32, rule: &str, message: &str| Finding {
        path: Some(path.into()),
        line: Some(line),
        severity: "error".into(),
        rule: rule.into(),
        message: message.into(),
    };
    let r = QualityReport {
        conditions: [
            "1 findings on lines the change added; each is listed below.",
            "1 findings of the `test-integrity` check; each is listed below.",
            "1 tests fail; each is listed below.",
            "Coverage of new code is 50.0%; the gate wants at least 80% once a change has 20 or more new lines to \
             cover.",
        ]
        .map(String::from)
        .into(),
        issues: vec![
            error("f.py", 4, "ruff B023", "Function definition does not bind loop variable `x`"),
            error("tests/test_f.py", 4, "test-integrity skip", "`pytest.mark.skip` added: it skips a test or expects \
                   it to fail."),
        ],
        to_weigh: vec![Finding {
            severity: "warning".into(),
            ..error("tests/test_f.py", 5, "test-integrity removed-test", "`test_two` is gone; this was line 5 at the \
                     base.")
        }],
        elsewhere: 1,
        failing_tests: Some(vec![("tests.T.test_limit".into(), "AssertionError: 3 != 2".into())]),
        coverage: Some(NewCoverage { lines: 20, covered_lines: 10, ..Default::default() }),
        suppressions: vec![("f.py".into(), 23, "import os  # noqa: F401".into())],
        edited_config: vec!["uv.lock".into()],
        ..report()
    };
    assert_golden("gate_error", &r);
}
