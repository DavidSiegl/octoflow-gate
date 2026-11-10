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
- **A symlinked `.rs` file is not read**, at the base or in the working tree, nor a submodule, a directory or a special
  file: a symlink's change is the name it points to, a submodule's object is a commit, and reading a FIFO or a device
  could block. gate.py hands the helper the working tree's path, which follows the link, reads a base symlink's target
  as Rust, and cannot read a submodule at the base.
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

### vague-names

- **Two vague names first bound on one line** are listed by column; gate.py lists them in the helper's order.
- **Case** is folded in ASCII; gate.py folds Unicode. The vague words are ASCII, so no name differs.

## The parser checks over Python (gate-standalone/15)

`gate.py` reads Python with the standard library: `ast`, `tokenize` and the codecs. The Rust gate reads it with
`crates/lang-python`, on tree-sitter-python, into the same model and checks as Rust.

### Reading a Python file

- **Encodings.** A file is decoded as its byte order mark or coding cookie says when that is UTF-8, ASCII or Latin-1;
  a cookie that names any other encoding makes the file one that "could not be read (cannot be decoded as its coding
  cookie or UTF-8 says: ...)", where gate.py decodes whatever codec Python has and quotes Python's error otherwise.
  The Rust gate takes no table of codecs (spec decision 7).
- **What does not parse.** A file tree-sitter cannot parse "does not parse as Python", at the line of the first token
  it could not place; gate.py quotes `ast`'s `SyntaxError`, on the whole file. tree-sitter-python also reads Python 2,
  so `print x` parses, and it does not yet read a type parameter's default (PEP 696), which Python 3.13 does.
- **A test file is decoded as any other Python**, by its mark or cookie; gate.py's test-integrity reads both sides of
  a test file as UTF-8 alone and calls any other `unreadable`.
- **A base file is read only where git holds a regular file.** gate.py reads a base symlink's target, the text git
  stores for it, as Python. The working tree's rule is gate.py's: only a regular file is read.
- **A working-tree file that cannot be read stops the gate** (exit 2), as for Rust above; gate.py lists it as a finding
  of each check. One that is gone by the time the gate reads it has no working-tree side, where gate.py's
  cognitive-complexity says it "could not be read".
- **A base file git cannot show stops the gate** (exit 2) when it is one the gate reads as Python by its name, or the
  base of a Python file. gate.py's cognitive-complexity then judges the file's functions as new.

### cognitive-complexity over Python

- **A function takes its base only from Python.** gate.py pairs a function over the limit with a base function of
  its name in its file's base whatever the language, which shows only when a change renames `a.py` to `a.rs` or back.

### test-integrity over Python

- **Quoted code is quoted as written.** An assertion appears in a message as the file writes it, its lines joined into
  one, and an expected value and a skip's decorator by their first line; gate.py prints `ast.unparse` of them:
  single-quoted strings, adjacent strings joined, an escape as the character it stands for (`é` for `\xe9`), no
  redundant parentheses or trailing comma, and "too deep" or "too long to print" for what it cannot unparse. `cargo
  xtask compare` compares messages whole, so a finding that quotes a double-quoted string differs in its message alone.
- **Literals are compared as written, but for a string's quotes.** `"a"` and `'a'` are one value, and adjacent strings
  of one prefix are joined, f-strings too; `"\x41"` and `"A"`, `r"\d"` and `"\\d"`, `1_000` and `1000`, `0x10` and `16`
  are different values, where gate.py compares what they evaluate to. Adjacent strings of different prefixes, as `"a"
  f"b"`, are not joined.
- **Redundant parentheses are part of an expression**, so adding or removing them changes an assertion's shape, a
  function's body and a constant's value; `ast` drops them.
- **`unparsable` is reported only for a file whose base held tests**, as for Rust; gate.py reports it for any Python
  test file that does not parse.
- **A `Test*` class with no test methods** that the change removes is not reported, nor is a deleted file that held
  only such classes; gate.py reports the class.
- **Tests of one name in one file** are paired in the order they are written; gate.py keeps only the last.
- **Two constants first read in one assertion** are listed in the order written; gate.py lists them breadth-first
  over the expression's tree.
- **A skip's name written twice on one line** is reported once, at the first.

### Suppressions

- **A Python file that does not parse** has each added line the pattern matches listed, comment or not. gate.py falls
  back so only when `tokenize` fails, which a file that tokenizes but does not parse does not do: there it still
  tells a comment from a string.

## The checks that run Rust's tools (gate-standalone/14)

### Copies of the tree

- **The base is unpacked by the system's `tar`**, where gate.py uses Python's tarfile with its "tar" filter. Both
  refuse a member whose path leads out of the copy, through a link or otherwise, and write a tracked link as it is;
  the Rust gate takes no archive crate (spec decision 7).
- **A copy that cannot be made is worded by the Rust gate**: its findings say "cannot make PATH: ..." or "tar could
  not unpack the base: ...", where gate.py quotes Python's `OSError` or `TarError`.
- **A copied file keeps its permissions but not its times**: the Rust gate copies with `std::fs::copy`, gate.py with
  `shutil.copy2`, which keeps both.
- **A special file in the tree is refused**, "cannot copy PATH: it is no regular file, link or directory", whatever
  its kind. gate.py's `shutil.copy2` differs by kind: it raises `SpecialFileError` on a named pipe and `OSError`
  (ENXIO) on a Unix socket, both ending as the same not-run finding in other words, but it copies a device as a
  regular file holding what reading it gives, so the check runs where the Rust gate's is not run, and on a device
  that never ends, such as `/dev/zero`, it writes without end. Inside a copied directory `shutil.copytree`
  gathers the errors into one `shutil.Error`.

### passes-at-base

- **Only Rust's half runs**, with the whole `--base-run-budget`. gate.py runs Python's changed tests first and gives
  Rust's what is left of the budget; the Rust gate runs no Python tests yet (gate-standalone/16).
- **A scratch directory whose path is not UTF-8** gives "could not copy the base to run the changed Rust tests in:
  PATH is not UTF-8", since nextest's configuration cannot name it. gate.py writes the path with its bytes escaped,
  naming a store nextest then makes elsewhere.
- **An earlier run's junit.xml that cannot be removed** is a not-run finding, "could not remove the report of an
  earlier run", where gate.py lets the `OSError` end the gate.

### surviving-mutants

- **Only Rust's half runs**, with the whole `--mutation-budget`. gate.py gives mutmut half the budget when the change
  edits Python too, and cargo-mutants what is left; the Rust gate runs no mutmut yet (gate-standalone/16).
- **A mutant's outcome the gate cannot read is worded by the Rust gate**: the finding names what the mutant lacks, as
  "no `span` with a start line", where gate.py quotes Python's exception, as `KeyError('file')`. The Rust gate also
  needs every tried mutant's name, where gate.py reads only those of the first few survivors of a function.
- **outcomes.json that is not a list of outcomes** is "PATH holds no list of outcomes", where gate.py quotes the
  `KeyError` or `TypeError`.
