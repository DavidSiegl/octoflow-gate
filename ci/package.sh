#!/usr/bin/env bash
# Build the release of HEAD into <dir>, made if missing, and print the tarball's path:
#
#   <dir>/octoflow-gate-<version>.tar.gz          the files README.md's "Running the gate in a project" lists
#   <dir>/octoflow-gate-<version>.tar.gz.sha256   its sha256, as `sha256sum -c` reads it in <dir>
#
# <version> is HEAD's pyproject.toml's. Every file, and the source the gate-rust helper is built from, comes from
# HEAD's commit, never the working tree, so a tarball is the commit's whatever the checkout holds besides; the helper
# is built for the machine that runs this, which on Jenkins is the agent. Members are sorted, owned by root, dated
# with HEAD's commit time, and readable by all, writable by the owner alone. Exits non-zero, cargo or tar saying why,
# when the helper does not build or HEAD lacks a file. Jenkins's Package stage runs it on every build, and
# ci/smoke-release.py then checks the tarball before the Release stage publishes it.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: package.sh <dir>" >&2; exit 2; }
mkdir -p "$1"
dir=$(cd "$1" && pwd)

cd "$(git rev-parse --show-toplevel)"
src=$(mktemp -d)
stage=$(mktemp -d)
trap 'rm -rf "$src" "$stage"' EXIT
git archive HEAD gate.py README.md pyproject.toml schema tests/golden gate-rust | tar -x -C "$src"
version=$(uv version --short --directory "$src")
name=octoflow-gate-$version.tar.gz

cargo build --release --locked --manifest-path "$src/gate-rust/Cargo.toml" --target-dir "$src/target"
mv "$src/gate.py" "$src/README.md" "$src/schema" "$stage/"
mv "$src/tests/golden" "$stage/golden"
mv "$src/target/release/gate-rust" "$stage/gate-rust-linux-x86_64"

tar --sort=name --owner=0 --group=0 --numeric-owner --mode=u+rw,go-w,a+rX \
    --mtime="@$(git log -1 --format=%ct)" -C "$stage" -cf - \
    gate.py gate-rust-linux-x86_64 schema golden README.md | gzip -n >"$dir/$name"
(cd "$dir" && sha256sum "$name" >"$name.sha256")
echo "$dir/$name"
