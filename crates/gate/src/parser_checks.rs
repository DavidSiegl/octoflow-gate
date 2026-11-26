//! The parser checks (crates/checks) on a change: its source files as their languages' front ends read them, each side
//! of each file ([`Sources`]), and the checks run over them.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

use checks::{complexity, cycles, duplicates, integrity, names, Finding};
use model::{CodeToken, Import, ImportGraph};

use crate::change::Change;
use crate::sources::{GoChange, Language, Sources, GO, PYTHON, RUST};
use crate::Unjudged;

/// The test-integrity check's findings on `sources`: the golden files the change edits ([`changed_golden_files`]),
/// when it edits a language's code under test (PythonChange::edits_code, RustChange::edits_code,
/// [`GoChange::edits_code`]), by path, then the findings on Python, then those on Rust, then those on Go, each by path
/// and line. Each finding's severity says whether it fails the gate ([`Finding::severity`]).
pub fn weakened_tests(sources: &Sources) -> Vec<Finding> {
    let python_edited = sources.python.edits_code();
    let rust_edited = sources.rust.edits_code();
    let go_edited = sources.go.edits_code();
    let mut out = if python_edited || rust_edited || go_edited { changed_golden_files(sources.change) } else { vec![] };
    out.extend(integrity::weakened_tests(&sources.python.files, python_edited));
    out.extend(integrity::weakened_tests(&sources.rust.files, rust_edited));
    let mut go = integrity::weakened_tests(&sources.go.files, go_edited);
    go.extend(platform_named(&sources.go));
    out.extend(by_path_and_line(go));
    out
}

/// The test-integrity platform-only findings on the `_test.go` files of `go` whose names the change gave a
/// platform suffix that narrows the platforms their names at the base built them on
/// (lang_go::narrowing_platform_suffix), as `calc_test.go` renamed to `calc_windows_test.go`, which turns its tests
/// off on the other platforms as a `//go:build windows` line would. One about each whole file, by its path in the
/// working tree.
fn platform_named(go: &GoChange) -> Vec<Finding> {
    let mut out = vec![];
    for file in &go.files {
        let Some(after) = file.after.as_ref().filter(|a| a.among_tests) else { continue };
        let base = file.before.as_ref().map(|b| b.path.as_bytes());
        if let Some(suffix) = lang_go::narrowing_platform_suffix(base, after.path.as_bytes()) {
            out.push(Finding {
                path: Some(after.path.clone()),
                line: None,
                severity: "warning".into(),
                rule: "test-integrity platform-only".into(),
                message: format!(
                    "`{suffix}` added to the file's name: it turns the file's tests off on the other platforms."
                ),
            });
        }
    }
    out
}

/// The cognitive-complexity check's findings on `sources`, but on the files gate.toml's `complexity-exempt` names,
/// by path and line. A function takes its base score only from a function of its own language.
pub fn too_complex(sources: &Sources) -> Vec<Finding> {
    let exempt = |path: &OsStr| sources.change.settings.complexity_exempt.matches(path);
    let mut out = complexity::too_complex(&sources.python.files, &exempt);
    out.extend(complexity::too_complex(&sources.rust.files, &exempt));
    out.extend(complexity::too_complex(&sources.go.files, &exempt));
    by_path_and_line(out)
}

/// The vague-names check's findings on `sources`, by path and line.
pub fn vague_names(sources: &Sources) -> Vec<Finding> {
    let mut out = names::vague_names(&sources.python.files, false);
    out.extend(names::vague_names(&sources.rust.files, true));
    // Go names its types and its variables in MixedCaps, as `runInfo` and `RunInfo`.
    out.extend(names::vague_names(&sources.go.files, true));
    by_path_and_line(out)
}

/// The dependency-cycles check's findings on `sources`, by path and line: on Python where the change holds Python,
/// and on Rust where it holds Rust, each language's imports read (lang_python::imports, lang_rust::imports) over all
/// the tree's judged files of its name (Change::judged), `.py` or `.rs`, outside mutmut's state, at the base and in
/// the working tree. The files gate.toml's `dependency-cycles.exempt` names take part in no cycle. Errs when one of the
/// working tree's files cannot be read.
pub fn dependency_cycles(sources: &Sources) -> Result<Vec<Finding>, Unjudged> {
    let mut out = vec![];
    if !sources.python.files.is_empty() {
        out.extend(new_cycles_in(sources, b".py", lang_python::imports)?);
    }
    if !sources.rust.files.is_empty() {
        out.extend(new_cycles_in(sources, b".rs", lang_rust::imports)?);
    }
    Ok(by_path_and_line(out))
}

/// The dependency-cycles check's findings on the files of `sources`' change whose paths end in `suffix`, which
/// `imports` reads, as [`dependency_cycles`] says.
fn new_cycles_in(sources: &Sources, suffix: &[u8], imports: ReadImports) -> Result<Vec<Finding>, Unjudged> {
    let change = sources.change;
    let (base, now) = sources.trees(suffix)?;
    let renamed: HashMap<&OsStr, &OsString> = change
        .files
        .iter()
        .filter_map(|f| Some((f.before.as_deref()?, f.after.as_ref()?)))
        .filter(|(before, after)| before != after)
        .collect();
    let named_now = |path: OsString| renamed.get(path.as_os_str()).map_or(path, |&after| after.clone());
    let base_graph: ImportGraph = imports(&base)
        .into_iter()
        .map(|(path, found)| {
            let found = found.into_iter().map(|i| Import { module: named_now(i.module), ..i }).collect();
            (named_now(path), found)
        })
        .collect();
    let exempt = |path: &OsStr| change.settings.cycles_exempt.matches(path);
    Ok(cycles::new_cycles(&base_graph, &imports(&now), &change.changed, &exempt))
}

/// A front end's reading of a tree's files into the imports between them, as lang_python::imports.
type ReadImports = fn(&[(OsString, Vec<u8>)]) -> ImportGraph;


/// The duplicated-blocks check's findings on `sources`, by path and line, on each of Python, Rust and Go where the
/// change adds a line to a file the check judges: each language's tokens read (lang_python::code_tokens,
/// lang_rust::code_tokens, lang_go::code_tokens) over the working tree's judged files outside mutmut's state that
/// hold the language as the other parser checks tell it, by name or first line and in one of its projects, and are
/// not generated (Sources::tree_sources). The files gate.toml's
/// `duplicated-blocks.exempt` names are neither judged nor matched. Errs when one of the working tree's files cannot be
/// read.
pub fn duplicated_blocks(sources: &Sources) -> Result<Vec<Finding>, Unjudged> {
    let change = sources.change;
    let languages: [(&Language, ReadTokens); 3] =
        [(&PYTHON, lang_python::code_tokens), (&RUST, lang_rust::code_tokens), (&GO, lang_go::code_tokens)];
    let mut out = vec![];
    for (language, code_tokens) in languages {
        let wanted = |path: &OsStr| !change.settings.duplicates_exempt.matches(path);
        // Every window the check reports lies on added lines, so without one in a judged file the tree is not read.
        let judged = |path: &OsStr| wanted(path) && sources.holds_source(path, language);
        if !change.changed.iter().any(|(path, lines)| !lines.is_empty() && judged(path)) {
            continue;
        }
        let files: Vec<(OsString, Vec<CodeToken>)> = (sources.tree_sources(language, wanted)?.into_iter())
            .map(|(path, bytes)| (path, code_tokens(&bytes)))
            .collect();
        out.extend(duplicates::copied_blocks(&files, &change.changed, change.settings.duplicates_min_tokens));
    }
    Ok(by_path_and_line(out))
}

/// A front end's reading of a file's bytes into its code's tokens, as lang_python::code_tokens.
type ReadTokens = fn(&[u8]) -> Vec<CodeToken>;

/// `findings` by path and line, a lineless one first, those of one place in the order given.
fn by_path_and_line(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by(|a, b| (&a.path, a.line.unwrap_or(0)).cmp(&(&b.path, b.line.unwrap_or(0))));
    findings
}


/// A test-integrity expected-value-changed finding, on the whole file, for each golden file (Settings::golden) that
/// `change` edits or deletes, by path, which the gate weighs as it does a changed literal in an assertion.
///
/// [`weakened_tests`] lists them only when the change also edits code outside its tests, which only a language can
/// tell: a golden file is then an expected value outside any assertion, which `UPDATE_GOLDEN=1` rewrites to whatever
/// the code now writes. A golden file the change adds, or renames without editing, has no old value to lose.
fn changed_golden_files(change: &Change) -> Vec<Finding> {
    let finding = |path: &OsString, what: &str| Finding {
        path: Some(path.clone()),
        line: None,
        severity: "warning".into(),
        rule: "test-integrity expected-value-changed".into(),
        message: format!("a golden file, the expected output of a test, {what}"),
    };
    let mut out = vec![];
    for file in &change.files {
        let Some(before) = file.before.as_ref().filter(|p| change.settings.golden.matches(p)) else { continue };
        match &file.after {
            None => out.push(finding(
                before,
                "deleted in a change that also edits the code under test; check that no test still needs it.",
            )),
            Some(after)
                if change.changed.get(after).is_some_and(|l| !l.is_empty())
                    || change.removal_points.get(after).is_some_and(|l| !l.is_empty()) =>
            {
                out.push(finding(
                    after,
                    "changed in a change that also edits the code under test; check that its diff is what the \
                     change means the output to become.",
                ))
            }
            Some(_) => {}
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;
    use crate::settings::Settings;

    #[test]
    fn golden_files_edited_or_deleted_are_found_but_not_added_or_renamed_ones() {
        let file = |before: Option<&str>, after: Option<&str>| crate::diff::ChangedFile {
            before: before.map(OsString::from),
            after: after.map(OsString::from),
        };
        let golden = |name: &str| format!("tests/golden/{name}");
        let change = Change {
            root: PathBuf::from("/"),
            base: "b".into(),
            // Removes the one line of `cut`; `edited` gains one.
            diff: b"diff --git a/tests/golden/cut b/tests/golden/cut\n--- a/tests/golden/cut\n\
                    +++ b/tests/golden/cut\n@@ -1 +0,0 @@\n-x\n"
                .to_vec(),
            removal_points: [(golden("cut").into(), [0].into())].into(),
            changed: [(golden("edited").into(), [1].into()), (golden("cut").into(), [].into())].into(),
            files: vec![
                file(Some(&golden("gone")), None),
                file(Some(&golden("edited")), Some(&golden("edited"))),
                file(Some(&golden("cut")), Some(&golden("cut"))),
                file(None, Some(&golden("new"))),
                file(Some(&golden("old")), Some(&golden("moved"))),
                file(Some("src/edited"), Some("src/edited")),
            ],
            mutmut_state: vec![],
            judged: vec![],
            settings: Settings::default(),
            python: PathBuf::from("python3"),
        };
        let found: Vec<(String, String)> = changed_golden_files(&change)
            .into_iter()
            .map(|f| (crate::text::path(f.path.as_deref().unwrap()), f.message.split(" in a change").next().unwrap().into()))
            .collect();
        let expected = |name: &str, what: &str| {
            (golden(name), format!("a golden file, the expected output of a test, {what}"))
        };
        assert_eq!(found, vec![expected("cut", "changed"), expected("edited", "changed"), expected("gone", "deleted")]);
    }
}
