# TODO

What is known to be open in the gate (`gate.py`), its Rust helper (`gate-rust/`) and their CI, and why. A fixed entry
is deleted; git history keeps what was done. An entry that a fix only narrows is rewritten to say what is left.

The entries that say "moved from octoflow-ai" were written while the gate lived there; the octoflow paths they cite
(`engine/`, `ui/`, `.scratch/`) are octoflow-ai's.

## Open issues

- **A project with `gate.toml` still needs a `pyproject.toml`.** `judge_change` refuses to judge without the rules file
  (`no rules: ... is not a file`), though a Rust-only project's linter, clippy, takes its rules from Cargo's files.
  Found with gate-standalone/03 (2026-11-06). Closes with the rules file optional when no linter that reads it runs.
- **The Rust helpers take the whole `GateSettings` to ask one question of it.** `_rust_tests`, `_rust_skips`,
  `_rust_swallowed_errors`, `_rust_test_documents`, `_touched_rust_tests`, `_rust_scored_functions` and the helpers
  around them in `gate.py` take `settings` only to pass it down to `_in_rust_tests`. Raised by cross-review 02 of
  octoflow-gate (F6, 2026-11-06): "A pass-through parameter by deep-modules' rule; a `rust_test` predicate, or the
  per-file boolean as `_rust_attribute_place` already takes `test_file`, would keep the signatures." Left for the Rust
  port (gate-standalone/12–14), which rewrites these helpers. Closes with the checks given which files are Rust tests
  once, rather than the settings that decide it.

- **`gate.py` and docs/quality-gate.md still speak in octoflow's words.** The quality file is written for the
  Builder to answer and the Reviewer to judge (`quality_report`'s docstring, `TO_WEIGH_NOTE`'s comment, the
  suppressions' comment), `FAILED_CONDITIONS`' comment and `one_line`'s docstring name the orchestrator as the reader,
  and docs/quality-gate.md cites octoflow's pull requests, its Python engine and its `QUALITY_TIMEOUT` where a decision
  was measured; docs/quality-gate.md's intro says so. Found by gate-standalone/02 (2026-11-05), which moved the gate
  here and stripped only the assumptions its ticket named. Closes with the comments naming "the quality file's
  reader", and the measurements read as history, once `quality.json` (gate-standalone/04) settles who reads what.
- **Test code outside the places the spec names is judged as code.** The spec of the Rust checks (requirement 2)
  places Rust's tests in a `tests/` directory, a `tests.rs` and an inline `#[cfg(test)]` module; `_in_rust_tests` and
  `GateSettings.rust_test` in `gate.py`, which both Rust checks read, follow it by default (test-integrity's side of this is the
  out-of-line `mod name;` item in its list below). So `cognitive-complexity` judges a file that a parent takes in
  with `#[cfg(test)] mod name;`, as the engine keeps its fakes and `test_support.rs` (`engine/src/host.rs:24` on
  rust-engine): at rust-engine `476b770`, 3 of the 21 engine functions over the limit are theirs (`FakeState::git`,
  `FakeJenkinsState::answer`, `helper_cases`). Found by gate-rust-checks ticket 03 (2026-10-30). And, as cross-review
  108 put it (F4): "A file-level `#[cfg(test)] fn` or `#[cfg(test)] impl`, or a `#[test]` fn outside the places
  requirement 2 names, is judged against 15." Closes with the user's ruling on the spec's definition, and, if those
  are tests, the helper listing each out-of-line `mod name;` with its `cfg_test`, so that the gate can follow a
  declaration to its file, and a `cfg_test` of each fn's own and its impl's attributes. Until then a project can name
  such files in its `gate.toml`'s `test-files.rust` and `complexity.exempt`. Moved from octoflow-ai on 2026-11-05.
- **A macro whose arguments are no expressions separated by commas adds nothing to a Rust fn's score.** The helper's
  `flow` reads a macro's arguments only where they parse that way (`FlowWalker::visit_macro` in
  `gate-rust/src/main.rs`), as its `asserts` do. Raised by cross-review 108 (F3, 2026-10-30): "A macro whose
  arguments do not parse as comma-separated expressions (`vec![if a {..} else {..}; n]`, `matches!(x, P if a && b)`,
  `tracing::info!(target: "x", ..)`, `select!{..}`) is dropped silently, so its branches, loops and closures add
  nothing to the score." Clippy's own `cognitive_complexity` reads no macro either. Closes with the helper parsing the
  common other shapes (`expr; n`, `expr, pattern if guard`, `name: expr` arguments) and a `macro` node, unread, that
  gate.py can count or report. Moved from octoflow-ai on 2026-11-05.
- **Rust's test code takes no part in pairing a function with its base.** `_rust_scored_functions` leaves the fns
  `_in_rust_tests` places among the tests out on both sides, where a Python file under `tests/` stays in
  `_base_scores`' pool as a file the check does not judge. So a fn moved unchanged into Rust's tests cannot keep its
  own base before a copy of it elsewhere takes it, and that copy passes as the original. Found by gate-rust-checks
  ticket 03 (2026-10-30); since its merge with ticket 02 it holds for test files as for inline test modules. Closes
  with a per-function "exempt" mark on `_ScoredFunction` that `_BasePool` reads where it now asks
  `_complexity_exempt` of a path. Moved from octoflow-ai on 2026-11-05.
- **A Rust test's own lines count toward the coverage of new code.** cargo-llvm-cov's report lists the lines of a
  `#[cfg(test)]` module as it lists the code's (docs/quality-gate.md, "Rust tests and coverage"), and a passing
  test runs them all, so a change that adds a Rust function and a test of it is judged on both: 15 untested lines
  beside a 10-line test read as 40%, not 0%. coverage.py's report of the Python has only `gate`.
  Found while adding Rust coverage (rust-engine ticket 03, 2026-10-27). Closes with test modules left out of the
  report, by `#[coverage(off)]` once it is stable or by the tests moving to files that `--ignore-filename-regex`
  can leave out, or with `gate.py` skipping the lines a `#[cfg(test)]` item spans. The Rust engine's tests are in
  such files already (`engine/src/**/tests.rs`, `engine/src/prompt_parity.rs`, `engine/src/test_support.rs`); the
  desktop shell's are a `#[cfg(test)]` module in `ui/src-tauri/src/lib.rs`. That module also makes a change to the
  desktop shell's tests alone need a version raise, and a place in the one-bumping-PR-at-a-time queue:
  `ci/version-raised.sh` exempts `**/tests.rs`, not `lib.rs` (cross-review 117, F4, 2026-11-03). Moving it to
  `ui/src-tauri/src/tests.rs` closes both: tried by ui-test-text-file-busy/01 (2026-11-03) and dropped then because
  the gate failed the move on the tests' own `cfg`s (next entry). Their `cfg`s all name platforms, which the gate
  now weighs rather than fails (`platform-only`, gate-platform-cfg/01), so the move is open again.
  Moved from octoflow-ai on 2026-11-05.
- **The gate takes a moved Rust test's own skip for one the change added.** `_rust_skips` (`gate.py`) reports a
  skipping attribute on any line the change added, so a test moved to another file with its attributes is reported
  though nothing new is skipped: a platform `cfg` (`#[cfg(unix)]`) as a `platform-only` finding to weigh, any other
  (`#[ignore]`, `#[cfg(feature = "slow")]`) as a `test-integrity skip` error; the `removed-test` check follows the
  same move by the test's `body_digest` (docs/quality-gate.md, "Rust tests"). Found 2026-11-03 moving the desktop
  shell's 26 tests out of `ui/src-tauri/src/lib.rs` (ui-test-text-file-busy/01): six errors, one for each `cfg` the
  tests carried, which since gate-platform-cfg/01 (2026-11-04) are six findings to weigh. Closes with a skip
  attribute on a test matched to the base's test of the same body digest counted only when that test did not carry
  it. Moved from octoflow-ai on 2026-11-05.
- **A misread literal on an added attribute line can report a test's existing skip as added.** `_rust_attribute`
  (`gate.py`) lexes only plain strings, so a char literal (`'"'`) or a raw string ending in `\` (`r"C:\"`) can leave
  an attribute open until a `]` inside a later string, many lines down; `_rust_line_attributes` then reads every `#[`
  on the lines in between as the added line's. So a change that adds only `#[doc = r"C:\"]` above an existing
  `mod tests` with an `#[ignore]`d test whose body holds `"a]b"` fails the build with a `test-integrity skip` error
  naming that `#[ignore]`. It fails loudly and hides nothing. Separately, `_rust_line_attributes`'s "cannot hide a
  skip after it" holds only on the line where an attribute starts: a misread literal on a wrapped attribute's
  continuation line can still hide a skip later on that line, as `#[cfg(miri)]` in `doc = "x")] #[cfg(miri)]` after
  `#[cfg_attr(any(), doc = ']',`. Raised by cross-review 136 (R5-F1, should-fix) and deferred by the user
  (2026-11-04). Closes with either of the reviewer's fixes: read the lines in between only when the attribute's text
  holds no `"` or `'`; or bound how far an attribute may span, to the lines before the next one that starts with `#[`
  or an item keyword. Moved from octoflow-ai on 2026-11-05.
- **`GateChange.rust_files` starts one `git cat-file` per base `.rs` file.** `_rust_paths` in `gate.py` copies each
  base side out of git with its own process, so a change to N Rust files starts N git processes before the helper's
  one run; about 5 ms each here. Raised by cross-review 106 (F5, 2026-10-30): "one `cat-file --batch` or `git archive`
  would do." Closes with one `git cat-file --batch` over all the base paths. Moved from octoflow-ai on 2026-11-05.
- **test-integrity's Rust detector does not see some weakenings that Python's does, or that its rules name.** Found
  while writing it (gate-rust-checks ticket 02, 2026-10-30); docs/quality-gate.md, "Rust tests", lists each:
  - an expected value held in a `const` or `static` the change edits: `_changed_constants` follows Python's
    module-level constants, and the Rust side has no counterpart. Closes with the helper listing a file's `const`s
    and `static`s with a digest of their expressions, and `rust_weakened_tests` comparing those an assertion names;
  - a `Result` thrown away that holds no assertion macro and no `catch_unwind`, such as `let _ = check();` where
    `check` returns the `Err` that should fail the test, and a `catch_unwind` result bound to a name and never read.
    The helper knows no types, so `_rust_swallowed_errors` cannot tell such a value from a cleanup's. Closes with a
    rule on the text the change replaced, such as a `.unwrap()`, `.expect(...)` or `?` at the base on the same
    expression, which the helper would need to report;
  - a skipping attribute that does not start its line, or a multi-line `cfg_attr` whose `ignore` is on a later line,
    in a test's body or on a module (`_rust_skips` reads those lines' text; a test's own attributes come whole from
    the helper). Closes with the helper listing the attributes of modules and of statements in a test, which raises
    its schema.
  - a test module in a file of its own that a `#[cfg(test)] mod name;` names, where that file is neither under
    `tests/` nor `tests.rs`, as `mod cli_tests;`: `_rust_test_file` reads a path alone, as requirement 2 of the spec
    defines the files, so that file's tests are not read and its edits count as code. Raised by cross-review 107
    (2026-10-30, not recorded). Closes with the helper listing each `mod name;` with its `cfg_test`, and the check
    resolving the file it names as rustc does (`name.rs` or `name/mod.rs` beside the file, or a `#[path]`).
  - removing the first of two tests of one name in one file, as under two platforms' `cfg`s: `_RustTest.namesake`
    numbers them in order, so the second then takes the first's number and is compared with the removed one's base,
    which reads as a fewer-assertions or a changed literal on it, or as nothing, rather than as a removed test. Raised
    by cross-review 107 (round 2, not recorded). Closes with namesakes matched by their `cfg` attributes, or by body
    first as removed tests are. Moved from octoflow-ai on 2026-11-05.
- **`BUILD_CONFIG_FILES` does not list `gate.py`.** A pull request or main build runs its own tree's `gate.py`, so a
  change that loosens the gate gets no "Build configuration" notice in its quality file; `orchestrator.py`, which
  held the gate, was not listed either. Raised by cross-review 61 (F7, 2026-10-27). Closes with `gate.py` listed,
  with a notice of its own wording (a quality build runs main's gate, not the edit).
  Moved from octoflow-ai on 2026-11-05.
- **surviving-mutants takes only the top directory's pytest configuration.** Since `_pytest_groups`, flaky-tests and
  passes-at-base run each changed test under the configuration pytest finds for it below the copy's top, but
  `_pytest_config`, which surviving-mutants gives mutmut, still looks in the top directory alone, so a project whose
  configuration is in `tests/pytest.ini` or `pkg/pyproject.toml` runs its mutants without that configuration's
  `pythonpath`, `addopts` and plugins. Raised by cross-review 35 (R1-6). Closes with mutmut's runs taking the changed
  functions' tests' configurations, which mutmut's one `pytest_add_cli_args` makes one per run at most.
  Moved from octoflow-ai on 2026-11-05.
- **flaky-tests' reruns get one time limit a run, whatever the number of configurations.** Each pytest run of
  flaky-tests has `FLAKY_RUN_TIMEOUT` of its own, and since `_pytest_groups` a change whose tests fall under several
  configurations collects and reruns once for each, so its worst case grows with them (3 configurations and 5 seeds:
  3 x 6 x 30 s), against `QUALITY_TIMEOUT`. passes-at-base's groups share its one budget. Raised by cross-review 35
  (R1-5), which reads the spec's "the per-group runs share the check's time budget" as meant for passes-at-base,
  flaky-tests having none. Decided against (2026-10-27,
  `.scratch/gate-open-decisions/issues/01-shared-time-budget.md`): a time budget shared by the reruns, their groups
  and mutmut; the 30 s limit and mutmut's 600 s stay, and `not-rerun` showing up often would be a question of its own.
  Moved from octoflow-ai on 2026-11-05.
- **`TestGate.test_a_missing_tool_fails_naming_it` fails where shellcheck lives beside git.** `TestGate.setUp`
  (`tests/test_gate.py`) puts git's own directory on `PATH` after the stubs, so on a machine with
  `/usr/bin/shellcheck` (agentbox, Debian's package) removing the stub still finds the real one and the gate passes.
  Seen 2026-10-25 on `main` at `9448612`. On such a machine it also fails mutmut's stats run, which runs every test,
  so `surviving-mutants` is `not-run` there, "failed to collect stats" (2026-10-27). Closes with `PATH` holding a
  directory with only a link to `git`. Moved from octoflow-ai on 2026-11-05.
- **A branch build from a root commit cannot be judged.** As cross-review 01 put it (F2, 2026-11-06): "The
  root-commit branch of the Change stage sets GATE_BASE to a commit of the empty tree. `scripts/gate` exits 2 for any
  base without gate.py, so a build on that path always fails with 'the gate could not judge the change (exit status
  2)'." Reachable only for a branch build whose HEAD is an orphan commit with no earlier successful build. The
  Jenkinsfile is the agent-estate template's, whose root-commit base suits a gate that does not judge with the base's
  own code. Closes with `scripts/gate` deciding what judges a change that has no gate before it.
- **`scripts/gate` takes any `WORKSPACE` for a Jenkins build.** As cross-review 01 put it (F5, 2026-11-06): "In a dev
  container that exports `WORKSPACE` but not `WORKSPACE_TMP`, a local run exits 2. If both are set, it moves
  CARGO_HOME, empties `$WORKSPACE_TMP/tmp` and switches to the `ci` Hypothesis profile." Closes with a variable only
  a Jenkins build sets deciding it, once one is confirmed on the agent.
- **Under the gate's `--cap-lints warn`, an `#[allow]` overrides a workspace's `forbid`.** `Clippy._lint_workspace` in
  `gate.py` caps every lint at warn so that a lint at deny cannot stop cargo before a compile error in a later crate
  (cross-review 66, F1); rustc's cap also lowers `forbid`, so an added `#[allow(unsafe_code)]` under `unsafe_code =
  "forbid"` no longer gives E0453 and its unsafe block is no finding: the gate is OK on a change the workspace's rules
  make uncompilable, and only "Suppressions added" lists the allow, and not even that when the change adds the lint as
  one line inside an existing wrapped `#[allow(\n    a,\n    b,\n)]`, since `_RUST_SUPPRESSION` reads one added line at
  a time (F15). Nothing in this repository sets `forbid` (the top-level `Cargo.toml`'s `[workspace.lints]` are all
  `warn`), and the Test stage's uncapped `cargo llvm-cov nextest` and the UI stage's `cargo check` would still fail
  such a build of the workspace.
  Raised by cross-review 66 (F11, 2026-10-27): "The gate is OK on a change that the workspace's rules make
  uncompilable". Closes with a second, uncapped clippy run whose rustc errors (codes `E` and four digits, such as E0453)
  are compile errors, or with the forbidden lints read from the manifests and an added allow of one failing the gate.
  Moved from octoflow-ai on 2026-11-05.
- **`expected-value-changed` misses an inline literal's sign, type or structure.** `_changed_literals` pairs
  assertions by `_shape` and compares `_literals`, which drops types and operators: `assert f() == -5` becoming
  `== 5` changes the shape (no pair, no finding), and `== 1` becoming `== True` gives equal literal lists
  (`[1] == [True]`). Raised by the cross-review of `test-integrity-constants` (F4, 2026-10-27): "`_literals`
  equality misses sign, type and structure changes"; the constant path compares `_tree_fingerprint`s instead.
  Closes with the inline path comparing typed literals and pairing a unary minus with its operand.
  Moved from octoflow-ai on 2026-11-05.
- **The gate's intro writes the rules and settings paths (`--config`) as they are.** `judge_change`'s intro names the
  rules file and the settings file without `one_line` (`gate.py`, `_intro`), so a path
  holding a line breaker splits the
  quality file's third line, and `docs/quality-gate.md`'s "wherever it appears in a list item or a name" says more
  than holds for it. `gate_failures` does not read the intro, so no round is misjudged. Raised by cross-review 38
  (round 3, 2026-10-26). Closes with the intro written through `one_line`, or with each quality-file writer escaping
  once where it joins its lines. Moved from octoflow-ai on 2026-11-05.
- **Small leftovers in the changed-file walker and its decorators.** Raised by cross-review 45 (F2-F5, 2026-10-26),
  none giving a wrong result: `_ChangedFunction.decorators` unparses each decorator only for `_unmutatable`'s
  `staticmethod`/`classmethod` test, which an `ast.Name` check would make without `_unparsed`; `_parsed_changed_files`
  computes `_removal_points` for `_judged_files` and `vague_names`, which never read them; `_ChangedPythonFile` is a
  frozen dataclass with `set` fields, so its generated `__hash__` raises; `vague_names` unpacks three fields and reads
  `.why` beside them. Closes with the four changed, in one commit, when the walker is next touched.
  Moved from octoflow-ai on 2026-11-05.
- **A flaky-tests rerun sometimes waits out `FLAKY_RUN_TIMEOUT` on Jenkins.**
  `TestFlakyTests.test_a_server_a_test_leaves_holding_the_output_does_not_hold_up_the_reruns` takes 8.0 s on
  Jenkins (PR-98, PR-99/3, PR-100), but 14.4 s on PR-101 build 1 and 31.0 s on build 3, failing its 30 s bound: one
  of its six pytest runs waited about the whole `FLAKY_RUN_TIMEOUT`. PR-101 changes only the tests' git fixture.
  Twenty runs here under the load of a parallel suite, with SIGINT ignored, all passed. A rerun that hangs gives a
  `not-rerun` warning, not a wrong verdict. Closes when the run that waits is caught, e.g. with each pytest run's
  wall time and exit in the test's failure message, and its cause fixed. A second cause, of a different kind: on
  PR-190 build 3 (gate-rust-checks 04–06) the reruns of the changed tests did not finish within the 30 s at all,
  since those tests include dozens that build a small Cargo package with real cargo, cargo-nextest or
  cargo-mutants, each from a cold target directory in the rerun's copy. A change that adds many such tests is not
  rerun; that one closes with a limit that grows with the tests rerun (a time budget shared with mutmut was decided
  against, "flaky-tests' reruns get one time limit a run", above). Moved from octoflow-ai on 2026-11-05.
- **`vague-names` does not split CamelCase in Python.** `vague_names` matches a whole Python name or its last
  `_`-separated word, so `user_data` is found but a class `InfoManager` or `UserData` is not; in Rust the last CamelCase
  word is matched too (`_is_vague_in_rust`, gate-rust-checks ticket 04), which is Rust's counterpart. Closes, if the run
  metrics show such names recur, with Python's matched the same way. Moved from octoflow-ai on 2026-11-05.
- **`vague-names` reports a Rust trait impl's associated types and constants.** Only the name and parameters of a `fn`
  an `impl Trait for T` holds are skipped (`_rust_fixed_bindings`), as gate-rust-checks ticket 04 asked; a `type Value`
  or `const STATUS` the trait fixes is reported, as in serde's `impl Visitor for V { type Value = ...; }`. The helper's
  `bindings` give no holder, so the gate cannot tell them apart from an inherent `impl`'s. Found by ticket 04
  (2026-10-30). Closes with the helper marking each binding an `impl Trait for T` holds directly, a schema change.
  Moved from octoflow-ai on 2026-11-05.
- **`passes-at-base` builds a Rust package's dependencies anew on every run.** `_rust_base_run_findings` gives each run
  a target directory in its scratch directory, outside the copy, so every gate run compiles the base's dependencies
  before it runs a test, within `--base-run-budget`; on a package of many crates that may use the whole budget, which
  is a `budget-reached` finding. Not measured on Jenkins: gate-rust-checks ticket 05 asked for the Gate stage's added
  time, which needs a Jenkins build of a Rust change on the branch. Decided against for now: the working tree's own
  `target/`, where the base's build would overwrite the binaries Cargo copies up to `target/debug`. Closes, if the
  budget runs out on real changes, with a target directory that lasts between gate runs outside every workspace, as
  `ci/cargo-lock.sh`'s lock file does. Moved from octoflow-ai on 2026-11-05.
- **`surviving-mutants` splits its budget by language, not by work, and builds Rust anew.** `surviving_mutants` gives
  mutmut half the `--mutation-budget` when the change has Rust to mutate too, and cargo-mutants
  (`_cargo_mutants_on_copy`) what is left by the check's deadline, however many functions each language has; and
  cargo-mutants runs `--in-place` in a copy of its own without `target/`, beside mutmut's, so each run copies the
  tree again and compiles the workspace and its dependencies before its baseline. Not measured on Jenkins: gate-rust-checks ticket 06 asked for the Gate stage's added time on a
  Rust change, which needs a Jenkins build on the branch. Found by ticket 06 (2026-10-30). Closes, if the run metrics
  show either language starved, with the budget split by each language's changed functions, one copy for both tools,
  and a target directory that lasts between gate runs (the same as passes-at-base's above).
  Moved from octoflow-ai on 2026-11-05.
- **cargo-mutants is pinned by the sha256 of its archive as downloaded, not as published.** Its releases publish no
  checksums file (`ci/install-tools.sh`), so the pin of v27.1.0 holds the archive the gate's author downloaded on
  2026-10-30, trusted on first use. Closes with a release that publishes checksums, or the asset digest GitHub shows on
  the release page, checked by hand as cargo-llvm-cov's was. Moved from octoflow-ai on 2026-11-05.
- **`passes-at-base` places a Rust test's target by Cargo's default layout only.** `_rust_target` reads the file's
  place: a target a manifest puts elsewhere (`[[test]] path = ...`, `autotests = false`), a module a `#[path]`
  attribute names, a module of `src/main.rs` in a package that also has `src/lib.rs` (taken for the library's), and a
  file under `tests/x/` that `tests/x/main.rs` does not declare are named wrong, so nextest reports no such test and
  the check gives nothing; a file under `benches/` or `examples/`, or under a `tests/x/` with no `main.rs`, as a
  shared `tests/common/mod.rs`, is a `not-run` finding. Found by gate-rust-checks
  ticket 05 (2026-10-30). Closes with the targets read from `cargo metadata` and the module tree from the helper's
  `mod name;` items. Moved from octoflow-ai on 2026-11-05.
- **`passes-at-base` cannot tell a Rust test that needs the change from a base that does not build.** A target whose
  tests do not compile at the base counts as failing there, whatever rustc's error is (`_NEXTEST_BUILD_FAILED` and
  `error: could not compile` in `_rust_target_findings`): the test reaching for what the change adds, as the ticket
  meant, but also a base that does not compile on the gate's machine at all. Any other cargo error, as a lock file
  `--locked` refuses, is a `not-run` finding. Python's run tells the two apart by whether the failure passes through
  a file the change edits (`_outside_failed_conftest`). The ticket asked for the build failure to be said "in the
  finding-free summary, not as a finding"; the quality file has no such summary for a check, so docs/quality-gate.md
  says it instead. Found by ticket 05 (2026-10-30). Closes with cargo's JSON messages read for the files the errors
  are in, and the user's ruling on where the summary goes. Moved from octoflow-ai on 2026-11-05.
- **`vague-names` skips the parameters of a Rust trait impl's `fn`, which Rust lets the impl rename.** Unlike Python's
  keyword arguments, a Rust caller never names a parameter, so the trait fixes only the `fn`'s name; gate-rust-checks
  ticket 04 asked for the parameters to be skipped too, as `@override`'s are, and clippy's `renamed_function_params`
  (a restriction lint) holds an impl to the trait's names. So a `raw_data` parameter of an `impl Opener for Door` `fn`
  is not reported. Raised by the code review of ticket 04 (2026-10-30). Closes with the user's ruling on whether the
  parameters count as fixed. Moved from octoflow-ai on 2026-11-05.
- **`vague-names` reports a name an undecorated override cannot change.** A `def handle(self)` in a
  `socketserver.BaseRequestHandler` subclass, or a parameter the overridden signature names, has no fix; only a
  method marked `@override` is skipped (`_bound_names`). Raised by the cross-review of `written-rules` (F6,
  2026-10-24): "names fixed by a parent class or framework are reported, though they cannot be renamed". Closes, if
  the run metrics show it is common, with methods whose class has a base from outside the repository left out.
  Moved from octoflow-ai on 2026-11-05.
- **`passes-at-base` lists the whole tree of the base, and of each submodule, to find its submodules.**
  `_unpack_commit` runs `git ls-tree -r -z` on every run, even in a repository without `.gitmodules`, and splits the
  whole listing, only to find the entries of mode 160000. Raised by cross-review 41 (F9, 2026-10-26): "a full
  recursive `ls-tree` on every run, only to find gitlinks". Checking `<commit>:.gitmodules` first would skip a gitlink
  that has no `.gitmodules` entry, such as a nested repository added by mistake, which is filled today. Closes, if run
  metrics show the listing costs, by listing only the paths `.gitmodules` names, with such gitlinks left out.
  Moved from octoflow-ai on 2026-11-05.
- **The copy's `src/` on `PYTHONPATH` comes before the standard library in `passes-at-base`'s run at the base.** A
  module there named as one of the standard library's, such as `src/token.py`, shadows it, as one at the copy's top
  already did through `python -m`. Raised by cross-review 41 (round 1, not recorded, 2026-10-26). Not seen failing.
  Closes, if it is, by putting the roots on the path after the standard library and before site-packages.
  Moved from octoflow-ai on 2026-11-05.
- **A test file that cannot be imported at the base hides all its changed tests from `passes-at-base`.** A
  module-level import of something the change adds makes pytest's collection of the whole file fail at the base,
  which `_collect_at_base` takes as the expected outcome for every changed test in it, including ones that would pass
  there. Accepted: the check reports only, and splitting the file's tests from its imports would mean rewriting
  them. Closes, if its run metrics show these files are common, by importing such names inside the tests that use
  them, or by running each test with the failing import stubbed. Moved from octoflow-ai on 2026-11-05.
- **A submodule under `tests/` whose commit the change moves makes `passes-at-base` not-run.** `_beside_tests`
  takes the submodule's path for a file that goes with the tests, so `_copy_base_with_tests` removes it with
  `os.remove`, which refuses a directory, and the copy fails as a whole (`could not copy the base`). It was so before
  submodules were filled (`_unpack_commit`). Found while filling them (2026-10-26), not seen failing. Closes when the
  overlay leaves a path the base records as a submodule (mode 160000) to `_unpack_commit`.
  Moved from octoflow-ai on 2026-11-05.
- **A test file whose name is not UTF-8 is never rerun by `flaky-tests`.** pytest-randomly 5.0.0 hashes each node
  id as strict UTF-8 (`pytest_randomly._crc32`, called from `pytest_runtest_setup` and its neighbours) and stops
  with an internal error, exit status 3. `_pytest` already lets pytest print such an id
  (`PYTHONIOENCODING=utf-8:surrogateescape`), and the collection runs with `-p no:randomly`, so the id is found;
  each rerun then crashes, and `flaky_tests` reports `not-rerun` quoting the `UnicodeEncodeError`. Raised by the
  cross-review of `flaky-tests` (F10, 2026-10-23). Closes when pytest-randomly hashes the id's bytes, or when the
  reruns can seed such a test without it. Moved from octoflow-ai on 2026-11-05.
- **The reruns' time limit is too short for slow tests on the Jenkins agent.** `FLAKY_RUN_TIMEOUT` is 30 s a run. On
  `flaky-tests`' own change each run took about 9 s on an idle laptop and about twice that under load. Measured on
  Jenkins on 2026-10-24 (PR #50, surviving-mutants' change): the first rerun did not finish within 30 s, so the check
  reported `not-rerun` and judged no test; the tests that change edits start mutmut, and rerunning their four test
  classes once takes about 50 s on the laptop. A limit that covers them (about 90 s, five seeds) would push the quality
  build past `QUALITY_TIMEOUT` beside the mutation budget, so the user kept 30 s (2026-10-24). `not-rerun` is a finding
  to weigh, never a wrong verdict. Closes with lighter tests for the gate's own checks; one time budget the reruns and
  mutmut share was decided against ("flaky-tests' reruns get one time limit a run", above).
  Moved from octoflow-ai on 2026-11-05.
- **The mutation budget leaves most of a large change's mutants untried.** `MUTATION_BUDGET` is 600 s. Measured on
  Jenkins on 2026-10-24: on PR #50 (surviving-mutants' change) mutmut tried 756 of 3079 mutants and the Gate stage took
  683 s, the build about 850 s; on PR #51, 677 of 801 in a 638 s Gate stage. That fits `QUALITY_TIMEOUT` (1200 s,
  `engine/src/constants.rs`) with about 350 s to spare, but not after a wait in Jenkins' queue, which counts against a
  quality round (PR #52 waited about 860 s behind the others). Those were pull request builds; a quality build has
  skipped the Package stage since 2026-10-24, so it has more to spare than that. The user kept 600 s (2026-10-24): a
  stopped run says `budget-reached`, never "none survived". Closes with mutants chosen by the changed lines rather than
  whole functions (a time budget shared with the reruns was decided against, "flaky-tests' reruns get one time limit a
  run", above). On PR #113 (2026-10-27) mutmut was stopped at 600 s after trying 0 of 253 mutants: its three runs of the
  whole suite before the first mutant took the budget. Narrowed (2026-10-28, gate-open-decisions ticket 06): the
  test run records a coverage context per test (`scripts/gate`), and `gate.py --test-contexts` has mutmut run only the
  tests that reach the changed functions and its selection collects (`_recorded_tests`, `_narrowed_table`); replayed
  locally, PR #113's change took mutmut 77 s instead of 709 s, with the same verdicts. Still open: a change to a
  function nearly every test reaches, such as `tool_env`, still costs about the whole suite three times, and a large
  change still has more mutants than the budget tries. Not yet shown on Jenkins: the first pull request build with the
  flag. Moved from octoflow-ai on 2026-11-05.
- **`--test-contexts` collects mutmut's selection in a fuller copy than mutmut runs it in.** `_narrowed_table` runs
  `pytest --collect-only` in the gate's copy of the tree, which holds every tracked file; mutmut's own `mutants/` holds
  only `source_paths`, `also_copy`, `tests/`, `test/` and a few top-level files. A selection that reaches outside those,
  such as none in a project with tests under `other/`, can name a recorded test there that mutmut's pytest cannot find,
  and mutmut's run fails as `not-run`. This repository's selection is `tests/`, so it is not affected. Closes with the
  collection run in a copy laid out as mutmut lays out `mutants/`, or by mutmut itself.
  Moved from octoflow-ai on 2026-11-05.
- **Mutation-testing the code that runs mutmut leaves processes behind.** When mutmut stops a mutant at its own
  timeout it kills the worker, and a real mutmut that `TestSurvivingMutants` started in its own session lives on as
  an orphan group of one process per CPU. It only happens when a change edits that code. Jenkins' process-tree
  killer should end them with the build; not checked. Closes with a check on a PR build, or with that test run
  without its own session under mutmut. Moved from octoflow-ai on 2026-11-05.
- **The surviving-mutants check mutates Rust by added lines only.** cargo-mutants runs `--in-diff`, which mutates only
  what the diff's added lines hold, so a Rust function whose edit only removes lines, such as a guard clause, is not
  mutation-tested; Python's functions count removals (`_edits_function`, gate-open-decisions ticket 03). Closes with the
  Rust half choosing whole functions, as `_edits_function` does, and passing them to cargo-mutants with `--re` or
  `--file` instead of the diff. Moved from octoflow-ai on 2026-11-05.
- **mutmut reads a symlinked `.py` file in the surviving-mutants copy.** `_copy_tracked` copies a tracked symlink
  as a link (`gate.py`), `_mark_for_mutmut` leaves links unmarked, and mutmut 3.8.0's `walk_all_files` lists
  a symlink to a file and `create_mutants_for_file` opens it. So once any changed function starts mutmut, a change
  adding `x.py -> calc.py` has every function of `calc.py` mutated a second time, as `x.py`, spending the budget on
  mutants no finding reports; and `x.py -> /dev/zero` or a FIFO reads without end or blocks until the budget runs
  out. The gate's own selection skips such links (`_parsed_changed_files`). Raised by the cross-review of
  `gate-python-files` (R2-F1, 2026-10-26); rests on mutmut's source, not on a run. Closes with the copy or
  `_mark_for_mutmut` keeping links out of mutmut's walk, without breaking a tracked link the tests import through,
  and a test that also edits a real function. Moved from octoflow-ai on 2026-11-05.
- **A body moved into a file hides a function with that body moved out of it, which is then judged as new.** The
  cognitive-complexity check takes a base function as gone from its file when the file now holds its body fewer times
  than the base did (`_BasePool._gone`), without asking where the working tree's copies came from. So when `calc.py`'s
  `big` moves unchanged to `lib.py` and `other.py`'s `dup`, with the same body, moves unchanged into `calc.py`, `dup`
  takes `big`'s base in `calc.py` and `big` is "a new function": two unchanged moves fail the gate (review 01 round 7,
  R7-F1, a regression from the round-6 rule; deferred by the user, 2026-10-24, as rare: it needs two over-limit
  functions with one body in two of the change's files). Closes with a count that pairs each working copy with the
  base it came from, so an arrival does not make up for a departure. Moved from octoflow-ai on 2026-11-05.
- **A copy of a function whose original the change edits keeps the old score and passes.** The cognitive-complexity
  check pairs identical bodies before names (`_base_scores`), so that a function renamed unchanged keeps its score
  and a new function under its old name is new. A change that edits `big` in place and adds `copied`, a copy of
  `big`'s old body, has the same shape: `copied` takes `big`'s score and passes, and the edited `big` is judged as
  new, so nothing is reported when the edit takes `big` below the limit. The same holds for a copy in a file the
  check does not judge, such as one under `tests/`, since a function moved unchanged there must keep its base from
  a new function under its old name (review round 6, R6-F1). Decided by the user in review round 4 of ticket 06.
  Closes with a way to tell a copy from a rename, such as git's copy detection (`git diff -C`) on the change's
  files. Moved from octoflow-ai on 2026-11-05.
