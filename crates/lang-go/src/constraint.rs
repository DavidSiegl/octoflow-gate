//! A Go file's build constraint, `//go:build expr` (<https://pkg.go.dev/cmd/go#hdr-Build_constraints>), read as what
//! it does to the tests of a `_test.go` file: it builds them only where `expr` holds, so it turns them off everywhere
//! else.

use model::{first_line, Weakening, WeakeningKind};

/// The `GOOS` values, from `go tool dist list`, with the older `nacl`.
const GOOSES: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "js",
    "linux",
    "nacl",
    "netbsd",
    "openbsd",
    "plan9",
    "solaris",
    "wasip1",
    "windows",
    "zos",
];

/// The `GOOS` values the `unix` constraint holds on (go/build's `unixOS`).
const UNIX: &[&str] = &[
    "aix",
    "android",
    "darwin",
    "dragonfly",
    "freebsd",
    "hurd",
    "illumos",
    "ios",
    "linux",
    "netbsd",
    "openbsd",
    "solaris",
];

/// The `GOARCH` values go/build knows (`knownArch`).
const GOARCHES: &[&str] = &[
    "386",
    "amd64",
    "amd64p32",
    "arm",
    "armbe",
    "arm64",
    "arm64be",
    "loong64",
    "mips",
    "mipsle",
    "mips64",
    "mips64le",
    "mips64p32",
    "mips64p32le",
    "ppc",
    "ppc64",
    "ppc64le",
    "riscv",
    "riscv64",
    "s390",
    "s390x",
    "sparc",
    "sparc64",
    "wasm",
];

/// The weakening the comment `comment`, on line `line` of a `_test.go` file's header, is when it is a build constraint:
/// platform-only when its expression names only platforms (`GOOS` and `GOARCH` values and `unix`) and holds on one of
/// them at least, since that leaves the tests out only where what they test may not be built, as Rust's `cfg(unix)`
/// does; a skip otherwise, as for a custom tag such as `integration` or `ignore`, or an expression go cannot read.
/// None for a comment that is no `//go:build` line.
pub fn weakening(comment: &str, line: u32) -> Option<Weakening> {
    let expression = comment.strip_prefix("//go:build")?;
    if !expression.is_empty() && !expression.starts_with([' ', '\t']) {
        return None;
    }
    let platform_only = parse(expression).is_some_and(|e| e.names_only_platforms() && e.holds_on_a_platform());
    let written = first_line(comment.trim_end());
    Some(if platform_only {
        Weakening {
            line,
            kind: WeakeningKind::PlatformOnly,
            message: format!("`{written}` added: it turns the file's tests off on the other platforms."),
        }
    } else {
        Weakening {
            line,
            kind: WeakeningKind::Skip,
            message: format!("`{written}` added: it turns the file's tests off."),
        }
    })
}

/// A build constraint's expression.
#[derive(Debug)]
enum Expr {
    Tag(String),
    Not(Box<Expr>),
    And(Box<Expr>, Box<Expr>),
    Or(Box<Expr>, Box<Expr>),
}

impl Expr {
    fn names_only_platforms(&self) -> bool {
        match self {
            Expr::Tag(tag) => tag == "unix" || GOOSES.contains(&tag.as_str()) || GOARCHES.contains(&tag.as_str()),
            Expr::Not(e) => e.names_only_platforms(),
            Expr::And(a, b) | Expr::Or(a, b) => a.names_only_platforms() && b.names_only_platforms(),
        }
    }

    /// Whether it holds for some pair of [`GOOSES`] and [`GOARCHES`].
    fn holds_on_a_platform(&self) -> bool {
        GOOSES.iter().any(|os| GOARCHES.iter().any(|arch| self.holds(os, arch)))
    }

    /// Whether it holds when building for `os` and `arch`, with the tags go sets for them: `unix` on [`UNIX`], and
    /// `linux` on android, `darwin` on ios and `solaris` on illumos.
    fn holds(&self, os: &str, arch: &str) -> bool {
        match self {
            Expr::Tag(tag) => {
                let implied = match os {
                    "android" => "linux",
                    "ios" => "darwin",
                    "illumos" => "solaris",
                    _ => os,
                };
                tag == os || tag == implied || tag == arch || (tag == "unix" && UNIX.contains(&os))
            }
            Expr::Not(e) => !e.holds(os, arch),
            Expr::And(a, b) => a.holds(os, arch) && b.holds(os, arch),
            Expr::Or(a, b) => a.holds(os, arch) || b.holds(os, arch),
        }
    }
}

/// `expression` as an [`Expr`], `||` binding looser than `&&` and `!` tighter, as go/build/constraint parses it; None
/// where it does not parse.
fn parse(expression: &str) -> Option<Expr> {
    let tokens = lex(expression)?;
    let mut at = 0;
    let expr = or(&tokens, &mut at)?;
    (at == tokens.len()).then_some(expr)
}

/// `expression` split into its tags and operators; None for a character no constraint holds.
fn lex(expression: &str) -> Option<Vec<String>> {
    let mut out = vec![];
    let mut chars = expression.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            ' ' | '\t' => {}
            '(' | ')' | '!' => out.push(c.to_string()),
            '&' | '|' => {
                if chars.next() != Some(c) {
                    return None;
                }
                out.push(format!("{c}{c}"));
            }
            c if c.is_alphanumeric() || c == '_' || c == '.' => {
                let mut tag = c.to_string();
                while let Some(&next) = chars.peek().filter(|n| n.is_alphanumeric() || **n == '_' || **n == '.') {
                    tag.push(next);
                    chars.next();
                }
                out.push(tag);
            }
            _ => return None,
        }
    }
    Some(out)
}

fn or(tokens: &[String], at: &mut usize) -> Option<Expr> {
    let mut left = and(tokens, at)?;
    while tokens.get(*at).is_some_and(|t| t == "||") {
        *at += 1;
        left = Expr::Or(Box::new(left), Box::new(and(tokens, at)?));
    }
    Some(left)
}

fn and(tokens: &[String], at: &mut usize) -> Option<Expr> {
    let mut left = not(tokens, at)?;
    while tokens.get(*at).is_some_and(|t| t == "&&") {
        *at += 1;
        left = Expr::And(Box::new(left), Box::new(not(tokens, at)?));
    }
    Some(left)
}

fn not(tokens: &[String], at: &mut usize) -> Option<Expr> {
    let token = tokens.get(*at)?;
    *at += 1;
    match token.as_str() {
        "!" => Some(Expr::Not(Box::new(not(tokens, at)?))),
        "(" => {
            let inner = or(tokens, at)?;
            (tokens.get(*at)? == ")").then(|| *at += 1)?;
            Some(inner)
        }
        ")" | "&&" | "||" => None,
        tag => Some(Expr::Tag(tag.into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kind(comment: &str) -> Option<WeakeningKind> {
        weakening(comment, 1).map(|w| w.kind)
    }

    #[test]
    fn a_constraint_naming_only_platforms_that_holds_on_one_is_platform_only() {
        for comment in [
            "//go:build linux",
            "//go:build unix && !darwin",
            "//go:build (linux || windows) && amd64",
            "//go:build !windows",
        ] {
            assert_eq!(kind(comment), Some(WeakeningKind::PlatformOnly), "{comment}");
        }
    }

    #[test]
    fn any_other_constraint_is_a_skip_and_any_other_comment_none() {
        for comment in [
            "//go:build integration",
            "//go:build ignore",
            "//go:build linux && windows",
            "//go:build linux &&",
            "//go:build",
        ] {
            assert_eq!(kind(comment), Some(WeakeningKind::Skip), "{comment}");
        }
        for comment in ["// go:build linux", "//go:buildx linux", "// +build linux", "// Package x."] {
            assert_eq!(kind(comment), None, "{comment}");
        }
    }
}
