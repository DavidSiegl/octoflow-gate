# octoflow-gate

## Releases

A change reaches users only when a merge into `main` raises the version in `crates/gate/Cargo.toml`: that merge
releases it as GitHub release `v<version>`, whose assets `ci/package.sh` builds (Jenkinsfile, Package and Release
stages).

- A change to `crates/` or to what a release ships raises that version, then `cargo update -w` for `Cargo.lock`:
  `patch` for a fix, `minor` for a new check, finding or flag, and, while the version is 0.x, `minor` too for a change
  that breaks existing flags, exit statuses or quality.json's contract (docs/quality-gate.md, "The report as a
  contract"). `major` makes it 1.0.0, a deliberate release decision. quality.json names the version.
- Changes only to docs, tests, CI or repository tooling raise nothing, a shipped doc such as README.md included: its
  edit goes out with the next release.
- `ci/gate.pin` names the release that judges this repo's changes. Raise it only to a release that exists, with the
  sha256 of its downloaded tarball; `scripts/gate` refuses a `gate.toml` the pinned release would not read, so a new
  key lands with or after the raise.
- Jenkins creates tags and releases after the merge.

## Work

- Specs and tickets live in `.scratch/` (gitignored), the tracker the `scratch-issue-tracker` skill describes; the
  gate's spec so far is octoflow-ai's `.scratch/gate-standalone/`.
- `TODO.md` is the ledger of known open issues.
- Commit subjects say what the change does, in a full sentence, and name the ticket in parentheses at the end.
- Run the tests with SIGINT ignored, as Jenkins runs them (`trap '' INT` first), so that a local run sees what a
  build sees. Some tests run cargo-nextest, cargo-llvm-cov and cargo-mutants and fail without them on `PATH`;
  `ci/install-tools.sh <dir>` puts them in `<dir>`. Others run the linters, pytest and mutmut of `pyproject.toml`'s
  `dev` group, through `uv run --frozen`.

@docs/agents/README.md
