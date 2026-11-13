# TODO

What is known to be open in the gate (`crates/`), its scripts and its CI, and why. A fixed entry is deleted; git history
keeps what was done. An entry that a fix only narrows is rewritten to say what is left.

The entries that say "moved from octoflow-ai" were written while the gate lived there; the octoflow paths they cite
(`engine/`, `ui/`, `.scratch/`) are octoflow-ai's.

## Open issues

- **The gate reads an XML report only in UTF-8, or in UTF-16 that opens with a byte-order mark.** As cross-review 09 put
  it (F21, 2026-11-07): "Without a UTF-16 byte-order mark, `report_format` takes every report as UTF-8: one that
  declares another encoding (`<?xml version='1.0' encoding='ISO-8859-1'?>` and a byte such as `E9` in a path) fails
  `String::from_utf8` … UTF-16LE without a mark (`3C 00 3F 00 …`) passes `from_utf8`, since NUL is UTF-8, and then
  fails in `xml::parse`; UTF-16BE without a mark (`00 3C 00 3F …`) does not start with `<` past ASCII whitespace, so
  it goes to `lcov_lines` and fails". gate.py's `ElementTree.parse` read all three. The JUnit reader, `xml::parse_file`
  (`crates/gate/src/xml.rs`), refuses everything that is not UTF-8, so a JUnit report has the same gap; `report_format`
  (`crates/gate/src/coverage.rs`) is the Cobertura one. Deferred by the user (2026-11-07): the tools the gate is meant
  for, coverage.py, cargo-llvm-cov and cargo-nextest, write UTF-8, and the fix adds a dependency outside spec decision
  7's set. Closes by decoding in `xml.rs` for both readers, from the mark and the declaration, as quick-xml's `encoding`
  feature (encoding_rs) does, `report_format` then keeping only the test of XML against LCOV.
- **A branch build from a root commit cannot be judged.** As cross-review 01 put it (F2, 2026-11-06): "The root-commit
  branch of the Change stage sets GATE_BASE to a commit of the empty tree. `scripts/gate` exits 2 for any base without
  gate.py, so a build on that path always fails with 'the gate could not judge the change (exit status 2)'." Now it is
  the judge's `ci/gate.pin`, `ci/fetch-gate.sh`, `pyproject.toml` and `gate.toml` that such a base lacks. Reachable only
  for a branch build whose HEAD is an orphan commit with no earlier successful build. The Jenkinsfile is the
  agent-estate template's, whose root-commit base suits a gate that does not judge with the base's own code. Closes with
  `scripts/gate` deciding what judges a change that has no gate before it.
- **A build of main can skip the gate for a head that main's newest rules never judged.** As cross-review 32 put it
  (R1-1, 2026-11-13): a pull request build judges by origin/main as it stood then, not by HEAD's tree, so the skip in
  `ci/judged-at-pr.sh` "is sound only when the PR build's judge was HEAD^1, and nothing checks that. It holds when
  HEAD^1 was born on main as a merge commit [...]. It fails when HEAD^1 reached main by a fast-forward push of a
  commit that already existed in a branch": a commit P that tightens gate.toml or raises ci/gate.pin, pushed straight
  to main beneath a head h that passed under the older rules, lets h's merge skip the gate. Git does not record which
  main a pull request build judged with. Reachable only by a push to main that bypasses pull requests, which
  `agent.directToMain` allows. Closes with the build of main learning which judge the pull request build used, from
  Jenkins rather than from anything that build archives, and requiring HEAD^1's.
- **`vague-names` does not split CamelCase in Python.** `vague` (`crates/checks/src/names.rs`) matches a whole Python
  name or its last `_`-separated word, so `user_data` is found but a class `InfoManager` or `UserData` is not; in Rust
  the last CamelCase word is matched too (its `camel`, gate-rust-checks ticket 04), which is Rust's counterpart. Closes,
  if the run metrics show such names recur, with Python's matched the same way. Moved from octoflow-ai on 2026-11-05.
- **`passes-at-base` builds a Rust package's dependencies anew on every run.** `base_run_findings`
  (`crates/gate/src/passes_at_base.rs`) gives each run a target directory in its scratch directory, outside the copy, so
  every gate run compiles the base's dependencies before it runs a test, within `--base-run-budget`; on a package of
  many crates that may use the whole budget, which is a `budget-reached` finding. Not measured on Jenkins:
  gate-rust-checks ticket 05 asked for the Gate stage's added time, which needs a Jenkins build of a Rust change on the
  branch. Decided against for now: the working tree's own `target/`, where the base's build would overwrite the binaries
  Cargo copies up to `target/debug`. Closes, if the budget runs out on real changes, with a target directory that lasts
  between gate runs outside every workspace, as the lock file of octoflow-ai's `ci/cargo-lock.sh` does. Moved from
  octoflow-ai on 2026-11-05.
- **`surviving-mutants` splits its budget by language, not by work, and builds Rust anew.** `surviving_mutants`
  (`crates/gate/src/mutants.rs`) gives mutmut half the `--mutation-budget` when the change has Rust to mutate too, and
  cargo-mutants (`on_copy` there) what is left by the check's deadline, however many functions each language has; and
  cargo-mutants runs `--in-place` in a copy of its own without `target/`, beside mutmut's, so each run copies the tree
  again and compiles the workspace and its dependencies before its baseline. Not measured on Jenkins: gate-rust-checks
  ticket 06 asked for the Gate stage's added time on a Rust change, which needs a Jenkins build on the branch. Found by
  ticket 06 (2026-10-30). Closes, if the run metrics show either language starved, with the budget split by each
  language's changed functions, one copy for both tools, and a target directory that lasts between gate runs (the same
  as passes-at-base's above). Moved from octoflow-ai on 2026-11-05.
- **`vague-names` reports a name an undecorated override cannot change.** A `def handle(self)` in a
  `socketserver.BaseRequestHandler` subclass, or a parameter the overridden signature names, has no fix; only a method
  marked `@override` is skipped (`marked_override` in `crates/lang-python/src/lib.rs`). Raised by the cross-review of
  `written-rules` (F6, 2026-10-24): "names fixed by a parent class or framework are reported, though they cannot be
  renamed". Closes, if the run metrics show it is common, with methods whose class has a base from outside the
  repository left out. Moved from octoflow-ai on 2026-11-05.
- **`passes-at-base` lists the whole tree of the base, and of each submodule, to find its submodules.** `unpack_commit`
  (`crates/gate/src/copies.rs`) runs `git ls-tree -r -z` on every run, even in a repository without `.gitmodules`, and
  splits the whole listing, only to find the entries of mode 160000. Raised by cross-review 41 (F9, 2026-10-26): "a full
  recursive `ls-tree` on every run, only to find gitlinks". Checking `<commit>:.gitmodules` first would skip a gitlink
  that has no `.gitmodules` entry, such as a nested repository added by mistake, which is filled today. Closes, if run
  metrics show the listing costs, by listing only the paths `.gitmodules` names, with such gitlinks left out. Moved from
  octoflow-ai on 2026-11-05.
- **The copy's `src/` on `PYTHONPATH` comes before the standard library in `passes-at-base`'s run at the base.** A
  module there named as one of the standard library's, such as `src/token.py`, shadows it, as one at the copy's top
  already did through `python -m`. Raised by cross-review 41 (round 1, not recorded, 2026-10-26). Not seen failing.
  Closes, if it is, by putting the roots on the path after the standard library and before site-packages.
  Moved from octoflow-ai on 2026-11-05.
- **A test file that cannot be imported at the base hides all its changed tests from `passes-at-base`.** A module-level
  import of something the change adds makes pytest's collection of the whole file fail at the base, which `collect`
  (`crates/gate/src/python_at_base.rs`) takes as the expected outcome for every changed test in it, including ones that
  would pass there. Accepted: the check reports only, and splitting the file's tests from its imports would mean
  rewriting them. Closes, if its run metrics show these files are common, by importing such names inside the tests that
  use them, or by running each test with the failing import stubbed. Moved from octoflow-ai on 2026-11-05.
- **A test file whose name is not UTF-8 is never rerun by `flaky-tests`.** pytest-randomly 5.0.0 hashes each node id as
  strict UTF-8 (`pytest_randomly._crc32`, called from `pytest_runtest_setup` and its neighbours) and stops with an
  internal error, exit status 3. `run` in `crates/gate/src/pytest.rs` already lets pytest print such an id
  (`PYTHONIOENCODING=utf-8:surrogateescape`), and the collection runs with `-p no:randomly`, so the id is found; each
  rerun then crashes, and flaky-tests reports `not-rerun` quoting the `UnicodeEncodeError`. Raised by the cross-review
  of `flaky-tests` (F10, 2026-10-23). Closes when pytest-randomly hashes the id's bytes, or when the reruns can seed
  such a test without it. Moved from octoflow-ai on 2026-11-05.
- **`--test-contexts` collects mutmut's selection in a fuller copy than mutmut runs it in.** `narrowed_table`
  (`crates/gate/src/python_mutants.rs`) runs `pytest --collect-only` in the gate's copy of the tree, which holds every
  tracked file; mutmut's own `mutants/` holds only `source_paths`, `also_copy`, `tests/`, `test/` and a few top-level
  files. A selection that reaches outside those, such as none in a project with tests under `other/`, can name a
  recorded test there that mutmut's pytest cannot find, and mutmut's run fails as `not-run`. Closes with the collection
  run in a copy laid out as mutmut lays out `mutants/`, or by mutmut itself. Moved from octoflow-ai on 2026-11-05.
- **mutmut reads a symlinked `.py` file in the surviving-mutants copy.** `copy_tracked` (`crates/gate/src/copies.rs`)
  copies a tracked symlink as a link, `mark_for_mutmut` (`crates/gate/src/python_mutants.rs`) leaves links unmarked, and
  mutmut 3.8.0's `walk_all_files` lists a symlink to a file and `create_mutants_for_file` opens it. So once any changed
  function starts mutmut, a change adding `x.py -> calc.py` has every function of `calc.py` mutated a second time, as
  `x.py`, spending the budget on mutants no finding reports; and `x.py -> /dev/zero` or a FIFO reads without end or
  blocks until the budget runs out. The gate's own selection skips such links (`Stored::files` in
  `crates/gate/src/parser_checks.rs`). Raised by the cross-review of `gate-python-files` (R2-F1, 2026-10-26); rests on
  mutmut's source, not on a run. Closes with the copy or `mark_for_mutmut` keeping links out of mutmut's walk, without
  breaking a tracked link the tests import through, and a test that also edits a real function. Moved from octoflow-ai
  on 2026-11-05.
- **A body moved into a file hides a function with that body moved out of it, which is then judged as new.** The
  cognitive-complexity check takes a base function as gone from its file when the file now holds its body fewer times
  than the base did (`gone` of `BasePool` in `crates/checks/src/complexity.rs`), without asking where the working tree's
  copies came from. So when `calc.py`'s `big` moves unchanged to `lib.py` and `other.py`'s `dup`, with the same body,
  moves unchanged into `calc.py`, `dup` takes `big`'s base in `calc.py` and `big` is "a new function": two unchanged
  moves fail the gate (review 01 round 7, R7-F1, a regression from the round-6 rule; deferred by the user, 2026-10-24,
  as rare: it needs two over-limit functions with one body in two of the change's files). Closes with a count that pairs
  each working copy with the base it came from, so an arrival does not make up for a departure. Moved from octoflow-ai
  on 2026-11-05.
- **A Python expression nested thousands deep can overflow the gate's stack.** `flow::flow`
  (`crates/lang-python/src/flow.rs`) calls itself at each construct that nests flow, so a chain of thousands of nested
  conditional expressions or lambdas, which tree-sitter parses without recursing, ends the gate with a stack overflow
  rather than a finding; plain nesting, runs of boolean operators and `elif` chains are walked with a worklist. Python's
  own parser refuses to parse such a file. Found with gate-standalone/15 (2026-11-10). Closes with the walk on a
  worklist throughout, or a depth past which the file is `not-scored`.
- **The gate's copies still hold other linked configuration as links.** `own_pyproject`
  (`crates/gate/src/python_mutants.rs`) makes only mutmut's copy's `pyproject.toml` a file. As cross-review 14 noted
  (round 4, 2026-11-10, not recorded as a finding): "the other files `pytest::inifile` looks for (pytest.toml,
  pytest.ini, tox.ini, setup.cfg; `pytest.rs:182`) are still copied as the tree's links, so one that is a relative link
  out of the tree is taken for no configuration in mutmut's copy, as F16 was for pyproject.toml; and flaky-tests' and
  passes-at-base's copies still hold a linked pyproject.toml as the link." And `own_pyproject` "reads what the tree's
  link names with `std::fs::read`, whole and whatever it is (a FIFO, a device)". Not seen in a project. Closes with
  every copy the Python checks make holding the configuration files pytest reads as regular files, read as the rules'
  own file is.
- **The gate's Python checks: three design nits of cross-review 14 (2026-11-10), in its words.**
  - F8, `crates/gate/src/parser_checks.rs`: "`changed_units` reads and parses each changed Python file again after
    `Sources::read` did, and is reached three times a run (flaky_tests, python_at_base, python_mutants); its "could not
    be read" branch is all but dead." Closes with the three checks given the units read once.
  - F9, `crates/gate/src/python_at_base.rs`, `mutants.rs`: "The Python halves and the Rust halves' modules import each
    other (`passes_at_base` <-> `python_at_base` for `budget_reached`; `mutants` <-> `python_mutants` for `KILL_WAIT`,
    `STOP_GRACE`, `SURVIVOR_EXAMPLES`), consts widened to `pub(crate)` to cross the cycle." Closes with what both halves
    of a check share in a module neither half is. The dependency-cycles check (structural-checks/01) fails a change
    that closes such a cycle from now on, but not one the base has that the change adds no import of a new file to, so
    these two stay until a change breaks them; an import a change adds between two files of such a cycle that were not
    yet joined does fail it. The run over 50 commits found more such cycles in `crates/gate` (`judge` holds `Change`
    and calls every check that imports it; `report` <-> `config_edits`, `golangci` <-> `linters`).
  - F12, `crates/gate/src/glob.rs`: "`fnmatch` arrives as an `across_slashes` flag through `translate_name` and
    `bracket` with two added match arms, the bolted-on flag the user's CLAUDE.md design rule names; it also compiles a
    Regex on each call." Closes with fnmatch's translation a function of its own, compiled once a pattern.
- **A linter learns where it runs from a flag.** As cross-review 17 put it (R1-6): "Five entries and a test carry
  `apart: false` so that pymarkdown can take a second path in `run`": `Kind::Lines`'s `apart`
  (`crates/gate/src/linters.rs`). Closes with the linter's command saying where it runs, should a second linter need
  to run apart; with one, a flag documented at its field is the smaller design.
- **An edited build-configuration file whose name git quotes may go unnamed.** `touched_paths` in
  `crates/gate/src/config_edits.rs` does not unquote a name git wrote in quotes: after `rename from` it is taken with
  its quotes and escapes, and a `--- "a/…"` line is not read. The gate's diff is made with `core.quotePath=false`
  (`change_diff` in `crates/gate/src/diff.rs`), so a name of bytes above 0x7f is not quoted; one that holds a `"`, a `\`
  or a control character still is, as a `clippy.toml` or a Cargo configuration file in a directory so named. Such a file
  is named under "Build configuration" only while the working tree still has it under that name, not when the change
  deletes it or renames it away. Found with gate-standalone/18 (2026-11-11). Closes with the names read through the diff
  module's own `unquote`.
- **A Go test's error thrown away as `v, _ := f()` or `_ = f()` is not seen.** `discard` in
  `crates/lang-go/src/testing.rs` reports `_ = err` alone, an identifier named as an error: left out on purpose, since
  a parser knows no types and both forms are common for values that are no error (`v, _ := m[k]`). Found with
  go-support/01 (2026-11-11). Closes only with the types, which the gate does not read.
- **A Go error block emptied by the change is not seen.** As cross-review 30 put it (R9-1, 2026-11-13): a change that
  turns `if err != nil { t.Fatal(err) }` into the same block with `// t.Fatal(err)`, or deletes the statement, "adds
  only the comment line, which is no line of the weakening, and the `if` and `{` lines are kept". An
  `if err != nil {}`'s lines end at its block's `{` (`if_statement` in `crates/lang-go/src/testing.rs`), so that an
  edit to a comment in the block or to an `else` after it is none; 0.11.1, which looked at the `if` line alone, missed
  both too. Deferred by the user (2026-11-13). Closes with test-integrity finding an empty error block whose block held
  a statement at the base, when the change edited the block.
- **A dot-imported testify package's calls are not assertions.** `Imports::of` in `crates/lang-go/src/testing.rs`
  skips an import named `.` or `_`, so with `import . "github.com/stretchr/testify/assert"` a test's
  `Equal(t, 3, Add(1, 2))` is no assertion, and removing it or changing its 3 beside the code is not seen. Rare, and
  linters warn against dot imports. As cross-review 22 put it (F9), go-support/01 (2026-11-11). Closes with a
  dot-imported testify package's exported functions, called unqualified, read as its assertions, `New` excepted.
- **A generated Go file whose header follows a `/* */` comment holding a line that starts with `package` is judged.**
  `generated` in `crates/lang-go/src/lib.rs` looks for the header line by line up to the first that starts with
  `package`, without telling a comment's lines from code, so such a line inside a leading block comment, as a licence
  may hold, ends the search before the header. The file is then read and judged as hand-written, which fails safe:
  findings on generated code, escaped by `complexity-exempt`. Found by cross-review 22 (round 2), go-support/01
  (2026-11-11). Closes with the search ending at the `package` clause tree-sitter-go finds.
- **Each language's front end repeats the same tree-sitter helpers.** `crates/lang-rust/src/syntax.rs`,
  `crates/lang-python/src/syntax.rs` and `crates/lang-go/src/syntax.rs` each have their own `named`, `children`, line
  numbering, a token digest (FNV-1a) and a `first_error`, and the digests must agree with what the checks compare only
  within a language, so nothing breaks; but a fix to one is a fix to three. Found with go-support/01 (2026-11-11), which
  followed the two front ends before it. structural-checks/03 added one more: each front end's `code_tokens` walks its
  tree the same way, but for which kinds are literals, imports and test code. Closes with a small crate of the shared
  helpers that the three depend on.
- **`NEWEST_RELEASE` is raised by hand with each Go release.** A release tag in a test file's `//go:build` counts as set
  only up to `NEWEST_RELEASE` in `crates/lang-go/src/constraint.rs`, `go1.27`, so a test file naming a later release's
  tag is a failing `skip` rather than a `platform-only` finding to weigh. Go 1.28's release candidates already set
  `go1.28`, so the constant wants raising, in a `patch` release, once the first of them ships, and so on for each
  release. The user's choice in go-support/01 (2026-11-11), over reading the tag set from the go tool on `PATH`, which
  would make the finding depend on the machine. Closes only if that choice is revisited.
- **A comment or blank line inside a Go block of statements is a line to cover.** A Go coverage profile gives a block
  by where it starts and ends, not by its lines, so `go_profile_lines` (`crates/gate/src/coverage.rs`) counts every
  line between, covered or not as the block is, as gocover-cobertura and Codecov read profiles too. Found with
  go-support/01 (2026-11-11). Closes with each such line read from the file and left out when it holds no token.
- **A Go package that does not compile is listed without the compiler's words.** gotestsum's report holds only
  `FAIL <package> [build failed]` for it, under the test case `TestMain`, which `junit_outcomes`
  (`crates/gate/src/junit.rs`) takes as the detail; the compiler's lines go to the command's standard output, which
  `run_test_command` (`crates/gate/src/test_commands.rs`) passes to stderr unread. Narrowed from failing-test-detail/02
  (2026-11-13), whose Done when asked for the compiler's first lines. Closes with the gate keeping a `[[test]]`
  command's output and giving a build-failed case the lines that name the package's files, or by deciding the
  command's log in the build is enough.
- **A file tree-sitter cannot parse goes unread by dependency-cycles, saying nothing.** `imports` in
  `crates/lang-rust/src/imports.rs` and `crates/lang-python/src/imports.rs` read such a file as making no imports and
  declaring no modules. A changed file that does not parse already fails the gate, cognitive-complexity's `not-scored`
  finding (`crates/checks/src/complexity.rs`); a file the change leaves alone does not, and as cross-review 24 put it
  (F3, 2026-11-12): "one construct it rejects anywhere in `src/lib.rs` drops all its `mod` declarations, every module
  becomes its own crate root, `crate::` paths stop resolving, and a cycle the change closes among them is passed with
  OK. The gate's verdict then says nothing about the check not having read the crate." `test_modules` (same file)
  reads such a file alike, so the files its `#[cfg(test)] mod name;` declarations take in are judged as code. Closes
  with the front ends saying which files they could not read, and the gate naming those in a note that fails nothing.
- **dependency-cycles reads and parses every tracked file of a language twice a run.** As cross-review 24 put it (F6,
  2026-11-12): "Every untouched file's bytes are cloned into `base` and each tree is parsed in full by `imports`, so a
  one-line change parses the repository's whole `.py`/`.rs` set twice; `git ls-files -z` repeats `judge_change`'s
  listing, and the base side reruns `ls-tree`/`cat-file` for files `Stored` already read" (`trees` in
  `crates/gate/src/parser_checks.rs`). duplicated-blocks (structural-checks/03) lists and reads the working tree's
  files of each language a third time, and parses them once more (`duplicated_blocks`, same file); and, for Rust,
  `RustChange::of` (same file) reads both trees once more and parses them for `lang_rust::test_modules`. Closes with
  an untouched file's imports and modules read once for both sides, the listing and the base's files taken from what the gate
  already read, and each working-tree file read once for both checks.
- **lang-rust reads an item's outer attributes two ways.** As cross-review 24 put it (F7, 2026-11-12): `path_attribute`
  (`crates/lang-rust/src/imports.rs`) "looks back through siblings, the pattern e7f9115 removed for test detection",
  via `syntax::preceding_attributes`, while `walk_children` passes each item's attributes as it walks them. Closes with
  `walk_children` collecting a `mod` declaration's `#[path]` as it collects `cfg(test)`.
- **dependency-cycles does not read an edit to the line that decides whether an import counts.** As cross-review 24 put
  it (F11, 2026-11-12): "Base `src/a.rs`: `#[cfg(test)]\nuse crate::b::X;`, `src/b.rs`: `use crate::a::Y;`. The change
  edits only line 1 to `#[cfg(feature = "x")]`. a → b is now an edge the base lacked, but `added` for a.rs is {1},
  the statement is 2..=2, `added_line` is None, and a → b → a passes with OK." Python's `if TYPE_CHECKING:` rewritten
  to `if FLAG:` likewise; docs/quality-gate.md says so. Such a line may sit far from the import: an inline module's or
  a file's `#![cfg(test)]`, or the `#[cfg(test)] mod x;` in another file that makes all of `x.rs` test code. Closes
  with `model::Import::statement` taking in the lines that gate it (`cycles::added_line`), each front end giving the
  attribute, `if` or declaration lines it read the import under.
- **test-integrity carries only a test's own weakenings, and in Rust compares them as text.** As cross-review 30 put it
  (round 2, 2026-11-13): weakenings on a group or file (a class decorator, `pytestmark`, `#[cfg(..)] mod tests`,
  `//go:build`) are not carried when the group moves, so a moved class's `skipif` is reported as new; and Rust's
  `Weakening::code` is the attribute's text without whitespace (`skip` in `crates/lang-rust/src/skips.rs` joins every
  skipping attribute on a line into one), so `#[cfg(unix)] #[ignore]` on one line before a move and on two after it, or
  a comment edited inside an attribute, never match. Each is reported as under 0.11.1. Closes with `carried_weakenings`
  (`crates/checks/src/integrity.rs`) matching groups as it matches tests, and lang-rust taking `code` from each
  attribute's tokens, as lang-python and lang-go take it from their digest.
