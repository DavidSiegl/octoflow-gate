//! Each golden report, `tests/golden/*.json`, which crates/gate's tests hold the gate's quality.json to, follows
//! `schema/quality-v1.json`, the contract a project reads quality.json by (docs/quality-gate.md, "The report as a
//! contract"): a field it requires that a report's shape leaves out, or one of a type or value it does not allow,
//! fails here rather than only when smoke-release's one fixture report happens to have that shape.

use std::path::{Path, PathBuf};

use serde_json::Value;

/// The workspace's root, where `schema/` and `tests/golden/` are.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// The JSON in the file at `path`; panics, naming it, when it cannot be read or is not JSON.
fn json(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("{} is not JSON: {e}", path.display()))
}

#[test]
fn every_golden_report_follows_the_schema() {
    let schema = json(&root().join("schema/quality-v1.json"));
    let validator = jsonschema::validator_for(&schema).expect("schema/quality-v1.json is a JSON Schema");
    let golden = root().join("tests/golden");
    let entries = std::fs::read_dir(&golden).unwrap_or_else(|e| panic!("cannot list {}: {e}", golden.display()));
    let mut reports: Vec<PathBuf> = entries
        .map(|entry| entry.unwrap_or_else(|e| panic!("cannot list {}: {e}", golden.display())).path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    reports.sort();
    assert!(!reports.is_empty(), "no golden report in {}", golden.display());
    for report in reports {
        let errors: Vec<String> = validator.iter_errors(&json(&report)).map(|e| e.to_string()).collect();
        assert!(errors.is_empty(), "{} does not follow the schema: {errors:?}", report.display());
    }
}
