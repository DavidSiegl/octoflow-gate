# Quality gate

What `gate` judges and why: the [gate](#the-gate) itself, its [Rust tests and
coverage](#rust-tests-and-coverage), a Go project's [tests and coverage](#go-tests-and-coverage), how it [reads Rust,
Python and Go](#reading-the-source),
[whose rules judge a change](#whose-rules-judge-a-change), and [the report as a contract](#the-report-as-a-contract)
with the programs that read it back. [README.md](../README.md) says how to
run it in a project.

The gate was written inside [octoflow](https://github.com/DavidSiegl/octoflow-ai), whose orchestrator has an agent,
the Builder, answer the quality file and another, the Reviewer, judge the answer; this document still names them
where it says who a part of the file is for, and cites octoflow's pull requests and measurements where a decision
was made on them.

## The gate

`gate --base REV` judges the change from `REV` to the working tree and writes the quality file:

```bash
gate --base main --out quality.md
```

`gate` is one binary, which a release ships built ([README.md](../README.md), "Running the gate in a project"); it
runs in the tree it judges, and reads the tree's Rust, Python and Go [itself](#reading-the-source). The tools
it runs it finds on `PATH`, but for the judged project's Python, in which that project's pytest, pytest-randomly and
mutmut are installed: `--python PATH` names it, else the `python` key of [the settings](#the-gates-settings), else it
is `python3`.

It first runs the tests, with the commands the project's `gate.toml` lists as `[[test]]`
([running the tests](#running-the-tests)). This repository's lists one, `cargo llvm-cov nextest`, which runs the
workspace's tests and measures their coverage and takes [cargo-llvm-cov and
cargo-nextest](#rust-tests-and-coverage).

It runs seven linters over the files git tracks outside `mutants/` (below), each over the files it reads, and keeps
only the findings on lines the change adds, and the lineless ones on files it touches. Findings on older lines are
counted, never listed and never held against the change, so the gate judges the change rather than the codebase.
The files in the table are each linter's default, which a project's [settings](#the-gates-settings) can replace.

| Linter | Files | Rules |
|---|---|---|
| ruff | `**/*.py` | `[tool.ruff.lint]` in `pyproject.toml` |
| mypy | `[tool.mypy] files`, here `crates/gate/src`; without rules, `**/*.py` | `[tool.mypy]` |
| pymarkdown | `**/*.md` | `[tool.pymarkdown]` |
| shellcheck | `**/*.sh` | its defaults |
| actionlint | `.github/workflows/*.yml`, `*.yaml` | its defaults |
| clippy | `**/*.rs` and `**/Cargo.toml`, in each Cargo workspace, here the one at the top level | the workspace's `[lints]` and `clippy.toml` |
| golangci-lint | `**/*.go` and `**/go.mod`, in each Go module | the module's `.golangci.yml`, `.yaml`, `.toml` or `.json` |

ruff's rules include the coding rules the agents are given as prose that a linter can apply, so that every harness
gets them, every time: a docstring on every public module, class, method and function (`D100`-`D104`, `D106`,
presence only, since the repository's docstrings are prose rather than a convention). A blind
`except Exception` (`BLE001`) is in ruff 0.17's default rules, which `extend-select` adds to and a `select` would
drop; an `except` that does nothing (`S110`, `S112`) and `subprocess.run` without `check` (`PLW1510`) come with the
`S` and `PLW` rules it selects. `D105` and `D107` are not selected: a class's docstring documents its magic methods
and its `__init__`. Which rule to mechanize next is a question for the run metrics: the findings that recur.

clippy runs as `cargo clippy --workspace --all-targets --message-format=json -- --cap-lints warn` in each workspace,
its tests and examples included, and with `--locked` where git tracks the workspace's `Cargo.lock`, so that it judges
the versions the change commits and never rewrites the file. Which lints are on is the workspace's own choice, as
ruff's rules are the project's; the gate caps each at warn, so that a lint at `deny` or `forbid`, which would fail its
crate's build and stop cargo before the crates after it, is a finding like the rest. The cap also lets an
`#[allow(...)]` override a `forbid`, which rustc otherwise refuses, so such an allow silences its lint for the gate;
it is listed under the suppressions added only where its added line holds the allow itself, not a lint added inside an
existing wrapped `#[allow(` list (`TODO.md` has the rest). The top-level `Cargo.toml` sets its `[workspace.lints]`,
which every member takes, to `warn` all the same, since rustc's `unsafe_code` at `deny` would stop `cargo test`. Its
table adds to clippy's defaults the lints `main` passed when it was set, so that clippy found nothing on old lines. A
workspace that still
does not compile is a failed condition naming the compiler's first error in a tracked file, wherever it is, else
cargo's own: a change that deletes a function an old line calls breaks the build on no line it added. A linter
that outlives its timeout is killed with every process it started, such as cargo's rustc.

golangci-lint runs as `golangci-lint run --output.json.path stdout --show-stats=false --max-issues-per-linter 0
--max-same-issues 0 --issues-exit-code 1 --path-mode abs ./...` in the directory of each go.mod among its files, its
tests included, but for a module the go tool leaves out of `./...`, as one under `testdata` or `vendor` (`linted_module`
in `crates/gate/src/golangci.rs`). Which linters it runs, and how, is the module's own choice, the configuration
golangci-lint finds from the module's directory up, as clippy's lints are the workspace's; without one it runs its
standard set (errcheck, govet, ineffassign, staticcheck, unused). The gate asks for every finding, none held back as a
repeat of another, and for exit status 1 on findings, whatever the configuration says, and each is an `error` named by
its linter, as `golangci-lint errcheck`. Code that does not compile is a failed condition naming the type checker's
first error, as clippy's is, since golangci-lint places it at the package clause, on no line the change added. A module
with no Go file of its own, its code all in modules nested in it, has nothing to lint (exit status 5). A run that exits
with any other status but 0 or 1, or prints no JSON report, as for a configuration it cannot read, is a failed condition
quoting what it said.

The gate runs each linter as a process, found on `PATH`. In this repository ruff, mypy and pymarkdownlnt are in uv's
`dev` group, and `scripts/gate` starts the gate with `uv run`, which puts them there; shellcheck, actionlint, cargo
and clippy come from the machine, in Jenkins the agent image, and so do go and golangci-lint.

After the linters come the checks in the table below, in its order, which judge what the change did rather than how
the tree reads. Each either fails the gate or only reports: a failing check's findings are issues,
like the linters', listed as `error`, and a reporting check's are findings to weigh, listed as `warning`, which the
Builder answers and the Reviewer judges without the gate failing. A failing check may weigh some of its rules
instead, as `test-integrity` does for what a spec can ask for. Either kind is listed wherever it is,
not only on added lines, and names the check that raised it. A check that cannot run says so as one of its
findings, so a failing check that cannot run fails the gate.

The gate reads the change without what the tree tracks under `mutants/`, mutmut's state (`Change::from_tree` in
`crates/gate/src/judge.rs`), and every part of it reads that one change: no linter finding, suppression, line end or
coverage counts there, no check selects a test, a function or a changed line there, or counts an edit there as a change
to the code, and a file moved out of it counts as added. A change that adds or edits files there, or moves files into
it, fails the gate with one condition naming them, since a committed `mutants/` is always mutmut's scratch state
committed by accident. Deleting such files is no fault.

| Check | Fails? | Finds |
|---|---|---|
| `test-integrity` | yes, but for removed tests, fewer assertions, changed expected values and Rust and Go tests left out on other platforms | tests the change weakened, in test files (by default `tests/**`, `test_*.py`, `*_test.py`; Rust's below) |
| `cognitive-complexity` | yes | functions the change edits or adds that it takes over a cognitive complexity of 15 |
| `flaky-tests` | no | tests the change adds or edits that pass in one order and fail in another |
| `surviving-mutants` | no | changes to the functions the change edits that no test notices |
| `passes-at-base` | no | tests the change adds or edits that pass without the rest of the change |
| `vague-names` | no | names the change binds that fit anything, such as `data` or `raw_info` |

**Test integrity** compares each test file the change touches, outside `mutants/` as above, with its base, so a
change cannot weaken its tests unseen. Each of these is a finding:

- `removed-test`: a test function, method or class is gone, at its line at the base. A class whose tests all
  went is one finding, and so is a deleted file. A test that reappears with the same body under another name or
  in another file was renamed or moved, not removed. A test removed together with the code it tested is still a
  finding. It is one to weigh: the gate cannot see the spec, so the Builder says why in its report and the
  Reviewer judges;
- `skip`: a `skip`, `skipIf`, `skipUnless`, `skipTest`, `SkipTest`, `xfail` or `expectedFailure` on an added
  line;
- `fewer-assertions`: a test with fewer `assert` statements, `self.assert*` calls and `pytest.raises` calls than at
  the base. It is one to weigh, like `removed-test`: a test the spec reshapes may need fewer;
- `swallowed-error`: a bare `except:`, or an `except Exception:` that does nothing, on an added line;
- `expected-value-changed`: an assertion whose literals changed while the rest of it stayed, in a change that also
  edits code outside the tests, which is Python source as **Passes at base** below counts it, on either side of the
  change: the "update the expected value to match the output" move. The finding names the
  assertion before and after. An expected value held in a constant counts too: when a kept test's assertions name a
  constant of its own file at the base and still name it, bare or as `module.NAME` (the module by its dotted name or a
  module-level import alias of it), and the constant's expression changed other than in layout, or it is no longer a
  constant, that is one finding per constant and test, at the test's first assertion naming it, naming the constant
  and its line. A constant whose name the test binds anywhere, even by a binding that does nothing (`global NAME`, a
  comprehension target, a lambda parameter), is not checked, so such a line hides an edited constant; the Reviewer
  sees it in the diff. Neither is a `module.NAME` whose `module` the test binds. Only a top-level import statement's
  alias counts as the module; one imported under a module-level `if` or `try` does not. A constant is a module-level
  assignment of one name, bound nowhere else in the module's scope, to an expression with no call in it. One the
  change adds, one imported from another module or computed by a call, one reached through a local or a helper, an
  edit to another constant one is built from, and a fixture's return value do not count. It is one to weigh, like
  `removed-test`, since the spec may ask for the new value. A golden file, by default one under a `golden` directory
  inside a `tests` directory, as this repository's `tests/golden/`, is an expected value too, one that a suite can
  usually rewrite to whatever the code now writes: a change that edits or deletes one and also edits code outside the
  tests gets one such finding per file, on the whole file, also to weigh. A golden file added, or renamed unedited,
  is none.

It reads Python, Rust and Go, and golden files whatever their language; another language is another front end beside
`crates/lang-python`, `crates/lang-rust` and `crates/lang-go` ([reading the source](#reading-the-source)).

**Rust tests** it reads as [reading the source](#reading-the-source) says. A Rust test is a `fn` with an
attribute whose path ends in the segment `test`, as `#[test]` and `#[tokio::test]` (not `#[rstest]`), in a `.rs` file of
Rust's test files, by default under a `tests` directory or named `tests.rs`, or anywhere inside an inline module marked
as test code (`#[cfg(test)] mod tests { ... }`) in any other `.rs` file, on either side of the change. A file with such
a module holds both: the check compares the items of its test modules, and the rest of the file is code. Rust code
outside the tests is any other `.rs` file in a directory that holds a `Cargo.toml`, or below one, `build.rs` included,
and a change edits it when it adds, deletes or renames such a file, or adds or removes a line of it outside its
`cfg(test)` modules; so a change to the tests of `lib.rs` alone edits no code. A file that a `#[cfg(test)] mod name;`
elsewhere names is read by its own path alone, so its tests count only under `tests/` or as `tests.rs` (`TODO.md`). A
test is known by its file and the names of the items that hold it, joined by `::`, as `tests::adds`; tests of one name
in one file, as under two `cfg`s for two platforms, are each a test of its own, matched with the other side's in their
order. The findings are Python's, read of Rust:

- `removed-test`: a test gone from its file. Its body is compared by a digest of its tokens, the same whatever the
  layout and comments, so a test moved to another file or module, or renamed, with the same body, is not removed. An
  inline module whose tests all went is one finding, at its `mod` line, as a class is, and so is a deleted file, or one
  that is no longer Rust;
- `skip`: an attribute added that skips a test or turns it off: `#[ignore]`, a `cfg` other than `cfg(test)` itself,
  `cfg(all(test, feature = "slow"))` included, which turns tests off as Python's `skipIf` does, or a `cfg_attr` that
  writes `ignore`, as an argument after its predicate. It counts among a test's attributes; on a line of its own
  inside a test's body, where a statement's `cfg` turns an assertion off; among the outer attributes of an inline
  module holding a test; and as an inner attribute, `#![...]`, on a line of its own inside such a module or anywhere
  in a file under `tests/`. A `cfg` on a helper `fn` is no skip, nor `#[cfg(test)]` on a new test module. Lines are
  read for attributes only where they start with `#`, so an attribute after other code on its line, or one written
  over several lines whose `ignore` is not on its first, is not seen there; a test's own attributes the parser gives
  whole;
- `platform-only`: a `cfg` that would be a `skip` but names only platforms (`unix`, `windows`, `target_os = "…"`,
  `target_family = "…"`, and `any`, `all` and `not` of them, `test` allowed beside them in an outermost `all`) and holds
  on at least one, on a test, a test module or a file of tests, read whole when rustfmt wraps it. It leaves a test out
  only on platforms where what it tests is not built, as a test of a Linux-only system call, so it is one to weigh: the
  Reviewer checks that the code under test is as limited. The platforms are a list of common `target_os` values with
  their families (`TARGET_OSES` in `crates/lang-rust/src/skips.rs`); one it lacks, as a misspelt os, and a predicate
  that holds on none of them, as `all(unix, windows)`, are still a `skip`. So is a platform `cfg` on a line of its own
  inside a test's body, and a line that also holds another skip;
- `fewer-assertions`: a kept test with fewer of the assertion macros (`assert!`,
  `assert_eq!`, `assert_ne!`, their `debug_` forms, and `panic!`) than at the base. It is one to weigh, as Python's;
- `swallowed-error`: on an added line, a `#[should_panic]` without `expected` on a test, which any panic passes, a
  failed assertion's included; or, in any `fn` among the tests, a value thrown away (`let _ = expr;`, `_ = expr;`,
  `expr.ok();`) whose expression holds one of that `fn`'s assertion macros or a call of
  `catch_unwind`, which throws away the panic or the `Err` that would fail the test. A parser knows no types, so a
  `Result` thrown away that holds neither, as `let _ = check();` or `check().ok();`, is not seen, since that is
  also how a test's cleanup ignores a failure, nor a `catch_unwind` whose result is bound to a name and never read;
- `expected-value-changed`: a kept test's assertion, its macro and its shape the same, whose literals changed,
  the assertions of the two sides aligned by macro and shape, in a change that also edits Rust code outside the
  tests, on either side. Constants are not followed, as Python's are: an expected value held in a `const` that the
  change edits is not seen. A golden file changed with Rust code is one such finding, as above;
- `unparsable`: a test file that does not parse in the working tree while its base held tests, at the line of the
  first token the parser could not place, failing the gate: those tests cannot be compared. One whose base held none
  has none to lose, as a fixture under `tests/` that is not Rust on purpose, and is not judged; nor is a base that
  does not parse. A Python test file that does not parse is judged the same way.

**Go tests** it reads as [reading the source](#reading-the-source) says, in a `_test.go` file of a Go module, which is
test code whole: the go tool fixes which files are tests, so the settings have no key for them. Go code outside the
tests is any other `.go` file of a module outside a `testdata` directory, and a change that touches one edits it, also
where the gate does not judge it: a vendored, generated or symlinked file is code the tests run (`GoChange::is_code` in
`crates/gate/src/parser_checks.rs`). A test is a function `TestX(t *testing.T)`, `BenchmarkX(b *testing.B)` or
`FuzzX(f *testing.F)`, as `go test` finds them, and a subtest, `t.Run("name", func(t *testing.T) {...})`, named by its
string literal after the test holding it, as `TestAdd/negative`, with `_` for a space as `go test` writes it. A subtest
named by any other expression, as a table-driven test's `t.Run(tc.name, ...)`, is part of the test holding it. An
example, `ExampleX()` with its `// Output:` comment, is no test, though `go test` runs it and checks its output, so
removing one or changing its output is not seen (`TODO.md`). The findings are Python's, read of Go:

- `removed-test`: a test or subtest gone from its file, its body compared by a digest of its tokens as Rust's is; a
  deleted file is one finding. Go has no item that holds tests, so there is no group finding;
- `skip`: a `t.Skip`, `t.Skipf` or `t.SkipNow` on an added line, called on a parameter of type `*testing.T`, `*B`,
  `*F` or `testing.TB` of any function or `func` literal of the file, a helper's included; and a `//go:build` line
  added above the file's `package` clause, which builds the file's tests only where its expression holds, as Rust's
  `cfg` does;
- `platform-only`: such a `//go:build` line whose expression names no custom tag, only platforms (`GOOS` and `GOARCH`
  values and `unix`), release tags (`go1.21`, written as go writes them) and the toolchain's tags (`cgo`, `gc`,
  `gccgo`), with `!`, `&&`, `||` and parentheses, and holds on at least one platform and toolchain, as
  `//go:build linux || darwin` or `//go:build go1.21`. A release tag, `go1.1` up to the newest Go release the gate
  knows, `go1.27` (`NEWEST_RELEASE` in `crates/lang-go/src/constraint.rs`, raised with each Go release), is taken as
  set, as on the newest toolchain, where the tests that matter run; a `go1.N` above it, as `go1.99`, is set by no
  toolchain and is a custom tag, so until the constant is raised a test file naming a new release's tag is a `skip`.
  A `platform-only` finding is one to weigh, as Rust's is; a custom tag such as `integration` or `ignore` beside
  those names makes the line a `skip`, as does an expression that holds nowhere, as `cgo && !cgo` or `!go1.21`;
- `fewer-assertions`: a kept test with fewer assertions than at the base. An assertion is a call, in the test's own
  body or a `func` literal in it, of testify's `assert` or `require` packages (`assert.Equal(t, 3, got)`, under the
  names the file imports them by; `assert.New` is none, and a package imported with `.` is not followed, so its
  unqualified `Equal(t, ...)` is none), or of `Error`, `Errorf`, `Fatal`, `Fatalf`, `Fail` or `FailNow` on a testing
  parameter as above. Such a call that is a statement of an `if`'s block makes that `if` one assertion, however many
  it holds, since the `if` holds the expected value: `if got := Add(1, 2); got != 3 { t.Errorf(...) }` is one. A
  helper's calls are not followed into the tests that call it: `check(t, Add(1, 2), 3)`, where `check` holds the
  `t.Errorf`, is no assertion of the test, so a test that asserts only through helpers has none to lose (`TODO.md`);
- `swallowed-error`: on an added line anywhere in the file, `_ = err`, or an `if err != nil {}` whose block holds no
  statement, `err` being a name Go gives an error by habit: `err`, `err` then a capital or a digit, or a name ending
  in `Err`. A parser knows no types, so an error thrown away under another name, or as `v, _ := f()`, is not seen;
- `expected-value-changed`: as Rust's, an assertion whose literals (numbers, runes and strings) changed while its shape
  stayed, an `if`'s initializer and condition included, in a change that also edits Go code outside the tests.
  Constants and a table-driven test's table of cases are not followed, so an expected value held there is not seen;
- `unparsable`: as Rust's.

Go's tests run through a `[[test]]` command, as [Go tests and coverage](#go-tests-and-coverage) says,
golangci-lint lints Go as the table above says, and the surviving-mutants check mutation-tests the functions a change
edits with gremlins (**Go's mutants**, below). The flaky-tests and passes-at-base checks do not judge Go (`TODO.md`).

**Cognitive complexity** scores the functions the change edits, so that a change cannot pile branches into the function
it is editing rather than split it. A function counts when the change adds a line to it, decorators included, in a
Python file, a `.py` file or a script whose `#!` line names python, outside the files the settings exempt, by default
`tests/`: any function that no other function holds, a method of a nested class and a function under a module-level `if`
or `try` included. A function nested in another is part of that one. Files are decoded by their byte order mark or
coding cookie, as [reading the source](#reading-the-source) says. Its score is G. Ann Campbell's [cognitive
complexity](https://www.sonarsource.com/docs/CognitiveComplexity.pdf), which SonarQube's rule S3776 measured: a point
for each `if`, `elif`, `else`, ternary, loop, `except`, `match`, sequence of like boolean operators and recursion, and
one more for each level of nesting on those that nest. Recursion is a function that calls itself, or one on a cycle of
calls through other functions of its module, each of which then scores the point. Calls are followed by name, as
Python's scopes resolve them, so a call to a parameter or a local of the same name is not one to the module's function;
not through an alias, nor into another module.

Rust is judged the same way, with the same limit, base and findings. A function is a `fn` with a body that no other `fn`
holds, in an `impl`, a `trait` or an inline module or not, named by them as `checks::Workflow<T>::poll`; a `fn` or
closure inside it is part of it. Its lines start at its first outer attribute, doc comments included. Rust's tests are
exempt as Python's are, where the spec of the Rust checks places them: by default every file in a directory named
`tests`, at any depth, or named `tests.rs`, and every `fn` in an inline module marked `#[cfg(test)]`. Nothing else is: a
`#[test]` or `#[cfg(test)]` `fn` outside those, and a file that a `#[cfg(test)] mod name;` takes in, are judged
(`TODO.md`). Each `if`, `else if`, `else`, `if let`, `match`, `loop`, `while`, `while let`, `for` and `let ... else`
scores as Python's branches do; a `match` arm adds nothing, as a `switch`'s case does not in the paper; a run of `&&`
and `||` is a sequence of boolean operators, a labelled `break` or `continue` a point, and `?` nothing. Recursion
follows calls by path within the file: `f(...)`, `self.f(...)`, `Self::f(...)` and `Type::f(...)`, not `crate::f(...)`
or a module's `m::f(...)`.

Go is judged the same way too, in the `.go` files of a Go module but its `_test.go` files, which are tests. A function
is a function or method with a body, a method named by its receiver's type as `Stack.Push`; a `func` literal inside it
is part of it and nests its body as a closure does. Its lines start at its doc comment. Each `if`, `else if`, `else`,
`for` of any form, expression `switch`, type switch and `select` scores as Python's branches do, a `case` adding
nothing; a run of `&&` and `||` is a sequence of boolean operators; a labelled `break` or `continue`, and a `goto`, a
point. Recursion follows calls by name within the file: `f(...)` and `f[T](...)`, and `r.m(...)` on the method's own
receiver `r` to a method of its type; not `pkg.F(...)`, nor a method on any other variable.

`Flow` in `crates/model` says how the paper's rules score, and each front end which of its language's constructs
is which. Each of these is a finding:

- `over-limit`: a function over 15, at its `def` line, when it is new or the change raised its score, as
  "`_poll`: cognitive complexity 19, 14 at the base; the limit is 15". A function already over 15 that the change
  leaves no worse is not a finding, since the gate judges the change. Its base is a function of the base, and each
  base function is the base of one function at most:
  - first, one with the same body, since an unchanged function is that function whatever its name: of its name in
    its file's base, then of its name in the base of another file of its language that the change touches, a file it
    deletes included, which counts as Python by its `.py` or its `#!` line at the base, then of any name in its file's
    base, then elsewhere. So a function moved to another module, renamed or made a method, but otherwise
    unchanged, keeps its score, and a new function given its old name is new. Its score itself can rise with the
    move: two functions that call each other from two modules score no recursion, and moved into one they each
    score its point. Of a function moved unchanged and a
    copy of it, the one that kept the name takes it, and a function moved unchanged into a file the check does not
    judge, such as one under `tests/`, takes its own before
    any copy can, whatever the copy's name. A function takes the base of a function in another file only when that
    one's body is gone from it, so an unchanged copy takes nothing from an original still in its place, under its
    name or another; an original edited in place is gone by its body, which is the cost below;
  - failing that, the highest-scoring function of its name left in its file's base, under the file's old name,
    whatever that name, when the change renamed it. So with a name defined more than once, as a property's getter and
    setter or `@singledispatch` registrations named `_` are, adding or removing one never makes a function the change
    left no worse a finding; the cost is that a rise below a higher-scoring namesake goes unseen;
  - failing that, the function is new: a copy whose original is still in place, a function moved and edited, and a
    function whose base is under `tests/`, which was never held to the limit.

  Bodies pairing first has a cost: a copy of a function whose original the change edits keeps the old score and
  passes, as if it were the original renamed and the edited one new, even when the edit takes the original below the
  limit, and wherever the copy is, in a file the check does not judge too. `TODO.md` holds it.
- `not-scored`: a file the change adds lines to that could not be decoded or parsed, at the line the parser stopped
  at. Like any finding of the check, it fails the gate: a check that cannot do its work says so rather than pass. A
  base that cannot be decoded or parsed is not a finding: its file's functions are judged as new, as `test-integrity`
  takes a base that does not parse to have no tests.

ruff's `C90` stays beside it. It counts branches without their nesting, so it misses a deeply nested function that
cognitive complexity catches, but it still catches a flat function of many branches, which cognitive complexity scores
lower. It reports at the `def` line, and the gate keeps a linter's finding only on a line the change adds: when a change
grows an existing function, only this check reports it. A function whose `def` line the change adds, such as a new
one, and that both flag, is two findings at its `def`.

**Flaky tests** reruns the tests the change adds or edits, so a test that passed the build's one run by luck of
the order is caught. A test counts when the change adds or removes a line of it in a test file outside `mutants/`, as
above, from the line after the statement before it to its last: its decorators and the comments above its `def`
count, since a decorator replaced or removed changes the test, and so do lines removed right below it. pytest
decides which of those are tests. It runs them five times, alone, under pytest-randomly with seeds 1 to 5
(`-p randomly -p no:cacheprovider --randomly-seed=N`), each run in the judged project's Python (`python -m pytest`
with the Python that `--python` or the settings' `python` names, whatever `pytest` is on `PATH`) and within 30
seconds. They run in
a temporary copy of the files git tracks, so a report the project's `addopts` write, such as `coverage.xml`, lands in
the copy and the build's own stays as the gate reads it; a test that needs an untracked file, such as a build
product, does not find it there. Each changed test file takes the configuration pytest would find for it: the first
in its own directory, else in the nearest one above it, up to the copy's top and never above it, in pytest's order
within a directory (`pytest.toml`, `.pytest.toml`, `pytest.ini`, `.pytest.ini`, then `pyproject.toml`, `tox.ini` and
`setup.cfg` with a pytest table or section). The tests are rerun once for each configuration found, given with `-c`
and with `--rootdir` its directory, or with an empty configuration for files under none; pytest runs from the copy's
top, and `--confcutdir` is the copy. So a `tests/pytest.ini` or `pkg/pyproject.toml` configures the tests under it,
and no `pytest.ini` or `conftest.py` that another account put above the temporary directory is read. A group that
cannot run is a finding about its tests alone:

- `mixed-results`: a test that failed under some seeds and passed under the others, at its `def`, naming the seeds
  of each. It depends on the order of the tests, on state another test leaves, or on randomness pytest-randomly
  seeds. An xfail counts as an outcome of its own, so a test that xfails under every seed is consistent, and one
  that xfails under some seeds and passes or xpasses under others is mixed. A run that skips a test, or leaves it
  out of its report (an `addopts` with `--maxfail`, say), neither passed nor failed it;
- `failed-every-run`: a test that failed under every seed that judged it, at least two, in the copy. It is not
  flaky. If the build failed it too, the failing tests list it; if the build passed it, it needs something the copy
  lacks, such as git's metadata, an untracked or ignored file or a build product, and the reruns could not judge it.
  The gate does not compare the two, so the finding says both;
- `not-rerun`: the reruns could not judge the tests: pytest could not start, took longer than 30 seconds, ended in a
  usage or collection error, or collected tests without listing their ids; a changed test file could not be read or
  parsed; or the temporary copy could not be made. After a run that went wrong, the finding says which earlier runs
  were judged, and the reruns stop there. A test that is not mixed and that fewer than two runs passed or failed is
  one of these too, at its `def`, once any run has told something. pytest runs at `--verbosity=-1`, which a `-v`
  or `-q` in the project's `addopts` or `PYTEST_ADDOPTS` does not change, nor its `verbosity_test_cases`;
- `not-removed`: the copy could not be removed afterwards; the reruns' results stand.

A change to a helper or fixture outside the tests' `def`s reruns nothing. The check adds at most six pytest runs of
30 seconds to the gate, the collection included, and only when the change touches a test. pytest-randomly must be
installed in the project's Python, so every pytest run there shuffles the tests too, printing the seed it used;
`-p no:randomly` turns it off, and `--randomly-seed=N` repeats an order.

**Surviving mutants** mutation-tests the functions the change edits, so that code its tests run without checking what
it does is found; coverage counts such code as tested. A function counts when the change adds lines to it or removes
lines from it, decorators included, since a deleted guard clause needs a test as much as an added one: a top-level
function, or a method of a top-level class, in a Python file outside the test files and, as above, `mutants/`.
mutmut, installed in the project's Python, makes small changes to each, one at a time - an operator
swapped, a number or a string altered, an argument dropped - and runs each of these mutants against the tests that
call the function:

- `survived`: a function with mutants no test failed on, at its `def`: how many of its mutants survived out of how many
  were tried, and what the first three change, each as a line before and after. No test checks what those lines do;
- `untested`: a changed function that no test calls, at its `def`, with how many mutants it has; under
  `--test-contexts`, when no test of the selection reaches any changed function, each is one without that number, and
  mutmut is not started;
- `budget-reached`: mutmut ran out of time, as "budget reached after N of M mutants", N counting the mutants it tried
  against a test, as the `survived` counts do. What the N showed is listed; the others may survive too. It also names,
  with their files, the changed functions in modules mutmut had not reached when it was stopped: it saves a module's
  mutants, none or more, in `mutants/<path>.meta` once it has made them all, and theirs had none;
- `not-mutated`: a changed function mutmut does not mutate, at its `def`, with the reason: its file is outside
  `[tool.mutmut]`'s `source_paths`, a script with no `.py`, which mutmut never reads, or left out by `only_mutate` or
  `do_not_mutate`; it is decorated, other than with
  `@staticmethod` or `@classmethod` alone; or it is `__new__`, `__getattribute__` or `__setattr__`. And a function
  mutmut was given but made no mutant of, with the cause where the gate can tell it, a `# pragma: no mutate block` on
  the line its header ends on, the one form of mutmut's pragmas the gate recognises and names as the cause, or
  `do_not_mutate_patterns` matching each statement in it, else every cause there can be: it has nothing mutmut
  mutates, `do_not_mutate_patterns` or `mutate_only_covered_lines` leave its lines out, a `# pragma: no mutate` keeps
  them out, or the gate could not read mutmut's names for its mutants. A block pragma on a line of its own above the
  `def` or atop its body, one on its class, and a `start`/`end` pair around it keep a function out too, and get that
  list;
- `not-run`: mutmut could not start, failed, as it does when a test fails, or left results the gate could not read; the
  rules name no `source_paths`, or give a key the gate reads a value that is no list of strings; there are no rules,
  no `pyproject.toml` beside the settings ([whose rules judge a change](#whose-rules-judge-a-change)); the directory
  to copy the tree into could not be made; or a changed file could not be read or parsed;
- `not-removed`: the copy mutmut ran in could not be removed afterwards, named so that it can be; the findings stand;
- `not-narrowed`: `--test-contexts` was given, but its file could not tell which tests reach the changed functions, so
  mutmut was given its whole test selection, with the reason (see below). The other findings stand.

mutmut runs in the project's Python, on a copy of the files git tracks, so the working tree stays as it was; its state,
`mutants/`, goes into the copy too. Its own selector, mutant names such as `pkg.x_parse__mutmut_*`, picks which mutants
run only after mutmut has made a mutant of every function, which took minutes for the Python engine. So in the copy,
each function and class the change does not touch gets mutmut's `# pragma: no mutate block` at the end of its header,
and mutmut skips it. The rules' `[tool.mutmut]` says what can be mutated (`source_paths`, `only_mutate`,
`do_not_mutate`), which tests run (`pytest_add_cli_args_test_selection`), what the tests read beside the code
(`also_copy`), and which lines are left alone (`do_not_mutate_patterns`); see [whose rules judge a
change](#whose-rules-judge-a-change). Python files are decoded by a byte order mark or a coding cookie, as [reading the
source](#reading-the-source) says. mutmut runs pytest in the copy's `mutants/`, where it puts the mutated code.
Its runs get, with `-c`, the configuration of the copy's top directory, in the order the reruns look in a directory: the
project's own, which `also_copy` puts in `mutants/` so that paths in it, such as `pythonpath`, name the mutated code, or
an empty one; and `--confcutdir` is `mutants/`. Both are added to the table's `pytest_add_cli_args`, so these runs too
read no `pytest.ini` or `conftest.py` from above the copy. Where the project's Python has no mutmut, or the settings
have no `pyproject.toml` beside them, a change to a function gets a `not-run` finding.

**Which tests mutmut runs.** Before it tries a mutant, mutmut runs its whole test selection three times: to find which
tests call each function, to check that they pass, and once more with `-x` until the first test that calls a mutated
function. Over a large suite, those runs alone can take longer than the budget. With
`--test-contexts COVERAGE`, the gate narrows `[tool.mutmut]`'s selection first, to the tests that both:

- the coverage.py data file `COVERAGE` records running a line of a mutated function's body, by their pytest node ids;
- pytest collects in the copy, run with `--collect-only` and the selection and `pytest_add_cli_args` that mutmut's own
  runs get, so pytest itself applies their paths, `--ignore`, `--deselect` and the rest.

A project's pytest `[[test]]` command writes the file, by default `.coverage`, when it runs with pytest-cov's
`--cov-context=test`, and whoever starts the gate passes it.
mutmut then runs those tests three times. For PR #113's change, 253 mutants of 7 functions of the Python engine, that is
312 tests of 1351, and mutmut took 77 seconds instead of 709 on the build host, with the same verdicts, mutant by
mutant. The cost grows with the tests that reach the change, not with the suite: a change to a function nearly every
test calls still costs about as much as before. The collection runs in the gate's copy of the tree, which holds every
tracked file, where mutmut's own copy holds `source_paths`, `also_copy` and `tests/`; a selection that reaches
outside those is collected wider than mutmut runs it.

The file must hold the run's contexts as pytest-cov writes them (`<node id>|run`, `|setup`, `|teardown`), recorded by
coverage's C tracer: `[tool.coverage.run] core = "ctrace"` in `pyproject.toml`. Under Python 3.14, coverage's default
core, `sys.monitoring`, records each line for only the first few tests to run it, so mutmut would miss tests that kill
its mutants and report them as survivors. Any of these makes a `not-narrowed` finding, and mutmut runs the whole
selection: the file cannot be read, holds no test's context, or does not measure a changed function's file; no line in
it is recorded for at least a tenth of the tests it records, as that core writes it; or pytest's collection of the
selection fails or does not finish. Without `--test-contexts`, as in a local run, mutmut runs the whole selection.
Tests that reach a function only in a subprocess count neither in mutmut's own record nor in the file.

`--mutation-budget SECONDS`, else the settings' `budgets.mutation`, 600 by default, bounds mutmut's run. Stopping it
takes at most 15 seconds more: SIGINT, 10 seconds to exit, a kill of its process group, and 5 seconds to read what it
printed. Listing its results and showing the survivors then take at most 60 seconds, each call's stop included. Before
mutmut starts, reading `--test-contexts` takes at most 35, and collecting the selection 35 more. So the check takes at
most the budget and 145 seconds, and the few it takes to copy the tree. With the flaky-tests check's three minutes,
passes-at-base's two and the test run, a quality build stays inside the orchestrator's `QUALITY_TIMEOUT` of 1200 seconds
when Jenkins starts it without a wait in the queue.

A table can keep mutmut off texts that no test reads word for word, such as argparse's help and descriptions:
`do_not_mutate_patterns = ['\bhelp=', '\bdescription=']`. mutmut matches each pattern against each source line and
skips every expression that starts on a matching line, with all it holds, so a text's later lines are skipped too, and
so is a whole `add_argument(...)` or `add_parser(...)` call that starts on its `help=` or `description=` line. A change
to such a text alone makes no mutant. Flag names that start a line of their own are still mutated.

**Rust's mutants.** After mutmut, with what is left of the one `--mutation-budget`, the check mutation-tests the Rust a
change adds lines to outside the tests (a `.rs` file of a Cargo package, as for test integrity) with
[cargo-mutants](https://mutants.rs/). The languages share the one budget in turn, Python, Rust, then Go (below): each
has an equal share of what those before it left, among itself and the languages after it that have something to
mutate, so it also takes whatever an earlier one left unused. With Python and Rust to mutate, mutmut gets half the
budget and cargo-mutants the rest; with Go as well, mutmut a third, cargo-mutants half of what is left and gremlins the
rest; a change with no Python functions to mutate gives cargo-mutants all of it, or half with Go to mutate. A
`budget-reached` finding names the seconds of the share its tool was stopped at.
cargo-mutants chooses what to mutate itself: it runs `--in-diff` with the change's diff of the workspace's `.rs` files,
which mutates only what the added lines hold, `--in-place` in the gate's copy of the files git tracks, once per Cargo
workspace (that of the nearest `Cargo.toml` above the file, as `cargo locate-project` tells it), with `--test-tool
nextest` and `--timeout 60` for each mutant's tests. Its outcomes, from `mutants.out/outcomes.json`, count as mutmut's
do: a missed mutant survived; a caught one, and a timeout, did not; an unviable one, which does not build, was not
tried. Added lines with nothing to mutate, such as a `use` or a `const`, give nothing. The findings are mutmut's kinds:

- `survived`: a function with missed mutants, at the line its `fn` starts, with how many of its tried mutants survived
  and the first three, each by its line and cargo-mutants' description, such as `replace > with == in unchecked`;
- `budget-reached`: the budget ran out, before cargo-mutants started (it needs the 15 seconds of its stop, as mutmut
  does) or while it ran, with how many mutants it had tried; the workspaces after it are `not-run`;
- `not-run`: cargo-mutants could not start, wrote no outcomes the gate can read, or found the unmutated baseline not
  passing, as for tests that fail or code that does not build in the copy; or the copy could not be made;
- `not-mutated`: a changed `.rs` file with no `Cargo.toml` above it, or one whose workspace cargo cannot tell;
- `not-removed`: the copy could not be removed afterwards; the findings stand.

cargo-mutants' own skip attribute, `#[mutants::skip]`, also inside a `cfg_attr`, wrapped or not, is a suppression: the
quality file lists the change's, as it lists an `#[allow(...)]`. The copy has no `target/`, so each run builds the
workspace and its dependencies anew within the budget (see `TODO.md`).

**Go's mutants.** After cargo-mutants, with what is left of the budget, shared as for Rust's, the check mutation-tests
the Go functions a change edits with [gremlins](https://gremlins.dev/): a function or method, from its `func` line to
its closing brace, that the change adds a line to or removes lines from, as Python's are chosen, in a `.go` file of a Go
module that is not a `_test.go` file nor in a `testdata` directory, and that the gate reads ([reading the
source](#reading-the-source)), so not a vendored, generated or symlinked one; but not one whose file's base has a
function of its name with the same declaration, its receiver, parameters, results and body, whatever its comments and
layout, as an edit to its comments or layout alone changes nothing to mutate; a signature edited alone is an edit.
gremlins runs as `gremlins unleash --silent=false --output-statuses lctkvsr --threshold-efficacy 0 --threshold-mcover 0
--timeout-coefficient 10 --diff <base>`, with `-count=1` added to `GOFLAGS`, in the directory of the go.mod nearest
above the functions' files, once per module, in the gate's copy of the files git tracks. It first runs the module's
tests for their coverage, then each mutant against the tests of its package, and mutates only what it reads as changed
from `git diff --merge-base <base>`: the gate puts a `git` first on its `PATH` that answers that one command with the
edited functions' lines, and passes any other to git, so gremlins mutates the edited functions whole, though the copy
has no repository. The flags pin what a module's `.gremlins.yaml` could otherwise change under the gate: every mutant's
line printed, no threshold's exit status, and a timeout for each mutant's tests of ten times what gremlins' first run of
the module's tests took, a run the test cache cannot shorten to milliseconds, which would time out every mutant and
count it caught. Which mutators run stays the module's choice. gremlins writes its report only when it finishes, so the
gate reads the line it prints for each mutant, which a run the budget stopped has printed for those it tried. A mutant
that LIVED survived; a KILLED one, and one TIMED OUT, did not; one NOT COVERED is on a line no test runs; a NOT VIABLE
one, which does not build, was not tried. The findings are mutmut's kinds:

- `survived`: a function with mutants that lived, at its `func` line, with how many of its tried mutants survived and
  the first three, each by its line, column and gremlins' kind, such as `line 13, column 11: CONDITIONALS_BOUNDARY`;
- `untested`: a function with mutants on lines no test runs, with how many;
- `budget-reached`: the budget ran out, before gremlins started (it needs the 15 seconds of its stop, as mutmut does)
  or while it ran, with how many mutants it had tried; the modules after it are `not-run`;
- `not-run`: gremlins could not start, or failed, as it does when a test of the module fails, quoting its `ERROR:`
  line; a changed file could not be parsed; or the copy could not be made;
- `not-mutated`: a changed function whose file lies under no go.mod in the copy;
- `not-removed`: the copy could not be removed afterwards; the findings stand.

**Passes at base** runs the tests the change adds or edits, picked as flaky-tests picks them, against the base's code,
since a test that passes without the change cannot be what shows the change works: coverage counts the lines it runs,
surviving-mutants sees only the functions the change edits, and test integrity only tests getting weaker. For a bug
fix, a test that fails at the base is the proof the bug was reproduced. The check runs only when the change edits,
adds or deletes Python source outside the tests: a `.py` file, or a script whose `#!` names python in the working
tree or at the base, that is not a test file (as for test integrity) or a file that goes with the tests, by default a
`conftest.py` or one under a `tests` directory.
A change to the tests alone, or to the tests and the docs, has no "without the change" a test could see. The base
is the files git tracked at `--base`, as `git archive` writes them, with every one of the change's files that belong
with the tests (those three kinds) taken from the working tree, or removed where the change removed it, so the
changed tests keep their fixtures and data. A submodule
holds the files of the commit the base records for it, from its own repository in the working tree; one that is not
initialised there, or lacks that commit, is left empty, and a `not-run` finding names it. pytest, in the project's
Python, with the configurations flaky-tests' reruns get, found the same way in the base, once for each, and the
copy's own `TMPDIR`, first collects the changed test files with `--continue-on-collection-errors`, then runs those of
the changed tests it collected, all within the one time limit:

- `passes-at-base`: a test that passed at the base, each of its parameter sets that pytest did not skip included, at
  its `def`. A test that fails or errors there tests the change and gives nothing; so does one pytest does not collect
  there: a test whose file cannot be imported at the base, or that lies under a `conftest.py` that fails to load there
  through a file the change edits, the `conftest.py` itself or a helper it imports, whatever it raises, typically
  because it reaches for what the change adds (such a `conftest.py` hides only the tests under it, and the others are
  collected in one more run), and a `test_*` method of a class pytest does not take for a test class. A refactor that
  keeps behaviour gives these findings rightly: the Builder says it is one, and the Reviewer judges;
- `budget-reached`: `--base-run-budget SECONDS`, else the settings' `budgets.base-run`, 120 by default, shared by the
  collection and the run, ran out; the finding says when, and how many of its test cases the run had run. pytest runs in
  a session of its own, and is killed with every process it started;
- `not-run`: pytest could not start, stopped before it collected (as for a plugin a `conftest.py` names that cannot be
  imported), failed to load a `conftest.py` through no file the change edits, as the base itself fails there, stopped
  with another usage error, or wrote no readable report; the run imported the project's code from the working tree
  (below); a submodule was left empty (above); a changed test file could not be read or parsed; or the copy could not
  be made;
- `not-removed`: the copy could not be removed afterwards; the findings stand.

The base's code is what the copy's import roots, its top and its `src/` when it has one, and its configuration, such
as `[tool.pytest.ini_options] pythonpath`, put on the path. The roots go on `PYTHONPATH`, ahead of site-packages and of
what an editable install puts there, so a `src/` layout installed editable into the project's Python is judged at
its base. A project whose code the run still imports from the working tree cannot be judged: one installed editable
from a directory other than those roots, or through an import hook placed ahead of Python's own path search. The run
writes where each module it imported came from, and one that took a project file of the working tree's, whether for a
changed test, a `conftest.py` or a helper, is one `not-run` finding naming the packages, with no test judged.

Rust's tests are judged the same way after Python's, with what is left of the one `--base-run-budget`, when the change
edits Rust code outside the tests: a `.rs` file of a Cargo package that is not among Rust's tests (as for test
integrity), edited outside its inline `#[cfg(test)]` modules, added, deleted or renamed. The changed tests are the
tests, read as test integrity reads them, whose lines, attributes included, the change adds to or removes from. They run
in a copy of the base made as above, with these files taken from the working tree: Rust's test files, the files that go
with the tests (by default any file under a `tests` directory), and a `.rs` file the change edits only inside its
`#[cfg(test)]` modules. A test in a `#[cfg(test)]` module of a file the change also edits outside it cannot be separated
from the code it tests, and is a `not-run` finding at its `fn`. Each Cargo target the changed tests are in is built and
run on its own, with `cargo nextest run --locked --no-fail-fast`, filtered to its tests (`-E 'test(=path::name)'`), in
the copy's package directory, with a target directory and a nextest store outside the copy and the gate's own nextest
configuration in place of the copy's. The target is found by Cargo's default places: the package is the nearest
`Cargo.toml` above the file; `tests/x.rs`, and a file under a `tests/x/` that holds a `main.rs`, are the integration
test `x`; `src/bin/x.rs` and a file under `src/bin/x/` likewise the binary `x`; `src/main.rs` the package's binary; any
other file under `src/` a module of the library where `src/lib.rs` exists, else of the binaries. A file elsewhere, as a
shared `tests/common/mod.rs`, is a `not-run` finding. A target whose tests do not compile at the base (rustc's `could
not compile`) counts as failing there, which is what the check expects of a test that needs the change, and gives no
finding: so a change that adds a function and a test file calling it is judged as one whose tests fail at the base. Any
other cargo error, as a lock file `--locked` refuses, is a `not-run` finding. The findings are those above, `cargo
nextest` in place of pytest; a test passes when every case nextest reports under its name passed, and a test it did not
report, as one a `cfg` leaves out at the base, gives nothing. A test edited by removing its first attribute, as an
`#[ignore]`, counts as edited. The target directory is new for every run, so each run compiles the package's
dependencies anew, within the budget (see `TODO.md`).

**Vague names** lists the names a change binds that tell a reader nothing about what they hold: `data`, `info`, `value`,
`status`, `handle` and `manager` (`VAGUE_WORDS` in `crates/checks`), or a name ending in one after an underscore, such
as `raw_info`, in any case. A name counts where a line the change adds to a Python file, a `.py` file or a script whose
`#!` line names python, binds it: an assignment to a name or to an attribute of `self` or `cls`, a parameter, a `def` or
`class`, a `for` or comprehension target, a `with`'s or an `except`'s `as`, an import, a `match` pattern's capture, or a
type parameter. Each is a `vague-name` finding once per file, at its first such line; a binding on a line the change did
not add is never reported, though an added line binding the same name is. A function marked `@override` (PEP 698) is
skipped, name and parameters, since its base class fixes them; an override without the mark is reported (see `TODO.md`).
`result` is not on the list, since the rules allow it for what a function returns. A file the change adds lines to that
cannot be decoded or parsed is a `not-read` finding.

Rust is read the same way, by the bindings [reading the source](#reading-the-source) lists: a name
counts where a line the change adds to a `.rs` file binds it, by any kind of binding listed there, from a `let` to a
`use ... as`. Rust names its types and traits in CamelCase, with no underscore to split on, so its counterpart of the
underscore rule is a name whose last CamelCase word is one of the words, such as `RunInfo`, `StateManager` or
`HTTPStatus`, but not `InfoStore`. A constant in capitals, as MAX_VALUE, ends in a word after an underscore, so the
Python rule catches it. The name and parameters of each `fn` of an `impl Trait for T` are skipped, as `@override` is
in Python, since the trait fixes them; what such a `fn`'s body binds is still read, as is every `fn` of an inherent
`impl`. A `.rs` file that does not parse is a `not-read` finding at the line the parser stopped at.

Go is read the same way, in a Go module's `.go` files, tests included, with Rust's CamelCase rule, since Go names in
MixedCaps (`runInfo`, `StateManager`): a name counts where an added line binds it by `:=` (in a statement, an
initializer, a `range`, a type switch or a `select` case), `var`, `const`, a parameter, a receiver, a named result, a
function, method, type or interface method, or an import's own name. Which interface a method implements, and so
whether its name was the author's choice, cannot be told from its file, so every method's name is read, as an
override without `@override` is in Python: `func (v Version) Value() (driver.Value, error)` is a finding to weigh.

The gate is `GATE: ERROR` when any of these holds, and `GATE: OK` otherwise:

- a finding on a line the change adds, or a finding of a failing check that it does not weigh;
- a linter that is missing, fails, or exits "found something" with nothing the gate can read, or a Cargo workspace
  that clippy cannot compile. A gate that cannot run a check never reports it as passed;
- a file a linter reads, by the globs in the table above or the settings', with a character that linter ends a line at
  and git does not, on any line up to the last one the change adds to it, whichever commit put it there. For every
  linter but clippy that is a carriage return not followed by a newline; actionlint also ends a line at NEL (U+0085),
  U+2028 and U+2029, though YAML 1.2 does not (see the comment on it in `LINTERS`, `crates/gate/src/linters.rs`). rustc
  numbers lines at a newline alone, so clippy's set is empty. Each linter declares its own set. ruff, mypy, pymarkdown
  and actionlint number every later line of the file past git's, and shellcheck stops reading there. Either way the gate
  cannot judge their findings below it. The condition names the file, the first such line and the character. A file no
  linter reads, such as a text file with old Mac line ends, is not held to this;
- a failing test, in any JUnit report a `[[test]]` command writes, or that `--tests` names;
- a `[[test]]` command whose tests could not be judged, as [running the tests](#running-the-tests) says;
- with a coverage report, Cobertura, LCOV or a Go coverage profile, from a `[[test]]` command or `--coverage`,
  coverage of new code below 80%, once the change has 20 or more new lines to cover. Coverage
  of new code is (covered branch outcomes + covered lines) / (branch outcomes + coverable lines), over the added
  lines the report lists; a smaller change is not judged on coverage, since a few lines swing it too far.
  With more than one, as a project with Python and Rust has, the reports are merged by path and line: a
  line is coverable when any report lists it and covered when any report covers it, and its branch outcomes are
  the most any report counts and covers. A Cobertura report's paths are taken relative to its `<source>`
  directories, an LCOV report's as its `SF:` lines give them, and a Go profile's, which name a file by its package's
  import path, through the `module` line of the go.mod nearest above each changed `.go` file.

Exit status 0 means `GATE: OK`, 1 `GATE: ERROR`, and 2 that the gate could not judge (a bad argument, `REV` names no
commit, not a git tree, settings it cannot read, an unreadable report, a source file of the change it cannot read),
in which case it writes nothing; 130 (`EXIT_INTERRUPTED`) means Ctrl-C stopped it, and it wrote nothing either.

A change to a file whose contents are not UTF-8, such as a Latin-1 line or a binary file that git diffs as text,
is judged like any other. The gate reads the diff, git's file lists and the linters' output as they were printed,
keeping the bytes it cannot decode, and only a newline ends a line of them. So a carriage return or form feed
inside an added line cannot pass for a diff header that would move the lines after it to another file, and a file
name that holds one keeps its linter findings. In the contents of a file a linter reads, a character that linter
ends a line at is still the failed condition above. A test file that `test-integrity` cannot decode is an
`unreadable` finding of that check, since its tests cannot be compared.

A file whose name is not UTF-8 is read the same way, and the quality file writes each byte it cannot decode as
`\xNN`. By the same convention it writes a character that ends a line in some reader, such as `\r`, `\x85` or
U+2028, as `\x0d`, `\x85` or `\u2028`, wherever it appears in a list item or a name, so that each item stays on
its line and reads the same in every round; `one_line` in `crates/gate/src/text.rs` is where that convention
lives. Not
every tool can read such a name:

- ruff prints it with U+FFFD for each such byte, which the gate matches to the tracked file, so ruff's findings
  on it are listed as usual.
- mypy stops with `INTERNAL ERROR` (exit status 2) whenever the file is among its `[tool.mypy] files`, and
  pymarkdown stops with `Configuration Error: 'utf-8' codec can't encode character` when it has a finding in such
  a `.md` file. Either is a failed condition quoting the tool's first line, which need not name the file; mypy's
  comes back on every change until the file has a UTF-8 name.
- coverage.py collects no data for such a file, so its added lines count as not coverable; the quality file names
  each such `.py` file the change adds lines to under the coverage sentence, as not measured.
- `flaky-tests` collects the tests of such a test file, but pytest-randomly stops with an internal error,
  `UnicodeEncodeError`, when it shuffles them, so a change to one of them is a `not-rerun` finding quoting it.

Any other git output the gate cannot decode, such as a work tree whose own path is not UTF-8, leaves the change
unjudged (exit 2).

The quality file starts with the `GATE:` line and a sentence that names what was judged, "`gate` judged the working tree
at `<commit>` against the base `<commit>`", with "by the rules from ..." and "with the settings from ..." where those
were not the tree's own `pyproject.toml` and `gate.toml`. Then it has the failed conditions, the issues (the findings on
added lines, then the failing checks' that they do not weigh), the findings to weigh, the failing tests, coverage of new
code, the suppressions the change adds, and the build configuration it edits. Issues and findings to weigh are numbered
as one list, issues first, at most 50 in all, then a count:

- **Suppressions added** (`# noqa`, `# type: ignore`, `# shellcheck disable`, `<!-- pyml disable`, `# pragma: no cover`,
  `# pragma: no branch`, `# pragma: no mutate`, and Rust's `#[allow(` and `#[expect(`) are listed, not failed, so the
  Reviewer weighs each one. A `# pragma: no cover` or `no branch` is listed because it takes lines or branches out of
  the coverage condition, and a `# pragma: no mutate` because it takes them out of the surviving-mutants check. Each
  counts only in the syntax of the files that what it silences reads: in a Python file (a `.py` file or a `#!` python
  script), only within a comment, so the same words in a string or a docstring are not one, but in a file that does not
  parse, where the two cannot be told apart, on any line; `# shellcheck disable` in a `.sh` file or a workflow under
  `.github/workflows/`; in Markdown, a `<!-- pyml disable` pragma that is a line of its own, from its first column, as
  pymarkdown reads one; the attributes in Rust; in Go, golangci-lint's `//nolint`, with or without a space after the
  `//`, staticcheck's `//lint:ignore` and `//lint:file-ignore`, and the generated-code header
  `// Code generated ... DO NOT EDIT.`, which takes the whole file out of the checks that read Go
  ([reading the source](#reading-the-source)). Any other file has none.
- **Build configuration**: an edit to `pyproject.toml` or `uv.lock` (`BUILD_CONFIG_FILES`), to the `gate.toml` at the
  top level, or to a file the settings add with a note of their own is noted, since each can change how the change was
  judged, as is one to a file anywhere in the tree that clippy takes its rules or its toolchain from
  (`Cargo.toml`, `clippy.toml`, `rust-toolchain.toml`, `.cargo/config.toml`; `CLIPPY_CONFIG_NAMES` in
  `crates/gate/src/config_edits.rs`),
  and cargo's `config.toml` in the `CARGO_HOME` clippy runs with when that lies inside the tree, as `scripts/gate`'s
  `.cargo-home` does in a Jenkins build; and, anywhere in the tree, a `go.mod` or a `go.work`, which the go tool builds
  a module by, its `replace` directives swapping a dependency for other code, a file under the `vendor` directory of a
  Go module (one beside a go.mod), which the go tool builds in place of the dependency's download without checking it
  against go.sum, or golangci-lint's `.golangci.yml`, `.yaml`, `.toml` or `.json` (`GO_CONFIG_NAMES` and `tool_config`,
  beside clippy's).

### Rust tests and coverage

The Rust tests run with [cargo-nextest](https://nexte.st/), under
[cargo-llvm-cov](https://github.com/taiki-e/cargo-llvm-cov), which builds them with rustc's `-C instrument-coverage`
and turns what they ran into a Cobertura report with the toolchain's own `llvm-profdata` and `llvm-cov`, from rustup's
`llvm-tools-preview` component. This repository's `gate.toml` runs them in its one Cargo workspace, but for `xtask`,
the workspace's own tasks, as a `[[test]]` command:

```bash
cargo llvm-cov nextest --locked --workspace --exclude xtask --ignore-run-fail --profile ci \
    --config-file ci/nextest.toml --cobertura --output-path coverage-gate.xml
```

- **What it measures** is lines, not branches: branch coverage of Rust needs a nightly compiler. Each line of the
  workspace's own crates that the compiler maps to code counts, the dependencies' do not. That includes the lines of
  a `#[cfg(test)]` module: the tests'
  own lines count as covered when they run, so they raise a change's coverage that adds both a function and its tests
  (`TODO.md`).
- **Paths.** cargo-llvm-cov names each file relative to the workspace's directory, which it gives as the report's
  `<source>`, as an absolute path, so the gate reads `src/main.rs` of a workspace in `engine/` as
  `engine/src/main.rs`, as long as the report was written in the tree the gate judges; an absolute path stands as it
  is.
- **Failing tests** do not stop the build, as pytest's do not: `--ignore-run-fail` runs every test and writes the report
  whatever fails, and the `ci` profile of `ci/nextest.toml` writes their JUnit report to `target/nextest/ci/junit.xml`
  in the workspace, here at the top level, which `gate.toml` names as the command's JUnit report. A workspace that does
  not compile is a failed condition naming the command, and so is a run that leaves no JUnit report, since
  `--ignore-run-fail` also passes a nextest that ran no test ([running the tests](#running-the-tests)). `--config-file`
  replaces the workspace's own `.config/nextest.toml`, so a change cannot configure how its tests run in the gate.
- **Doctests** do not run: cargo-nextest has none, and cargo-llvm-cov runs them only on a nightly toolchain.
- **Where the tools come from**: `ci/install-tools.sh` downloads cargo-llvm-cov and cargo-nextest, and cargo-mutants for
  the surviving-mutants check, each pinned by version and the sha256 of its release's archive; rustup's `llvm-tools`
  component comes with the agent image, as clippy does.

### Go tests and coverage

A Go project runs its tests as a `[[test]]` command with [gotestsum](https://github.com/gotestyourself/gotestsum),
which runs `go test` and writes its JUnit report, the go tool's own `-coverprofile` writing the coverage profile:

```toml
[[test]]
name = "go"
command = ["gotestsum", "--junitfile", "junit-go.xml", "--", "-coverprofile=coverage-go.out", "./..."]
junit = ["junit-go.xml"]
coverage = ["coverage-go.out"]
```

- **Tests.** gotestsum names a test case by its package's import path and the test, a subtest after a slash, so the
  failing tests list `example.com/calc.TestAdd/negative`; a failure's detail is gotestsum's word for it, `Failed`
  (`TODO.md`). A package that does not compile is one failing test case, `TestMain`. gotestsum exits non-zero when a
  test fails, as the gate expects; a run that leaves no report is a failed condition naming the command.
- **What it measures** is the statements of each package that its own tests run, unless `-coverpkg` widens it: a line
  is coverable when a block of statements holds some of it, and covered when such a block ran. Go counts no branches.
- **Paths.** The profile names a file by its package's import path, as `example.com/calc/sub/calc.go`, which the gate
  reads back to a path of the tree through the `module` line of the go.mod nearest above each changed `.go` file; a
  module at the top of the tree or below it is read the same way.
- **One module a command.** A command runs at the tree's top level; a module below it runs its tests from its own
  directory, as `["sh", "-c", "cd tools && gotestsum --junitfile ../junit-tools.xml -- ./..."]`.
- **Where the tools come from**: the machine, as cargo's do; in Jenkins the agent image provides go, gotestsum,
  golangci-lint and gremlins, which `ci/install-tools.sh` does not download. The gate's tests of them fail, rather
  than skip, without them.

### Reading the source

The gate parses source itself, on [tree-sitter](https://tree-sitter.github.io/tree-sitter/): Rust with
`crates/lang-rust`, Go with `crates/lang-go` and Python with `crates/lang-python`, which is not Python's own `ast`, so
the checks that only read a change (test integrity, cognitive complexity and vague names) start no other program and
need no Python. Each front end reads a file into the model of `crates/model`, which the checks of `crates/checks` judge
whatever the language; another language is another front end.

**Which files.** The gate reads, once per run, each file of the change that holds Rust, by a `.rs` name, Python, by a
`.py` name or a first line that is a `#!` naming python, or Go, by a `.go` name in a directory that holds a `go.mod`, or
below one, a go.mod the change deletes included, outside what the go tool leaves out of a package (a directory named
`testdata`, a name or directory that starts with `.` or `_`) and of `./...` (a directory named `vendor`, which holds the
module's dependencies), and not generated: a Go file with the header [Go's convention](https://go.dev/s/generatedcode)
fixes, a line `// Code generated ... DO NOT EDIT.` before its `package` clause, is the generator's output, not code
written to be judged; an added header is listed among the suppressions, so a hand-written file that gains one is seen. A
file not read can still be the code a change edits (`GoChange::is_code`). Each is read on each side of the change where
it is a regular file: at the base as git stores it, and in the working tree. A side that is a symbolic link, a
submodule, a directory or a special file is not read: a link's change is the name it points to, a submodule's object is
a commit, and reading a FIFO or a device could block. A file of the working tree that cannot be read, and a base file
git cannot show, leave the change unjudged (exit 2). Lines are numbered from 1, at `\n` alone, as git numbers them.

**What cannot be read as its language.** Rust and Go source is UTF-8. Python source is decoded as its byte order mark or
its coding cookie ([PEP 263](https://peps.python.org/pep-0263/)) says, else as UTF-8; the gate decodes UTF-8, ASCII
and Latin-1, and a cookie that names any other encoding makes the file one that could not be read. A file that does
not parse is named at the line of the first token the parser could not place. Either is a finding of each check that
needed the file, as the check's own text above says: `unreadable` or `unparsable`, `not-scored`, `not-read`.
tree-sitter's grammars are not the compilers': tree-sitter-python also reads Python 2, so `print x` parses, and does
not read a type parameter's default (PEP 696); `TODO.md` lists what else is known to be misread.

**What is read of a Rust file:**

- every `fn` that has a body or could have one: free, in an `impl` or a `trait` (a trait's declaration without a body
  included), and nested in another `fn`, in the order they start. Not a `fn` in an `extern` block, nor one inside a
  `macro_rules!` definition. Each has its name, without `r#`; the line of its first token after its outer
  attributes, the line its first outer attribute starts on, doc comments included, and the line of its last token;
  the items it lies in, outermost first (an inline module, an `impl` with its type and its trait, a `trait`, a `fn`,
  a `const` or a `static`); and its outer attributes other than doc comments, each by its path, as `test`,
  `tokio::test`, `should_panic` or `cfg`, and as written. Which attributes make a test is the checks' to say;
- the assertion macros a `fn`'s body calls: `assert!`, `assert_eq!`, `assert_ne!`, `debug_assert!`,
  `debug_assert_eq!`, `debug_assert_ne!` and `panic!`, named by the last segment of the macro's path, so
  `pretty_assertions::assert_eq!` counts. Each has its text as written, its literals, every literal token in its
  arguments, and its shape, the arguments' tokens with each of those literals blanked, so that two calls that differ
  only in their literals have one shape. An assertion inside a closure the body holds counts, as does one in the
  arguments of another macro whose arguments read as expressions separated by commas, such as `vec![...]` or another
  assertion; one inside a nested `fn` is that `fn`'s, and one inside any other macro, such as `tokio::select!`, is
  not seen;
- the values a `fn`'s body throws away: `let _ = expr;`, `_ = expr;` and a statement `expr.ok();`, each with `expr`
  as written. As for the assertions, those of a nested `fn` are its own;
- a digest of a `fn`'s body, the same for two bodies of the same tokens, whatever their layout and comments;
- the flow of the body of each `fn` that no other `fn` holds, as cognitive complexity weighs it, and the
  path of each call in it. A macro's arguments are read as for the assertions, any other macro's tokens not;
- every inline module, `mod name { ... }`, nested ones included, with its lines and whether it is test code: an
  attribute of its own, outer or inner, is `cfg(P)` with a `P` that holds only under `cfg(test)`, which is `test`
  itself or an `all(...)` with such a `P` among its arguments. A `mod name;` that names another file is not read as a
  module;
- every name the file binds, with its line: the names of a `let`'s pattern, a `fn` and its parameters, a closure's
  parameters, the pattern of a `for`, of a `match` arm and of a `let` in an `if`'s or a `while`'s condition, a
  `struct`, `enum`, `union`, `trait`, type alias or associated type, a `const`, a `static`, and the new name of a
  `use ... as name`. A pattern binds each identifier in it that does not start with an uppercase letter: a parser
  cannot tell `None` or a `const` from a new name, and Rust names those in CamelCase or capitals, its bindings in
  snake case. `_` binds nothing. Bindings inside a macro's tokens are read only where the assertions are.

**What is read of a Go file** is what each check's text above names: its functions and methods with a body, with the
flow of each and the calls in it; in a `_test.go` file, its tests and subtests, their assertions, its skips, its
`//go:build` lines and the errors it throws away; and every name it binds.

**What is read of a Python file** is what each check's text above names: its functions, each `def` that no other
`def` holds, named by its classes as `C.D.f`, with the flow of its body; in a test file, its tests, their assertions,
the names that skip a test, the `except`s that swallow an error and the module's constants; and every name it binds.

### The gate's settings

Which files are tests and the like is the project's to say, in a `gate.toml` at its top level, or in the one file
`--config` names, a gate.toml under any name but `pyproject.toml`, which must be there (`Settings` and
`JudgedBy::for_tree` in `crates/gate/src/settings.rs`). Each key
has the gate's default, so a project without the file, or a key the file leaves out, is judged as by the defaults
below. A key the gate does not know, or a value of the wrong kind, stops it with exit status 2, naming the key: a
misspelt key never judges by the default. A glob reads as git's `:(glob)` pathspecs do, against the path from the top
level: `*`, `?` and a bracket expression stop at a slash, `**/` stands for any directories, none included, and a
trailing `/**` for all below.

| Key | Default | What it says |
|---|---|---|
| `python` | `python3` | the judged project's Python, which has its pytest, pytest-randomly and mutmut: a path, from the tree's top level unless absolute, or a name without a slash for `PATH` to find. `--python` replaces it |
| `test-files.python` | `**/tests/**/*.py`, `**/test_*.py`, `**/*_test.py` | Python's test files |
| `test-files.rust` | `**/tests/**/*.rs`, `**/tests.rs` | Rust's test files by where they are; an inline `#[cfg(test)]` module holds tests wherever it is |
| `test-files.support` | `**/conftest.py`, `**/tests/**` | files that go with the tests rather than the code, which the checks that run tests take from the working tree |
| `test-files.golden` | `**/tests/**/golden/**` | golden files, which test integrity reads as expected values |
| `complexity.exempt` | `tests/**`, `**/tests/**/*.rs`, `**/tests.rs` | files whose functions cognitive complexity does not score |
| `build-config` | none | a table of file to note, a sentence the quality file gives after "The change edits `file`." |
| `linters.<name>` | the table above | the globs of the files that linter reads, for `ruff`, `mypy`, `pymarkdown`, `shellcheck`, `actionlint`, `clippy` or `golangci-lint`; `[]` runs it on none. Clippy lints the workspaces of the `Cargo.toml` files among its files, and golangci-lint the modules of the `go.mod` files, so their globs must take those in |
| `budgets.mutation` | 600 | seconds, as `--mutation-budget`, which replaces it |
| `budgets.base-run` | 120 | seconds, as `--base-run-budget`, which replaces it |
| `[[test]]` | none | the commands that run the tests: `name`, `command` (an argv list), `junit` and `coverage` (the reports it writes, JUnit and Cobertura, LCOV or a Go coverage profile, paths inside the tree from its top level). The gate runs them, as [running the tests](#running-the-tests) says |

The linters' rules stay in their own files, as below. A CI that passes `main`'s `gate.toml` with `--config`, with
`main`'s `pyproject.toml` beside it when `main` has one, judges a change by the settings and the rules of `main`, so a
change cannot loosen how it is judged through its own `gate.toml` either. An edit to the tree's own `gate.toml` is noted
under build configuration as one that did not change how the change was judged when the settings came from another file
or from the defaults, and as one that did when they came from the tree's own.

### Running the tests

The gate runs each `[[test]]` command of the settings that judge the change, in the order listed, before any linter
or check: the surviving-mutants check reads the test contexts that a pytest run with `--cov-context=test` writes. Each
runs at the tree's top level, as an argv and never through a shell, with no input; its output goes to the gate's
standard error, never into the quality file. No time limit applies to it, and it does not count against a check's
budget. Before a command runs, the reports it names are deleted, so a report an earlier run left is never read.

A failing test fails the gate as one of the failing tests the quality file lists. A command whose tests could not be
judged is a failed condition of its own, naming the command, and none of its reports is read, when it

- could not start, such as a program not on `PATH`;
- left a report it names missing, whatever its exit status: a run that exits 0 and writes no JUnit report would
  otherwise pass tests nobody judged;
- left a JUnit report the gate cannot read;
- exited non-zero while its JUnit reports list no failing test, so that what failed was not a test, such as a build
  that did not compile.

A coverage report a command wrote that cannot be read stops the gate, which exits 2, as one `--coverage` names does.
`--tests` or `--coverage`, given at all, replace every command: a CI that has already run the tests passes their
reports, and no command runs. Ctrl-C during a command kills its process group, and the gate exits 130.

A CI that judges a change by `main`'s settings, as below, runs `main`'s commands, so a change cannot alter which
commands judge it.

### Whose rules judge a change

ruff, mypy and pymarkdown get their rules from one `pyproject.toml`: the one beside the `gate.toml` that judges,
which is the tree's own unless `--config` names a gate.toml elsewhere (`JudgedBy::for_tree` in
`crates/gate/src/settings.rs`). Where no `pyproject.toml` is there, the three run on their defaults, reading no
configuration the tree holds (ruff with `--isolated`, mypy with an empty `--config-file`, over the files it reads),
and the surviving-mutants check does not run mutmut (below): no project needs a `pyproject.toml`. One that is there
but is no file leaves the change unjudged (exit 2). A
quality build passes `main`'s `gate.toml`, with `main`'s `pyproject.toml` beside it, and runs the release of the gate
that `main` pins, so a change cannot loosen the rules it is judged by.
pymarkdown runs in an empty directory of the gate's own, on absolute paths: given a configuration with `--config`,
it still adds what a `pyproject.toml` or `.pymarkdown` in its current directory says, so at the tree's top level the
tree's own `[tool.pymarkdown]` could turn a rule off.
golangci-lint has none there either: like clippy's, its rules are the module's own configuration whatever
`--config` names, and an edit to one is noted under build configuration.
shellcheck and actionlint have no table there; they run on their defaults, with `--norc` and an empty config file
so that a `.shellcheckrc` or `.github/actionlint.yaml` added to the tree cannot loosen them either. What a
snapshot still controls is visible in its quality file: tool versions through `uv.lock` (noted under build
configuration), clippy's rules, which are the tree's own `Cargo.toml` and `clippy.toml` whatever `--config` names
(noted there too), and inline suppressions (listed). shellcheck and actionlint versions live in the agent image,
outside this repository, as does clippy's, unless a `rust-toolchain.toml` (noted) names one.

Up to 0.6.0, `--config` could name a `pyproject.toml`, for the rules, and be given twice, once for the rules and once
for the settings. It names one gate.toml, the last when given twice, and a `--config` whose file name is
`pyproject.toml` is refused as a bad argument (exit 2), saying that the rules are those of the `pyproject.toml`
beside the gate.toml.

The surviving-mutants check takes `[tool.mutmut]` from the same file, so a change cannot tell mutmut to leave its code
alone. In the copy mutmut runs on, that table replaces the tree's own, and only that table: the rest of the tree's
`pyproject.toml`, such as `[tool.pytest.ini_options]`, is how its tests run, which a change may need to alter. Without
`--config`, the tree's table judges it, as the linters' do; without a `pyproject.toml` beside the settings, no table
says what mutmut may mutate, and a change to a Python function gets a `not-run` finding in place of mutmut's. What the
tree holds under `mutants/`, mutmut's own state and cache of results, is left out of that copy, so mutmut starts afresh
and a change cannot mark its mutants killed; a change that adds or edits files there fails the gate, as [the
gate](#the-gate) says.

This repository's rule sets are ruff's defaults plus bugbear,
simplifications, pyupgrade, bandit, cyclomatic complexity (`C90`, `max-complexity = 15`; nesting is the
`cognitive-complexity` check's) and pylint's errors and warnings, with the docstring rules [the gate](#the-gate)
names; mypy at its default strictness on the Python the gate embeds, in `crates/gate/src`; pymarkdown without the
line-length and inline-HTML rules. Only lines a change adds count, so what `main` already has does not block.

## The report as a contract

The gate writes one report two ways. `quality.json` is the contract with a program that reads it back: the JSON
Schema [schema/quality-v1.json](../schema/quality-v1.json) says what each field holds, and its `schema_version` is
raised on any change a reader could notice; adding an optional field does not raise it, so a reader ignores fields it
does not know. It holds everything the quality file shows, as data and without the file's limits: every finding, with
`fails` saying whether it fails the gate and a `fingerprint` that stays the same when the finding moves to another
line or its message counts something else; every failing test, with its whole detail; and the strings as the gate read
them, line breaks included. The failed conditions, and the text each fingerprint is taken over, are instead as the
quality file writes them, a line break escaped, so that a reader gets the same condition and fingerprint from either
file. A reader checks itself against the `tests/golden/*.json` files, which the gate's tests hold its report to
(`crates/gate/tests/golden_reports.rs`) and a release ships as `golden/`, each beside the quality file of the same
report.

The quality file is prose for the people and agents who answer it, and its wording and layout may change in any
release. [octoflow](https://github.com/DavidSiegl/octoflow-ai)'s orchestrator, which runs the gate in Jenkins after
each round of its Builder's, still reads the parts below from it until it reads `quality.json` instead, keeping its own
copy of them and checking that copy against `tests/golden/*.txt`; until then a change to one of them is a change to
its reader.

- **The gate line.** The first non-blank line is `GATE: OK` or `GATE: ERROR`; a reader should tolerate Markdown
  emphasis around it. The gate writes no other; a reader may add its own, as octoflow's `GATE: BUILD_FAILED` for a
  build that ended before the gate ran.
- **Sections.** Each starts with a level-2 heading, a line that starts with `##` and a space, and runs to the next.
- **Failed conditions.** Under `## Failed conditions`, one bullet per condition, as the file words it.
- **Numbered findings.** Issues and findings to weigh, numbered as one list over the whole file, at most
  `QUALITY_ISSUE_LIMIT` (50), the rest counted. Each is a line `N.`, then where, then its severity, its rule, a
  colon and its message; where is the path and line in backticks, as `` `a.py:3` ``, the path and
  `(the whole file)` in backticks for a finding on no line, or `The change as a whole:` for one on no file. The
  rule's first word names the check, or the linter, that raised it.
- **Findings to weigh.** In a section, the line `These do not fail the gate. Fix each, or say in your report why
  it should stand.` (`TO_WEIGH_NOTE`) comes before its findings to weigh; the numbered findings above it fail the
  gate.
- **Failing tests.** Under `## Failing tests`, one bullet per test, `` - `name`: detail ``.
- **One line per item.** A character that ends a line in some reader, such as `\r` or U+2028, is written as an escape,
  `\x0d` or `\u2028`, in every list item and name (`one_line` in `crates/gate/src/text.rs`), so each item is read back
  as the one line it was written as.

In both files, a byte of a path or a message that is not UTF-8 is written as the four characters `\xNN`.
