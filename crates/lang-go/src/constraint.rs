//! A Go file's build constraint, `//go:build expr`, or in a file with none the legacy `// +build` lines
//! (<https://pkg.go.dev/cmd/go#hdr-Build_constraints>), read as what it does to the tests of a `_test.go` file: it
//! builds them only where the constraint holds, so it turns them off everywhere else. A file's name constrains it too
//! ([`platform_suffix`]).

use model::{first_line, Lines, Weakening, WeakeningKind};

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

/// The weakenings that the build constraint lines of `source`, a `_test.go` file's text, are, in the order written.
/// They are its `//go:build` lines; or, where it has none, its `// +build` lines, which go 1.17 and later still obey
/// then: its space-separated options or'd, each option's comma-separated tags and'd, a tag negated by `!`, and the
/// lines and'd with one another, a term that is no tag set by no build and a line too long for go none
/// ([`plus_build`]). Go reads them where go/build's `parseFileHeader` does ([`header`]): a `//go:build` line on any
/// `//` line before the first code, a `// +build` line only above the last blank line before it, so not in a package's
/// doc comment. Each is a weakening at its line of the kind its file's whole constraint is: platform-only when the
/// constraint names no custom tag, only platforms (`GOOS` and `GOARCH` values and `unix`), release tags (`go1.1` to
/// the newest release the gate knows, each taken as set, as on the newest toolchain: [`release_tag`]) and the
/// toolchain's tags (`cgo`, `gc`, `gccgo`), and holds on some platform and toolchain, since that leaves the tests out
/// only where what they test may not be built, as Rust's `cfg(unix)` does; a skip otherwise, as for a custom tag such
/// as `integration`, `ignore` or a release no toolchain sets yet, `go1.99`, a negated release tag such as `!go1.21`,
/// or a `//go:build` line go cannot read.
pub fn weakenings(source: &str) -> Vec<Weakening> {
    let (comments, plus_build_end) = header(source);
    let read = |line: &dyn Fn(&str) -> Option<Option<Expr>>, end: u32| {
        let above = comments.iter().filter(|&&(_, at)| at < end);
        above.filter_map(|&(comment, at)| Some((comment, at, line(comment)?))).collect::<Vec<_>>()
    };
    let go_build = read(&go_build, u32::MAX);
    let lines = if go_build.is_empty() { read(&|c| plus_build(c).map(Some), plus_build_end) } else { go_build };
    let whole = lines.iter().try_fold(None, |whole: Option<Expr>, (_, _, expr)| {
        let expr = expr.clone()?;
        Some(Some(match whole {
            Some(w) => Expr::And(Box::new(w), Box::new(expr)),
            None => expr,
        }))
    });
    let platform_only = whole.flatten().is_some_and(|e| e.names_no_custom_tag() && e.holds_on_some_build());
    lines.into_iter().map(|(comment, at, _)| weakening(comment, at, platform_only)).collect()
}

/// The header of `source`, a Go file's text, as go/build's `parseFileHeader` reads it: its lines up to the first that
/// holds text outside a comment. Returns the `//` comments among them, but those inside a `/* */` comment, each
/// trimmed and with its line, from 1; and the line of the last blank line before the first line that is neither blank
/// nor a `//` comment, 0 where there is none.
fn header(source: &str) -> (Vec<(&str, u32)>, u32) {
    // go/build's importReader drops a leading byte order mark before it reads the header (go/build/read.go).
    let source = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut comments = vec![];
    let mut last_blank = 0;
    // Whether a line neither blank nor a `//` comment has been met.
    let mut ended = false;
    let mut in_block_comment = false;
    for (at, line) in (1..).zip(source.lines()) {
        let rest = line.trim();
        if rest.is_empty() && !ended {
            last_blank = at;
            continue;
        }
        ended |= !rest.starts_with("//");
        if !in_block_comment && rest.starts_with("//") {
            comments.push((rest, at));
        }
        if holds_code(rest, &mut in_block_comment) {
            break;
        }
    }
    (comments, last_blank)
}

/// Whether `line`, trimmed, holds text outside its comments, `in_block_comment` saying whether a `/* */` comment is
/// open at its start; where it holds none, sets `in_block_comment` to whether one is open at its end.
fn holds_code(mut line: &str, in_block_comment: &mut bool) -> bool {
    loop {
        if *in_block_comment {
            let Some(i) = line.find("*/") else { return false };
            *in_block_comment = false;
            line = line[i + 2..].trim();
        } else if line.is_empty() || line.starts_with("//") {
            return false;
        } else if let Some(after) = line.strip_prefix("/*") {
            *in_block_comment = true;
            line = after.trim();
        } else {
            return true;
        }
    }
}

/// The platform suffix of the Go file name at `path`, as written, as `_windows` of `calc_windows_test.go`, where it
/// narrows the platforms the name at `base` builds the file on: where it names a `GOOS` or `GOARCH` that the suffix at
/// `base` does not, by itself or as one it implies, as `android` implies `linux`. So a name with none renamed to
/// `calc_windows_test.go`, or `_linux` to `_linux_amd64`, narrows; `_linux_amd64` to `_amd64`, or `_android` to
/// `_linux`, does not, and gives None. A suffix is a name's last `_`-separated part before `.` but for a `_test`, a
/// `GOOS` or `GOARCH` value, or its last two, a `GOOS` then a `GOARCH` value, as go/build reads a file name
/// (<https://pkg.go.dev/go/build#hdr-Build_Constraints>). `base` is None for a file new at the change.
pub fn narrowing_platform_suffix(base: Option<&[u8]>, path: &[u8]) -> Option<String> {
    let suffix = platform_suffix(path)?;
    let had = base.and_then(platform_suffix).unwrap_or_default();
    let had: Vec<&str> = had.split('_').filter(|p| !p.is_empty()).flat_map(|p| [p, implied_os(p)]).collect();
    let narrows = !suffix.split('_').filter(|p| !p.is_empty()).all(|part| had.contains(&part));
    narrows.then_some(suffix)
}

/// The `GOOS` that go sets as a tag beside `os`, as `linux` on `android`; `os` itself for any other.
fn implied_os(os: &str) -> &str {
    match os {
        "android" => "linux",
        "ios" => "darwin",
        "illumos" => "solaris",
        _ => os,
    }
}

/// The suffix of the Go file name at `path`, from the tree's top level, that builds the file on one platform alone,
/// as written, as `_windows` of `calc_windows_test.go`: its last `_`-separated part before `.` but for a `_test`, a
/// `GOOS` or `GOARCH` value ([`GOOSES`], [`GOARCHES`]), or its last two, a `GOOS` then a `GOARCH` value, as go/build
/// reads a file name (<https://pkg.go.dev/go/build#hdr-Build_Constraints>). None for a name with no such suffix, as
/// `windows_test.go`, whose part before its first `_` is no suffix, or `calc_unix_test.go`, as `unix` constrains a
/// file only in a constraint line.
fn platform_suffix(path: &[u8]) -> Option<String> {
    let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
    let name = std::str::from_utf8(name).ok()?.split('.').next().unwrap_or_default();
    let name = name.strip_suffix("_test").unwrap_or(name);
    let parts: Vec<&str> = name.split('_').skip(1).collect();
    let suffix = match parts.as_slice() {
        [.., os, arch] if GOOSES.contains(os) && GOARCHES.contains(arch) => &parts[parts.len() - 2..],
        [.., last] if GOOSES.contains(last) || GOARCHES.contains(last) => &parts[parts.len() - 1..],
        _ => return None,
    };
    Some(suffix.iter().map(|p| format!("_{p}")).collect())
}

/// The constraint a `//go:build` line, `comment`, writes; None inside for one go cannot read, and None for a comment
/// that is no such line.
fn go_build(comment: &str) -> Option<Option<Expr>> {
    let expression = comment.strip_prefix("//go:build")?;
    if !expression.is_empty() && !expression.starts_with([' ', '\t']) {
        return None;
    }
    Some(parse(expression))
}

/// The constraint a `// +build` line, `comment`, writes, as go/build/constraint's `parsePlusBuildExpr` does: a term
/// that is no tag, as `foo-bar`, an empty one, `!` or one starting `!!`, is [`Expr::Unset`], and so is a line with no
/// option. None for a comment that is no such line, and for one of more than [`MOST_PLUS_BUILD_OPERATORS`], which go
/// ignores.
fn plus_build(comment: &str) -> Option<Expr> {
    let options = comment.strip_prefix("//")?.trim_start_matches([' ', '\t']).strip_prefix("+build")?;
    if !options.is_empty() && !options.starts_with([' ', '\t']) {
        return None;
    }
    let operators = options.matches(',').count() + options.split_whitespace().count().saturating_sub(1);
    if operators > MOST_PLUS_BUILD_OPERATORS {
        return None;
    }
    let term = |term: &str| {
        if term.starts_with("!!") || term == "!" {
            return Expr::Unset;
        }
        let (negated, tag) = term.strip_prefix('!').map_or((false, term), |t| (true, t));
        let readable = !tag.is_empty() && tag.chars().all(|c| c.is_alphanumeric() || c == '_' || c == '.');
        let tag = if readable { Expr::Tag(tag.into()) } else { Expr::Unset };
        if negated { Expr::Not(Box::new(tag)) } else { tag }
    };
    let option = |option: &str| option.split(',').map(term).reduce(|a, b| Expr::And(Box::new(a), Box::new(b)));
    let options = options.split_whitespace().filter_map(option);
    Some(options.reduce(|a, b| Expr::Or(Box::new(a), Box::new(b))).unwrap_or(Expr::Unset))
}

/// The most `,` and space-separated options a `// +build` line may join for go to read it (go/build/constraint's
/// `maxOldSize`).
const MOST_PLUS_BUILD_OPERATORS: usize = 100;

/// The weakening a constraint line `comment`, on line `line`, is: platform-only or a skip ([`weakenings`]).
fn weakening(comment: &str, line: u32, platform_only: bool) -> Weakening {
    let written = first_line(comment.trim_end());
    let lines = Lines { first: line, last: line };
    let code = model::without_whitespace(comment);
    if platform_only {
        Weakening {
            lines,
            kind: WeakeningKind::PlatformOnly,
            code,
            message: format!("`{written}` added: it turns the file's tests off on the other platforms and toolchains."),
        }
    } else {
        let message = format!("`{written}` added: it turns the file's tests off.");
        Weakening { lines, kind: WeakeningKind::Skip, code, message }
    }
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
#[derive(Clone, Debug)]
enum Expr {
    Tag(String),
    /// A `// +build` term go cannot read as a tag, which it reads as the tag `ignore`, taken as set by no build.
    Unset,
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
            Expr::Unset => true,
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
                tag == os
                    || tag == implied_os(os)
                    || tag == build.arch
                    || (tag == "unix" && UNIX.contains(&os))
                    || tag == build.compiler
                    || (tag == "cgo" && build.cgo)
                    || release_tag(tag)
            }
            Expr::Unset => false,
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
        weakenings(&format!("{comment}\n\npackage calc\n")).first().map(|w| w.kind)
    }

    #[test]
    fn a_file_name_s_suffix_is_read_from_its_name_alone_up_to_its_first_dot() {
        for (path, suffix) in [
            ("pkg/calc_windows_test.go", Some("_windows")),
            ("calc_windows.pb_test.go", Some("_windows")),
            ("calc_darwin_arm64.go", Some("_darwin_arm64")),
            ("calc_x_amd64_test.go", Some("_amd64")),
            ("windows/calc_test.go", None),
            ("calc_windows_x_test.go", None),
        ] {
            assert_eq!(platform_suffix(path.as_bytes()).as_deref(), suffix, "{path}");
        }
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
    fn a_plus_build_term_go_cannot_read_is_a_tag_no_build_sets_and_a_line_too_long_for_go_is_none() {
        for comment in ["// +build linux darwin foo-bar", "// +build !foo-bar", "// +build linux !"] {
            assert_eq!(kind(comment), Some(WeakeningKind::PlatformOnly), "{comment}");
        }
        for comment in ["// +build foo-bar", "// +build linux,", "// +build !", "// +build !!linux", "// +build"] {
            assert_eq!(kind(comment), Some(WeakeningKind::Skip), "{comment}");
        }
        let longest = format!("// +build {}", ["linux"; 101].join(","));
        assert_eq!(kind(&longest), Some(WeakeningKind::PlatformOnly));
        let too_long = format!("// +build {}", ["linux"; 102].join(","));
        assert_eq!(kind(&too_long), None);
    }

    #[test]
    fn a_byte_order_mark_before_the_header_is_passed_over_as_go_does() {
        assert_eq!(kind("\u{feff}//go:build ignore"), Some(WeakeningKind::Skip));
        assert_eq!(kind("\u{feff}// +build linux"), Some(WeakeningKind::PlatformOnly));
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
        for comment in ["// go:build linux", "//go:buildx linux", "// +buildx linux", "// Package x."] {
            assert_eq!(kind(comment), None, "{comment}");
        }
    }
}
