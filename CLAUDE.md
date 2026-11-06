# octoflow-gate

## Releases

A change reaches users only when a merge into `main` raises the version in `pyproject.toml`: that merge releases it
(gate-standalone/06 adds the Release stage; until then nothing is released).

- A change to `gate.py`, `gate-rust/` or what a release ships raises the version with `uv version --bump <level>`, which
  also updates `uv.lock`: `patch` for a fix, `minor` for a new check, finding or flag, and, while the version is 0.x,
  `minor` too for a change that breaks existing flags, exit statuses or the quality file's contract
  (docs/quality-gate.md, "The quality file as a contract"). `major` makes it 1.0.0, a deliberate release decision.
- Changes only to docs, tests, CI or repository tooling raise nothing.
- Jenkins creates tags and releases after the merge.

## Work

- Specs and tickets live in `.scratch/` (gitignored), the tracker the `scratch-issue-tracker` skill describes; the
  gate's spec so far is octoflow-ai's `.scratch/gate-standalone/`.
- `TODO.md` is the ledger of known open issues.
- Commit subjects say what the change does, in a full sentence, and name the ticket in parentheses at the end.
- Run the tests with SIGINT ignored, as Jenkins runs them (`trap '' INT` first), so that a local run sees what a
  build sees. Some tests run cargo-nextest, cargo-llvm-cov and cargo-mutants and fail without them on `PATH`;
  `ci/install-tools.sh <dir>` puts them in `<dir>`.

@docs/agents/README.md
