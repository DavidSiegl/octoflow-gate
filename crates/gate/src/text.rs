//! How the gate writes what it read: file names and tool output that need not be UTF-8, text that must stay on one
//! line of the quality file, lists in prose, and the fingerprint quality.json gives a finding.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use sha2::{Digest, Sha256};

/// `bytes` as text: UTF-8 where it is, and each byte that is not written `\xNN`, two lowercase hex digits, so a file
/// or a terminal can take it. gate.py holds such a byte as a lone surrogate and writes it the same way (its `_utf8`).
pub fn lossless(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        for b in chunk.invalid() {
            out.push_str(&format!("\\x{b:02x}"));
        }
    }
    out
}

/// A path as the gate writes it, by [`lossless`].
pub fn path(p: &OsStr) -> String {
    lossless(p.as_bytes())
}

/// `bytes` as text with each byte that is not UTF-8 one U+FFFD, so a glob counts it as the one character gate.py's
/// surrogate is; unlike `String::from_utf8_lossy`, which gives one U+FFFD for a run of such bytes.
pub fn per_byte_lossy(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for chunk in bytes.utf8_chunks() {
        out.push_str(chunk.valid());
        out.extend(chunk.invalid().iter().map(|_| char::REPLACEMENT_CHARACTER));
    }
    out
}

/// The characters some reader of the quality file ends a line at, a Markdown renderer or Python's `str.splitlines`,
/// which [`one_line`] writes as escapes.
pub const LINE_BREAKS: &[char] =
    &['\n', '\r', '\u{b}', '\u{c}', '\u{1c}', '\u{1d}', '\u{1e}', '\u{85}', '\u{2028}', '\u{2029}'];

/// `text` as the quality file writes it within one of its lines, such as an item of a list or a path in a sentence:
/// each character of [`LINE_BREAKS`] written `\xNN`, or `\uNNNN` past U+00FF, anything else as it is. A reader of
/// the quality file reads it back line by line, so the escape is what it reads, the same in every round.
pub fn one_line(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            c if !LINE_BREAKS.contains(&c) => out.push(c),
            c if u32::from(c) <= 0xff => out.push_str(&format!("\\x{:02x}", u32::from(c))),
            c => out.push_str(&format!("\\u{:04x}", u32::from(c))),
        }
    }
    out
}

/// `text` without the whitespace at either end, as Python's `str.strip()` takes it: Unicode's White_Space and the
/// separators U+001C to U+001F, which Python also counts as whitespace and Rust's `str::trim` does not.
pub fn py_strip(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&c))
}

/// `items` as a list in prose: "a", "a and b" or "a, b and c"; "" for none. With `limit`, at most that many are
/// named, then how many are not, as "a, b and 3 more".
pub fn series(items: &[String], limit: Option<usize>) -> String {
    let mut named: Vec<String> = items.to_vec();
    if let Some(limit) = limit.filter(|&limit| items.len() > limit) {
        named.truncate(limit);
        named.push(format!("{} more", items.len() - limit));
    }
    match named.split_last() {
        None => String::new(),
        Some((last, [])) => last.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// `percent` to one decimal, rounded down, so that a value just below a threshold never prints as the threshold.
pub fn floor_tenth(percent: f64) -> String {
    format!("{:.1}", (percent * 10.0).floor() / 10.0)
}

/// A finding's fingerprint in quality.json, 16 lowercase hex digits, from its path (None for one about the whole
/// change), rule and message as the quality file writes them: the same for a finding that moved, or whose message
/// differs only in its ASCII digits, and different when the path, the rule or the rest of the message differs. It is
/// the start of the SHA-256 of path ("" for None), rule and message without its ASCII digits, joined by NUL
/// characters, as UTF-8; octoflow's engine computes it the same way (engine/src/quality.rs, finding_fingerprint).
pub fn fingerprint(path: Option<&str>, rule: &str, message: &str) -> String {
    let message: String = message.chars().filter(|c| !c.is_ascii_digit()).collect();
    let digest = Sha256::digest([path.unwrap_or(""), rule, &message].join("\0").as_bytes());
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lossless_escapes_each_byte_that_is_not_utf8() {
        assert_eq!(lossless(b"m\xff.py"), "m\\xff.py");
        assert_eq!(lossless(b"\xe2\x82a\xc3\xa9"), "\\xe2\\x82a\u{e9}");
    }

    #[test]
    fn per_byte_lossy_gives_one_character_a_byte() {
        assert_eq!(per_byte_lossy(b"a\xe2\x82b"), "a\u{fffd}\u{fffd}b");
    }

    #[test]
    fn one_line_escapes_line_breaks_alone() {
        assert_eq!(one_line("a\nb\rc\u{85}d\u{2028}e\tf"), "a\\x0ab\\x0dc\\x85d\\u2028e\tf");
    }

    #[test]
    fn series_reads_as_prose() {
        let items = |n: usize| (1..=n).map(|k| k.to_string()).collect::<Vec<_>>();
        assert_eq!(series(&items(0), None), "");
        assert_eq!(series(&items(1), None), "1");
        assert_eq!(series(&items(2), None), "1 and 2");
        assert_eq!(series(&items(3), None), "1, 2 and 3");
        assert_eq!(series(&items(7), Some(5)), "1, 2, 3, 4, 5 and 2 more");
        assert_eq!(series(&items(5), Some(5)), "1, 2, 3, 4 and 5");
    }

    #[test]
    fn floor_tenth_rounds_down() {
        assert_eq!(floor_tenth(79.99), "79.9");
        assert_eq!(floor_tenth(100.0), "100.0");
        assert_eq!(floor_tenth(45.833_333_333_333_336), "45.8");
    }

    #[test]
    fn the_fingerprint_is_the_engine_s() {
        // gate.py's test_the_fingerprint_is_the_engine_s, whose values are octoflow-ai's
        // engine/tests/oracle/quality.json.
        for (path, rule, message, want) in [
            (Some("a.py"), "ruff C901", "too complex (16 > 15)", "d0917b36716743cd"),
            (None, "x", "y", "2081f4f26135019e"),
            (Some("\u{e9}/\u{fc}.py"), "r", "\u{663} and 3 and \u{b2}", "d4b7b570d3ec0fc7"),
            (Some(""), "", "", "96a296d224f285c6"),
            (Some("a\0b"), "rule", "m", "62f3c27545353791"),
        ] {
            assert_eq!(fingerprint(path, rule, message), want, "{path:?} {rule} {message}");
        }
    }
}
