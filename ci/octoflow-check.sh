#!/bin/sh
# Check that octoflow can drive the release candidate <tarball>, an octoflow-gate-<version>.tar.gz as ci/package.sh
# builds it: clone octoflow's main into <dir>, emptied first, and run its e2e scenario real_gate on the candidate
# (OCTOFLOW_GATE_TARBALL; engine/e2e/tests/e2e/real_gate.rs in https://github.com/DavidSiegl/octoflow-ai), which has
# octoflow start a quality round on it, stop and resume it, and read its reports. Prints the octoflow commit it checked
# and the test's output. Exits 0 when the scenario passed; non-zero, saying why, when it failed, when octoflow has no
# such scenario any more, or when the clone, the tools or the build failed.
#
#   ci/octoflow-check.sh <tarball> <dir>
#
# Jenkins's octoflow stage runs it on every build, before Release, so that a release octoflow cannot drive fails its
# build rather than octoflow's pull request that raises its pin to it. When the gate changes something octoflow must
# follow first, octoflow's change is made against the tarball this build archives, and merged; a build after that
# passes.
#
# Needs git, curl, sha256sum, tar, openssl and the Rust toolchain octoflow's rust-toolchain.toml pins. octoflow's
# ci/install-tools.sh puts gitleaks, which the scenario runs for real, in <dir>/tools. The build holds octoflow's
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
status=0
PATH="$dir/tools:$PATH" OCTOFLOW_REAL_GATE=1 OCTOFLOW_GATE_TARBALL="$tarball" \
    ci/cargo-lock.sh cargo test --locked -p octoflow-e2e --test e2e -- --exact real_gate::real_gate \
    > "$dir/test.log" 2>&1 || status=$?
cat "$dir/test.log"
if [ "$status" -ne 0 ]; then
    echo "octoflow-check.sh: octoflow cannot drive $tarball: its real_gate scenario failed (exit $status)" >&2
    exit 1
fi
# A filter that matches no test passes too; octoflow renaming the scenario must not turn the check off.
grep -q '^test real_gate::real_gate \.\.\. ok$' "$dir/test.log" || {
    echo "octoflow-check.sh: octoflow's e2e tests ran no real_gate::real_gate; this script must follow its new name" >&2
    exit 1
}
