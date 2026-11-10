//! The gate's parser checks: what a change did to the functions, tests and names of its source files, judged over the
//! model of each file (crates/model) whatever its language. Each check takes one language's [`ChangedFile`]s and
//! gives its [`Finding`]s, which the gate lists beside the linters' (gate::judge).
//!
//! - [`complexity`]: functions the change took over the cognitive-complexity limit.
//! - [`integrity`]: tests the change removed or weakened.
//! - [`names`]: vague names the change bound.

use std::collections::BTreeSet;
use std::ffi::OsString;

pub use model::first_line;
use model::{SourceFile, Unreadable};

pub mod complexity;
pub mod integrity;
pub mod names;

/// One thing the gate found, as a linter or a check reports it.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct Finding {
    /// The file, from the tree's top level; None for a finding about the whole change.
    pub path: Option<OsString>,
    /// The line, from 1; None for a finding about the whole file.
    pub line: Option<u32>,
    /// What the gate does with it: "error" fails the gate, and the gate lists a check's "warning" for the Reviewer to
    /// weigh; a linter's finding has the linter's own grade, and fails the gate whatever it is.
    pub severity: String,
    /// The tool or check that found it, then its own rule, space-separated: `ruff F401`, `test-integrity skip`.
    pub rule: String,
    pub message: String,
}

impl Finding {
    /// The tool or check that found it: the rule's first word.
    pub fn check(&self) -> &str {
        self.rule.split(' ').next().unwrap_or("")
    }

    /// A finding of the check named `check`, of its own `rule` ("" for none): one that fails the gate when `fails`,
    /// else one the gate lists for the Reviewer to weigh, as what a spec may have asked for.
    fn of(check: &str, rule: &str, fails: bool, path: Option<&OsString>, line: Option<u32>, message: String) -> Finding {
        Finding {
            path: path.cloned(),
            line,
            severity: if fails { "error" } else { "warning" }.into(),
            rule: if rule.is_empty() { check.into() } else { format!("{check} {rule}") },
            message,
        }
    }
}

/// A file of one language that a change touches: as it was at the base and as the working tree has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ChangedFile {
    /// The file at the base; None where the change adds it, or where it was no file of the language there.
    pub before: Option<Side>,
    /// The file in the working tree; None where the change deletes it, or where it is no file of the language now.
    pub after: Option<Side>,
    /// The lines of `after` the change added.
    pub added: BTreeSet<u32>,
    /// The lines of `before` the change removed.
    pub removed: BTreeSet<u32>,
}

/// One side of a [`ChangedFile`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Side {
    /// The file's path on this side, from the tree's top level.
    pub path: OsString,
    /// Whether the project's settings place the file among the tests, as its front end was told: by its path, and
    /// for a language whose test code is told by its file alone, as Python's, by its other side's path too. The only
    /// sign of a test file left where `source` is an error.
    pub among_tests: bool,
    /// The file as its front end read it, or why it could not.
    pub source: Result<SourceFile, Unreadable>,
}

impl ChangedFile {
    /// The model of the file at the base, when there is one that its front end could read.
    fn base(&self) -> Option<&SourceFile> {
        self.before.as_ref().and_then(|s| s.source.as_ref().ok())
    }

    /// The model of the file in the working tree, when there is one that its front end could read.
    fn now(&self) -> Option<&SourceFile> {
        self.after.as_ref().and_then(|s| s.source.as_ref().ok())
    }
}
