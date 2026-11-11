# Where `gate` differs from gate.py

Up to release 0.6.0 the gate was `gate.py`, a Python program that read Rust through a helper of its own, gate-rust.
From 0.7.0 it is `gate`, one binary written in Rust (`crates/gate`), called the Rust gate below. This file is the
record of where the two judge or word a change differently, for a project that raises its pin across 0.7.0: each
difference that is deliberate, or that comes of reading a language with another parser, with its reason. What is
not listed here the Rust gate does as gate.py did, as far as the gate's tests and the two gates' runs side by side
over this repository's changes showed (gate-standalone/17); a difference found later is a bug, or an open issue in
`TODO.md`. gate.py is at the tag `v0.6.0`.

## Arguments, rules and reports (gate-standalone/12 and 18)

- **`gate` is the program**, where a project ran `python gate.py` with the helper named by `--rust-syntax`. That
  option is gone, and refused as any argument the gate does not know (exit 2): the gate reads Rust itself.
- **`--config` names a gate.toml alone.** gate.py took `--config` for the rules, a `pyproject.toml`, and for the
  settings, a gate.toml, once each. The Rust gate takes the rules of ruff, mypy, pymarkdown and mutmut from the
  `pyproject.toml` beside the gate.toml that judges, and refuses a `--config` named `pyproject.toml` (exit 2). With
  no `pyproject.toml` there the linters run on their defaults and mutmut is not run, where gate.py refused to judge
  ("no rules"): no project needs a `pyproject.toml` (docs/quality-gate.md, "Whose rules judge a change").
- **pymarkdown runs in an empty directory of the gate's own**, on absolute paths. Under gate.py it ran at the
  tree's top level, where it adds the tree's own `[tool.pymarkdown]` or `.pymarkdown` to the rules it is given, so a
  change could turn a rule off though `--config` named `main`'s rules.
- **An option is written whole.** argparse took an unambiguous beginning, `--cov` for `--coverage`, and refused a
  value that starts with `-`; the Rust gate takes neither abbreviation nor offence.
- **A coverage report may be LCOV**, as cargo-llvm-cov's `--lcov` writes it, beside Cobertura; gate.py read
  Cobertura alone.
- **An XML report is read in UTF-8, or in UTF-16 that opens with a byte order mark**, JUnit and Cobertura alike;
  gate.py's `ElementTree` read whatever encoding the report declared (`TODO.md`, "reads an XML report only in
  UTF-8").
- **The quality file's second line names `gate`**, "`gate` judged the working tree at ...", where gate.py's said
  "`gate.py` judged ...", and its rules "by the rules from ...", where gate.py's said "by the rules in ...". Where
  `--config` names a gate.toml outside the tree and no `pyproject.toml` is beside it, it says "by the rules from the
  defaults (there is no `<path>`)", which gate.py never did, refusing to judge; a tree judged by its own gate.toml,
  or by the defaults for want of one, gets no rules clause, as under gate.py. The line is prose, outside the
  report's contract; quality.json's fields are unchanged, `schema_version` 1.
- **A linter's first line**, which a "failed" condition quotes up to 200 characters, counts a byte that is not
  UTF-8, written `\xNN`, as four characters, where gate.py counted one.
- **A git command past its minute** ends "did not finish within 60 seconds", where gate.py's ended with Python's
  "timed out after 60 seconds".

## The parser checks over Rust (gate-standalone/13)

`gate.py` reads Rust through the gate-rust helper, built on syn. The Rust gate reads it with `crates/lang-rust`, on
tree-sitter-rust, into the model of `crates/model`, and runs the checks of `crates/checks` over it
(docs/quality-gate.md, "Reading the source").

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
  front end is part of the gate, with no run, timeout, schema or scratch directory of its own to fail.
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
  A format spec that starts with `=`, as in `f"{x:=^9}"`, it reads as an assignment expression; the gate finds such
  specs and reads them as Python does, but for one inside an f-string nested in a set that is itself in a
  replacement field, where the set is read as a dict or the file as one that does not parse (`TODO.md`, "A set
  inside a replacement field...").
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
  redundant parentheses or trailing comma, and "too deep" or "too long to print" for what it cannot unparse. So a
  finding that quotes a double-quoted string differs in its message, and with it in its fingerprint.
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

- **A scratch directory whose path is not UTF-8** gives "could not copy the base to run the changed Rust tests in:
  PATH is not UTF-8", since nextest's configuration cannot name it. gate.py writes the path with its bytes escaped,
  naming a store nextest then makes elsewhere.
- **An earlier run's junit.xml that cannot be removed** is a not-run finding, "could not remove the report of an
  earlier run", where gate.py lets the `OSError` end the gate.

### surviving-mutants

- **A mutant's outcome the gate cannot read is worded by the Rust gate**: the finding names what the mutant lacks, as
  "no `span` with a start line", where gate.py quotes Python's exception, as `KeyError('file')`. The Rust gate also
  needs every tried mutant's name, where gate.py reads only those of the first few survivors of a function.
- **outcomes.json that is not a list of outcomes** is "PATH holds no list of outcomes", where gate.py quotes the
  `KeyError` or `TypeError`.

## The checks that run Python's tools (gate-standalone/16)

### Which Python

- **The judged project's Python is named, not the one that runs the gate.** gate.py runs pytest, pytest-randomly and
  mutmut through `sys.executable`, so it is started with the project's Python. The Rust gate is no Python program:
  it takes `--python PATH`, else the `python` key at the top of `gate.toml`, else `python3` from `PATH`. A name
  without a slash is found on `PATH`; a path is read from the current directory when `--python` gives it and from the
  tree's top level when `gate.toml` does. gate.py knows neither the flag nor the key, and refuses a `gate.toml` that
  holds the key as it does any key it does not know.

### What the tools and the system say

- **A system error is worded by Rust**: a finding that quotes one ends "No such file or directory (os error 2)" where
  gate.py's ends "[Errno 2] No such file or directory: 'PATH'". So are TOML and JSON errors: the toml crate's
  one-line message and serde_json's, where gate.py quotes `tomllib`'s and `json`'s.
- **A scratch directory is named `gate-<pid>-<n>`** in a finding that quotes a path inside one, where gate.py's is
  `tempfile`'s `gate-<random>`.
- **A pattern over bytes takes `\s` and `\d` as ASCII.** gate.py matches the same patterns over text, where they
  take in Unicode's spaces and digits: a no-break space before a pragma, or a digit of another script in a line
  number a tool printed.

### passes-at-base over Python

- **A path that is not UTF-8 in a report of the imports** makes the report one the gate cannot read, and the check a
  `not-run` finding: the plugin writes such a path as lone surrogates, which serde_json refuses. gate.py reads it.

### surviving-mutants over Python

- **`[tool.mutmut]` is written back as TOML's own inline values**, for the copy mutmut runs in, where gate.py writes
  each value as JSON and says of one JSON cannot hold that it cannot write it. So a date, an inline table, `inf` and
  a string with a character beyond the Basic Multilingual Plane reach mutmut, where gate.py reports the table as one
  it cannot narrow.
- **A `match` statement holds a block**, in the reading of which lines of a function are a block's first; gate.py's
  list of block statements leaves it out.
- **Why a function has a `# pragma: no mutate` is read from the file's own syntax tree**, where gate.py parses the
  function's text again, dedented.
- **Unchanged functions are marked `do_not_mutate` in the file's bytes.** A file whose coding cookie names an
  encoding other than UTF-8, ASCII or Latin-1 is left unmarked, so mutmut mutates all of it; gate.py decodes and
  encodes with any codec Python knows.
- **`do_not_mutate_patterns` are read by Rust's regex crate**, to say why mutmut made no mutant of a function. It
  has no lookaround or backreference: with a pattern that uses one, the finding names every cause there can be
  rather than the patterns, as it does for a pattern that is no regex at all.
- **A directory in the tree that cannot be read stops the copy**, and the check is a `not-run` finding; gate.py's
  `os.walk` skips it.
- **A `.meta` of mutmut's is cut off when serde_json cannot read it**: one holding `NaN` or `Infinity`, or bytes that
  are not UTF-8, counts as a save the stop cut off, where Python's `json` reads it.
- **The budget in a finding is written as Rust writes a float**, `30` or `0.5`, where gate.py's `%g` writes a large
  or small one with an exponent.
- **A `pyproject.toml` that is a tracked symbolic link is replaced in the copy by a file** that holds what it
  links to with the rules' `[tool.mutmut]`; gate.py writes through the link, into the file it names, which may lie
  outside the copy. What it links to is read from the tree's place, before pytest's configuration is looked for in
  the copy; gate.py follows the copy's link from the copy's place, where a relative link out of the tree names
  another file or none, and takes one that names none for an empty file. A link that names no file from the tree
  either, which only a change judged by `--config`'s rules can hold, is a "could not start mutmut" finding that
  says so.
- **A Python that cannot be started is named**: "could not start pytest with the Python `PATH`, ..." and "could not
  start mutmut with the Python `PATH`: ...", where gate.py, which runs both with the Python that runs it, has
  "could not start pytest, ..." and "could not start mutmut: ...", and likewise "coverage.py could not be started
  with the Python `PATH` to read ..." for `--test-contexts`. A blank `--python` is refused as a bad argument.
- **A rules file or `TMPDIR` whose path is not UTF-8** is a "could not start mutmut" finding, since a
  `[tool.mutmut]` setting cannot name it; gate.py writes the path with its bytes escaped.
- **`--test-contexts`: a node id that is not UTF-8** makes what coverage.py's reader printed something the gate
  cannot read, and mutmut runs unnarrowed, with the finding that says so; gate.py reads the lone surrogates.
