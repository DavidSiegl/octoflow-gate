# Where the Rust gate differs from gate.py

The Rust gate (`crates/gate`) judges beside `gate.py` until the cutover (gate-standalone/18), and `cargo xtask
compare` holds the two to the same reports over the parts ported so far. This file lists each difference that is
deliberate, or that comes of reading a language with another parser, with its reason, so that a difference `compare`
shows is either listed here or a bug. Differences still to be decided are open issues in `TODO.md` instead.

## The parser checks over Rust (gate-standalone/13)

`gate.py` reads Rust through the gate-rust helper, built on syn (docs/quality-gate.md, "Reading Rust: the gate-rust
helper"). The Rust gate reads it with `crates/lang-rust`, on tree-sitter-rust, into the model of `crates/model`, and
runs the checks of `crates/checks` over it.

### Reading a file

- **What does not parse.** tree-sitter and syn accept slightly different Rust, and stop at different tokens: an
  `unparsable`, `not-scored` or `not-read` finding can name another line than gate.py's, and says "does not parse as
  Rust (unexpected `...`)" or "(expected `...`)" where gate.py quotes syn's message. tree-sitter is the more lenient
  inside a macro's arguments, such as `Foo { .. }` read as an expression.
- **A file that is not UTF-8** is one that "is not UTF-8", with no line, where gate.py names the helper's error.
- **A symlinked `.rs` file in the working tree is not read**, nor a directory or a special file: a symlink's change is
  the name it points to, and reading a FIFO or a device could block. gate.py hands the helper the path, which follows
  the link.
- **A working-tree file that cannot be read stops the gate** (exit 2), as an unreadable file stops it elsewhere;
  gate.py lists it as a finding of the check.
- **No `not-run` findings.** They report a helper that was not given, is missing, fails or outlives its time; the
  front end is part of the gate. So do the helper's own tests in TestRustFiles not carry over: the run, its timeout,
  its schema and its scratch directory.
- **`cfg(all(test, a::b))`**, with a path of more than one segment beside `test`, is read as not marking test code,
  unlike gate.py's helper.
- **A platform cfg nested deeper than 64 levels** of `all`, `any` and `not` is not read as platform-only, so that no
  attribute can make the reader recurse without bound.
- **The attributes of a test fn** are those tree-sitter gives the fn, so a `#[cfg(miri)]` after a wrapped attribute's
  closing bracket is its own skip there, where gate.py reads whole lines. A literal in an attribute is lexed as Rust
  lexes it, so `r"C:\"` is a whole raw string (`TODO.md`, "A misread literal on an added attribute line...").

### cognitive-complexity

- **Base pairing keeps test code in the pool.** A function among the tests stays in the base pool, judged never but
  paired as any other, so a function moved unchanged into the tests takes its own base before a copy elsewhere does.
  gate.py leaves Rust's test fns out of both sides (`_rust_scored_functions`). The user ruled for the Rust gate's
  way, for every language (gate-standalone/13, 2026-11-07).
- **A macro's arguments** are split at top-level commas and each piece read as an expression, as the helper's
  `FlowWalker::visit_macro` does; tree-sitter gives a token tree where syn gives parsed expressions, so a piece one
  parser accepts and the other does not scores differently.

### test-integrity

- **An unreadable test file's message** is the front end's reason, "..., so its tests were not checked.", where
  gate.py's names the helper: "the gate-rust helper could not read it as Rust at line N: ...".
- **Golden files** are listed when the change edits Rust code under test; gate.py lists them when it edits code in
  any language. The Rust gate reads no Python yet (gate-standalone/15-16), so a change that edits only Python code
  lists them in gate.py alone.

### vague-names

- **Two vague names first bound on one line** are listed by column; gate.py lists them in the helper's order.
- **Case** is folded in ASCII; gate.py folds Unicode. The vague words are ASCII, so no name differs.
