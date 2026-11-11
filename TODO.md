# TODO

What is known to be open in the gate (`crates/`), its scripts and its CI, and why. A fixed entry is deleted; git history
keeps what was done. An entry that a fix only narrows is rewritten to say what is left.

The entries that say "moved from octoflow-ai" were written while the gate lived there; the octoflow paths they cite
(`engine/`, `ui/`, `.scratch/`) are octoflow-ai's.

## Open issues

- **The release's `gate` needs the build agent's glibc or newer.** `ci/package.sh` builds it for the default target, so
  it is linked against the Jenkins agent's GNU C library. As cross-review 07 put it (R1-2) of the helper that the
  releases up to 0.6.0 shipped beside gate.py: "A project on Debian 11 (2.31), Ubuntu 20.04 (2.31) or RHEL 8 (2.28) that
  follows the README gets 'GLIBC_2.34 not found' from `--rust-syntax`, and every Rust check is not run." Since 0.7.0 it
  is the gate itself that does not start there. The README says so ("Running the gate in a project"). Closes with `gate`
  built for `x86_64-unknown-linux-musl`, statically linked, once the agent image's rustup has that target (a build
  cannot add it: the rustup home is root's).
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
- **`crates/checks/src/lib.rs` and docs/quality-gate.md still speak in octoflow's words.** `Finding`'s doc comments name
  the Reviewer as the one a warning is listed for, and docs/quality-gate.md names the Builder and the Reviewer where it
  says who a part of the quality file is for, and cites octoflow's pull requests, its orchestrator's `QUALITY_TIMEOUT`
  and its measurements where a decision was measured; its intro says so. Found by gate-standalone/02 (2026-11-05), which
  moved the gate here and stripped only the assumptions its ticket named. `quality.json` (gate-standalone/04) settled
  who reads what: a program reads the JSON, people and agents the Markdown; but octoflow's orchestrator reads the
  Markdown until octoflow-ai's gate-standalone/08, so those words still hold. Closes after 08, with them naming the
  quality file's reader, and the measurements read as history.
- **This repo's Jenkinsfile departs from agent-estate's template in three places.** It archives `quality.json` beside
  `quality.md` (gate-standalone/04); it builds, checks and releases the release tarball in its Package and Release
  stages (gate-standalone/06), which no other repo of the template has; and it publishes the JUnit report the gate's
  `[[test]]` command writes, `target/nextest/ci/junit.xml`, where the template publishes `test-results*.xml`
  (gate-standalone/05, 2026-11-06; the one report since gate-standalone/18). All three are edits to the rendered file
  that a `copier update` must merge. And the path repeats `gate.toml`'s `junit` list, as cross-review 06 put it (R1-4):
  "A change to a `junit` path in gate.toml without the Jenkinsfile silently stops publishing"; display only, never the
  verdict. Closes with the template archiving `quality.json` for every repo whose gate writes it, taking its JUnit paths
  from the repo's `gate.toml` or an answer, and leaving room for a repo's own stages after the Gate stage.
- **Test code outside the places the spec names is judged as code.** The spec of the Rust checks (requirement 2) places
  Rust's tests in a `tests/` directory, a `tests.rs` and an inline `#[cfg(test)]` module; `Settings`'s `rust_tests`
  (`crates/gate/src/settings.rs`) and `model` (`crates/lang-rust/src/lib.rs`), by which both Rust checks tell test code,
  follow it by default (test-integrity's side of this is the out-of-line `mod name;` item in its list below). So
  `cognitive-complexity` judges a file that a parent takes in with `#[cfg(test)] mod name;`, as the engine keeps its
  fakes and `test_support.rs` (`engine/src/host.rs:24` on rust-engine): at rust-engine `476b770`, 3 of the 21 engine
  functions over the limit are theirs (`FakeState::git`, `FakeJenkinsState::answer`, `helper_cases`). Found by
  gate-rust-checks ticket 03 (2026-10-30). And, as cross-review 108 put it (F4): "A file-level `#[cfg(test)] fn` or
  `#[cfg(test)] impl`, or a `#[test]` fn outside the places requirement 2 names, is judged against 15." Closes with the
  user's ruling on the spec's definition, and, if those are tests, `crates/lang-rust` reading each out-of-line `mod
  name;` with its `cfg_test` (`module` in `crates/lang-rust/src/syntax.rs` passes over a `mod` without a body), so that
  the gate can follow a declaration to its file, and a `cfg_test` of each fn's own and its impl's attributes. Until then
  a project can name such files in its `gate.toml`'s `test-files.rust` and `complexity.exempt`. Moved from octoflow-ai
  on 2026-11-05.
- **A macro whose arguments are no expressions separated by commas adds nothing to a Rust fn's score.** `macro_args` in
  `crates/lang-rust/src/syntax.rs` splits a macro's token tree at its top-level commas and gives its arguments only
  where each piece parses as an expression, to a fn's flow as to its assertions and bindings. Raised by cross-review 108
  (F3, 2026-10-30): "A macro whose arguments do not parse as comma-separated expressions (`vec![if a {..} else {..};
  n]`, `matches!(x, P if a && b)`, `tracing::info!(target: "x", ..)`, `select!{..}`) is dropped silently, so its
  branches, loops and closures add nothing to the score." Clippy's own `cognitive_complexity` reads no macro either.
  Kept so in the port, as the user ruled for gate-standalone/13 (2026-11-07). Closes with `macro_args` parsing the
  common other shapes (`expr; n`, `expr, pattern if guard`, `name: expr` arguments) and a `macro` node, unread, that the
  check can count or report. Moved from octoflow-ai on 2026-11-05.
- **A Rust test's own lines count toward the coverage of new code.** cargo-llvm-cov's report lists the lines of a
  `#[cfg(test)]` module as it lists the code's (docs/quality-gate.md, "Rust tests and coverage"), and a passing test
  runs them all, so a change that adds a Rust function and a test of it is judged on both: 15 untested lines beside a
  10-line test read as 40%, not 0%. Found while adding Rust coverage (rust-engine ticket 03, 2026-10-27). Closes with
  test modules left out of the report, by `#[coverage(off)]` once it is stable or by the tests moving to files that
  `--ignore-filename-regex` can leave out, or with the gate (`crates/gate/src/coverage.rs`) skipping the lines a
  `#[cfg(test)]` item spans. The Rust engine's tests are in such files already (`engine/src/**/tests.rs`,
  `engine/src/prompt_parity.rs`, `engine/src/test_support.rs`); the desktop shell's are a `#[cfg(test)]` module in
  `ui/src-tauri/src/lib.rs`. That module also makes a change to the desktop shell's tests alone need a version raise,
  and a place in the one-bumping-PR-at-a-time queue: `ci/version-raised.sh` exempts `**/tests.rs`, not `lib.rs`
  (cross-review 117, F4, 2026-11-03). Moving it to `ui/src-tauri/src/tests.rs` closes both: tried by
  ui-test-text-file-busy/01 (2026-11-03) and dropped then because the gate failed the move on the tests' own `cfg`s
  (next entry). Their `cfg`s all name platforms, which the gate now weighs rather than fails (`platform-only`,
  gate-platform-cfg/01), so the move is open again. Moved from octoflow-ai on 2026-11-05.
- **The gate takes a moved Rust test's own skip for one the change added.** `added_weakenings`
  (`crates/checks/src/integrity.rs`) reports a skipping attribute on any line the change added, so a test moved to
  another file with its attributes is reported though nothing new is skipped: a platform `cfg` (`#[cfg(unix)]`) as a
  `platform-only` finding to weigh, any other (`#[ignore]`, `#[cfg(feature = "slow")]`) as a `test-integrity skip`
  error; the `removed-test` check follows the same move by the test's body (`Test::body`; docs/quality-gate.md, "Rust
  tests"). Found 2026-11-03 moving the desktop shell's 26 tests out of `ui/src-tauri/src/lib.rs`
  (ui-test-text-file-busy/01): six errors, one for each `cfg` the tests carried, which since gate-platform-cfg/01
  (2026-11-04) are six findings to weigh. Closes with a skip attribute on a test matched to the base's test of the same
  body counted only when that test did not carry it. Moved from octoflow-ai on 2026-11-05.
- **A misread literal on an added attribute line can report a test's existing skip as added.** `attribute`
  (`crates/lang-rust/src/skips.rs`) lexes only plain strings, so a char literal (`'"'`) or a raw string ending in `\`
  (`r"C:\"`) can leave an attribute open until a `]` inside a later string, many lines down; `line_attributes` then
  reads every `#[` on the lines in between as the added line's. So a change that adds only `#[doc = r"C:\"]` above an
  existing `mod tests` with an `#[ignore]`d test whose body holds `"a]b"` fails the build with a `test-integrity skip`
  error naming that `#[ignore]`. It fails loudly and hides nothing. Separately, `line_attributes`'s "cannot hide a skip
  after it" holds only on the line where an attribute starts: a misread literal on a wrapped attribute's continuation
  line can still hide a skip later on that line, as `#[cfg(miri)]` in `doc = "x")] #[cfg(miri)]` after
  `#[cfg_attr(any(), doc = ']',`. Raised by cross-review 136 (R5-F1, should-fix) and deferred by the user (2026-11-04).
  Closes with either of the reviewer's fixes: read the lines in between only when the attribute's text holds no `"` or
  `'`; or bound how far an attribute may span, to the lines before the next one that starts with `#[` or an item
  keyword. Moved from octoflow-ai on 2026-11-05. A test fn's own attributes are not read this way: tree-sitter gives
  them, their literals lexed as Rust lexes them (docs/port-differences.md, "The attributes of a test fn"); the
  misreading is left for the attributes of a module and of a statement in a test's body, which `skips` reads off the
  lines' text.
- **A line of a string literal in a test's body that starts with a skipping attribute is read as a skip.** `skips`
  (`crates/lang-rust/src/skips.rs`) reads each line's text, so a test that holds Rust source as a fixture, in a
  multi-line string, fails the gate with a `test-integrity skip` error on its `#[cfg(...)]` or `#[ignore]` lines. Found
  2026-11-07 on `crates/lang-rust/tests/{complexity,integrity}.rs` (gate-standalone/13), whose two such literals are
  wrapped so that no line starts with one. Closes with the line read as a statement's attribute only where no string
  literal of the test's body spans it, which tree-sitter can tell.
- **A skipping attribute on the line of the `mod` it turns off is not seen.** `attribute_place`
  (`crates/lang-rust/src/skips.rs`) judges a holding module's outer attributes only on the lines before its `mod` line,
  so `#[cfg(miri)] mod tests {` added on one line, as rustfmt never writes it but a hand can, turns the module's tests
  off with no finding. Found 2026-11-07 by a surviving mutant (`<` to `<=` there, gate-standalone/13). Closes with the
  attributes on the `mod` line before the `mod` keyword judged too.
- **test-integrity's Rust detector does not see some weakenings that Python's does, or that its rules name.** Found
  while writing it (gate-rust-checks ticket 02, 2026-10-30); docs/quality-gate.md, "Rust tests", lists each:
  - an expected value held in a `const` or `static` the change edits: `changed_constants`
    (`crates/checks/src/integrity.rs`) follows the constants a front end lists, Python's module-level ones, and
    `crates/lang-rust` lists none (`model` in `crates/lang-rust/src/lib.rs`). Closes with it listing a file's `const`s
    and `static`s with their expressions, and the names of them each assertion reads;
  - a `Result` thrown away that holds no assertion macro and no `catch_unwind`, such as `let _ = check();` where `check`
    returns the `Err` that should fail the test, and a `catch_unwind` result bound to a name and never read. A parser
    knows no types, so `swallowed_errors` (`crates/lang-rust/src/lib.rs`) cannot tell such a value from a cleanup's.
    Closes with a rule on the text the change replaced, such as a `.unwrap()`, `.expect(...)` or `?` at the base on the
    same expression, which the front end would need to read on both sides;
  - a skipping attribute that does not start its line, or a multi-line `cfg_attr` whose `ignore` is on a later line, in
    a test's body or on a module (`skips` in `crates/lang-rust/src/skips.rs` reads those lines' text; a test's own
    attributes come whole from the parser). Closes with the attributes of modules and of statements in a test taken from
    the parser too.
  - a test module in a file of its own that a `#[cfg(test)] mod name;` names, where that file is neither under `tests/`
    nor `tests.rs`, as `mod cli_tests;`: `rust_tests` of `Settings` (`crates/gate/src/settings.rs`) matches a path
    alone, as requirement 2 of the spec defines the files, so that file's tests are not read and its edits count as
    code. Raised by cross-review 107 (2026-10-30, not recorded). Closes with `crates/lang-rust` reading each `mod name;`
    with its `cfg_test`, and the gate resolving the file it names as rustc does (`name.rs` or `name/mod.rs` beside the
    file, or a `#[path]`).
  - removing the first of two tests of one name in one file, as under two platforms' `cfg`s: `keyed`
    (`crates/checks/src/integrity.rs`) numbers them in order, so the second then takes the first's number and is
    compared with the removed one's base, which reads as a fewer-assertions or a changed literal on it, or as nothing,
    rather than as a removed test. Raised by cross-review 107 (round 2, not recorded). Closes with namesakes matched by
    their `cfg` attributes, or by body first as removed tests are. Moved from octoflow-ai on 2026-11-05.
- **surviving-mutants takes only the top directory's pytest configuration.** flaky-tests and passes-at-base run each
  changed test under the configuration pytest finds for it below the copy's top (`groups` in
  `crates/gate/src/pytest.rs`), but `config` there, which surviving-mutants gives mutmut (`on_copy` in
  `crates/gate/src/python_mutants.rs`), looks in the top directory alone, so a project whose configuration is in
  `tests/pytest.ini` or `pkg/pyproject.toml` runs its mutants without that configuration's `pythonpath`, `addopts` and
  plugins. Raised by cross-review 35 (R1-6). Closes with mutmut's runs taking the changed functions' tests'
  configurations, which mutmut's one `pytest_add_cli_args` makes one per run at most. Moved from octoflow-ai on
  2026-11-05.
- **flaky-tests' reruns get one time limit a run, whatever the number of configurations.** Each pytest run of
  flaky-tests has `RUN_TIMEOUT` (`crates/gate/src/flaky_tests.rs`) of its own, and a change whose tests fall under
  several configurations collects and reruns once for each, so its worst case grows with them (3 configurations and 5
  seeds: 3 x 6 x 30 s), against octoflow's `QUALITY_TIMEOUT`. passes-at-base's groups share its one budget. Raised by
  cross-review 35 (R1-5), which reads the spec's "the per-group runs share the check's time budget" as meant for
  passes-at-base, flaky-tests having none. Decided against (2026-10-27,
  `.scratch/gate-open-decisions/issues/01-shared-time-budget.md`): a time budget shared by the reruns, their groups and
  mutmut; the 30 s limit and mutmut's 600 s stay, and `not-rerun` showing up often would be a question of its own. Moved
  from octoflow-ai on 2026-11-05.
- **A branch build from a root commit cannot be judged.** As cross-review 01 put it (F2, 2026-11-06): "The root-commit
  branch of the Change stage sets GATE_BASE to a commit of the empty tree. `scripts/gate` exits 2 for any base without
  gate.py, so a build on that path always fails with 'the gate could not judge the change (exit status 2)'." Now it is
  the judge's `ci/gate.pin`, `ci/fetch-gate.sh`, `pyproject.toml` and `gate.toml` that such a base lacks. Reachable only
  for a branch build whose HEAD is an orphan commit with no earlier successful build. The Jenkinsfile is the
  agent-estate template's, whose root-commit base suits a gate that does not judge with the base's own code. Closes with
  `scripts/gate` deciding what judges a change that has no gate before it.
- **`scripts/gate` takes any `WORKSPACE` for a Jenkins build.** As cross-review 01 put it (F5, 2026-11-06): "In a dev
  container that exports `WORKSPACE` but not `WORKSPACE_TMP`, a local run exits 2. If both are set, it moves
  CARGO_HOME, empties `$WORKSPACE_TMP/tmp` and switches to the `ci` Hypothesis profile." Closes with a variable only
  a Jenkins build sets deciding it, once one is confirmed on the agent.
- **Under the gate's `--cap-lints warn`, an `#[allow]` overrides a workspace's `forbid`.** `lint_workspace` in
  `crates/gate/src/clippy.rs` caps every lint at warn so that a lint at deny cannot stop cargo before a compile error in
  a later crate (cross-review 66, F1); rustc's cap also lowers `forbid`, so an added `#[allow(unsafe_code)]` under
  `unsafe_code = "forbid"` no longer gives E0453 and its unsafe block is no finding: the gate is OK on a change the
  workspace's rules make uncompilable, and only "Suppressions added" lists the allow, and not even that when the change
  adds the lint as one line inside an existing wrapped `#[allow(\n    a,\n    b,\n)]`, since the Rust pattern of
  `SYNTAXES` (`crates/gate/src/suppressions.rs`) reads one added line at a time (F15). Nothing in this repository sets
  `forbid` (the top-level `Cargo.toml`'s `[workspace.lints]` are all `warn`), and its test command's uncapped `cargo
  llvm-cov nextest` (`gate.toml`) would still fail to build such a workspace. Raised by cross-review 66 (F11,
  2026-10-27): "The gate is OK on a change that the workspace's rules make uncompilable". Closes with a second, uncapped
  clippy run whose rustc errors (codes `E` and four digits, such as E0453) are compile errors, or with the forbidden
  lints read from the manifests and an added allow of one failing the gate. Moved from octoflow-ai on 2026-11-05.
- **`expected-value-changed` misses an inline literal's sign or structure.** `changed_literals`
  (`crates/checks/src/integrity.rs`) pairs assertions by their `shape` and compares their `literals`, and an operator or
  a bracket around a literal is part of the shape: `assert f() == -5` becoming `== 5` changes the shape (no pair, no
  finding), as `== ("a",)` becoming `== "a"` does. Raised by the cross-review of `test-integrity-constants` (F4,
  2026-10-27): "`_literals` equality misses sign, type and structure changes"; the constant path compares whole
  expressions instead (`changed_constants`). A type's change is seen since the port, which compares literals as written,
  so `== 1` becoming `== True` is a finding (docs/port-differences.md, "Literals are compared as written"). Closes with
  the inline path pairing a unary minus with its operand, and a literal with what holds it. Moved from octoflow-ai on
  2026-11-05.
- **The gate's intro writes the rules and settings paths (`--config`) as they are.** `QualityReport::intro`
  (`crates/gate/src/report.rs`) names the rules file and the settings file without `one_line`, so a path holding a line
  breaker splits the quality file's third line, and `docs/quality-gate.md`'s "wherever it appears in a list item or a
  name" says more than holds for it. octoflow's `gate_failures` does not read the intro, so no round is misjudged.
  Raised by cross-review 38 (round 3, 2026-10-26). Closes with the intro written through `one_line`, or with each
  quality-file writer escaping once where it joins its lines. Moved from octoflow-ai on 2026-11-05.
- **A flaky-tests rerun sometimes waits out `RUN_TIMEOUT` on Jenkins.** Seen with gate.py, whose limit for a pytest run
  was the same 30 s (`RUN_TIMEOUT` in `crates/gate/src/flaky_tests.rs`). A test of its then Python suite, whose port is
  `a_server_a_test_leaves_holding_the_output_does_not_hold_up_the_reruns` in `crates/gate/tests/flaky_tests.rs`, took
  8.0 s on Jenkins (PR-98, PR-99/3, PR-100), but 14.4 s on PR-101 build 1 and 31.0 s on build 3, failing its 30 s bound:
  one of its six pytest runs waited about the whole limit. PR-101 changes only the tests' git fixture. Twenty runs here
  under the load of a parallel suite, with SIGINT ignored, all passed. The port's test allows the five reruns 60 s.
  A rerun that hangs gives a `not-rerun` warning, not a wrong verdict. Closes when the run that waits is caught, e.g.
  with each pytest run's wall time and exit in the test's failure message, and its cause fixed. A second cause, of a
  different kind: on PR-190 build 3 (gate-rust-checks 04–06) the reruns of the changed tests did not finish within the
  30 s at all, since those tests, of the gate's then Python suite, include dozens that build a small Cargo package with
  real cargo, cargo-nextest or cargo-mutants, each from a cold target directory in the rerun's copy. A change that adds
  many such tests is not rerun; that one closes with a limit that grows with the tests rerun (a time budget shared with
  mutmut was decided against, "flaky-tests' reruns get one time limit a run", above). Moved from octoflow-ai on
  2026-11-05.
- **`vague-names` does not split CamelCase in Python.** `vague` (`crates/checks/src/names.rs`) matches a whole Python
  name or its last `_`-separated word, so `user_data` is found but a class `InfoManager` or `UserData` is not; in Rust
  the last CamelCase word is matched too (its `camel`, gate-rust-checks ticket 04), which is Rust's counterpart. Closes,
  if the run metrics show such names recur, with Python's matched the same way. Moved from octoflow-ai on 2026-11-05.
- **`vague-names` reports a Rust trait impl's associated types and constants.** Only the name and parameters of a `fn`
  an `impl Trait for T` holds are skipped (`Binding::fixed`, as `function` in `crates/lang-rust/src/syntax.rs` sets it),
  as gate-rust-checks ticket 04 asked; a `type Value` or `const STATUS` the trait fixes is reported, as in serde's `impl
  Visitor for V { type Value = ...; }`. Found by ticket 04 (2026-10-30). Closes with `Walk` there, which knows each
  item's holder, marking fixed each `type` and `const` an `impl Trait for T` holds directly. Moved from octoflow-ai on
  2026-11-05.
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
- **cargo-mutants is pinned by the sha256 of its archive as downloaded, not as published.** Its releases publish no
  checksums file (`ci/install-tools.sh`), so the pin of v27.1.0 holds the archive the gate's author downloaded on
  2026-10-30, trusted on first use. Closes with a release that publishes checksums, or the asset digest GitHub shows on
  the release page, checked by hand as cargo-llvm-cov's was. Moved from octoflow-ai on 2026-11-05.
- **`passes-at-base` places a Rust test's target by Cargo's default layout only.** `target`
  (`crates/gate/src/passes_at_base.rs`) reads the file's place: a target a manifest puts elsewhere (`[[test]] path =
  ...`, `autotests = false`), a module a `#[path]` attribute names, a module of `src/main.rs` in a package that also has
  `src/lib.rs` (taken for the library's), and a file under `tests/x/` that `tests/x/main.rs` does not declare are named
  wrong, so nextest reports no such test and the check gives nothing; a file under `benches/` or `examples/`, or under a
  `tests/x/` with no `main.rs`, as a shared `tests/common/mod.rs`, is a `not-run` finding. Found by gate-rust-checks
  ticket 05 (2026-10-30). Closes with the targets read from `cargo metadata` and the module tree from the files' `mod
  name;` items, which `crates/lang-rust` would need to read. Moved from octoflow-ai on 2026-11-05.
- **`passes-at-base` cannot tell a Rust test that needs the change from a base that does not build.** A target whose
  tests do not compile at the base counts as failing there, whatever rustc's error is (`NEXTEST_BUILD_FAILED` and
  `error: could not compile` in `target_findings`, `crates/gate/src/passes_at_base.rs`): the test reaching for what the
  change adds, as the ticket meant, but also a base that does not compile on the gate's machine at all. Any other cargo
  error, as a lock file `--locked` refuses, is a `not-run` finding. Python's run tells the two apart by whether the
  failure passes through a file the change edits (`outside_failed_conftest` in `crates/gate/src/python_at_base.rs`). The
  ticket asked for the build failure to be said "in the finding-free summary, not as a finding"; the quality file has no
  such summary for a check, so docs/quality-gate.md says it instead. Found by ticket 05 (2026-10-30). Closes with
  cargo's JSON messages read for the files the errors are in, and the user's ruling on where the summary goes. Moved
  from octoflow-ai on 2026-11-05.
- **`vague-names` skips the parameters of a Rust trait impl's `fn`, which Rust lets the impl rename.** Unlike Python's
  keyword arguments, a Rust caller never names a parameter, so the trait fixes only the `fn`'s name; gate-rust-checks
  ticket 04 asked for the parameters to be skipped too, as `@override`'s are, and clippy's `renamed_function_params`
  (a restriction lint) holds an impl to the trait's names. So a `raw_data` parameter of an `impl Opener for Door` `fn`
  is not reported. Raised by the code review of ticket 04 (2026-10-30). Closes with the user's ruling on whether the
  parameters count as fixed. Moved from octoflow-ai on 2026-11-05.
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
- **The reruns' time limit is too short for slow tests on the Jenkins agent.** `RUN_TIMEOUT`
  (`crates/gate/src/flaky_tests.rs`) is 30 s a run, as gate.py's limit was. Measured with gate.py on its then Python
  suite: on `flaky-tests`' own change each run took about 9 s on an idle laptop and about twice that under load.
  Measured on Jenkins on 2026-10-24 (PR #50, surviving-mutants' change): the first rerun did not finish within 30 s, so
  the check reported `not-rerun` and judged no test; the tests that change edited started mutmut, and rerunning their
  four test classes once took about 50 s on the laptop. A limit that covers them (about 90 s, five seeds) would push
  octoflow's quality build past its `QUALITY_TIMEOUT` beside the mutation budget, so the user kept 30 s (2026-10-24).
  `not-rerun` is a finding to weigh, never a wrong verdict. Those tests went with the cutover (gate-standalone/18), and
  the gate's own tests, now Rust, are not rerun; what is left is a project whose changed tests take longer than 30 s a
  run. Closes as the second cause of "A flaky-tests rerun sometimes waits out `RUN_TIMEOUT` on Jenkins" above does, with
  a limit that grows with the tests rerun; one time budget the reruns and mutmut share was decided against
  ("flaky-tests' reruns get one time limit a run", above). Moved from octoflow-ai on 2026-11-05.
- **The mutation budget leaves most of a large change's mutants untried.** The budget is 600 s unless
  `--mutation-budget` or `gate.toml` says otherwise (`mutation_budget` in `crates/gate/src/settings.rs`). Measured with
  gate.py on changes to itself, on Jenkins on 2026-10-24: on PR #50 (surviving-mutants' change) mutmut tried 756 of 3079
  mutants and the Gate stage took 683 s, the build about 850 s; on PR #51, 677 of 801 in a 638 s Gate stage. That fits
  octoflow's `QUALITY_TIMEOUT` (1200 s, `engine/src/constants.rs`) with about 350 s to spare, but not after a wait in
  Jenkins' queue, which counts against a quality round (PR #52 waited about 860 s behind the others). Those were pull
  request builds; a quality build has skipped the Package stage since 2026-10-24, so it has more to spare than that. The
  user kept 600 s (2026-10-24): a stopped run says `budget-reached`, never "none survived". Closes with mutants chosen
  by the changed lines rather than whole functions (a time budget shared with the reruns was decided against,
  "flaky-tests' reruns get one time limit a run", above). On PR #113 (2026-10-27) mutmut was stopped at 600 s after
  trying 0 of 253 mutants: its three runs of the whole suite before the first mutant took the budget. Narrowed
  (2026-10-28, gate-open-decisions ticket 06): where a project's pytest command records a coverage context per test,
  `--test-contexts` has mutmut run only the tests that reach the changed functions and its selection collects
  (`recorded_tests` and `narrowed_table` in `crates/gate/src/python_mutants.rs`); replayed locally, PR #113's change
  took mutmut 77 s instead of 709 s, with the same verdicts. Still open: a change to a function nearly every test
  reaches still costs about the whole suite three times, and a large change still has more mutants than the budget
  tries. Whether a Jenkins build showed it is not recorded, and this repository's builds no longer can: since the
  cutover (gate-standalone/18) they have no Python tests to record, and `scripts/gate` does not pass the flag. Moved
  from octoflow-ai on 2026-11-05.
- **`--test-contexts` collects mutmut's selection in a fuller copy than mutmut runs it in.** `narrowed_table`
  (`crates/gate/src/python_mutants.rs`) runs `pytest --collect-only` in the gate's copy of the tree, which holds every
  tracked file; mutmut's own `mutants/` holds only `source_paths`, `also_copy`, `tests/`, `test/` and a few top-level
  files. A selection that reaches outside those, such as none in a project with tests under `other/`, can name a
  recorded test there that mutmut's pytest cannot find, and mutmut's run fails as `not-run`. Closes with the collection
  run in a copy laid out as mutmut lays out `mutants/`, or by mutmut itself. Moved from octoflow-ai on 2026-11-05.
- **Mutation-testing the code that runs mutmut leaves processes behind.** Seen with gate.py under mutmut: when mutmut
  stopped a mutant at its own timeout it killed the worker, and a real mutmut that the test had started in its own
  session lived on as an orphan group of one process per CPU. The Rust tests have the same shape: those of
  `crates/gate/tests/python_mutants.rs` run the gate, which starts a real mutmut in a session of its own (`start` in
  `crates/gate/src/process.rs`), so a test that cargo-mutants kills at its `--timeout` would leave that session running;
  not seen, since this repo's Rust is not mutation-tested yet ("This repo's own Rust is not mutation-tested", below). It
  only happens when a change edits that code. Jenkins' process-tree killer should end them with the build; not checked.
  Closes with a check on a PR build whose mutants run those tests, or with the gate ending what it started when it is
  killed. Moved from octoflow-ai on 2026-11-05.
- **The surviving-mutants check mutates Rust by added lines only.** cargo-mutants runs `--in-diff`, which mutates only
  what the diff's added lines hold, so a Rust function whose edit only removes lines, such as a guard clause, is not
  mutation-tested; Python's functions count removals (`PythonChange::changed_units` in
  `crates/gate/src/parser_checks.rs`, gate-open-decisions ticket 03). Closes with the Rust half choosing whole
  functions, as Python's does, and `run_workspace` (`crates/gate/src/mutants.rs`) passing them to cargo-mutants with
  `--re` or `--file` instead of the diff. Moved from octoflow-ai on 2026-11-05.
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
- **A copy of a function whose original the change edits keeps the old score and passes.** The cognitive-complexity
  check pairs identical bodies before names (`base_scores` in `crates/checks/src/complexity.rs`), so that a function
  renamed unchanged keeps its score and a new function under its old name is new. A change that edits `big` in place and
  adds `copied`, a copy of `big`'s old body, has the same shape: `copied` takes `big`'s score and passes, and the edited
  `big` is judged as new, so nothing is reported when the edit takes `big` below the limit. The same holds for a copy in
  a file the check does not judge, such as one under `tests/`, since a function moved unchanged there must keep its base
  from a new function under its old name (review round 6, R6-F1). Decided by the user in review round 4 of ticket 06.
  Closes with a way to tell a copy from a rename, such as git's copy detection (`git diff -C`) on the change's files.
  Moved from octoflow-ai on 2026-11-05.
- **The gate's tests leave their temporary files behind when they fail, and cargo-mutants makes them fail by the
  thousand.** Each test under `crates/gate/src` (coverage.rs's most, mutants.rs's too) makes a directory or file under
  the system's temporary directory and removes it on its last line, after the assertions, so a failing test keeps it; a
  cargo-mutants run over this repo fails such tests once per caught mutant. On agentbox, where /tmp is a 5.9 GB tmpfs,
  13 000 were left by 2026-11-09, and a full /tmp then failed cargo-mutants' own writes in a run that compared the gate
  with gate.py (gate-standalone/14). Closes with a guard that removes the path when dropped, used by every such test, in
  one commit.
- **A removal just below a Rust test counts as touching it, so a neighbour of a deleted test is run at the base.** A
  removal point is the line after which lines were removed, and passes-at-base counts one up to the test's last line
  (`touches` in `crates/gate/src/passes_at_base.rs`, `<= last`). Deleting test B right after test A gives the -U0 hunk
  the removal point A's last line, so A is run at the base, passes, and fails the gate with "passes without the change"
  although the change never touched it (review 12, F1, 2026-11-10). Closes with `< last` and a test deleting the test
  after another.
- **The workspace prefix is stripped anywhere in a diff header, so a nested path names a file that does not exist.**
  surviving-mutants' `diff_under` (`crates/gate/src/mutants.rs`) replaces every `a/<root>/` and `b/<root>/` in a
  section's header, not only at a path's start. With workspace root `core` and file `core/lib/core/src/x.rs`,
  `b/core/lib/core/src/x.rs` becomes `b/lib/src/x.rs`, so `--in-diff` names a missing file and it is not mutation-tested
  (review 12, F2, 2026-11-10). Closes with the prefix replaced only at a path's start.
- **passes-at-base runs cargo nextest with `--locked` even where git tracks no Cargo.lock.** In a workspace that does
  not commit its lock file the base copy has none, cargo refuses to build, and every changed test gets a not-run finding
  instead of a verdict (`target_findings` in `crates/gate/src/passes_at_base.rs`). clippy adds `--locked` only when git
  tracks the lock file (`lint_workspace` in `crates/gate/src/clippy.rs`) (review 12, F3, 2026-11-10). Closes with
  nextest taking clippy's rule.
- **A submodule beside the tests stops passes-at-base's base copy.** `copy_base_with_tests`
  (`crates/gate/src/copies.rs`) removes each base path that belongs with the tests with `remove_file`; a submodule
  there, such as a fixture repository under `tests/` that `test-support` covers, is a directory, so the removal fails
  and the whole check is "could not copy the base" (review 12, F4, 2026-11-10; the submodule tests cover only
  `vendor/`). The base path is one of the change's, so it takes a change that moves the submodule's commit, or removes
  it; first found with gate.py while submodules were being filled (2026-10-26), not seen failing in a project. Closes
  with a directory removed as a whole, or a path the base records as a submodule (mode 160000) left to `unpack_commit`,
  and a test with a submodule under `tests/`.
- **The gate repeats per-path work.** `RustChange::is_code` (`crates/gate/src/parser_checks.rs`) stats the file and
  scans the change's files on every call, and its callers ask again for the same paths, so the work grows with the
  square of the files; `cargo_root` (`crates/gate/src/mutants.rs`) starts one `cargo locate-project` per changed .rs
  path, even for paths of one manifest (review 12, F8, 2026-11-10). Neither changes a finding. Closes with `is_code`
  answered once per path when the `RustChange` is built, and one `locate-project` per directory. `Stored::at_base` there
  starts one `git cat-file` per base file it reads, as cross-review 106 raised of gate.py's reading of `.rs` files (F5,
  2026-10-30: "one `cat-file --batch` or `git archive` would do"), and since gate-standalone/15 one more per base file
  not named `*.py`, to read its `#!` line; one `git cat-file --batch` for the change would serve.
- **A Python expression nested thousands deep can overflow the gate's stack.** `flow::flow`
  (`crates/lang-python/src/flow.rs`) calls itself at each construct that nests flow, so a chain of thousands of nested
  conditional expressions or lambdas, which tree-sitter parses without recursing, ends the gate with a stack overflow
  rather than a finding; plain nesting, runs of boolean operators and `elif` chains are walked with a worklist. Python's
  own parser refuses to parse such a file. Found with gate-standalone/15 (2026-11-10). Closes with the walk on a
  worklist throughout, or a depth past which the file is `not-scored`.
- **A type parameter named as a skip is read as one in Python.** `is_expression`
  (`crates/lang-python/src/testing.rs`) does not leave out a type parameter's own name, so `def test_a[skip]():`
  added to a test file is a `skip` finding of test-integrity, where `ast` has no `Name` there. Found with
  gate-standalone/15 (2026-11-10). Closes with `type_parameter`'s name told from an expression.
- **A `.rs` file whose first line is `#![...python...]` is read as Python.** As review 13 put it (F9, 2026-11-10):
  "`crates/lang-python/src/lib.rs:70` (`is_python`). `#![cfg(feature = "python")]` starts with `#!` and holds `python`.
  Run, `crate/src/python.rs` so begun and edited: the Rust gate exits 1 with `cognitive-complexity not-scored` and
  `vague-names not-read`, "does not parse as Python". gate.py's `_holds_python` reports the same two findings, so the
  port agrees with it; the defect is both gates'." Closes with a `#!` that `[` follows read as no interpreter line.
- **A Python file is parsed once more for each format spec in it that starts with `=`.** `parse`
  (`crates/lang-python/src/lib.rs`) takes one such `=` out a round, since after the first the tree may not be the
  file's (review 13, F20 and F22). Review 13 measured `read` alone, in a release build (rounds 4 and 5, 2026-11-10):
  800 lines `print(f"{title:=^60}")`, each an error to tree-sitter, 10.1 s against 12 ms with `>` for the `=`; 800
  lines `print(f"{title:=60}")`, which make no error, 5.9 s, 200 lines 0.40 s and 3000 lines 86 s, where one round
  for all of them took 19 ms, 5.5 ms and 73 ms. The gate parses a changed Python file twice (`read` and
  `comments`). Nothing is read wrong, and a file holds a handful of such specs. The round's own measurements rule
  out two remedies: without `node.parent()` in the scan the times are the same, and tree-sitter's incremental
  reparse (`Tree::edit`) saves a fifth at most. Closes with the entry below, whose scanner needs no second parse.
- **A set inside a replacement field, with a nested f-string's `=` spec after its assignment expression, is read as a
  dict or the file as unreadable.** As review 13 put it (F24, should-fix, 2026-11-10):
  "`crates/lang-python/src/syntax.rs:114` (the doc of `first_misread_spec_start`, "Only the first is given, because only
  it is sure") with `:131` (`parent == "ERROR" && open_strings > 0`). The tree that says where the first misread `:=` is
  has every later one still in it. When a later spec's `#` runs on as a comment, the brackets around it are under an
  ERROR, and a set's `{` inside a replacement field is then a `{` under an ERROR with a string open, which the scan
  counts as a field's. The set's own assignment expression comes first in the file, so its `=` is the one taken out.
  [...] Run, `read` on `def show(title):\n    return f"{ {info := f'{title:=#x}'} }"\n`: bindings `show`, `title`, and
  no finding says the file was not read; `ast.parse` has one assignment expression, to `info` at line 2 [...]. Run, the
  same with `{info := title, f'{title:=#x}'}` for the set (`ast.parse`: `info` at line 2): "does not parse as Python",
  line 2, which through the gate is `cognitive-complexity not-scored` and `vague-names not-read` [...]. The review pass
  reports 126 of 30,000 generated files that CPython accepts read differently from `ast`, [...] every one it reduced of
  this kind, with `=.`, `=]`, `=)` and `=lambda` in place of `=#` as well [...]. It also reports that keeping one stack
  of open strings and brackets, a `{` a field's only when a string is innermost, reads these and breaks `{b:=^{b:=#}}`".
  The sentence is corrected; the reading stands, and gate.py read these files as Python does (docs/port-differences.md,
  "What does not parse"). The cause is that where a spec starts is taken from a tree that misreads it, which is a guess
  wherever that tree holds an error. Closes with the spec starts found in the source itself, before any parse, by a
  scanner that follows Python's strings, their prefixes, brackets and replacement fields, a `:` in a field and in no
  bracket inside it starting the spec; or with tree-sitter-python reading the spec as Python does.
- **`the_budget_running_out_is_a_finding` fails on a loaded machine.** `crates/gate/tests/surviving_mutants.rs`: the
  test gives cargo-mutants 20 seconds, of which the stop may take 15, so the unmutated baseline, a build and a test
  run of 2 seconds, has 5. With two other runs of the gate's tests on the machine (load average 16 to 18) the
  baseline did not end in them, and the finding was "budget reached before cargo-mutants had tested the unmutated
  baseline" where the test expects "budget reached after ". Seen twice in `cargo nextest run --workspace` on
  2026-11-10 at `bfb184b`; alone, on the same machine and under the same load, the test passed. Seen once more that
  day at a load average of 4 to 6, while a Jenkins build of another repository compiled. Not seen in a Jenkins build
  of this one. Closes with a budget the baseline fits in whatever the load, or a test that does not depend on how
  long a build takes.
- **This repo's own Rust is not mutation-tested: the workspace's tests outlast cargo-mutants' `--timeout`.** The gate
  gives cargo-mutants `--timeout 60` (`TEST_TIMEOUT` in `crates/gate/src/mutants.rs`), which holds the unmutated
  baseline's test run too, and `cargo nextest run --workspace` takes about 165 seconds since gate-standalone/16 added
  the tests that run pytest and mutmut (`crates/gate/tests/flaky_tests.rs`, `python_at_base.rs`, `python_mutants.rs`).
  So `scripts/gate` reports "the unmutated baseline of the workspace at `.` did not pass (Timeout, as cargo-mutants
  words it ...), so it tried no mutant", a warning, on every change to the workspace's Rust. Seen on 2026-11-10 with
  `scripts/gate origin/main` at `standalone-16`. As cross-review 14 put it (F6, 2026-11-10): "surviving-mutants judges
  none of `crates/gate` from this merge on, the 2,700 lines of check code added here included. The diff records it in
  TODO.md instead of keeping the slow tests out of the mutants' runs." Left out, those tests would leave every mutant of
  the Python checks they alone cover to survive. Closes with a baseline the timeout does not hold, or with a nextest
  profile for the mutants' runs that leaves the slow tests out, either a change to what the gate runs.
- **The gate's copies still hold other linked configuration as links.** `own_pyproject`
  (`crates/gate/src/python_mutants.rs`) makes only mutmut's copy's `pyproject.toml` a file. As cross-review 14 noted
  (round 4, 2026-11-10, not recorded as a finding): "the other files `pytest::inifile` looks for (pytest.toml,
  pytest.ini, tox.ini, setup.cfg; `pytest.rs:182`) are still copied as the tree's links, so one that is a relative link
  out of the tree is taken for no configuration in mutmut's copy, as F16 was for pyproject.toml; and flaky-tests' and
  passes-at-base's copies still hold a linked pyproject.toml as the link." And `own_pyproject` "reads what the tree's
  link names with `std::fs::read`, whole and whatever it is (a FIFO, a device)". Not seen in a project. Closes with
  every copy the Python checks make holding the configuration files pytest reads as regular files, read as the rules'
  own file is.
- **The gate stops what it started on SIGINT alone.** `catch_interrupt` (`crates/gate/src/process.rs`) catches no other
  signal, and the gate runs each tool in a session of its own, so a gate stopped with SIGTERM, as `timeout` stops a
  command, leaves its tools running. Found by cross-review 15 (finding 1 and its rounds 2 to 5, 2026-11-10) under each
  form of a time limit for the run of the Rust gate beside gate.py that the builds made until the cutover. Closes with
  the gate stopping what it started on SIGTERM too.
- **The gate's Python checks: six design nits of cross-review 14 (2026-11-10), in its words.**
  - F7, `crates/gate/src/python_mutants.rs`: "`mark_for_mutmut` reads, parses and rewrites every regular `*.py` in the
    copy, though mutmut makes mutants only under `[tool.mutmut]` source_paths, which `outside_mutmut` already tells."
    Closes with only the files mutmut mutates marked.
  - F8, `crates/gate/src/parser_checks.rs`: "`changed_units` reads and parses each changed Python file again after
    `Sources::read` did, and is reached three times a run (flaky_tests, python_at_base, python_mutants); its "could not
    be read" branch is all but dead." Closes with the three checks given the units read once.
  - F9, `crates/gate/src/python_at_base.rs`, `mutants.rs`: "The Python halves and the Rust halves' modules import each
    other (`passes_at_base` <-> `python_at_base` for `budget_reached`; `mutants` <-> `python_mutants` for `KILL_WAIT`,
    `STOP_GRACE`, `SURVIVOR_EXAMPLES`), consts widened to `pub(crate)` to cross the cycle." Closes with what both halves
    of a check share in a module neither half is.
  - F10, `crates/gate/src/python_mutants.rs`, `on_copy`: "The set-up closure returns `Result<Option<Table>, String>` and
    also pushes into the captured `out`, which the `Ok(None)` arm then discards; correct only because nothing is pushed
    on that path. `seconds` goes u64 -> Duration -> f64 -> Duration between `mutants.rs` and here." Closes with the
    set-up returning its findings, and the budget one type from the settings to the run.
  - F11, `crates/gate/src/python_mutants.rs`: "`narrowed_table` reads a `pytest --collect-only` listing itself (and
    words "printed no node id the gate could read") beside `pytest.rs`, which owns that reading, with a different
    filter." Closes with one reader of a listing in `pytest.rs`.
  - F12, `crates/gate/src/glob.rs`: "`fnmatch` arrives as an `across_slashes` flag through `translate_name` and
    `bracket` with two added match arms, the bolted-on flag the user's CLAUDE.md design rule names; it also compiles a
    Regex on each call." Closes with fnmatch's translation a function of its own, compiled once a pattern.
- **`scripts/gate` still knows the name of the gate in release 0.6.0's tarball.** `released_gate` in `scripts/gate`
  falls back to `gate-linux-x86_64` where a release has no `gate`, since 0.6.0, the one release before the cutover that
  ships the Rust gate, names it for its platform, and `ci/gate.pin` may still name that release. Found with
  gate-standalone/18 (2026-11-11). Closes once `main`'s `ci/gate.pin` is 0.7.0 or later and no rollback to 0.6.0 is
  wanted, with the fallback deleted, and `a_release_from_before_the_cutover_names_its_gate_for_its_platform`
  (`crates/gate/tests/repo_scripts.rs`) with it.
- **The Python the gate embeds has no tests of its own, and is not mutation-tested.**
  `crates/gate/src/_octoflow_gate_base_imports.py` and `crates/gate/src/_test_contexts.py`, which the gate runs in a
  judged project's Python, are exercised only through the Rust integration tests that run the gate
  (`crates/gate/tests/python_at_base.rs`, `python_mutants.rs`). No pytest test reaches them, so mutmut has nothing to
  try a mutant of them with: `[tool.mutmut]` is gone from `pyproject.toml`, and surviving-mutants judges no change to
  them. Found with gate-standalone/18 (2026-11-11). Closes with pytest tests of the two files, a pytest `[[test]]`
  command in `gate.toml` and a `[tool.mutmut]` that names them, or with the user's word that the Rust tests are enough.
- **`cargo xtask smoke-release` has no tests, and its lines no coverage.** `gate.toml`'s test command leaves `xtask` out
  (`--exclude xtask`), so nothing tests `smoke_release` (`xtask/src/main.rs`) and the gate measures no coverage of a
  change to it; it only runs, once a build, in the Jenkinsfile's Package stage, on a release it accepts. Found with
  gate-standalone/18 (2026-11-11). Closes with tests of it, a release it must refuse among them, and `xtask` taken into
  the test command.
- **No test validates the golden reports against `schema/quality-v1.json`.** Only `cargo xtask smoke-release` validates
  a quality.json against the schema, and only one: the report of the release tarball's run on its fixture project. The
  golden files `tests/golden/*.json` are held to the gate's output (`crates/gate/tests/golden_reports.rs`,
  `gate_cli.rs`), but nothing checks that they follow the schema, as the Python suite did until 0.6.0; a field the
  schema lacks, or one it requires and a report shape leaves out, would show only if the fixture project's report has
  that shape. Found with gate-standalone/18 (2026-11-11). Closes with a test that validates each golden JSON against the
  schema: in `xtask`, the one crate the `jsonschema` crate is allowed for (spec decision 7), or in `crates/gate` with
  the user's nod for the dev-dependency.
- **An edited build-configuration file whose name git quotes may go unnamed.** `touched_paths` in
  `crates/gate/src/config_edits.rs` does not unquote a name git wrote in quotes: after `rename from` it is taken with
  its quotes and escapes, and a `--- "a/…"` line is not read. The gate's diff is made with `core.quotePath=false`
  (`change_diff` in `crates/gate/src/diff.rs`), so a name of bytes above 0x7f is not quoted; one that holds a `"`, a `\`
  or a control character still is, as a `clippy.toml` or a Cargo configuration file in a directory so named. Such a file
  is named under "Build configuration" only while the working tree still has it under that name, not when the change
  deletes it or renames it away. Found with gate-standalone/18 (2026-11-11). Closes with the names read through the diff
  module's own `unquote`.
