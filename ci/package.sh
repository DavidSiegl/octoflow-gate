#!/usr/bin/env bash
# Build the release of the version pyproject.toml holds into <dir>, made if missing, and print the tarball's path:
#
#   <dir>/octoflow-gate-<version>.tar.gz          gate.py, gate-rust-linux-x86_64, schema/, golden/ and README.md,
#                                                 at the archive's top level
#   <dir>/octoflow-gate-<version>.tar.gz.sha256   its sha256, as `sha256sum -c` reads it in <dir>
#
# gate-rust-linux-x86_64 is the gate-rust helper, built here for the machine that runs this, which Jenkins's is;
# golden/ holds tests/golden's quality files, the Markdown and JSON renderings of the same reports. Members are
# sorted, owned by root and dated with HEAD's commit time, whoever checked the tree out when. Exits non-zero, cargo
# or tar saying why, when the helper does not build or a file is missing. Jenkins's Package stage runs it on every
# build, and ci/smoke-release.py then checks the tarball before the Release stage publishes it.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: package.sh <dir>" >&2; exit 2; }
mkdir -p "$1"
dir=$(cd "$1" && pwd)

top=$(git rev-parse --show-toplevel)
cd "$top"
version=$(uv version --short)
name=octoflow-gate-$version.tar.gz
stage=$(mktemp -d)
trap 'rm -rf "$stage"' EXIT

cargo build --release --locked --manifest-path gate-rust/Cargo.toml
cp gate.py README.md "$stage/"
cp gate-rust/target/release/gate-rust "$stage/gate-rust-linux-x86_64"
cp -R schema "$stage/schema"
cp -R tests/golden "$stage/golden"

tar --sort=name --owner=0 --group=0 --numeric-owner --mtime="@$(git log -1 --format=%ct)" -C "$stage" -cf - \
    gate.py gate-rust-linux-x86_64 schema golden README.md | gzip -n >"$dir/$name"
(cd "$dir" && sha256sum "$name" >"$name.sha256")
echo "$dir/$name"
