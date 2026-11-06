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
/// A report is LCOV unless its first character past any whitespace is `<`, Cobertura then. A Cobertura report's
/// paths are relative to its `<source>` directories, or to `root`, the tree's top, when it lists none; an LCOV
/// report's are as its `SF:` lines give them. A relative path, or source, is taken from the current directory; each
/// is matched to the tree's paths through its real path, so cargo-llvm-cov's report, whose source is its workspace's
/// directory, maps to the tree's paths as long as the tree is where the report was written. A changed line no report
/// lists is not coverable, like a comment. Errs when a report cannot be read or parsed.
pub fn new_coverage(
    reports: &[PathBuf],
    root: &Path,
    changed: &BTreeMap<OsString, BTreeSet<u32>>,
) -> Result<NewCoverage, Unjudged> {
    // (path, line): (covered, covered outcomes, outcomes)
    let mut merged: HashMap<(OsString, u32), (bool, usize, usize)> = HashMap::new();
    let real_root = real_path(root);
    for report in reports {
        let (sources, entries) = read_report(report, root)?;
        for entry in entries {
            let Some(path) = tree_path(&entry.file, &sources, &real_root, changed) else { continue };
            if !changed[&path].contains(&entry.line) {
                continue;
            }
            let was = merged.entry((path, entry.line)).or_default();
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

/// One line a report covers or not.
struct LineCoverage {
    /// The file as the report names it, relative to one of the report's sources unless absolute.
    file: OsString,
    line: u32,
    hit: bool,
    /// Branch outcomes on the line that were taken, of those there are; 0 of 0 for a line with no branch data.
    taken: usize,
    outcomes: usize,
}

/// The tree's path, a key of `changed`, that `file` names from the first of `sources` that names one; None when it
/// names none.
fn tree_path(
    file: &OsStr,
    sources: &[PathBuf],
    real_root: &Path,
    changed: &BTreeMap<OsString, BTreeSet<u32>>,
) -> Option<OsString> {
    sources
        .iter()
        .map(|source| rel_path(&real_path(&source.join(file)), real_root).into_os_string())
        .find(|candidate| changed.contains_key(candidate))
}

/// The directories the paths of the report at `report` are relative to, and its lines, as [`new_coverage`] reads
/// them.
fn read_report(report: &Path, root: &Path) -> Result<(Vec<PathBuf>, Vec<LineCoverage>), Unjudged> {
    let cannot = |why: String| Unjudged(format!("cannot read the coverage report {}: {why}", report.display()));
    let bytes = std::fs::read(report).map_err(|e| cannot(e.to_string()))?;
    if bytes.trim_ascii_start().starts_with(b"<") {
        let doc = xml::parse(&String::from_utf8(bytes).map_err(|e| cannot(e.to_string()))?).map_err(cannot)?;
        cobertura_lines(&doc, root)
            .map_err(|e| Unjudged(format!("the coverage report {} has a line it cannot read: {e}", report.display())))
    } else {
        let here = vec![PathBuf::from(".")];
        lcov_lines(&bytes)
            .map(|lines| (here, lines))
            .map_err(|e| Unjudged(format!("the coverage report {} has a line it cannot read: {e}", report.display())))
    }
}

/// The sources of a Cobertura document, `root` when it lists none, and its lines: each `<line>` of each `<class>`'s
/// `<lines>`. Errs on a line whose number or hits is not a whole number.
fn cobertura_lines(doc: &xml::Element, root: &Path) -> Result<(Vec<PathBuf>, Vec<LineCoverage>), String> {
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
        let file = OsString::from(class.get("filename").unwrap_or(""));
        for line in class.children_named("lines").flat_map(|l| l.children_named("line")) {
            let number = whole(line.get("number").unwrap_or(""))?;
            let (taken, outcomes) = match line.get("branch") {
                Some("true") => branch_counts(line.get("condition-coverage").unwrap_or("")),
                _ => (0, 0),
            };
            let hit = whole(line.get("hits").unwrap_or("0"))? > 0;
            out.push(LineCoverage { file: file.clone(), line: number, hit, taken, outcomes });
        }
    }
    Ok((sources, out))
}

/// The (taken, outcomes) of a Cobertura `condition-coverage` such as `50% (1/2)`; 0 of 0 when it has none.
fn branch_counts(text: &str) -> (usize, usize) {
    let counts = text.split_once('(').and_then(|(_, rest)| rest.split_once(')')).and_then(|(inside, _)| {
        let (taken, outcomes) = inside.split_once('/')?;
        Some((taken.parse().ok()?, outcomes.parse().ok()?))
    });
    counts.unwrap_or((0, 0))
}

fn whole(text: &str) -> Result<u32, String> {
    text.trim().parse().map_err(|_| format!("{text:?} is not a whole number"))
}

/// The lines of an LCOV report (https://manpages.debian.org/lcov/geninfo.1.en.html, "TRACEFILE FORMAT"): each
/// `DA:<line>,<hits>` of each `SF:` record, with the `BRDA:<line>,<block>,<branch>,<taken>` entries of its line as
/// its branch outcomes, a taken of `-` or 0 one not taken. Errs on a DA or BRDA line it cannot read.
fn lcov_lines(report: &[u8]) -> Result<Vec<LineCoverage>, String> {
    let mut out = Vec::new();
    // The current record's file, its lines' hits and, by line, its branches' (taken, outcomes).
    let mut file: Option<OsString> = None;
    let mut hits: BTreeMap<u32, bool> = BTreeMap::new();
    let mut branches: HashMap<u32, (usize, usize)> = HashMap::new();
    for raw in report.split(|&b| b == b'\n') {
        let line = raw.strip_suffix(b"\r").unwrap_or(raw);
        if let Some(path) = line.strip_prefix(b"SF:") {
            file = Some(OsStr::from_bytes(path).to_os_string());
        } else if let Some(fields) = line.strip_prefix(b"DA:") {
            let fields = std::str::from_utf8(fields).map_err(|e| e.to_string())?;
            let mut parts = fields.split(',');
            let number = whole(parts.next().unwrap_or(""))?;
            let count: i64 = parts.next().unwrap_or("").trim().parse().map_err(|_| format!("DA:{fields}"))?;
            *hits.entry(number).or_default() |= count > 0;
        } else if let Some(fields) = line.strip_prefix(b"BRDA:") {
            let fields = std::str::from_utf8(fields).map_err(|e| e.to_string())?;
            let parts: Vec<&str> = fields.split(',').collect();
            let [number, _, _, taken] = parts[..] else { return Err(format!("BRDA:{fields}")) };
            let was = branches.entry(whole(number)?).or_default();
            was.1 += 1;
            was.0 += usize::from(!matches!(taken.trim(), "-" | "0"));
        } else if line == b"end_of_record" {
            if let Some(file) = file.take() {
                out.extend(hits.iter().map(|(&number, &hit)| {
                    let (taken, outcomes) = branches.get(&number).copied().unwrap_or_default();
                    LineCoverage { file: file.clone(), line: number, hit, taken, outcomes }
                }));
            }
            hits.clear();
            branches.clear();
        }
    }
    Ok(out)
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
        assert_eq!(NewCoverage { lines: 19, ..c }.condition(), None);
    }

    use std::os::unix::ffi::OsStringExt;
}
