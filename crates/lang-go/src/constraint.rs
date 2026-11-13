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
/// platform-only when its expression names no custom tag, only platforms (`GOOS` and `GOARCH` values and `unix`),
/// release tags (`go1.1` to the newest release the gate knows, each taken as set, as on the newest toolchain:
/// [`release_tag`]) and the toolchain's tags (`cgo`, `gc`, `gccgo`), and holds on some platform and toolchain, since
/// that leaves the tests out only where what they test may not be built, as Rust's `cfg(unix)` does; a skip
/// otherwise, as for a custom tag such as `integration`, `ignore` or a release no toolchain sets yet, `go1.99`, a
/// negated release tag such as `!go1.21`, or an expression go cannot read. None for a comment that is no `//go:build`
/// line.
pub fn weakening(comment: &str, line: u32) -> Option<Weakening> {
    let expression = comment.strip_prefix("//go:build")?;
    if !expression.is_empty() && !expression.starts_with([' ', '\t']) {
        return None;
    }
    let platform_only = parse(expression).is_some_and(|e| e.names_no_custom_tag() && e.holds_on_some_build());
    let written = first_line(comment.trim_end());
    let code = model::without_whitespace(comment);
    Some(if platform_only {
        Weakening {
            line,
            kind: WeakeningKind::PlatformOnly,
            code,
            message: format!("`{written}` added: it turns the file's tests off on the other platforms and toolchains."),
        }
    } else {
        Weakening {
            line,
            kind: WeakeningKind::Skip,
            code,
            message: format!("`{written}` added: it turns the file's tests off."),
        }
    })
}

/// The compilers' names, which the go tool sets as a tag for the compiler that builds.
const COMPILERS: &[&str] = &["gc", "gccgo"];

/// The minor version of the newest Go release the gate knows, N of go 1.N: 27, for Go 1.27
/// (<https://go.dev/doc/devel/release>). It is raised with each Go release. Until it is, a test file whose
/// `//go:build` names the new release's tag, as `go1.28`, is a skip, a failing finding, since the tag reads as a
/// custom one ([`release_tag`]).
const NEWEST_RELEASE: u32 = 27;

/// Whether `tag` is a release tag, `go1.N`, N written as the go tool writes it, without a leading zero, and at most
/// [`NEWEST_RELEASE`]. A toolchain sets those up to its own release, and a change's tests matter where they run on the
/// newest, so each is taken as set: `go1.21` holds and `!go1.21` turns the tests off. A `go1.N` above the newest
/// release is set by no toolchain, so it is no release tag but a custom one.
fn release_tag(tag: &str) -> bool {
    let Some(minor) = tag.strip_prefix("go1.") else { return false };
    let written_as_go_writes_it = !minor.starts_with('0') && minor.bytes().all(|b| b.is_ascii_digit());
    written_as_go_writes_it && minor.parse::<u32>().is_ok_and(|n| n <= NEWEST_RELEASE)
}

/// What a build sets the tags of a constraint by: the platform, and the toolchain.
struct Build<'a> {
    os: &'a str,
    arch: &'a str,
    /// One of [`COMPILERS`].
    compiler: &'a str,
    cgo: bool,
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
    /// Whether each of its tags is a platform's ([`GOOSES`], [`GOARCHES`] and `unix`), the toolchain's (`cgo` and
    /// [`COMPILERS`]) or a release's ([`release_tag`]).
    fn names_no_custom_tag(&self) -> bool {
        match self {
            Expr::Tag(tag) => {
                let tag = tag.as_str();
                tag == "unix"
                    || tag == "cgo"
                    || GOOSES.contains(&tag)
                    || GOARCHES.contains(&tag)
                    || COMPILERS.contains(&tag)
                    || release_tag(tag)
            }
            Expr::Not(e) => e.names_no_custom_tag(),
            Expr::And(a, b) | Expr::Or(a, b) => a.names_no_custom_tag() && b.names_no_custom_tag(),
        }
    }

    /// Whether it holds for some [`Build`]: a pair of [`GOOSES`] and [`GOARCHES`], with one of [`COMPILERS`], with
    /// cgo or without.
    fn holds_on_some_build(&self) -> bool {
        let toolchains: Vec<(&str, bool)> = COMPILERS.iter().flat_map(|c| [(*c, false), (*c, true)]).collect();
        let holds = |os, arch, (compiler, cgo)| self.holds(&Build { os, arch, compiler, cgo });
        let on_platform = |os, arch| toolchains.iter().any(|&toolchain| holds(os, arch, toolchain));
        GOOSES.iter().any(|os| GOARCHES.iter().any(|arch| on_platform(os, arch)))
    }

    /// Whether it holds in `build`, with the tags go sets for it: its `GOOS` and `GOARCH`, `unix` on [`UNIX`], and
    /// `linux` on android, `darwin` on ios and `solaris` on illumos; its compiler's name and `cgo` where it has cgo;
    /// and every release tag ([`release_tag`]).
    fn holds(&self, build: &Build) -> bool {
        match self {
            Expr::Tag(tag) => {
                let os = build.os;
                let implied = match os {
                    "android" => "linux",
                    "ios" => "darwin",
                    "illumos" => "solaris",
                    _ => os,
                };
                tag == os
                    || tag == implied
                    || tag == build.arch
                    || (tag == "unix" && UNIX.contains(&os))
                    || tag == build.compiler
                    || (tag == "cgo" && build.cgo)
                    || release_tag(tag)
            }
            Expr::Not(e) => !e.holds(build),
            Expr::And(a, b) => a.holds(build) && b.holds(build),
            Expr::Or(a, b) => a.holds(build) || b.holds(build),
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
    fn a_constraint_naming_release_and_toolchain_tags_beside_platforms_is_platform_only() {
        for comment in [
            "//go:build go1.21",
            "//go:build go1.27",
            "//go:build cgo",
            "//go:build !cgo",
            "//go:build gc || gccgo",
            "//go:build linux && cgo && go1.22",
        ] {
            assert_eq!(kind(comment), Some(WeakeningKind::PlatformOnly), "{comment}");
        }
    }

    #[test]
    fn a_release_tag_is_taken_as_set_so_one_negated_turns_the_tests_off_and_a_malformed_one_is_custom() {
        for comment in [
            "//go:build !go1.1",
            "//go:build !go1.21",
            "//go:build go1.28",
            "//go:build go1.99",
            "//go:build linux || go1.99 && !linux",
            "//go:build go1.21 && !go1.21",
            "//go:build go1.21 && !go1.23",
            "//go:build linux && !go1.22",
            "//go:build cgo && !cgo",
            "//go:build gc && gccgo",
            "//go:build go1.21 && integration",
            "//go:build go1",
            "//go:build go1.x",
            "//go:build go1.",
            "//go:build go1.021",
            "//go:build go1.0",
        ] {
            assert_eq!(kind(comment), Some(WeakeningKind::Skip), "{comment}");
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
