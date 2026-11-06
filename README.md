# octoflow-gate

A quality gate for a change to a Python or Rust project: `gate.py` judges what a git working tree changed since a
commit and writes its report twice: as the quality file, Markdown whose first line is `GATE: OK` or `GATE: ERROR`,
for the people and agents who answer it, and as `quality.json`, the same report as data for a program to read. It was
written for [octoflow](https://github.com/DavidSiegl/octoflow-ai), whose agents answer the quality file in a loop,
and runs in any project as one stdlib-only Python file.

## What it judges

It judges the change rather than the codebase: findings on lines the change did not add are counted and never held
against it.

- **Linters** over the files git tracks: ruff and mypy for Python, pymarkdown for Markdown, shellcheck for shell
  scripts, actionlint for GitHub workflows, clippy for each Cargo workspace. Their findings on added lines fail the
  gate, and so does a linter that is missing or fails, or a Cargo workspace that does not compile.
- **Tests and coverage**, from the JUnit and Cobertura reports it is given: a failing test fails the gate, and so does
  coverage below 80% on 20 or more new lines to cover.
- **Checks of what the change did**: tests it weakened (removed, skipped, fewer assertions, expected values changed
  together with the code), functions whose cognitive complexity it took over 15, and, as findings to weigh that do not
  fail the gate, tests that are flaky, tests that pass without the change, surviving mutants of the functions it
  edits, and vague names it binds.
- **What to read**: the suppressions it added and the build configuration it edited.

[docs/quality-gate.md](docs/quality-gate.md) says what each of these judges and why.

## Running it

```bash
python gate.py --base REV [--config PYPROJECT] [--config GATE_TOML] [--tests JUNIT]... [--coverage XML]...
               [--out FILE] [--json FILE] [--mutation-budget SECONDS] [--base-run-budget SECONDS]
               [--test-contexts COVERAGE] [--rust-syntax PATH]
```

`--base` is the commit the change is measured from; the working tree of the git repository around the current
directory is the change. The quality file goes to `--out`, else to standard output; `quality.json` to `--json`, else
beside `--out`'s file, its `.md` made `.json` (`quality.md` gives `quality.json`) or `.json` added. Each file is
written whole or not at all. `python gate.py --help` describes each option.

| Exit status | Means |
|---|---|
| 0 | `GATE: OK` |
| 1 | `GATE: ERROR` |
| 2 | it could not judge the change, or its arguments were wrong; it wrote neither file |
| 130 | Ctrl-C stopped it; it wrote neither file |

### Reading the report from a program

`quality.json` is the gate's interface for a program that integrates it, described by the JSON Schema
[schema/quality-v1.json](schema/quality-v1.json): the gate, its failed conditions, every finding with whether it fails
the gate and a fingerprint that survives the finding moving, the failing tests, coverage of new code, the suppressions
added and the build configuration edited. Its `schema_version` is raised on any change a reader could notice; a new
optional field does not raise it. The quality file is prose, whose wording may change in any release: read the JSON,
not the Markdown. [The report as a contract](docs/quality-gate.md#the-report-as-a-contract) says more.

## Running the gate in a project

`gate.py` is one file and imports only the standard library, so a project runs it from a copy taken at a commit of
this repository, in the project's working tree and with the project's own Python:

```bash
curl -fsSLO https://raw.githubusercontent.com/DavidSiegl/octoflow-gate/<commit>/gate.py
uv run pytest --cov=<package> --cov-branch --cov-report=xml --junitxml=test-results.xml
uv run python gate.py --base main --tests test-results.xml --coverage coverage.xml --out quality.md
```

It takes what it runs from that project, so the project's dev dependencies hold them: ruff, mypy and pymarkdownlnt
from `PATH`, which `uv run` points at the project's environment, and pytest, pytest-randomly and mutmut through the
Python that runs `gate.py`, for the flaky-tests, passes-at-base and surviving-mutants checks. shellcheck, actionlint
and clippy come from the machine, and only for a project that tracks shell scripts, GitHub workflows or Rust files.

A change to Rust files is read with the [gate-rust helper](docs/quality-gate.md#reading-rust-the-gate-rust-helper),
built from this repository's `gate-rust/` at the same commit as `gate.py` and named with `--rust-syntax`; a project
without Rust needs neither:

```bash
cargo build --release --locked --manifest-path path/to/octoflow-gate/gate-rust/Cargo.toml
```

A linter that is missing is a failed condition naming it, and for clippy that is a `cargo clippy --version` that
fails, as it does where rustup's `clippy` component is not installed; a check that cannot run is one of its own
findings. The rules come from the project's `pyproject.toml`, or the one `--config` names, as [whose rules judge a
change](docs/quality-gate.md#whose-rules-judge-a-change) says.

## Settings: `gate.toml`

Which files are tests, which go with them, which hold expected output, which the complexity check leaves alone, which
further files are build configuration, which files each linter reads, the checks' time budgets and the commands that
run the tests: a `gate.toml` at the project's top level says each, or the file `--config` names. Every key has a
default, so a project without one is judged as the gate always judged; [the gate's
settings](docs/quality-gate.md#the-gates-settings) lists them. An unknown key or a value of the wrong kind exits 2.

octoflow's, for example:

```toml
# Its tests are where the defaults look: Python's under tests/, Rust's under engine/tests/, in tests.rs files and in
# #[cfg(test)] modules, its golden files under tests/golden/.

[build-config]
Jenkinsfile = "The quality job reads it from `main`, so this analysis ran without the edit."

# The defaults, written out: a quality round's timeout is sized for them.
[budgets]
mutation = 600
base-run = 120
```

A CI that passes `main`'s `pyproject.toml` with `--config` judges by the `gate.toml` beside it, or by the defaults
where there is none, never by the change's own, so a change cannot loosen its own settings: put `main`'s `gate.toml`
beside `main`'s `pyproject.toml`.

## Developing it

```bash
uv sync
ci/install-tools.sh <dir on PATH>   # cargo-nextest, cargo-llvm-cov, cargo-mutants, which some tests run
uv run pytest
cargo test --manifest-path gate-rust/Cargo.toml
scripts/gate                         # as Jenkins runs it: both test suites, then main's gate on the change since main
```

## License

MIT or Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).
