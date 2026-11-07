//! JUnit XML reports of a tree's tests, such as pytest's `--junitxml` and cargo-nextest's.

use std::path::Path;

use crate::xml;
use crate::Unjudged;

/// What became of one test case.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// A failure or an error.
    Failed,
    /// Failed as an xfail mark expected: pytest's `<skipped type="pytest.xfail">`.
    Xfailed,
    /// Any other skip.
    Skipped,
    Passed,
}

/// (test, outcome, what failed) for each test case of the JUnit report at `report`, the test named
/// `classname.name`; what failed is a failure's or an error's message, else its text, and "" for any other outcome.
/// Errs when the report cannot be read or parsed.
pub fn junit_outcomes(report: &Path) -> Result<Vec<(String, Outcome, String)>, Unjudged> {
    let doc = xml::parse_file(report)
        .map_err(|e| Unjudged(format!("cannot read the test report {}: {e}", crate::text::path(report.as_os_str()))))?;
    let mut out = vec![];
    for case in doc.descendants("testcase") {
        let name = format!("{}.{}", case.get("classname").unwrap_or(""), case.get("name").unwrap_or(""));
        let name = name.trim_matches('.').to_string();
        if let Some(failed) = case.child("failure").or_else(|| case.child("error")) {
            let detail = failed.get("message").filter(|m| !m.is_empty()).unwrap_or(&failed.text);
            out.push((name, Outcome::Failed, detail.to_string()));
        } else if let Some(skipped) = case.child("skipped") {
            let xfail = skipped.get("type") == Some("pytest.xfail");
            out.push((name, if xfail { Outcome::Xfailed } else { Outcome::Skipped }, String::new()));
        } else {
            out.push((name, Outcome::Passed, String::new()));
        }
    }
    Ok(out)
}

/// (test, what failed) for each failing or erroring test case of the JUnit report at `report`. Errs as
/// [`junit_outcomes`] does.
pub fn failing_tests(report: &Path) -> Result<Vec<(String, String)>, Unjudged> {
    Ok(junit_outcomes(report)?
        .into_iter()
        .filter(|(_, outcome, _)| *outcome == Outcome::Failed)
        .map(|(name, _, detail)| (name, detail))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_case_s_outcome() {
        let path = std::env::temp_dir().join(format!("gate-junit-{}.xml", std::process::id()));
        std::fs::write(
            &path,
            r#"<testsuites><testsuite>
            <testcase classname="t.T" name="a"><failure message="boom">trace</failure></testcase>
            <testcase classname="t.T" name="b"><error>only text</error></testcase>
            <testcase name="c"><skipped type="pytest.xfail"/></testcase>
            <testcase classname="t" name="d"><skipped/></testcase>
            <testcase classname="t" name="e"/>
            </testsuite></testsuites>"#,
        )
        .unwrap();
        let got = junit_outcomes(&path).unwrap();
        assert_eq!(
            got,
            [
                ("t.T.a".into(), Outcome::Failed, "boom".into()),
                ("t.T.b".into(), Outcome::Failed, "only text".into()),
                ("c".into(), Outcome::Xfailed, "".into()),
                ("t.d".into(), Outcome::Skipped, "".into()),
                ("t.e".into(), Outcome::Passed, "".into()),
            ]
        );
        assert_eq!(failing_tests(&path).unwrap().len(), 2);
        std::fs::write(&path, "<testsuite>").unwrap();
        assert!(failing_tests(&path).unwrap_err().0.starts_with("cannot read the test report "));
        std::fs::remove_file(path).unwrap();
    }
}
