# The parser checks test through a real tree, with no in-memory Sources

`Change` and `Sources` (`crates/gate/src/change.rs`, `crates/gate/src/sources.rs`) have one adapter: git and the working
tree. `Stored` reads the base's blobs through git and the working tree's files from disk, and `in_project` looks for a
file's `Cargo.toml` or `go.mod` in the working tree, or among the base paths the change removes. We decided against an
in-memory `Sources`, a fixture adapter behind `Change` and `Stored`, for the parser checks' tests. Only tests would use
it, so it would be a second implementation of what the gate reads, kept in step by hand. The tests it would speed up
already cross the interface the gate itself uses, on a small git repository per case
(`crates/gate/tests/parser_checks.rs`), so they catch what a fixture would not: how git lists and shows the files, and
what the tree holds above them.

## Considered Options

- **An in-memory `Sources`** (rejected): faster tests, without a git repository. But a manifest lookup, a symlink or
  submodule side, or a rename would each have to be modelled twice, and a test could pass on the fixture where the
  gate fails on git.
- **A real tree per test** (chosen): the cost is a `git init` and a commit or two per case.
