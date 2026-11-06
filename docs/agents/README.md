# Agent setup

Generated from the agent-estate template; `copier update` rewrites this file, so change the template, or an answer
with `uvx copier update --trust --defaults -d <key>=<value>` (the `agent-estate:init-repo` skill), not the text
here. CLAUDE.md imports it.

## Issue tracker

Specs and tickets live under `.scratch/` (gitignored), mirrored two-way to GitHub Issues on DavidSiegl/octoflow-gate
by ticket-sync. See `docs/agents/issue-tracker.md`.

## Triage labels

Default vocabulary: `needs-triage`, `needs-info`, `ready-for-agent`, `ready-for-human`, `wontfix`. See
`docs/agents/triage-labels.md`.

## Domain docs

Single-context: one `GLOSSARY.md` and `docs/adr/` at the repo root, created lazily, unless the repo already records
its decisions elsewhere. See `docs/agents/domain.md`.

## Review gate

On: Claude's pushes of a commit with no `Verdict: pass` record under `.scratch/reviews/` are refused. Run the branch
through the `cross-review` skill before pushing. A fresh clone has the gate off until `.scratch/reviews/` is created
at its root.

## Quality gate

`scripts/gate [BASE]` is this repo's own quality gate over the change since BASE (default the branch `main`); its
header says what it checks and what CI expects of it. Run it before handing a branch to review, and fix what it reports.

## CI and merging

Jenkins builds every pull request and `main` with the `Jenkinsfile`, running `scripts/gate` over a pull request since
its merge base and over `main` since its last successful build, and reports `continuous-integration/jenkins/pr-head`
on each pull request through its GitHub App.

Once that passes on a pull request's head commit, `.github/workflows/automerge.yml` merges it into `main` with a merge
commit and deletes its branch: opening a pull request is handing it over. To keep one for a human, open it as a draft
or label it `no-automerge`. It merges whatever files the pull request changes, CI's, the gate's and its own
configuration included; the header of `automerge.yml` is the rule, and names the `AUTOMERGE_TOKEN` secret it merges
with. Never merge a pull request yourself.
