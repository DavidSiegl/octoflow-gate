//! The report's two renderings held to gate.py's golden files under tests/golden/, byte for byte: each report here
//! is the one gate.py's TestQualityReport builds for the file of the same name (tests/test_gate.py), so the Rust
//! gate writes what gate.py writes.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

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
        clippy_config: (0..7).map(|k| OsString::from(format!("c{k}/Cargo.toml"))).collect(),
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
        clippy_config: vec!["clippy.toml".into(), "ui/Cargo.toml".into()],
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
