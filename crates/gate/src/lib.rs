//! The quality gate as a library, which the `gate` binary (main.rs) runs: it judges the change a git working tree
//! makes since a base commit and reports on it twice, as the quality file and quality.json (report.rs).
//! docs/port-differences.md lists where its reports differ from those of the gate up to 0.6.0.
//!
//! The re-exports below and [`Unjudged`] are the crate's whole interface, what main.rs and the integration tests
//! under tests/ use; every module is private. The library is not published: a release's contract is the binary's
//! (docs/quality-gate.md, "The report as a contract").

mod change;
mod changed_tests;
mod clippy;
mod config_edits;
mod copies;
mod coverage;
mod diff;
mod flaky_tests;
mod git;
mod glob;
mod golangci;
mod gotest;
mod judge;
mod junit;
mod linters;
mod parser_checks;
mod passes_at_base;
mod paths;
mod process;
mod pytest;
mod report;
#[cfg(test)]
mod scratch;
mod scratch_directory;
mod settings;
mod sources;
mod suppressions;
mod surviving_mutants;
mod test_commands;
mod text;
mod xml;

pub use change::Change;
pub use config_edits::{tool_config, touched_paths, ToolConfig};
pub use coverage::NewCoverage;
pub use diff::{changed_lines, unquote};
pub use glob::Globs;
pub use judge::{judge_change, Given};
pub use parser_checks::{dependency_cycles, duplicated_blocks, too_complex, vague_names, weakened_tests};
pub use paths::real_path;
pub use process::{catch_interrupt, exit_interrupted, interrupted};
pub use report::{Finding, QualityReport, Verdict, GATE_VERSION, QUALITY_FINDINGS_LIMIT};
pub use settings::{Settings, RULES_FILE};
pub use sources::Sources;
pub use text::{one_line, py_strip, series, shown_path};

/// A failure that stops the gate judging a change, so that it writes neither report and exits 2; the message, a
/// sentence fragment, is shown to whoever ran it.
#[derive(Debug, PartialEq, Eq)]
pub struct Unjudged(pub String);
