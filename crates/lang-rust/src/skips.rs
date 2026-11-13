//! The attributes that skip a Rust test or turn it off, as the parser places them. docs/quality-gate.md, "Test
//! integrity", says which attributes are skips and where.

use std::collections::BTreeMap;

use model::{first_line, Lines, Weakening, WeakeningKind};

use crate::syntax::{Attr, Fn, Mod};

/// The skips in a Rust file, one for each line an attribute [`skipping`] matches starts on: an attribute of one of
/// `tests`, its own or one written in its body (syntax::Fn::body_attrs); an outer attribute of a module of `holding`,
/// the inline modules that hold a test; or one of `inner`, the file's inner attributes, `#![...]`, inside such a
/// module, or anywhere in the file when `among_tests` places it among the tests. A cfg naming only platforms
/// ([`platform_only`]) is a platform-only weakening instead, but never on a statement or item in a test's body: there
/// it turns an assertion off on a platform where the test still runs. A line with any other skip on it is a skip.
pub fn skips(tests: &[&Fn], holding: &[&Mod], inner: &[Attr], among_tests: bool) -> Vec<Weakening> {
    let inside = |line: u32| among_tests || holding.iter().any(|m| m.line <= line && line <= m.end);
    // Each attribute judged, and whether a platform-only cfg is weighed there rather than a skip.
    let judged = tests
        .iter()
        // A body's inner attribute is the test's own, as an outer one is, and turns the whole of it off.
        .flat_map(|t| {
            let own = t.attrs.iter().map(|a| (a, true));
            own.chain(t.body_attrs.iter().map(|a| (a, a.text.starts_with("#!"))))
        })
        .chain(holding.iter().flat_map(|m| m.attrs.iter().map(|a| (a, true))))
        .chain(inner.iter().filter(|a| inside(a.line)).map(|a| (a, true)));
    let mut by_line: BTreeMap<u32, (Vec<&Attr>, bool)> = BTreeMap::new();
    for (a, weighed) in judged.filter(|(a, _)| skipping(&a.text)) {
        let (attributes, all_weighed) = by_line.entry(a.line).or_insert((vec![], true));
        attributes.push(a);
        *all_weighed &= weighed;
    }
    by_line.into_iter().map(|(n, (attributes, weighed))| skip(n, &attributes, weighed)).collect()
}

/// The weakening starting at line `n` whose skipping attributes are `attributes`: platform-only when `weighed` and each
/// is [`platform_only`], else a skip naming the first that is not.
fn skip(n: u32, attributes: &[&Attr], weighed: bool) -> Weakening {
    let code = model::without_whitespace(&attributes.iter().map(|a| a.text.as_str()).collect::<String>());
    let wraps = |a: &&Attr| u32::try_from(a.text.matches('\n').count()).unwrap_or(u32::MAX);
    let lines = Lines { first: n, last: n.saturating_add(attributes.iter().map(wraps).max().unwrap_or(0)) };
    match attributes.iter().find(|a| !(weighed && platform_only(&a.text))) {
        Some(failing) => Weakening {
            lines,
            kind: WeakeningKind::Skip,
            code,
            message: format!("`{}` added: it skips a test or turns it off.", first_line(&failing.text)),
        },
        None => Weakening {
            lines,
            kind: WeakeningKind::PlatformOnly,
            code,
            message: format!("`{}` added: it turns a test off on the other platforms.", first_line(&attributes[0].text)),
        },
    }
}

/// Whether `attribute`, from its `#`, skips a test or turns it or the module holding it off: `#[ignore]`, a `cfg`
/// other than `cfg(test)`, such as `cfg(all(test, feature = "slow"))`, or a `cfg_attr` that writes `ignore`, as an
/// argument after its predicate's comma. Read no further than its first `]`.
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
            let quoted = tokens.get(i + 2).filter(|v| v.starts_with('"') && v.len() >= 2)?;
            let named = &quoted[1..quoted.len() - 1];
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
}
