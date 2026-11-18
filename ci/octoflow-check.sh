#!/bin/sh
# Check that octoflow can drive the release candidate <tarball>, an octoflow-gate-<version>.tar.gz as ci/package.sh
# builds it: clone octoflow's main into <dir>, emptied first, and run its e2e scenario real_gate on the candidate
# (OCTOFLOW_GATE_TARBALL; engine/e2e/tests/e2e/real_gate.rs in https://github.com/DavidSiegl/octoflow-ai), which has
# octoflow start a quality round on it, stop and resume it, and read its reports; then runs it again on an empty
# tarball, which must fail. Prints the octoflow commit it checked and both runs' output as they go. Exits 0 when the
# scenario passed on the candidate and failed on the empty tarball; non-zero, saying why, when it failed on the
# candidate, when it passed on the empty one (octoflow no longer judges by the tarball it is given, or no longer runs
# the scenario at all), when octoflow has no such scenario any more, or when the clone, the tools or the build failed.
#
#   ci/octoflow-check.sh <tarball> <dir>
#
# Jenkins's octoflow stage runs it on every build, before Release, so that a release octoflow cannot drive fails its
# build rather than octoflow's pull request that raises its pin to it. CLAUDE.md, "Releases", says how a gate change
# that octoflow must follow first gets through.
#
# Needs git, curl, sha256sum, tar, openssl and the Rust toolchain octoflow's rust-toolchain.toml pins. octoflow's
# ci/install-tools.sh puts gitleaks, which the scenario runs for real, in <dir>/tools (with octoflow's CI tools, which
# the scenario does not use). The build holds octoflow's
# ci/cargo-lock.sh lock, which every octoflow build on the node takes for its cargo steps, since their compiles share
# the agent's memory.
set -eu

[ $# -eq 2 ] || { echo "usage: octoflow-check.sh <tarball> <dir>" >&2; exit 2; }
# Absolute: the scenario refuses a relative OCTOFLOW_GATE_TARBALL, and the steps below run in the clone.
tarball=$(realpath "$1")
rm -rf "$2"
mkdir -p "$2"
dir=$(cd "$2" && pwd -P)

git clone --quiet --depth 1 --branch main https://github.com/DavidSiegl/octoflow-ai.git "$dir/octoflow"
echo "octoflow-check.sh: octoflow's main at $(git -C "$dir/octoflow" rev-parse HEAD)"
"$dir/octoflow/ci/install-tools.sh" "$dir/tools"

cd "$dir/octoflow"

# real_gate TARBALL LOG: run the scenario on TARBALL, its output both shown and kept in LOG; exit with cargo's status.
real_gate() {
    { PATH="$dir/tools:$PATH" OCTOFLOW_REAL_GATE=1 OCTOFLOW_GATE_TARBALL="$1" \
          ci/cargo-lock.sh cargo test --locked -p octoflow-e2e --test e2e -- --exact real_gate::real_gate 2>&1
      echo $? > "$2.status"; } | tee "$2"
    return "$(cat "$2.status")"
}

status=0
real_gate "$tarball" "$dir/test.log" || status=$?
if [ "$status" -ne 0 ]; then
    echo "octoflow-check.sh: octoflow cannot drive $tarball: its real_gate scenario failed (exit $status)" >&2
    exit 1
fi
# A filter that matches no test passes too; octoflow renaming the scenario must not turn the check off.
grep -q '^test real_gate::real_gate \.\.\. ok$' "$dir/test.log" || {
    echo "octoflow-check.sh: octoflow's e2e tests ran no real_gate::real_gate; this script must follow its new name" >&2
    exit 1
}

# The control: a scenario that ignores OCTOFLOW_GATE_TARBALL judges by octoflow's built-in release and passes, and one
# that no longer sees OCTOFLOW_REAL_GATE passes without running, either way without having driven the candidate. On a
# tarball that is no release, one that drives it fails. The build is the run's above, so this costs the scenario's
# wait for the gate's test, which never starts.
mkdir "$dir/control"
empty="$dir/control/$(basename "$tarball")"
: > "$empty"
echo "octoflow-check.sh: the same scenario on an empty $empty, which must fail"
if real_gate "$empty" "$dir/control.log"; then
    echo "octoflow-check.sh: real_gate passed on an empty tarball, so its pass above does not show that octoflow" \
        "drove $tarball; this script must follow how octoflow's scenario now takes a candidate" >&2
    exit 1
fi
echo "octoflow-check.sh: it failed, as it must; octoflow drove $tarball"
