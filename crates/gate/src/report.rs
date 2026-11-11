//! The gate's report on one change and its two renderings: the quality file, Markdown for the people and agents who
//! answer it, and quality.json, the same report as data for a program, as schema/quality-v1.json describes it
//! (docs/quality-gate.md, "The report as a contract").

use std::ffi::OsString;

pub use checks::Finding;
use serde::Serialize;

use crate::coverage::NewCoverage;
use crate::text::{self, fingerprint, one_line, series};

/// The version this binary carries into quality.json: its crate's.
pub const GATE_VERSION: &str = env!("CARGO_PKG_VERSION");
/// quality.json's `schema_version`, the version of schema/quality-v1.json it follows: raised on any change to the
/// JSON a reader could notice, but not for a new optional field.
pub const QUALITY_SCHEMA_VERSION: u32 = 1;
/// The most findings the quality file lists, issues and findings to weigh numbered as one list, and the most failing
/// tests; the rest are counted.
pub const QUALITY_ISSUE_LIMIT: usize = 50;
/// The file at the top level of the judged tree that holds the gate's settings (settings.rs), unless `--config`
/// names another.
pub const SETTINGS_FILE: &str = "gate.toml";
/// The file beside the settings' that holds the rules of the tools that take theirs from one
/// (settings::JudgedBy::rules).
pub const RULES_FILE: &str = "pyproject.toml";
/// Files a change may edit that alter how the gate judges it: its rules and the versions of the linters it runs.
pub const BUILD_CONFIG_FILES: [&str; 2] = [RULES_FILE, "uv.lock"];

/// The headings of the sections a reader of the quality file finds the failed conditions and failing tests under.
const FAILED_CONDITIONS: &str = "## Failed conditions";
const FAILING_TESTS: &str = "## Failing tests";
/// Leads the findings to weigh; a reader takes the numbered findings after it as ones that do not fail the gate.
const TO_WEIGH_NOTE: &str = "These do not fail the gate. Fix each, or say in your report why it should stand.";

/// The gate's word on a change, the quality file's first line after `GATE: `.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Verdict {
    #[serde(rename = "OK")]
    Ok,
    #[serde(rename = "ERROR")]
    Error,
}

impl Verdict {
    /// The word as the quality file and the gate's standard error write it, the same as the serde names above give
    /// quality.json.
    pub fn word(self) -> &'static str {
        match self {
            Verdict::Ok => "OK",
            Verdict::Error => "ERROR",
        }
    }
}

/// What the gate found about one change, written two ways from this one value by [`QualityReport::markdown`] and
/// [`QualityReport::json`]. The verdict is ERROR when a condition failed, else OK.
#[derive(Clone, Debug, Default)]
pub struct QualityReport {
    /// The commits judged, as full shas: HEAD of the work tree, whose working tree was judged, and the base.
    pub head: String,
    pub base: String,
    /// The failed conditions, each a sentence; none means the gate is OK.
    pub conditions: Vec<String>,
    /// The findings that fail the gate, then the ones to weigh, which do not; both renderings list them in this
    /// order, and the quality file numbers them as one list.
    pub issues: Vec<Finding>,
    pub to_weigh: Vec<Finding>,
    /// How many linter findings were left out for not being on lines the change added.
    pub elsewhere: usize,
    /// The (name, detail) of the failing tests of every test report read; None when none was read.
    pub failing_tests: Option<Vec<(String, String)>>,
    /// Coverage of the new code; None when no coverage report was read.
    pub coverage: Option<NewCoverage>,
    /// The (path, line, text) of each suppression the change added (suppressions.rs).
    pub suppressions: Vec<(OsString, u32, String)>,
    /// The build configuration files the change edits, as config_edits::edited_config gives them.
    pub edited_config: Vec<OsString>,
    /// Where the gate took its rules and its settings from when that was not the tree's own pyproject.toml and
    /// gate.toml, each as settings::JudgedBy gives it.
    pub rules_from: Option<String>,
    pub settings_from: Option<String>,
    /// The (path, note) of each file gate.toml adds to the build configuration.
    pub config_notes: Vec<(String, String)>,
    /// The files the change edits that clippy takes its rules or its toolchain from.
    pub clippy_config: Vec<OsString>,
}

impl QualityReport {
    pub fn verdict(&self) -> Verdict {
        if self.conditions.is_empty() {
            Verdict::Ok
        } else {
            Verdict::Error
        }
    }

    /// The quality file: `GATE: ` and the verdict on its first line, then what whoever answers it has to answer.
    ///
    /// It numbers at most [`QUALITY_ISSUE_LIMIT`] findings and lists as many failing tests, each by the first line of
    /// its detail, and counts the rest. It is prose, whose wording may change in any release; a program reads
    /// [`QualityReport::json`] instead.
    pub fn markdown(&self) -> String {
        let mut out = vec![format!("GATE: {}", self.verdict().word()), String::new(), self.intro()];
        if !self.conditions.is_empty() {
            out.extend([String::new(), FAILED_CONDITIONS.into(), String::new()]);
            out.extend(self.conditions.iter().map(|c| one_line(&format!("- {c}"))));
        }
        out.extend(issues_section(&self.issues, self.elsewhere));
        if !self.to_weigh.is_empty() {
            out.extend(to_weigh_section(&self.to_weigh, self.issues.len().min(QUALITY_ISSUE_LIMIT)));
        }
        if let Some(tests) = &self.failing_tests {
            out.extend(tests_section(tests));
        }
        if let Some(coverage) = &self.coverage {
            out.extend([String::new(), "## Coverage on new code".into(), String::new(), coverage.sentence()]);
        }
        if !self.suppressions.is_empty() {
            out.extend([String::new(), "## Suppressions added".into(), String::new()]);
            out.extend(self.suppressions.iter().map(|(path, line, text)| {
                one_line(&format!("- `{}:{line}` `{text}`", text::path(path)))
            }));
        }
        let config = self.config_items();
        if !config.is_empty() {
            out.extend([String::new(), "## Build configuration".into(), String::new()]);
            out.extend(config.iter().map(|item| one_line(&format!("- {}", item.text))));
        }
        out.join("\n") + "\n"
    }

    /// quality.json: the report as one JSON object, as schema/quality-v1.json describes it, indented by two spaces,
    /// with a line end after it. It lists every finding and every failing test, with the whole of each test's detail;
    /// its strings hold the text as it is, line breaks included, but for a byte that is not UTF-8, written `\xNN`.
    pub fn json(&self) -> String {
        let findings: Vec<JsonFinding> = (self.issues.iter().map(|f| (f, true)))
            .chain(self.to_weigh.iter().map(|f| (f, false)))
            .map(|(f, fails)| JsonFinding::of(f, fails))
            .collect();
        let doc = JsonReport {
            schema_version: QUALITY_SCHEMA_VERSION,
            gate: self.verdict(),
            gate_version: GATE_VERSION,
            base: &self.base,
            head: &self.head,
            failed_conditions: self.conditions.iter().map(|c| one_line(c)).collect(),
            findings_total: findings.len(),
            findings,
            failing_tests: self.failing_tests.as_ref().map(|tests| {
                tests.iter().map(|(name, detail)| JsonTest { name, detail }).collect()
            }),
            coverage: self.coverage.as_ref().map(|c| JsonCoverage {
                lines: c.lines,
                covered_lines: c.covered_lines,
                conditions: c.conditions,
                covered_conditions: c.covered_conditions,
                percent: c.percent(),
                unmeasured: c.unmeasured.iter().map(|p| text::path(p)).collect(),
            }),
            suppressions: (self.suppressions.iter())
                .map(|(path, line, text)| JsonSuppression { path: text::path(path), line: *line, text })
                .collect(),
            build_config: self.config_items(),
        };
        // Serializing structs of strings and numbers cannot fail.
        serde_json::to_string_pretty(&doc).unwrap_or_default() + "\n"
    }

    /// The quality file's opening sentence: which commit's tree was judged against which base, and where the rules
    /// and settings came from when they were not the tree's own.
    fn intro(&self) -> String {
        let mut out = format!("`gate` judged the working tree at `{}` against the base `{}`", self.head, self.base);
        if let Some(rules) = &self.rules_from {
            out += &format!(", by the rules from {rules}");
        }
        if let Some(settings) = &self.settings_from {
            out += &format!(", with the settings from {settings}");
        }
        out + "."
    }

    /// The edits to the build configuration the report names: for each, the files it is about and the sentences
    /// that say what the edit means for how the change was judged.
    fn config_items(&self) -> Vec<ConfigItem> {
        let edited = |name: &str| self.edited_config.iter().any(|p| p == name);
        let mut out = Vec::new();
        if edited(RULES_FILE) {
            out.push(ConfigItem::of(RULES_FILE, match &self.rules_from {
                Some(rules) => format!(
                    "The change edits `{RULES_FILE}`. The gate took its rules from {rules}, so the edit did not \
                     change how the change was judged."
                ),
                None => format!(
                    "The change edits `{RULES_FILE}`, which holds the gate's rules; the change was judged by the \
                     edited rules."
                ),
            }));
        }
        if edited("uv.lock") {
            out.push(ConfigItem::of(
                "uv.lock",
                "The change edits `uv.lock`, which pins the versions of the linters the gate ran.".into(),
            ));
        }
        if edited(SETTINGS_FILE) {
            out.push(ConfigItem::of(SETTINGS_FILE, match &self.settings_from {
                Some(settings) => format!(
                    "The change edits `{SETTINGS_FILE}`. The gate took its settings from {settings}, so the edit did \
                     not change how the change was judged."
                ),
                None => format!(
                    "The change edits `{SETTINGS_FILE}`, which holds the gate's settings; the change was judged by \
                     the edited settings."
                ),
            }));
        }
        out.extend(
            (self.config_notes.iter())
                .filter(|(path, _)| edited(path))
                .map(|(path, note)| ConfigItem::of(path, format!("The change edits `{path}`. {note}"))),
        );
        if !self.clippy_config.is_empty() {
            let files: Vec<String> = self.clippy_config.iter().map(|p| text::path(p)).collect();
            let named = series(&files.iter().map(|f| format!("`{f}`")).collect::<Vec<_>>(), Some(5));
            out.push(ConfigItem {
                text: format!(
                    "The change edits {named}, which clippy takes its rules or its toolchain from; it judged the \
                     change by the edited ones."
                ),
                files,
            });
        }
        out
    }
}

/// One edit to the build configuration, as quality.json's `build_config` lists it.
#[derive(Serialize)]
struct ConfigItem {
    files: Vec<String>,
    text: String,
}

impl ConfigItem {
    fn of(file: &str, text: String) -> Self {
        ConfigItem { files: vec![file.into()], text }
    }
}

#[derive(Serialize)]
struct JsonReport<'a> {
    schema_version: u32,
    gate: Verdict,
    gate_version: &'a str,
    base: &'a str,
    head: &'a str,
    failed_conditions: Vec<String>,
    findings: Vec<JsonFinding>,
    findings_total: usize,
    failing_tests: Option<Vec<JsonTest<'a>>>,
    coverage: Option<JsonCoverage>,
    suppressions: Vec<JsonSuppression<'a>>,
    build_config: Vec<ConfigItem>,
}

#[derive(Serialize)]
struct JsonFinding {
    path: Option<String>,
    line: Option<u32>,
    severity: String,
    rule: String,
    check: String,
    message: String,
    fails: bool,
    fingerprint: String,
}

impl JsonFinding {
    /// `f` as quality.json lists it; `fails` says whether it fails the gate. Its fingerprint is of the text the
    /// quality file writes, where [`one_line`] has escaped each line break, so either rendering reads the same.
    fn of(f: &Finding, fails: bool) -> Self {
        let path = f.path.as_deref().map(text::path);
        JsonFinding {
            fingerprint: fingerprint(
                path.as_deref().map(one_line).as_deref(),
                &one_line(&f.rule),
                &one_line(&f.message),
            ),
            path,
            line: f.line,
            severity: f.severity.clone(),
            rule: f.rule.clone(),
            check: f.check().into(),
            message: f.message.clone(),
            fails,
        }
    }
}

#[derive(Serialize)]
struct JsonTest<'a> {
    name: &'a str,
    detail: &'a str,
}

#[derive(Serialize)]
struct JsonCoverage {
    lines: usize,
    covered_lines: usize,
    conditions: usize,
    covered_conditions: usize,
    percent: f64,
    unmeasured: Vec<String>,
}

#[derive(Serialize)]
struct JsonSuppression<'a> {
    path: String,
    line: u32,
    text: &'a str,
}

// The sections of the quality file after its first, each as lines that start with a blank one.

fn issues_section(issues: &[Finding], elsewhere: usize) -> Vec<String> {
    let mut out = vec![String::new(), "## Issues on changed lines".into(), String::new()];
    if issues.is_empty() {
        out.push("None.".into());
    }
    out.extend(issues.iter().take(QUALITY_ISSUE_LIMIT).enumerate().map(|(k, f)| numbered(k + 1, f)));
    if let Some(more) = issues.len().checked_sub(QUALITY_ISSUE_LIMIT).filter(|&more| more > 0) {
        out.extend([
            String::new(),
            format!("{more} more issues on changed lines are not listed; fix the ones above first."),
        ]);
    }
    if elsewhere > 0 {
        out.extend([
            String::new(),
            format!(
                "{elsewhere} other open issues in the project are not on lines this change added, so they are not \
                 listed."
            ),
        ]);
    }
    out
}

/// The findings to weigh, numbered on from `after`, the last issue's number, within [`QUALITY_ISSUE_LIMIT`].
fn to_weigh_section(to_weigh: &[Finding], after: usize) -> Vec<String> {
    let mut out = vec![String::new(), "## Findings to weigh".into(), String::new()];
    let room = QUALITY_ISSUE_LIMIT - after;
    if room == 0 {
        out.push(format!("{} findings to weigh are not listed; answer the issues above first.", to_weigh.len()));
        return out;
    }
    out.extend([TO_WEIGH_NOTE.into(), String::new()]);
    out.extend(to_weigh.iter().take(room).enumerate().map(|(k, f)| numbered(after + k + 1, f)));
    if to_weigh.len() > room {
        out.extend([String::new(), format!("{} more findings to weigh are not listed.", to_weigh.len() - room)]);
    }
    out
}

fn numbered(k: usize, f: &Finding) -> String {
    let at = match (&f.path, f.line) {
        (None, _) => "The change as a whole:".into(),
        (Some(path), None) => format!("`{} (the whole file)`", text::path(path)),
        (Some(path), Some(line)) => format!("`{}:{line}`", text::path(path)),
    };
    one_line(&format!("{k}. {at} {} {}: {}", f.severity, f.rule, f.message))
}

fn tests_section(tests: &[(String, String)]) -> Vec<String> {
    let mut out = vec![String::new(), FAILING_TESTS.into(), String::new()];
    if tests.is_empty() {
        out.push("None.".into());
    }
    out.extend(tests.iter().take(QUALITY_ISSUE_LIMIT).map(|(name, detail)| {
        one_line(&format!("- `{name}`: {}", first_line(detail).unwrap_or("failed")))
    }));
    if tests.len() > QUALITY_ISSUE_LIMIT {
        out.push(format!("- and {} more.", tests.len() - QUALITY_ISSUE_LIMIT));
    }
    out
}

/// The first line of `detail` stripped of the whitespace around it, as Python's `detail.strip().splitlines()[0]`
/// takes it, by [`text::py_strip`] and [`text::LINE_BREAKS`]; None when `detail` is only whitespace.
fn first_line(detail: &str) -> Option<&str> {
    let stripped = text::py_strip(detail);
    let end = stripped.find(text::LINE_BREAKS).unwrap_or(stripped.len());
    (!stripped.is_empty()).then(|| &stripped[..end])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_verdict_is_error_on_a_condition_alone() {
        let finding =
            Finding { path: None, line: None, severity: "warning".into(), rule: "r".into(), message: "m".into() };
        assert_eq!(QualityReport { to_weigh: vec![finding], ..Default::default() }.verdict(), Verdict::Ok);
        assert_eq!(QualityReport { conditions: vec!["c".into()], ..Default::default() }.verdict(), Verdict::Error);
    }

    #[test]
    fn a_failing_test_s_first_line_is_its_detail_s_after_stripping() {
        assert_eq!(first_line("  \n Error: 1\r\nline 2"), Some("Error: 1"));
        assert_eq!(first_line(" \n\u{1f} "), None);
    }
}
