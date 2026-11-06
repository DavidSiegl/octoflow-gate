# octoflow-gate

A quality gate for a change to a Python or Rust project: `gate.py` judges what a git working tree changed since a
commit and writes a Markdown report, the quality file, whose first line is `GATE: OK` or `GATE: ERROR`. It was
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
python gate.py --base REV [--config PYPROJECT] [--tests JUNIT]... [--coverage XML]... [--out FILE]
               [--mutation-budget SECONDS] [--base-run-budget SECONDS] [--test-contexts COVERAGE]
               [--rust-syntax PATH]
```

`--base` is the commit the change is measured from; the working tree of the git repository around the current
directory is the change. `python gate.py --help` describes each option.

| Exit status | Means |
|---|---|
| 0 | `GATE: OK` |
| 1 | `GATE: ERROR` |
| 2 | it could not judge the change, or its arguments were wrong; it wrote no quality file |
| 130 | Ctrl-C stopped it; it wrote no quality file |

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
change](docs/quality-gate.md#whose-rules-judge-a-change) says. Which files the test-integrity check takes for tests is
fixed for now: `tests/**`, `test_*.py` and `*_test.py`, and Rust's as [the gate](docs/quality-gate.md#the-gate)
says.

## Developing it

```bash
uv sync
ci/install-tools.sh <dir on PATH>   # cargo-nextest, cargo-llvm-cov, cargo-mutants, which some tests run
uv run pytest
cargo test --manifest-path gate-rust/Cargo.toml
scripts/gate main                    # what Jenkins runs: both test suites, then main's gate on the change since main
```

## License

MIT or Apache-2.0, at your option: [LICENSE-MIT](LICENSE-MIT), [LICENSE-APACHE](LICENSE-APACHE).
