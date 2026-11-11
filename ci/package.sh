#!/usr/bin/env bash
# Build the release of HEAD into <dir>, made if missing, and print the tarball's path:
#
#   <dir>/octoflow-gate-<version>.tar.gz          the files README.md's "Running the gate in a project" lists
#   <dir>/octoflow-gate-<version>.tar.gz.sha256   its sha256, as `sha256sum -c` reads it in <dir>
#
# <version> is HEAD's pyproject.toml's. Every file, and the source the gate-rust helper and the Rust gate are built
# from, comes from HEAD's commit, never the working tree, so a tarball is the commit's whatever the checkout holds
# besides; both programs are built for the machine that runs this, which on Jenkins is the agent. Members are sorted,
# owned by root, dated with HEAD's commit time, and readable by all, writable by the owner alone. Exits non-zero, the
# failing tool saying why, when either program does not build, HEAD lacks a file or its version cannot be read.
# Jenkins's Package stage runs it on every build, and ci/smoke-release.py then checks the tarball before the Release
# stage publishes it.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: package.sh <dir>" >&2; exit 2; }
mkdir -p "$1"
dir=$(cd "$1" && pwd)

cd "$(git rev-parse --show-toplevel)"
src=$(mktemp -d)
stage=$(mktemp -d)
trap 'rm -rf "$src" "$stage"' EXIT
git archive HEAD gate.py README.md pyproject.toml schema tests/golden gate-rust ci/shadow-run.sh \
    Cargo.toml Cargo.lock crates xtask | tar -x -C "$src"
version=$(uv version --short --directory "$src")
name=octoflow-gate-$version.tar.gz

cargo build --release --locked --manifest-path "$src/gate-rust/Cargo.toml" --target-dir "$src/target"
# The Rust gate, crates/gate of the workspace at the root, which holds xtask as a member and so needs its manifest.
cargo build --release --locked --manifest-path "$src/Cargo.toml" --package gate --target-dir "$src/target-gate"
mv "$src/gate.py" "$src/README.md" "$src/schema" "$stage/"
mv "$src/tests/golden" "$stage/golden"
mv "$src/target/release/gate-rust" "$stage/gate-rust-linux-x86_64"
mv "$src/target-gate/release/gate" "$stage/gate-linux-x86_64"
mv "$src/ci/shadow-run.sh" "$stage/shadow-run.sh"

tar --sort=name --owner=0 --group=0 --numeric-owner --mode=u+rw,go-w,a+rX \
    --mtime="@$(git log -1 --format=%ct)" -C "$stage" -cf - \
    gate.py gate-rust-linux-x86_64 gate-linux-x86_64 shadow-run.sh schema golden README.md | gzip -n >"$dir/$name"
(cd "$dir" && sha256sum "$name" >"$name.sha256")
echo "$dir/$name"
