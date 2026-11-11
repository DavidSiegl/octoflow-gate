#!/usr/bin/env bash
# Build the release of HEAD into <dir>, made if missing, and print the tarball's path:
#
#   <dir>/octoflow-gate-<version>.tar.gz          the files README.md's "Running the gate in a project" lists
#   <dir>/octoflow-gate-<version>.tar.gz.sha256   its sha256, as `sha256sum -c` reads it in <dir>
#
# <version> is that of HEAD's crates/gate/Cargo.toml, the gate's own. Every file, and the source the gate is built
# from, comes from HEAD's commit, never the working tree, so a tarball is the commit's whatever the checkout holds
# besides; the gate is built for the machine that runs this, which on Jenkins is the agent, Linux x86_64. Members are
# sorted, owned by root, dated with HEAD's commit time, and readable by all, writable by the owner alone. Exits
# non-zero, the failing tool saying why, when the gate does not build, HEAD lacks a file or its version cannot be read.
# Jenkins's Package stage runs it on every build, and `cargo xtask smoke-release` then checks the tarball before the
# Release stage publishes it.
set -euo pipefail

[ $# -eq 1 ] || { echo "usage: package.sh <dir>" >&2; exit 2; }
mkdir -p "$1"
dir=$(cd "$1" && pwd)

cd "$(git rev-parse --show-toplevel)"
src=$(mktemp -d)
stage=$(mktemp -d)
trap 'rm -rf "$src" "$stage"' EXIT
git archive HEAD README.md schema tests/golden Cargo.toml Cargo.lock crates xtask | tar -x -C "$src"
# cargo names the package as <source>#gate@<version>, or <source>#<version> where the source's last part is its name.
id=$(cargo pkgid --locked --manifest-path "$src/Cargo.toml" --package gate)
version=${id##*[#@]}
name=octoflow-gate-$version.tar.gz

# crates/gate of the workspace, which holds xtask as a member and so needs its manifest.
cargo build --release --locked --manifest-path "$src/Cargo.toml" --package gate --target-dir "$src/target"
mv "$src/README.md" "$src/schema" "$src/target/release/gate" "$stage/"
mv "$src/tests/golden" "$stage/golden"

tar --sort=name --owner=0 --group=0 --numeric-owner --mode=u+rw,go-w,a+rX \
    --mtime="@$(git log -1 --format=%ct)" -C "$stage" -cf - gate schema golden README.md | gzip -n >"$dir/$name"
(cd "$dir" && sha256sum "$name" >"$name.sha256")
echo "$dir/$name"
