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
/// `classname.name`; "" for an outcome other than Failed.
///
/// What failed is a failure's or an error's message, else its text. A message that says no more than that the test
/// failed, as gotestsum's `Failed`, gives way to a text that is not empty: then what failed is the text without the
/// lines `go test -v` frames a test's output with (`=== RUN`, `--- FAIL:` and the like), so that it opens with the
/// test's own first line, such as a `t.Errorf`'s or, for a package that does not compile, gotestsum's
/// `FAIL <package> [build failed]`. Errs when the report cannot be read or parsed.
pub fn junit_outcomes(report: &Path) -> Result<Vec<(String, Outcome, String)>, Unjudged> {
    let doc = xml::parse_file(report)
        .map_err(|e| Unjudged(format!("cannot read the test report {}: {e}", crate::text::path(report.as_os_str()))))?;
    let mut out = vec![];
    for case in doc.descendants("testcase") {
        let name = format!("{}.{}", case.get("classname").unwrap_or(""), case.get("name").unwrap_or(""));
        let name = name.trim_matches('.').to_string();
        if let Some(failed) = case.child("failure").or_else(|| case.child("error")) {
            let message = failed.get("message").unwrap_or("");
            let detail = if message.is_empty() {
                failed.text.clone()
            } else if message.trim().eq_ignore_ascii_case("failed") && !failed.text.trim().is_empty() {
                without_go_test_framing(&failed.text)
            } else {
                message.to_string()
            };
            out.push((name, Outcome::Failed, detail));
        } else if let Some(skipped) = case.child("skipped") {
            let xfail = skipped.get("type") == Some("pytest.xfail");
            out.push((name, if xfail { Outcome::Xfailed } else { Outcome::Skipped }, String::new()));
        } else {
            out.push((name, Outcome::Passed, String::new()));
        }
    }
    Ok(out)
}

/// `output` without the lines with which `go test -v` announces a test or its result: `=== RUN`, `=== PAUSE`,
/// `=== CONT`, `=== NAME`, and `--- FAIL:`, `--- PASS:`, `--- SKIP:` with any indentation a subtest gives them.
fn without_go_test_framing(output: &str) -> String {
    const FRAMING: [&str; 7] =
        ["=== RUN ", "=== PAUSE ", "=== CONT ", "=== NAME ", "--- FAIL: ", "--- PASS: ", "--- SKIP: "];
    output.split_inclusive('\n').filter(|line| !FRAMING.iter().any(|f| line.trim_start().starts_with(f))).collect()
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

    #[test]
    fn a_message_that_only_says_failed_gives_way_to_the_output_without_go_test_s_framing() {
        let path = std::env::temp_dir().join(format!("gate-junit-go-{}.xml", std::process::id()));
        std::fs::write(
            &path,
            "<testsuites><testsuite>\
             <testcase classname=\"example.com/calc\" name=\"TestAdd\"><failure message=\"Failed\" type=\"\">\
             === RUN   TestAdd&#xA;=== PAUSE TestAdd&#xA;=== CONT  TestAdd&#xA;    calc_test.go:6: got 2, want 3&#xA;\
             --- FAIL: TestAdd (0.00s)&#xA;</failure></testcase>\
             <testcase classname=\"\" name=\"TestMain\"><failure message=\"Failed\" type=\"\">\
             FAIL&#x9;example.com/calc/bad [build failed]&#xA;</failure></testcase>\
             <testcase classname=\"t\" name=\"a\"><failure message=\" failed \"></failure></testcase>\
             </testsuite></testsuites>",
        )
        .unwrap();
        let got = failing_tests(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert_eq!(
            got,
            [
                ("example.com/calc.TestAdd".into(), "    calc_test.go:6: got 2, want 3\n".into()),
                ("TestMain".into(), "FAIL\texample.com/calc/bad [build failed]\n".into()),
                ("t.a".into(), " failed ".into()),
            ]
        );
    }
}
