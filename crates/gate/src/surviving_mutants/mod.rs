//! The surviving-mutants check: a function the change touches whose mutants its tests do not notice is a finding.
//! mutmut mutates the Python functions (python.rs), cargo-mutants the Rust ones (rust.rs) and gremlins the Go ones
//! (go.rs), in turn on the one copy of the working tree this module makes; what the three share is here.

mod go;
mod python;
mod rust;

use std::collections::BTreeSet;
use std::ffi::{OsStr, OsString};
use std::time::Duration;

use checks::Finding;
use model::Function;

use crate::copies::{in_working_tree_copy, sort_by_place, Budget, OnCopy};
use crate::sources::Sources;
use crate::text;

pub use python::Mutmut;

use go::{edited_functions, go_mutants};
use python::python_mutants;
use rust::{edited_rust, rust_mutants};

/// How long cargo-mutants has, after the SIGINT that stops a run at the deadline, to write what it found and exit
/// before it is killed, and mutmut as long.
const STOP_GRACE: Duration = Duration::from_secs(10);

/// What a run is given beyond [`STOP_GRACE`] after the deadline: the most process::run_with waits for what a killed
/// process printed (its KILL_WAIT).
const KILL_WAIT: Duration = Duration::from_secs(5);

/// How many of a function's surviving mutants its finding shows, in the order the mutation tool made them.
const SURVIVOR_EXAMPLES: usize = 3;

/// The surviving-mutants check's findings on `sources`, by path and line, a finding on no path or line first; each
/// rule is the check's own, without its name, and each severity "warning".
///
/// Mutation-tests the Python functions the change touches with mutmut, set up as `mutmut` says
/// ([`python_mutants`]), then the Rust functions it edits with cargo-mutants ([`rust_mutants`]),
/// then the Go functions it edits with gremlins ([`go_mutants`]), all within `budget` seconds. Each language
/// in turn has an equal share of what the ones before it left, among itself and the languages after it that have
/// something to mutate: with all three, mutmut has a third of the budget, cargo-mutants half of what is left, and
/// gremlins the rest. Each one's findings name its share as the seconds it was stopped at.
///
/// The three run in turn on one copy of the working tree (copies::in_working_tree_copy), made only when one of them has
/// something to mutate. cargo-mutants builds outside the copy, and gremlins runs once mutmut's state, `mutants/`, is
/// removed from it ([`go_mutants`]), so that neither takes another's files for its own. A copy that cannot be
/// made is a `not-run` finding in place of all their runs, and one that cannot be removed afterwards a `not-removed`
/// one.
pub fn surviving_mutants(sources: &Sources, mutmut: &Mutmut, budget: u64) -> Vec<Finding> {
    let budget = Budget::starting_now(budget);
    let (rust, mut out) = edited_rust(&sources.rust);
    let (go, unread) = edited_functions(&sources.go);
    out.extend(unread);
    let (has_rust, has_go) = (!rust.is_empty(), !go.is_empty());
    let share = |later: &[bool]| budget.share(1 + later.iter().map(|&has| u32::from(has)).sum::<u32>());
    let python = share(&[has_rust, has_go]);
    let (found, mutmut_run) = python_mutants(&sources.python, mutmut, python.seconds);
    out.extend(found);
    let mut runs: Vec<OnCopy> = mutmut_run.into_iter().collect();
    if has_rust {
        runs.push(Box::new(|copy| rust_mutants(copy, &rust, &share(&[has_go]))));
    }
    if has_go {
        runs.push(Box::new(|copy| go_mutants(copy, &sources.go.change.base, &go, &share(&[]))));
    }
    let unjudged = "the change's functions were not mutation-tested";
    out.extend(in_working_tree_copy(sources.rust.change, "not-run", unjudged, runs));
    sort_by_place(&mut out);
    out
}

/// A function the change edits, which the mutation tool of its language mutates.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Edited {
    /// Its file, from the tree's top.
    path: OsString,
    /// Its name, as model::Function's, a Go method's as `Stack.Push`.
    name: String,
    /// The lines from its `fn` or `func` to its closing brace, its doc comment and attributes left out.
    line: u32,
    last: u32,
}

impl Edited {
    /// `f`, of the file at `path`, from the tree's top, as an edited function.
    fn of(path: &OsStr, f: &Function) -> Edited {
        Edited { path: path.to_os_string(), name: f.name.clone(), line: f.line, last: f.lines.last }
    }
}

/// Whether a change edits the function `f`: it adds one of `added`, the lines it adds to `f`'s file, from `f`'s `fn` or
/// `func` line to its closing brace, or removes lines from between two of those, at one of `removed`, as
/// change::Change::removal_points gives them. A deleted guard clause needs a test as much as an added one.
fn edits(f: &Function, added: &BTreeSet<u32>, removed: &BTreeSet<u32>) -> bool {
    added.range(f.line..=f.lines.last).next().is_some() || removed.iter().any(|&n| f.line <= n && n < f.lines.last)
}

/// The directory `dir`, from the tree's top, as findings name it, as a Cargo workspace's or a Go module's: `.` for the
/// top.
fn dir_name(dir: &OsStr) -> String {
    if dir.is_empty() {
        ".".to_string()
    } else {
        text::path(dir)
    }
}
