//! The quality gate as a library, the Rust port of gate.py, which the `gate` binary (main.rs) runs: it judges the
//! change a git working tree makes since a base commit and reports on it twice, as the quality file and quality.json
//! (report.rs). gate.py stays the judge until the cutover (gate-standalone/18); until then this judges only what has
//! been ported, as each module says, and `cargo xtask compare` checks it against gate.py.

pub mod coverage;
pub mod paths;
pub mod report;
pub mod text;
pub mod xml;

/// A failure that stops the gate judging a change, so that it writes neither report and exits 2; the message, a
/// sentence fragment, is shown to whoever ran it. gate.py's GateError.
#[derive(Debug, PartialEq, Eq)]
pub struct Unjudged(pub String);
