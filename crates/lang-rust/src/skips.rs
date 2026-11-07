//! The attributes that skip a Rust test or turn it off, read off the source text line by line, as gate.py's
//! `_rust_skips` reads them: so that an attribute the parser places nowhere, such as one inside a test's body, is
//! still seen. docs/quality-gate.md, "Test integrity", says which attributes are skips and where.

use model::{first_line, Weakening, WeakeningKind};

use crate::syntax::{Fn, Mod};

/// The skips in a Rust file, by the line they start on, `text` being the file: the attributes [`skipping`] matches
/// of each of `tests`; on a line of its own inside a test's body, as a statement's; among the outer attributes of a
/// module of `holding`, the inline modules that hold a test; or an inner attribute, `#![...]`, on a line of its own
/// inside such a module, or anywhere in the file when `among_tests` places it among the tests. A cfg naming only
/// platforms ([`platform_only`]) is a platform-only weakening instead, but never a statement's: in a test's body it
/// turns an assertion off on a platform where the test still runs. A line with any other skip on it is a skip.
pub fn skips(text: &str, tests: &[&Fn], holding: &[&Mod], among_tests: bool) -> Vec<Weakening> {
    let lines: Vec<&str> = text.split('\n').collect();
    let mut out = vec![];
    for n in 1..=u32::try_from(lines.len()).unwrap_or(u32::MAX) {
        // A test's own attributes on the line, as the parser gives them, else what the line's text holds.
        let own: Vec<&str> = tests
            .iter()
            .flat_map(|t| &t.attrs)
            .filter(|a| a.line == n && skipping(&a.text))
            .map(|a| a.text.as_str())
            .collect();
        if !own.is_empty() {
            out.push(skip(n, &own, true));
            continue;
        }
        let i = (n - 1) as usize;
        let line = lines[i].trim_start();
        let Some(place) = attribute_place(among_tests, n, line, tests, holding) else { continue };
        let found = line_attributes(line, &lines[i + 1..]);
        let found: Vec<&str> = found.iter().map(String::as_str).filter(|a| skipping(a)).collect();
        if !found.is_empty() {
            out.push(skip(n, &found, place != Place::Statement));
        }
    }
    out
}

/// Where [`skips`] judges the attributes on a line of a file.
#[derive(PartialEq, Eq)]
enum Place {
    /// On a line of its own in a test's body.
    Statement,
    /// Among a module's outer attributes, or an inner attribute of a module or file of tests.
    Module,
}

/// Where [`skips`] judges the attributes on line `n`, `line` being its text without its indentation; None where it
/// judges none, as on a line not starting with `#`.
fn attribute_place(among_tests: bool, n: u32, line: &str, tests: &[&Fn], holding: &[&Mod]) -> Option<Place> {
    if line.starts_with("#!") {
        let inside = among_tests || holding.iter().any(|m| m.line <= n && n <= m.end);
        return inside.then_some(Place::Module);
    }
    if !line.starts_with('#') {
        return None;
    }
    if tests.iter().any(|t| t.line < n && n <= t.end) {
        return Some(Place::Statement);
    }
    holding.iter().any(|m| m.attrs_line <= n && n < m.line).then_some(Place::Module)
}

/// The weakening at line `n` whose skipping attributes are `attributes`, each whole: platform-only when `weighed`
/// and each is [`platform_only`], else a skip naming the first that is not.
fn skip(n: u32, attributes: &[&str], weighed: bool) -> Weakening {
    match attributes.iter().find(|a| !(weighed && platform_only(a))) {
        Some(failing) => Weakening {
            line: n,
            kind: WeakeningKind::Skip,
            message: format!("`{}` added: it skips a test or turns it off.", first_line(failing)),
        },
        None => Weakening {
            line: n,
            kind: WeakeningKind::PlatformOnly,
            message: format!("`{}` added: it turns a test off on the other platforms.", first_line(attributes[0])),
        },
    }
}

/// Whether `attribute`, from its `#`, skips a test or turns it or the module holding it off: `#[ignore]`, a `cfg`
/// other than `cfg(test)`, such as `cfg(all(test, feature = "slow"))`, or a `cfg_attr` that writes `ignore`, as an
/// argument after its predicate's comma. Read no further than its first `]`. gate.py's `_RUST_SKIPPING`.
pub fn skipping(attribute: &str) -> bool {
    let Some(rest) = attribute.strip_prefix('#') else { return false };
    let rest = rest.strip_prefix('!').unwrap_or(rest);
    let Some(rest) = rest.strip_prefix('[') else { return false };
    let rest = rest.trim_start();
    if let Some(after) = rest.strip_prefix("ignore") {
        return !after.chars().next().is_some_and(is_word);
    }
    if let Some(after) = rest.strip_prefix("cfg_attr") {
        let Some(args) = after.trim_start().strip_prefix('(') else { return false };
        let args = &args[..args.find(']').unwrap_or(args.len())];
        // A comma, then `ignore`, then `=`, `,` or `)`, anywhere before the first `]`.
        return args.match_indices(',').any(|(i, _)| {
            let Some(after) = args[i + 1..].trim_start().strip_prefix("ignore") else { return false };
            after.trim_start().starts_with(['=', ',', ')'])
        });
    }
    if let Some(after) = rest.strip_prefix("cfg") {
        let Some(predicate) = after.trim_start().strip_prefix('(') else { return false };
        let only_test = predicate
            .trim_start()
            .strip_prefix("test")
            .and_then(|r| r.trim_start().strip_prefix(')'))
            .is_some_and(|r| r.trim_start().starts_with(']'));
        return !only_test;
    }
    false
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// The attribute that `first` starts with, whole: `first` up to its closing `]`, or, when it wraps as rustfmt wraps a
/// long one, joined by single spaces with the trimmed lines of `following` up to the one holding that `]`; and how
/// many lines of `following` it wraps onto, None when it never closes, the attribute then being `first` and all of
/// `following`. Brackets inside a string literal (`"..."`, its `\"` escapes skipped) are not counted; a char literal
/// or a raw string is read as a plain string, so one holding a `"` or a bracket may leave the attribute unclosed.
fn attribute(first: &str, following: &[&str]) -> (String, Option<usize>) {
    let mut depth = 0i64;
    let mut quoted = false;
    let mut parts: Vec<&str> = vec![];
    for (used, text) in std::iter::once(first).chain(following.iter().map(|l| l.trim())).enumerate() {
        let mut escaped = false;
        for (j, c) in text.char_indices() {
            if quoted {
                (quoted, escaped) = (escaped || c != '"', !escaped && c == '\\');
                continue;
            }
            quoted = c == '"';
            depth += match c {
                '[' => 1,
                ']' => -1,
                _ => 0,
            };
            if c == ']' && depth == 0 {
                parts.push(&text[..j + 1]);
                return (parts.join(" "), Some(used));
            }
        }
        parts.push(text);
    }
    (parts.join(" "), None)
}

/// Each attribute, whole ([`attribute`]), that starts at a `#[` or `#![` on `line`, followed in the file by the
/// lines `following`, or on a line one of them wraps onto before it closes, so that a wrapped attribute hides none.
/// Every `#[` is read, also one inside another attribute's string, as in `#[doc = "#[ignore]"]`: a literal
/// [`attribute`] misreads then cannot hide a skip after it, at the cost of reporting such a string's text.
fn line_attributes(line: &str, following: &[&str]) -> Vec<String> {
    let mut out = vec![];
    // Each line to read, as its index in following (-1 for `line`), and the last line of following that the
    // attributes read so far wrap onto (-1 for none).
    let mut read: Vec<isize> = vec![-1];
    let mut reached: isize = -1;
    let mut k = 0;
    while let Some(&i) = read.get(k) {
        k += 1;
        let text = if i < 0 { line } else { following[i.unsigned_abs()].trim() };
        let rest = &following[(i + 1).unsigned_abs()..];
        for (start, _) in text.match_indices('#') {
            let after = &text[start + 1..];
            if !(after.starts_with('[') || after.starts_with("![")) {
                continue;
            }
            let (found, used) = attribute(&text[start..], rest);
            out.push(found);
            let last = used.map(|used| i + used.cast_signed());
            if let Some(last) = last.filter(|&last| last > reached) {
                read.extend(i.max(reached) + 1..=last);
                reached = last;
            }
        }
    }
    out
}

/// The platforms a platform-only cfg is judged over: the `target_os` values of common targets, each with its
/// `target_family` values (<https://doc.rust-lang.org/reference/conditional-compilation.html>). A cfg naming an os or
/// a family not here is not platform-only, so that a misspelt or invented one cannot turn a test off with only a
/// warning.
const TARGET_OSES: &[(&str, &[&str])] = &[
    ("linux", &["unix"]),
    ("android", &["unix"]),
    ("macos", &["unix"]),
    ("ios", &["unix"]),
    ("freebsd", &["unix"]),
    ("netbsd", &["unix"]),
    ("openbsd", &["unix"]),
    ("dragonfly", &["unix"]),
    ("illumos", &["unix"]),
    ("solaris", &["unix"]),
    ("windows", &["windows"]),
    ("wasi", &["wasm"]),
    ("unknown", &["wasm"]),
    ("none", &[]),
];

/// A set of [`TARGET_OSES`], one bit each by index.
type Oses = u32;

const EVERYWHERE: Oses = (1 << TARGET_OSES.len()) - 1;

/// How deep [`cfg_oses`] reads `any`, `all` and `not` inside one another; a predicate nested deeper is not
/// platform-only, which only a change written to break the gate has.
const MAX_DEPTH: usize = 64;

/// Whether `attribute`, an attribute's whole text from its `#` (`#[...]` or `#![...]`, anything after its `]`
/// ignored), is a `cfg` whose predicate names only platforms and holds on one: `unix`, `windows`, `target_os = "..."`
/// or `target_family = "..."` of an os or family [`TARGET_OSES`] knows, alone or in `any(...)`, `all(...)` and
/// `not(...)` of them, with `test` also allowed as an argument of an outermost `all`. Such a cfg leaves a test out only
/// on platforms where what it tests is not built. False for any other attribute; for a predicate naming no platform,
/// holding on none of [`TARGET_OSES`] (as `all(unix, windows)`), or with an empty `any()` or `all()`; and for text
/// this reads no closing `]` in.
pub fn platform_only(attribute: &str) -> bool {
    let tokens = cfg_tokens(attribute);
    let opening: &[&str] = if attribute.starts_with("#!") { &["#", "!", "[", "cfg", "("] } else { &["#", "[", "cfg", "("] };
    if tokens.len() < opening.len() || tokens[..opening.len()] != *opening {
        return false;
    }
    match cfg_oses(&tokens, opening.len(), true, 0) {
        Some((oses, i)) => oses != 0 && tokens.get(i..i + 2) == Some(&[")", "]"][..]),
        None => false,
    }
}

/// The tokens of `attribute`, each after any whitespace: a name, a string literal with its quotes, or any other
/// character.
fn cfg_tokens(attribute: &str) -> Vec<&str> {
    let mut out = vec![];
    let mut rest = attribute;
    loop {
        rest = rest.trim_start();
        let Some(c) = rest.chars().next() else { return out };
        let len = if c.is_ascii_alphabetic() || c == '_' {
            rest.find(|c: char| !is_word(c)).unwrap_or(rest.len())
        } else if c == '"' {
            string_len(rest).unwrap_or(1)
        } else {
            c.len_utf8()
        };
        out.push(&rest[..len]);
        rest = &rest[len..];
    }
}

/// The length of the string literal `text` starts with, its quotes included; None when it does not close.
fn string_len(text: &str) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in text.char_indices().skip(1) {
        match c {
            _ if escaped => escaped = false,
            '\\' => escaped = true,
            '"' => return Some(i + 1),
            _ => {}
        }
    }
    None
}

/// The cfg predicate starting at `tokens[i]`, read as [`platform_only`] says, `outermost` when it is the cfg's own
/// rather than an argument of another, `depth` deep in others: the oses it holds on, and the index of the token after
/// it. None when it is anything but platforms, names an os or family [`TARGET_OSES`] lacks, or the tokens end inside
/// it.
fn cfg_oses(tokens: &[&str], i: usize, outermost: bool, depth: usize) -> Option<(Oses, usize)> {
    let name = *tokens.get(i)?;
    if matches!(name, "any" | "all" | "not") && tokens.get(i + 1) == Some(&"(") {
        return (depth < MAX_DEPTH).then(|| combined_oses(tokens, i, outermost, depth)).flatten();
    }
    let (oses, next) = match name {
        "unix" | "windows" => (family_oses(name), i + 1),
        "target_os" | "target_family" if tokens.get(i + 1) == Some(&"=") => {
            let value = tokens.get(i + 2).filter(|v| v.starts_with('"') && v.len() >= 2)?;
            let named = &value[1..value.len() - 1];
            let oses = if name == "target_os" {
                TARGET_OSES.iter().position(|(os, _)| *os == named).map_or(0, |k| 1 << k)
            } else {
                family_oses(named)
            };
            (oses, i + 3)
        }
        _ => return None,
    };
    (oses != 0).then_some((oses, next))
}

/// [`cfg_oses`] for the `any(...)`, `all(...)` or `not(...)` at `tokens[i]`: the union, the intersection or the
/// complement of its arguments' oses, a trailing comma allowed, and `test` skipped as an argument of an outermost
/// `all`. None as [`cfg_oses`] says, and for no argument but `test`, or a `not` of other than one.
fn combined_oses(tokens: &[&str], i: usize, outermost: bool, depth: usize) -> Option<(Oses, usize)> {
    let name = tokens[i];
    let mut arguments = vec![];
    let mut i = i + 2;
    while i < tokens.len() && tokens[i] != ")" {
        if outermost && name == "all" && tokens[i] == "test" {
            i += 1;
        } else {
            let (oses, next) = cfg_oses(tokens, i, false, depth + 1)?;
            arguments.push(oses);
            i = next;
        }
        match tokens.get(i) {
            Some(&",") => i += 1,
            Some(&")") => {}
            _ => return None,
        }
    }
    if i >= tokens.len() || arguments.is_empty() || (name == "not" && arguments.len() != 1) {
        return None;
    }
    let oses = match name {
        "any" => arguments.iter().fold(0, |a, b| a | b),
        "all" => arguments.iter().fold(EVERYWHERE, |a, b| a & b),
        _ => EVERYWHERE & !arguments[0],
    };
    Some((oses, i + 1))
}

/// The oses of [`TARGET_OSES`] in the target family `family`; none for a family it does not know.
fn family_oses(family: &str) -> Oses {
    TARGET_OSES.iter().enumerate().filter(|(_, (_, families))| families.contains(&family)).fold(0, |a, (k, _)| a | 1 << k)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skipping_attributes_are_ignore_cfgs_but_cfg_test_and_cfg_attrs_writing_ignore() {
        for (attribute, skips) in [
            ("#[ignore]", true),
            ("#[ignore = \"slow\"]", true),
            ("#[ignored]", false),
            ("#[cfg(test)]", false),
            ("#[cfg( test )]", false),
            ("#[cfg(all(test, feature = \"slow\"))]", true),
            ("#[cfg(unix)]", true),
            ("#![cfg(windows)]", true),
            ("#[cfg_attr(miri, ignore)]", true),
            ("#[cfg_attr(miri, ignore = \"slow\")]", true),
            ("#[cfg_attr(miri, ignored)]", false),
            ("#[cfg_attr(miri, allow(x))]", false),
            ("#[test]", false),
            ("# [ignore]", false),
        ] {
            assert_eq!(skipping(attribute), skips, "{attribute}");
        }
    }

    #[test]
    fn a_platform_only_cfg_names_only_known_platforms_and_holds_on_one() {
        for (attribute, only) in [
            ("#[cfg(unix)]", true),
            ("#[cfg(target_os = \"linux\")]", true),
            ("#[cfg(any(unix, target_family = \"wasm\"))]", true),
            ("#[cfg(all(test, windows))]", true),
            ("#[cfg(not(windows))]", true),
            ("#![cfg(unix)]", true),
            ("#[cfg(all(unix, windows))]", false),
            ("#[cfg(target_os = \"plan9\")]", false),
            ("#[cfg(feature = \"x\")]", false),
            ("#[cfg(any())]", false),
            ("#[cfg(all(test))]", false),
            ("#[cfg(not(unix, windows))]", false),
            ("#[cfg(any(all(test, unix)))]", false),
            ("#[cfg(unix)", false),
            ("#[ignore]", false),
        ] {
            assert_eq!(platform_only(attribute), only, "{attribute}");
        }
        let deep = format!("#[cfg({}unix{})]", "not(".repeat(200), ")".repeat(200));
        assert!(!platform_only(&deep));
    }

    #[test]
    fn an_attribute_is_read_whole_across_the_lines_it_wraps_onto() {
        assert_eq!(attribute("#[cfg(unix)] fn f() {}", &[]), ("#[cfg(unix)]".into(), Some(0)));
        assert_eq!(attribute("#[cfg(any(", &["  unix,", "  windows", "))]"]), ("#[cfg(any( unix, windows ))]".into(), Some(3)));
        assert_eq!(attribute("#[doc = \"]\"]", &[]), ("#[doc = \"]\"]".into(), Some(0)));
        assert_eq!(attribute("#[doc = \"\\\"]\"]", &[]), ("#[doc = \"\\\"]\"]".into(), Some(0)));
        assert_eq!(attribute("#[cfg(", &["unix"]), ("#[cfg( unix".into(), None));
        assert_eq!(
            line_attributes("#[cfg(any(", &["  unix,", "  #[ignore] windows", "))]"]),
            vec!["#[cfg(any( unix, #[ignore] windows ))]".to_string(), "#[ignore]".into()]
        );
        assert_eq!(line_attributes("#[doc = \"#[ignore]\"]", &[]).len(), 2);
    }
}
