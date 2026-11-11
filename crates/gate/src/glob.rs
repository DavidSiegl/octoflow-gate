//! Globs over a tree's paths, as gate.toml writes them and git's `:(glob)` pathspecs read them.

use std::ffi::OsStr;
use std::os::unix::ffi::OsStrExt;

use regex::Regex;

use crate::text;

/// A set of globs, which a path from the tree's top level matches when one of them matches it whole. A glob reads as
/// git's `:(glob)` pathspecs do: `*`, `?` and a bracket expression stop at a slash, `**/` stands for any
/// directories, none included, and a trailing `/**` for everything below; a name starting with a dot is matched like
/// any other. A bracket expression is fnmatch's: `[!...]` negates it, and a backslash in it is itself. No glob
/// matches nothing. A glob means what Python 3.13's `glob.translate(recursive=True, include_hidden=True)` makes of
/// it, except that no bracket expression matches a slash.
#[derive(Clone, Debug)]
pub struct Globs {
    globs: Vec<String>,
    whole: Regex,
}

impl Globs {
    pub fn new<S: AsRef<str>>(globs: &[S]) -> Globs {
        let globs: Vec<String> = globs.iter().map(|g| g.as_ref().to_string()).collect();
        let any: Vec<String> = globs.iter().map(|g| format!("(?s:{})", translate(g))).collect();
        let pattern = if any.is_empty() { NEVER.to_string() } else { any.join("|") };
        let whole = Regex::new(&format!("^(?:{pattern})$")).expect("a translated glob is a valid regex");
        Globs { globs, whole }
    }

    /// Whether one of the globs matches `path`, from the tree's top level. A byte of the path that is not UTF-8 is
    /// one character to `?` or a bracket expression.
    pub fn matches(&self, path: &OsStr) -> bool {
        self.whole.is_match(&text::per_byte_lossy(path.as_bytes()))
    }

    /// The globs as given.
    pub fn globs(&self) -> &[String] {
        &self.globs
    }
}

/// A regex no text matches, which Rust's regex writes as an empty class.
const NEVER: &str = "[a&&b]";

/// `glob` as a regex over a whole path, as [`Globs`] reads it.
fn translate(glob: &str) -> String {
    let parts: Vec<&str> = glob.split('/').collect();
    let last = parts.len() - 1;
    let mut out = String::new();
    for (k, part) in parts.iter().enumerate() {
        match *part {
            "*" if k < last => out.push_str("[^/]+/"),
            "*" => out.push_str("[^/]+"),
            "**" if k < last => {
                if parts[k + 1] != "**" {
                    out.push_str("(?:.+/)?");
                }
            }
            "**" => out.push_str(".*"),
            part => {
                out.push_str(&translate_name(part, false));
                if k < last {
                    out.push('/');
                }
            }
        }
    }
    out
}

/// Whether the pattern `pattern` matches `name` whole, as Python's `fnmatch.fnmatchcase` reads one: `*`, `?` and a
/// bracket expression match a slash like any other character, so `*` crosses directories and `**` means no more
/// than `*`. What mutmut's only_mutate and do_not_mutate are, where [`Globs`] are gate.toml's.
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    let whole = Regex::new(&format!("^(?s:{})$", translate_name(pattern, true)));
    whole.expect("a translated pattern is a valid regex").is_match(name)
}

/// `part` as a regex, by fnmatch's `_translate`: `*`, `?` and bracket expressions, which match a slash only when
/// `across_slashes`; without it `part` is one `/`-free part of a glob, and they stay within one name.
fn translate_name(part: &str, across_slashes: bool) -> String {
    let one = if across_slashes { "." } else { "[^/]" };
    let chars: Vec<char> = part.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        match c {
            '*' => {
                // A run of stars is one star.
                if i < 2 || chars[i - 2] != '*' {
                    out.push_str(one);
                    out.push('*');
                }
            }
            '?' => out.push_str(one),
            '[' => match bracket(&chars, i, across_slashes) {
                Some((class, end)) => {
                    out.push_str(&class);
                    i = end;
                }
                None => out.push_str(r"\["),
            },
            c => out.push_str(&regex::escape(&c.to_string())),
        }
    }
    out
}

/// The bracket expression whose members start at `chars[start]`, just past its `[`, as a regex class, which matches
/// a slash only when `across_slashes` and its members say so, and the index past its `]`; None when no `]` closes
/// it, so the `[` is itself.
fn bracket(chars: &[char], start: usize, across_slashes: bool) -> Option<(String, usize)> {
    let j = closing_bracket(chars, start)?;
    let negated = chars[start] == '!';
    let mut chunks = range_chunks(chars, start, j, negated);
    if negated {
        chunks[0].remove(0);
    }
    let members: String = chunks
        .iter()
        .map(|chunk| chunk.iter().map(|c| regex::escape(&c.to_string())).collect::<String>())
        .collect::<Vec<_>>()
        .join("-");
    let class = match (negated, members.is_empty()) {
        (false, true) => NEVER.to_string(),
        // `[!]` with no member: any character, a slash too, as fnmatch's `.`.
        (true, true) => "(?s:.)".to_string(),
        (false, false) if across_slashes => format!("[{members}]"),
        (true, false) if across_slashes => format!("[^{members}]"),
        (false, false) => format!("[[{members}]&&[^/]]"),
        (true, false) => format!("[^{members}/]"),
    };
    Some((class, j + 1))
}

/// The index of the `]` that closes the bracket expression whose members start at `chars[start]`, as fnmatch finds
/// it: a `]` first, after any `!`, is a member. None when no `]` closes it.
fn closing_bracket(chars: &[char], start: usize) -> Option<usize> {
    let mut j = start;
    if chars.get(j) == Some(&'!') {
        j += 1;
    }
    if chars.get(j) == Some(&']') {
        j += 1;
    }
    (j..chars.len()).find(|&j| chars[j] == ']')
}

/// The members `chars[start..end]` of a bracket expression, `!` included when `negated`, split at each hyphen that
/// makes a range, as fnmatch splits them: a chunk's last character and the next one's first are a range's ends. A
/// hyphen first, after any `!`, or last is a member. A range whose ends are out of order is dropped, as fnmatch drops
/// it, so it matches nothing. Never empty.
fn range_chunks(chars: &[char], start: usize, end: usize, negated: bool) -> Vec<Vec<char>> {
    let mut chunks: Vec<Vec<char>> = Vec::new();
    let mut i = start;
    let mut k = if negated { start + 2 } else { start + 1 };
    while let Some(at) = (k..end).find(|&at| chars[at] == '-') {
        chunks.push(chars[i..at].to_vec());
        i = at + 1;
        k = at + 3;
    }
    let rest = chars[i..end].to_vec();
    match chunks.last_mut() {
        Some(last) if rest.is_empty() => last.push('-'),
        _ => chunks.push(rest),
    }
    for k in (1..chunks.len()).rev() {
        if let (Some(&last), Some(&next)) = (chunks[k - 1].last(), chunks[k].first()) {
            if last > next {
                let tail = chunks.remove(k);
                chunks[k - 1].pop();
                chunks[k - 1].extend_from_slice(&tail[1..]);
            }
        }
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matches(glob: &str, path: &str) -> bool {
        Globs::new(&[glob]).matches(OsStr::new(path))
    }

    #[test]
    fn stars_stop_at_a_slash_and_double_stars_do_not() {
        assert!(matches("**/*.py", "a.py"));
        assert!(matches("**/*.py", "a/b/.c.py"));
        assert!(!matches("*.py", "a/b.py"));
        assert!(matches("**/tests/**", "x/tests/y/z"));
        assert!(!matches("**/tests/**", "x/tests"));
        assert!(matches("a*/b", "a/b"));
        assert!(matches(".github/workflows/*.yml", ".github/workflows/ci.yml"));
        assert!(!matches("a/*/b", "a//b"));
    }

    #[test]
    fn brackets_are_fnmatch_s_but_never_match_a_slash() {
        assert!(matches("a[!b]c", "axc"));
        assert!(!matches("a[!b]c", "a/c"));
        assert!(!matches("a[.-0]c", "a/c"));
        assert!(matches("a[.-0]c", "a.c"));
        assert!(matches("[]]", "]"));
        assert!(matches("[a-]", "-"));
        assert!(matches("[\\]", "\\"));
        assert!(matches("[a[]", "["));
        assert!(matches("a[", "a["));
        assert!(!matches("[z-a]", "m"));
        assert!(matches("[?]", "?"));
        assert!(matches("[a-c]", "b"));
        assert!(matches("[b-b]", "b"));
        assert!(!matches("[!a-c]", "b"));
        assert!(matches("[!a-c]", "d"));
        assert!(matches("[a-cx-z]", "y"));
        assert!(!matches("[a-cx-z]", "d"));
        assert!(matches("[!]]", "a"));
        assert!(!matches("[!]]", "]"));
        assert!(matches("[-a]", "-"));
    }

    #[test]
    fn fnmatch_s_wildcards_and_brackets_match_a_slash() {
        assert!(fnmatch("src/pkg/deep/mod.py", "src/*.py"));
        assert!(fnmatch("src/a.py", "src?a.py"));
        assert!(fnmatch("a/c", "a[.-0]c"));
        assert!(fnmatch("a/c", "a[!b]c"));
        assert!(!fnmatch("abc", "a[!b]c"));
        assert!(fnmatch("a/b/c.py", "**/c.py"));
        assert!(!fnmatch("c.py", "**/c.py"));
        assert!(!fnmatch("src/a.pyc", "src/*.py"));
        assert!(fnmatch("a\nb", "a*b"));
        assert!(fnmatch("a[", "a["));
        assert!(!fnmatch("m", "[z-a]"));
    }

    #[test]
    fn a_byte_that_is_not_utf8_is_one_character() {
        use std::os::unix::ffi::OsStrExt;
        assert!(Globs::new(&["a?b"]).matches(OsStr::from_bytes(b"a\xffb")));
    }

    #[test]
    fn no_glob_matches_nothing() {
        assert!(!Globs::new::<&str>(&[]).matches(OsStr::new("")));
        assert!(!Globs::new::<&str>(&[]).matches(OsStr::new("a")));
    }
}
