//! Coverage of the lines a change added, from the coverage reports of the tree's tests: Cobertura, such as
//! pytest-cov's coverage.xml and cargo-llvm-cov's `--cobertura`, and LCOV, such as cargo-llvm-cov's `--lcov`.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};

use crate::paths::{real_path, rel_path};
use crate::text::{self, floor_tenth, series};
use crate::xml;
use crate::Unjudged;

/// Coverage below this percent fails the gate once a change has [`MIN_NEW_LINES`] or more new lines to cover; a
/// smaller change is not judged on coverage. gate.py's GATE_MIN_COVERAGE and GATE_MIN_NEW_LINES.
pub const MIN_COVERAGE: f64 = 80.0;
pub const MIN_NEW_LINES: usize = 20;

/// Coverage of the lines a change added: of those lines, how many are coverable and covered, and of the branch
/// outcomes on them, how many are taken.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NewCoverage {
    /// Added lines any of the reports can cover.
    pub lines: usize,
    pub covered_lines: usize,
    /// Branch outcomes on those lines, and how many of them were taken.
    pub conditions: usize,
    pub covered_conditions: usize,
    /// The changed Python files coverage.py cannot record, whose lines both counts leave out (unmeasured_files).
    pub unmeasured: Vec<OsString>,
}

impl NewCoverage {
    /// (covered conditions + covered lines) / (conditions + lines), in percent; 100 with nothing to cover.
    pub fn percent(&self) -> f64 {
        let total = self.lines + self.conditions;
        if total == 0 {
            return 100.0;
        }
        100.0 * (self.covered_lines + self.covered_conditions) as f64 / total as f64
    }

    /// The quality file's coverage section: how much of the new code is covered, then, by name, the files it could
    /// not measure.
    pub fn sentence(&self) -> String {
        let mut out = if self.lines == 0 {
            "No new lines to cover.".to_string()
        } else {
            let branches = match self.conditions {
                0 => String::new(),
                n => format!(", counting their {n} branch outcomes"),
            };
            format!(
                "{}% of {} new lines to cover{branches}; {} lines are not covered.",
                floor_tenth(self.percent()),
                self.lines,
                self.lines - self.covered_lines
            )
        };
        if !self.unmeasured.is_empty() {
            let named: Vec<String> =
                self.unmeasured.iter().map(|p| format!("`{}`", text::one_line(&text::path(p)))).collect();
            out += &format!(
                "\n\nNot measured: {}; coverage.py records no data for a file whose name is not UTF-8.",
                series(&named, Some(5))
            );
        }
        out
    }

    /// The failed condition this coverage is, None when it does not fail the gate: below [`MIN_COVERAGE`] percent
    /// on [`MIN_NEW_LINES`] or more new lines to cover.
    pub fn condition(&self) -> Option<String> {
        (self.lines >= MIN_NEW_LINES && self.percent() < MIN_COVERAGE).then(|| {
            format!(
                "Coverage of new code is {}%; the gate wants at least {MIN_COVERAGE}% once a change has \
                 {MIN_NEW_LINES} or more new lines to cover.",
                floor_tenth(self.percent()),
            )
        })
    }
}

/// Coverage of the changed lines, `changed` the lines the change added by file as diff::changed_lines gives them,
/// from the coverage `reports`, merged by path and line: a line is coverable when any report lists it, and covered
/// when any report covers it; its branch outcomes are the most any report counts, and the most any report covers.
///
/// A report is LCOV unless its first character past a byte-order mark and any whitespace is `<`, Cobertura then. A
/// Cobertura report's paths are relative to its `<source>` directories, or to `root`, the tree's top, when it lists
/// none; an LCOV report's are as its `SF:` lines give them. A relative path, or source, is taken from the current
/// directory; each is matched to the tree's paths through its real path, so cargo-llvm-cov's report, whose source is
/// its workspace's directory, maps to the tree's paths as long as the tree is where the report was written. A changed
/// line no report lists is not coverable, like a comment. Only the entries of the files the change adds lines to are
/// read.
///
/// Errs when a report cannot be read or parsed: when it is not XML or LCOV, as an empty or truncated one is not; when
/// a Cobertura line of a file the change adds lines to has a number, or on a changed line hits, that is not a whole
/// number; when a DA or BRDA entry of such a file in an LCOV report has a line number that is not a whole number, or
/// on a changed line other fields that cannot be read.
pub fn new_coverage(
    reports: &[PathBuf],
    root: &Path,
    changed: &BTreeMap<OsString, BTreeSet<u32>>,
) -> Result<NewCoverage, Unjudged> {
    // (path, line): (covered, covered outcomes, outcomes)
    let mut merged: HashMap<(OsString, u32), (bool, usize, usize)> = HashMap::new();
    let real_root = real_path(root);
    for report in reports {
        for entry in changed_line_coverage(report, root, &real_root, changed)? {
            let was = merged.entry(entry.at).or_default();
            *was = (was.0 || entry.hit, was.1.max(entry.taken), was.2.max(entry.outcomes));
        }
    }
    Ok(NewCoverage {
        lines: merged.len(),
        covered_lines: merged.values().filter(|(hit, _, _)| *hit).count(),
        conditions: merged.values().map(|(_, _, outcomes)| outcomes).sum(),
        covered_conditions: merged.values().map(|(_, taken, _)| taken).sum(),
        unmeasured: unmeasured_files(changed),
    })
}

/// A changed line as one report covers it or not.
struct LineCoverage {
    /// The path, a key of the change's added lines, and the line.
    at: (OsString, u32),
    hit: bool,
    /// Branch outcomes on the line that were taken, of those there are; 0 of 0 for a line with no branch data.
    taken: usize,
    outcomes: usize,
}

/// Each changed line the report at `report` lists, as [`new_coverage`] reads it and errs.
fn changed_line_coverage(
    report: &Path,
    root: &Path,
    real_root: &Path,
    changed: &BTreeMap<OsString, BTreeSet<u32>>,
) -> Result<Vec<LineCoverage>, Unjudged> {
    let cannot = |why: String| Unjudged(format!("cannot read the coverage report {}: {why}", report.display()));
    let bytes = std::fs::read(report).map_err(|e| cannot(e.to_string()))?;
    // A byte-order mark may open an XML document (https://www.w3.org/TR/xml/#sec-guessing).
    let start = bytes.strip_prefix("\u{feff}".as_bytes()).unwrap_or(&bytes);
    if start.trim_ascii_start().starts_with(b"<") {
        let doc = xml::parse(&String::from_utf8(bytes).map_err(|e| cannot(e.to_string()))?).map_err(cannot)?;
        cobertura_lines(&doc, root, real_root, changed)
            .map_err(|e| Unjudged(format!("the coverage report {} has a line it cannot read: {e}", report.display())))
    } else {
        lcov_lines(&bytes, real_root, changed).map_err(cannot)
    }
}

/// The changed lines a Cobertura document lists: each `<line>` of the `<lines>` of each `<class>` whose file, taken
/// from each of the document's sources in turn, or `root` when it lists none, the change adds lines to. Errs, with
/// why, on such a line whose number, or on a changed line hits, is not a whole number.
fn cobertura_lines(
    doc: &xml::Element,
    root: &Path,
    real_root: &Path,
    changed: &BTreeMap<OsString, BTreeSet<u32>>,
) -> Result<Vec<LineCoverage>, String> {
    let mut sources: Vec<PathBuf> = (doc.children_named("sources"))
        .flat_map(|s| s.children_named("source"))
        .map(|s| s.text.trim())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .collect();
    if sources.is_empty() {
        sources.push(root.to_path_buf());
    }
    let mut out = Vec::new();
    for class in doc.descendants("class") {
        let file = OsStr::new(class.get("filename").unwrap_or(""));
        let Some((path, added)) = changed_file(file, &sources, real_root, changed) else { continue };
        for line in class.children_named("lines").flat_map(|l| l.children_named("line")) {
            let Some(number) = changed_line(line.get("number").unwrap_or(""), added)? else { continue };
            let (taken, outcomes) = match line.get("branch") {
                Some("true") => branch_counts(line.get("condition-coverage").unwrap_or("")),
                _ => (0, 0),
            };
            let hit = py_int(line.get("hits").unwrap_or("0"))? > 0;
            out.push(LineCoverage { at: (path.clone(), number), hit, taken, outcomes });
        }
    }
    Ok(out)
}

/// The tree's path, a key of `changed`, that `file` names from the first of `sources` that names one, and the lines
/// the change adds to it; None when it names none.
fn changed_file<'a>(
    file: &OsStr,
    sources: &[PathBuf],
    real_root: &Path,
    changed: &'a BTreeMap<OsString, BTreeSet<u32>>,
) -> Option<(OsString, &'a BTreeSet<u32>)> {
    sources.iter().find_map(|source| {
        let candidate = rel_path(&real_path(&source.join(file)), real_root).into_os_string();
        changed.get_key_value(&candidate).map(|(path, added)| (path.clone(), added))
    })
}

/// The line numbered `number` when it is among `added`, None when it is not; errs when `number` is not a whole number.
fn changed_line(number: &str, added: &BTreeSet<u32>) -> Result<Option<u32>, String> {
    Ok(u32::try_from(py_int(number)?).ok().filter(|n| added.contains(n)))
}

/// The (taken, outcomes) of a Cobertura `condition-coverage` such as `50% (1/2)`; 0 of 0 when it has none.
fn branch_counts(text: &str) -> (usize, usize) {
    let counts = text.split_once('(').and_then(|(_, rest)| rest.split_once(')')).and_then(|(inside, _)| {
        let (taken, outcomes) = inside.split_once('/')?;
        Some((taken.parse().ok()?, outcomes.parse().ok()?))
    });
    counts.unwrap_or((0, 0))
}

/// `text` as Python's `int()` reads a whole number, as gate.py reads a report's: surrounding whitespace, a sign, and
/// single underscores between digits allowed. Errs, with why, on anything else, or on a number beyond an i128, which
/// no coverage tool counts to.
fn py_int(text: &str) -> Result<i128, String> {
    let trimmed = text.trim();
    let digits = trimmed.strip_prefix(['+', '-']).unwrap_or(trimmed);
    let groups_ok = digits.split('_').all(|g| !g.is_empty() && g.bytes().all(|b| b.is_ascii_digit()));
    let n = if groups_ok { trimmed.replace('_', "").parse().ok() } else { None };
    n.ok_or_else(|| format!("{text:?} is not a whole number"))
}

/// The changed lines an LCOV report (https://manpages.debian.org/lcov/geninfo.1.en.html, "TRACEFILE FORMAT") lists:
/// each `DA:<line>,<hits>` of each `SF:` record whose file, taken from the current directory, the change adds lines
/// to, with the `BRDA:<line>,<block>,<branch>,<taken>` entries of its line as its branch outcomes, a taken of `-` or
/// 0 one not taken. Errs, with why, when the report holds no record, when an entry is outside a record or a record
/// has no `end_of_record`, as in a truncated report, or on a DA or BRDA entry whose line number it cannot read, or,
/// on a changed line, the rest of whose fields it cannot read.
fn lcov_lines(
    report: &[u8],
    real_root: &Path,
    changed: &BTreeMap<OsString, BTreeSet<u32>>,
) -> Result<Vec<LineCoverage>, String> {
    let here = [PathBuf::from(".")];
    let mut out = Vec::new();
    let mut records = 0;
    // The record being read, from its SF: entry to its end_of_record; None between records.
    let mut record: Option<LcovRecord> = None;
    for raw in report.split(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        let entry = || String::from_utf8_lossy(line).into_owned();
        if let Some(path) = line.strip_prefix(b"SF:") {
            if record.is_some() {
                return Err(format!("a record has no end_of_record before `{}`", entry()));
            }
            records += 1;
            let file = changed_file(OsStr::from_bytes(path), &here, real_root, changed);
            record = Some(LcovRecord { file, ..Default::default() });
        } else if line == b"end_of_record" {
            out.extend(record.take().ok_or("an end_of_record ends no record")?.lines());
        } else if let Some(record) = &mut record {
            record.read(line).map_err(|why| format!("`{}`: {why}", entry()))?;
        } else if !line.trim_ascii().is_empty() && !line.starts_with(b"TN:") {
            return Err(format!("`{}` is outside a record, so this is not LCOV", entry()));
        }
    }
    if record.is_some() {
        return Err("its last record has no end_of_record".into());
    }
    if records == 0 {
        return Err("it holds no LCOV record".into());
    }
    Ok(out)
}

/// What [`lcov_lines`] has read of one LCOV record so far.
#[derive(Default)]
struct LcovRecord<'a> {
    /// The record's file and the lines the change adds to it; None when the change adds no lines to it, so that its
    /// entries are not read.
    file: Option<(OsString, &'a BTreeSet<u32>)>,
    /// Of each added line the record has a DA entry for, whether any of them counts a hit.
    hits: BTreeMap<u32, bool>,
    /// Of each added line the record has BRDA entries for, how many of its branch outcomes were taken, and how many
    /// there are.
    branches: HashMap<u32, (usize, usize)>,
}

impl LcovRecord<'_> {
    /// Adds `line`, an entry of the record, when it is a DA or BRDA entry of an added line; errs, with why, on a DA
    /// or BRDA entry whose line number cannot be read, or, on an added line, the rest of whose fields cannot be.
    fn read(&mut self, line: &[u8]) -> Result<(), String> {
        let Some((_, added)) = self.file else { return Ok(()) };
        let (is_da, fields) = match (line.strip_prefix(b"DA:"), line.strip_prefix(b"BRDA:")) {
            (Some(fields), _) => (true, fields),
            (_, Some(fields)) => (false, fields),
            _ => return Ok(()),
        };
        let fields = std::str::from_utf8(fields).map_err(|e| e.to_string())?;
        // The line's number comes first; the other fields are read only on a changed line.
        let mut fields = fields.split(',');
        let Some(number) = changed_line(fields.next().unwrap_or_default(), added)? else { return Ok(()) };
        if is_da {
            let count = fields.next().ok_or("no hit count")?;
            *self.hits.entry(number).or_default() |= py_int(count)? > 0;
        } else {
            let [_, _, taken] = fields.collect::<Vec<_>>()[..] else { return Err("not 4 fields".into()) };
            let was = self.branches.entry(number).or_default();
            was.1 += 1;
            was.0 += usize::from(!matches!(taken.trim(), "-" | "0"));
        }
        Ok(())
    }

    /// The coverage of the added lines the record has DA entries for, in line order; none when the change adds no
    /// lines to its file.
    fn lines(self) -> Vec<LineCoverage> {
        let Some((path, _)) = self.file else { return vec![] };
        (self.hits.iter())
            .map(|(&number, &hit)| {
                let (taken, outcomes) = self.branches.get(&number).copied().unwrap_or_default();
                LineCoverage { at: (path.clone(), number), hit, taken, outcomes }
            })
            .collect()
    }
}

/// The Python files `changed` adds lines to whose names are not UTF-8, in order: coverage.py collects no data for
/// such a file, so the coverage section names them.
fn unmeasured_files(changed: &BTreeMap<OsString, BTreeSet<u32>>) -> Vec<OsString> {
    (changed.iter())
        .filter(|(path, added)| {
            !added.is_empty() && path.as_bytes().ends_with(b".py") && std::str::from_utf8(path.as_bytes()).is_err()
        })
        .map(|(path, _)| path.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn changed(entries: &[(&str, &[u32])]) -> BTreeMap<OsString, BTreeSet<u32>> {
        entries.iter().map(|(p, lines)| (OsString::from(p), lines.iter().copied().collect())).collect()
    }

    fn scratch(name: &str, text: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gate-coverage-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, text).unwrap();
        path
    }

    #[test]
    fn merges_cobertura_and_lcov_by_path_and_line() {
        let root = std::env::temp_dir().join(format!("gate-coverage-root-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let cobertura = scratch(
            "c.xml",
            &format!(
                "<coverage><sources><source>{}</source></sources><packages><package><classes>\
                 <class filename='a.py'><lines><line number='1' hits='0'/>\
                 <line number='2' hits='1' branch='true' condition-coverage='50% (1/2)'/>\
                 <line number='9' hits='1'/></lines></class></classes></package></packages></coverage>",
                root.display()
            ),
        );
        let lcov = scratch(
            "l.info",
            &format!(
                "SF:{}\nDA:1,3\nDA:2,1\nBRDA:2,0,0,1\nBRDA:2,0,1,1\nBRDA:2,0,2,-\nend_of_record\n",
                root.join("a.py").display()
            ),
        );
        let got = new_coverage(&[cobertura, lcov], &root, &changed(&[("a.py", &[1, 2, 3])])).unwrap();
        assert_eq!(
            got,
            NewCoverage { lines: 2, covered_lines: 2, conditions: 3, covered_conditions: 2, unmeasured: vec![] }
        );
    }

    #[test]
    fn cobertura_counts_hits_and_the_branches_of_lines_it_marks_as_branches() {
        let root = std::env::temp_dir().join(format!("gate-coverage-cobertura-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let report = scratch(
            "cobertura.xml",
            &format!(
                "<coverage><sources><source> </source><source>{}</source></sources><packages><package><classes>\
                 <class filename='a.py'><lines><line number='1' hits='0'/>\
                 <line number='2' hits='1' branch='true' condition-coverage='50% (1/2)'/>\
                 <line number='3' hits='1' branch='false' condition-coverage='0% (0/2)'/>\
                 </lines></class></classes></package></packages></coverage>",
                root.display()
            ),
        );
        let got = new_coverage(&[report], &root, &changed(&[("a.py", &[1, 2, 3])])).unwrap();
        assert_eq!(
            got,
            NewCoverage { lines: 3, covered_lines: 2, conditions: 2, covered_conditions: 1, unmeasured: vec![] }
        );
    }

    #[test]
    fn a_cobertura_report_may_start_with_a_byte_order_mark() {
        let root = std::env::temp_dir().join(format!("gate-coverage-bom-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let report = scratch(
            "bom.xml",
            &format!(
                "\u{feff}<?xml version='1.0'?><coverage><sources><source>{}</source></sources><packages><package>\
                 <classes><class filename='a.py'><lines><line number='1' hits='1'/></lines></class></classes>\
                 </package></packages></coverage>",
                root.display()
            ),
        );
        let got = new_coverage(&[report], &root, &changed(&[("a.py", &[1])])).unwrap();
        assert_eq!((got.lines, got.covered_lines), (1, 1));
    }

    #[test]
    fn a_report_that_is_neither_cobertura_nor_whole_lcov_is_an_error() {
        let root = std::env::temp_dir();
        let a = changed(&[("a.py", &[1])]);
        for (name, text) in [
            ("empty", ""),
            ("blank", "\n\n"),
            ("json", "{\"files\": {}}"),
            ("truncated", "SF:a.py\nDA:1,1\n"),
            ("unended", "SF:a.py\nDA:1,1\nSF:b.py\nend_of_record\n"),
            ("stray-end", "end_of_record\n"),
        ] {
            let report = scratch(name, text);
            assert!(new_coverage(&[report], &root, &a).is_err(), "{name}");
        }
        let report = scratch("tn-only-record", "TN:\nSF:x.py\nDA:1,1\nend_of_record\n");
        assert_eq!(new_coverage(&[report], &root, &a).unwrap().lines, 0);
    }

    #[test]
    fn only_the_changed_lines_of_changed_files_are_read() {
        let root = std::env::temp_dir().join(format!("gate-coverage-only-changed-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let cobertura = scratch(
            "only-changed.xml",
            "<coverage><class filename='other.py'><lines><line number='x' hits='y'/></lines></class>\
             <class filename='a.py'><lines><line number='1' hits='5000000000'/><line number='2' hits='y'/>\
             </lines></class></coverage>",
        );
        let lcov = scratch(
            "only-changed.info",
            &format!(
                "SF:{}\nDA:x,y\nBRDA:x\nend_of_record\nSF:{}\nDA:2,y\nDA:3,9000000000\nend_of_record\n",
                root.join("other.py").display(),
                root.join("a.py").display()
            ),
        );
        let a = changed(&[("a.py", &[1, 3])]);
        let got = new_coverage(&[cobertura, lcov], &root, &a).unwrap();
        assert_eq!((got.lines, got.covered_lines), (2, 2));
    }

    #[test]
    fn whole_numbers_are_python_s() {
        assert_eq!(py_int(" +1_000 "), Ok(1000));
        assert_eq!(py_int("-3"), Ok(-3));
        assert_eq!(py_int("18446744073709551616"), Ok(1 << 64));
        for bad in ["", "_1", "1_", "1__0", "1.0", "x", "+"] {
            assert!(py_int(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn lcov_entries_of_unchanged_lines_are_not_read_past_their_number() {
        let root = std::env::temp_dir().join(format!("gate-coverage-unchanged-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let file = root.join("a.py");
        let a = changed(&[("a.py", &[1])]);
        let text = format!("SF:{}\nDA:7\nBRDA:7,0,a,b,1\nDA:1,1\nend_of_record\n", file.display());
        let got = new_coverage(&[scratch("unchanged.info", &text)], &root, &a).unwrap();
        assert_eq!((got.lines, got.covered_lines), (1, 1));
        let text = format!("SF:{}\nDA:1\nend_of_record\n", file.display());
        assert!(new_coverage(&[scratch("changed.info", &text)], &root, &a).is_err());
    }

    #[test]
    fn an_unreadable_line_is_an_error() {
        let report =
            scratch("bad.xml", "<coverage><class filename='a.py'><lines><line number='x'/></lines></class></coverage>");
        let root = std::env::temp_dir();
        assert!(new_coverage(&[report], &root, &changed(&[("a.py", &[1])])).is_err());
    }

    #[test]
    fn the_sentence_names_what_it_cannot_measure() {
        let c = NewCoverage {
            lines: 20,
            covered_lines: 10,
            conditions: 4,
            covered_conditions: 1,
            unmeasured: vec![OsString::from_vec(b"m\xff.py".to_vec())],
        };
        assert_eq!(
            c.sentence(),
            "45.8% of 20 new lines to cover, counting their 4 branch outcomes; 10 lines are not covered.\n\n\
             Not measured: `m\\xff.py`; coverage.py records no data for a file whose name is not UTF-8."
        );
        assert_eq!(NewCoverage::default().sentence(), "No new lines to cover.");
        assert_eq!(
            c.condition().unwrap(),
            "Coverage of new code is 45.8%; the gate wants at least 80% once a change has 20 or more new lines to \
             cover."
        );
        assert_eq!(NewCoverage { lines: 19, ..c.clone() }.condition(), None);
        let at_the_limit = NewCoverage { lines: 20, covered_lines: 16, conditions: 0, covered_conditions: 0, ..c };
        assert_eq!(at_the_limit.condition(), None);
    }

    use std::os::unix::ffi::OsStringExt;
}
