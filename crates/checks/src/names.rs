//! The vague-names check: names a change binds that could name anything.

use std::collections::HashMap;

use crate::{ChangedFile, Finding};

const CHECK: &str = "vague-names";

/// The words a name may not be, or end in after an underscore, in any case: they fit whatever a variable holds. Not
/// "result": the rules allow it for what a function returns, and telling that use from the others needs more than a
/// parser.
pub const VAGUE_WORDS: [&str; 6] = ["data", "info", "value", "status", "handle", "manager"];

/// The check's findings on `files`, one language's files of a change, by path and then line: each vague name that a
/// line the change added binds, once per name and file, at its first binding on an added line. A binding the language
/// fixes ([`model::Binding::fixed`]) is not the author's choice and never vague. A file the change adds lines to that
/// its front end could not read is a not-read finding.
///
/// A name is vague when it is one of [`VAGUE_WORDS`] or ends in one after an underscore; and, when `camel`, which says
/// the language names its types in CamelCase with no underscore to split on, as Rust does, when its last CamelCase
/// word is one, as `RunInfo` or `StateManager`. The check only lists what it finds: none fails the gate.
pub fn vague_names(files: &[ChangedFile], camel: bool) -> Vec<Finding> {
    let mut out = vec![];
    for file in files.iter().filter(|f| !f.added.is_empty()) {
        let Some(after) = &file.after else { continue };
        let source = match &after.source {
            Ok(source) => source,
            Err(unreadable) => {
                let message = format!("{}, so its names were not read.", unreadable.why);
                out.push(Finding::of(CHECK, "not-read", false, Some(&after.path), unreadable.line, message));
                continue;
            }
        };
        // Each vague name's first binding on an added line, as (line, column).
        let mut first: HashMap<&str, (u32, u32)> = HashMap::new();
        for b in &source.bindings {
            if !b.fixed && file.added.contains(&b.line) && vague(&b.name, camel) {
                let at = first.entry(b.name.as_str()).or_insert((b.line, b.column));
                *at = (*at).min((b.line, b.column));
            }
        }
        let mut found: Vec<_> = first.into_iter().collect();
        found.sort_by_key(|&(_, at)| at);
        for (name, (line, _)) in found {
            let message = format!("`{name}` could name anything; name it for what it holds.");
            out.push(Finding::of(CHECK, "vague-name", false, Some(&after.path), Some(line), message));
        }
    }
    out.sort_by(|a, b| (&a.path, a.line.unwrap_or(0)).cmp(&(&b.path, b.line.unwrap_or(0))));
    out
}

/// Whether `name` is vague, as [`vague_names`] says, `camel` as there.
fn vague(name: &str, camel: bool) -> bool {
    let is_vague = |word: &str| VAGUE_WORDS.iter().any(|v| v.eq_ignore_ascii_case(word));
    let after_underscore = name.rsplit('_').next().unwrap_or(name);
    if is_vague(after_underscore) {
        return true;
    }
    // The last CamelCase word: an upper-case letter and the lower-case letters and digits after it, ending the name.
    let tail = name.trim_end_matches(|c: char| c.is_ascii_lowercase() || c.is_ascii_digit());
    camel && tail.ends_with(|c: char| c.is_ascii_uppercase()) && is_vague(&name[tail.len() - 1..])
}

#[cfg(test)]
mod tests {
    use super::vague;

    #[test]
    fn a_name_is_vague_by_its_word_after_the_last_underscore_in_any_case() {
        for name in ["data", "raw_info", "HANDLE", "DATA_info", "_value", "Data"] {
            assert!(vague(name, false), "{name}");
        }
        for name in ["Information", "value2", "data_", "Valuer", "RunInfo", "infos"] {
            assert!(!vague(name, false), "{name}");
        }
    }

    #[test]
    fn with_camel_a_name_is_also_vague_by_its_last_camel_case_word() {
        for name in ["RunInfo", "StateManager", "HTTPStatus", "fooData", "valueData", "raw_info"] {
            assert!(vague(name, true), "{name}");
        }
        for name in ["InfoStore", "Valuer", "Value2", "Information", "value2", "data_"] {
            assert!(!vague(name, true), "{name}");
        }
    }
}
