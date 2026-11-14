//! The quality gate as a library, which the `gate` binary (main.rs) runs: it judges the change a git working tree
//! makes since a base commit and reports on it twice, as the quality file and quality.json (report.rs).
//! docs/port-differences.md lists where its reports differ from those of the gate up to 0.6.0.

pub mod clippy;
pub mod config_edits;
pub mod copies;
pub mod coverage;
pub mod diff;
pub mod flaky_tests;
pub mod git;
pub mod glob;
pub mod go_mutants;
pub mod golangci;
pub mod judge;
pub mod junit;
pub mod linters;
pub mod mutants;
pub mod parser_checks;
pub mod passes_at_base;
pub mod paths;
pub mod process;
pub mod pytest;
pub mod python_at_base;
pub mod python_mutants;
pub mod report;
#[cfg(test)]
mod scratch;
pub mod settings;
pub mod suppressions;
pub mod test_commands;
pub mod text;
pub mod xml;

/// A failure that stops the gate judging a change, so that it writes neither report and exits 2; the message, a
/// sentence fragment, is shown to whoever ran it.
#[derive(Debug, PartialEq, Eq)]
pub struct Unjudged(pub String);
