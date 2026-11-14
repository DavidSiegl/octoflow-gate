<!-- The languages badge repeats the languages the first sentence below names, and docs/quality-gate.md's "It reads
     Python, Rust and Go": a change to one changes all three. -->
<!-- pyml disable-next-line first-line-heading -->
<p align="center">
  <a href="https://github.com/DavidSiegl/octoflow-gate/releases/latest"><img src="https://img.shields.io/github/v/release/DavidSiegl/octoflow-gate" alt="Latest release"></a>
  <a href="https://github.com/DavidSiegl/octoflow-gate/releases"><img src="https://img.shields.io/github/downloads/DavidSiegl/octoflow-gate/total?label=asset%20downloads" alt="Downloads of all release assets"></a>
  <a href="#what-it-judges"><img src="https://img.shields.io/badge/judges-Python%20%7C%20Rust%20%7C%20Go-informational" alt="Judges Python, Rust and Go"></a>
  <a href="https://www.rust-lang.org"><img src="https://img.shields.io/badge/built%20with-Rust-B7410E?logo=rust&logoColor=white" alt="Built with Rust"></a>
  <a href="https://github.com/DavidSiegl/octoflow-gate/commits/main"><img src="https://img.shields.io/github/last-commit/DavidSiegl/octoflow-gate" alt="Last commit"></a>
  <a href="https://ko-fi.com/davidsiegl"><img src="https://img.shields.io/badge/Ko--fi-support%20me-FF5E5B?logo=kofi&logoColor=white" alt="Support me on Ko-fi"></a>
</p>

# octoflow-gate

octoflow-gate is a quality gate for changes to Python, Rust and Go projects. `gate` judges what a git working tree
changed since a commit and writes its report twice: as the quality file, Markdown whose first line is `GATE: OK` or
`GATE: ERROR`, for the people and agents who answer it, and as `quality.json`, the same report as data for a program to
read. It was written for [octoflow](https://github.com/DavidSiegl/octoflow-ai), whose agents answer the quality file in
a loop, and runs in any project as one binary.

## What it judges

It judges the change rather than the codebase: findings on lines the change did not add are counted and never held
against it.

- **Linters** over the files git tracks: ruff and mypy for Python, pymarkdown for Markdown, shellcheck for shell
  scripts, actionlint for GitHub workflows, clippy for each Cargo workspace, golangci-lint for each Go module. Their
  findings on added lines fail the gate, and so does a linter that is missing or fails, or a Cargo workspace or Go
  module that does not compile.
- **Tests and coverage**, from JUnit reports and Cobertura, LCOV or Go coverage reports: a failing test fails the
  gate, and so does coverage below 80% on 20 or more new lines to cover.
- **Checks of what the change did**: tests it weakened (removed, skipped, fewer assertions, expected values changed
  together with the code), functions whose cognitive complexity it took over 15, imports it added that close a cycle
  between the project's Python or Rust modules, and, as findings to weigh that do not fail the gate, tests that are
  flaky, tests that pass without the change, surviving mutants of the functions it edits, vague names it binds, and
  blocks of code it adds that the tree already holds, names and literals aside. Go gets all of these but three: tests
  are rerun for flakiness in Python alone, and run without the change in Python and Rust, and Go's compiler refuses an
  import cycle itself.
- **What to read**: the suppressions it added and the build configuration it edited.

[docs/quality-gate.md](docs/quality-gate.md) says what each of these judges and why.

## Running it

```bash
gate --base REV [--config GATE_TOML] [--tests JUNIT]... [--coverage XML]... [--out FILE] [--json FILE]
     [--mutation-budget SECONDS] [--base-run-budget SECONDS] [--test-contexts COVERAGE] [--python PATH]
```

`--base` is the commit the change is measured from; the working tree of the git repository around the current
directory is the change. The quality file goes to `--out`, else to standard output; `quality.json` to `--json`, else
beside `--out`'s file, its `.md` made `.json` (`quality.md` gives `quality.json`) or `.json` added. Each file is
written whole or not at all. `gate --help` describes each option.

| Exit status | Means |
|---|---|
| 0 | `GATE: OK` |
| 1 | `GATE: ERROR` |
| 2 | it could not judge the change, or its arguments were wrong; it wrote neither file |
| 130 | Ctrl-C or SIGTERM stopped it; it wrote neither file |

### Reading the report from a program

`quality.json` is the gate's interface for a program that integrates it, described by the JSON Schema
[schema/quality-v1.json](schema/quality-v1.json): the gate, its failed conditions, every finding with whether it fails
the gate and a fingerprint that survives the finding moving, the failing tests, coverage of new code, the suppressions
added and the build configuration edited. Its `schema_version` is raised on any change a reader could notice; a new
optional field does not raise it, nor does a change to the words of a finding's message or a failing test's detail.
The quality file is prose, whose wording may change in any release: read the JSON, not the Markdown. [The report as a contract](docs/quality-gate.md#the-report-as-a-contract) says more.

## Running the gate in a project

A project runs a release, pinned by its version and its sha256. Each
[release](https://github.com/DavidSiegl/octoflow-gate/releases) `v<version>` has two assets:
`octoflow-gate-<version>.tar.gz`, which holds `gate`, built for Linux x86_64, the JSON Schema in `schema/`, golden
quality files in both formats in `golden/`, and this README; and `octoflow-gate-<version>.tar.gz.sha256`, its sha256.
`gate` is one binary, which runs in the project's working tree:

```bash
v=<version> gate=$(mktemp -d)   # outside the working tree, which is the change
release=https://github.com/DavidSiegl/octoflow-gate/releases/download/v$v
curl -fsSL -o "$gate/gate.tar.gz" "$release/octoflow-gate-$v.tar.gz"
echo "<sha256>  $gate/gate.tar.gz" | sha256sum -c -
tar -xzf "$gate/gate.tar.gz" -C "$gate"
"$gate/gate" --base main --out quality.md
```

[ci/fetch-gate.sh](ci/fetch-gate.sh) does the first five lines from a pin file, as this repository's own builds do. A
merge into this repository's `main` that raises its version publishes that version's release; every release stays
published. Releases up to 0.6.0 are judged by `gate.py`, the gate's first form, in Python;
[docs/port-differences.md](docs/port-differences.md) lists where `gate` judges differently, for a project that raises
its pin from one of them.

The release's `gate` is linked against the GNU C library of the Jenkins agent that built it, and does not start on a
machine whose glibc is older; there, and on another platform, build it from this repository at the release's tag:

```bash
cargo build --release --locked --manifest-path path/to/octoflow-gate/Cargo.toml --package gate
```

It runs the project's tests itself, with the commands its `gate.toml` lists as `[[test]]` (below), and judges the
JUnit and coverage reports they write. A CI that has already run the tests passes their reports instead, with
`--tests` and `--coverage`, and then no command runs.

It takes what it runs from the machine and the project: ruff, mypy and pymarkdown from `PATH`, which `uv run` points
at a project's environment, and only for a project that tracks Python or Markdown files; shellcheck, actionlint and
clippy likewise, for shell scripts, GitHub workflows and Rust files. The flaky-tests, passes-at-base and
surviving-mutants checks of a change to Python run pytest, pytest-randomly and mutmut through the project's Python,
which `gate.toml`'s `python` or `--python` names, `python3` without either; those of a change to Rust run
cargo-nextest and cargo-mutants, and the surviving-mutants check of a change to Go runs gremlins. golangci-lint, and
the go tool it and gremlins run, come from `PATH` for a project that tracks Go files; a Go project runs its tests with
gotestsum ([Go tests and coverage](docs/quality-gate.md#go-tests-and-coverage)).

A linter that is missing is a failed condition naming it, and for clippy that is a `cargo clippy --version` that
fails, as it does where rustup's `clippy` component is not installed; a check that cannot run is one of its own
findings. The rules of ruff, mypy, pymarkdown and mutmut come from the `pyproject.toml` beside the project's
`gate.toml`, and are the tools' defaults where there is none, as [whose rules judge a
change](docs/quality-gate.md#whose-rules-judge-a-change) says.

## Settings: `gate.toml`

Which files are tests, which go with them, which hold expected output, which the complexity check leaves alone, which
take part in no dependency cycle, which duplicated-blocks leaves alone and the shortest block it reports, which further
files are build configuration, which files each linter reads, the checks' time budgets and the commands that run the
tests: a `gate.toml` at the project's top level says each, or the
file `--config` names. Every key has a default, so a project without one is judged as the gate always judged; [the
gate's settings](docs/quality-gate.md#the-gates-settings) lists them. An unknown key or a value of the wrong kind
exits 2.

octoflow's, for example:

```toml
# Its tests are where the defaults look: Rust's under engine/tests/, in tests.rs files and in #[cfg(test)] modules,
# its golden files under tests/golden/.

[build-config]
Jenkinsfile = "The quality job reads it from `main`, so this analysis ran without the edit."
"ci/gate.pin" = "A quality build takes the gate release from `main`'s pin, so this analysis ran without the edit."

# A script, since a command is an argv with the gate's environment and octoflow's test run sets variables of its own.
[[test]]
name = "cargo-nextest"
command = ["sh", "ci/rust-tests.sh"]
junit = ["target/nextest/ci/junit.xml"]
coverage = ["coverage-rust.xml"]

# The defaults, written out: a quality round's timeout is sized for them.
[budgets]
mutation = 600
base-run = 120
```

A CI that passes `main`'s `gate.toml` with `--config` judges by it and by the rules of the `pyproject.toml` beside it,
or by the tools' defaults where there is none, never by the change's own, so a change cannot loosen how it is judged:
put `main`'s `pyproject.toml`, if it has one, beside `main`'s `gate.toml`.

## License

MIT or Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).
