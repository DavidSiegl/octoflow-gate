# Glossary

The words the gate's code and docs use for what it judges. docs/quality-gate.md says what each check does.

- **Base**: the commit the change is judged against, a full sha (`Change::base`); "at the base" means as git stores
  it there.
- **Change**: what the gate judges, from the base to the working tree, as `Change::from_tree` reads it once for every
  check (crates/gate/src/change.rs).
- **Sources**: the change's source files, each language's read once by its front end, for every check that judges or
  runs them (crates/gate/src/sources.rs).
- **Language change**: one language's part of the Change: its files read by the front end, which paths are code
  under test, and which go with the tests. `PythonChange`, `RustChange` and `GoChange`, each held by Sources.
- **Code under test**: a path of the change that holds a language's code rather than its tests, as the language
  change's `is_code` tells it. A change that edits none has no "without the change" for passes-at-base to run its
  tests in.
- **Beside the tests**: a path of the change that a copy of the base takes from the working tree, since it goes with
  the tests rather than with the code they test: the test files, their data and support files (`beside_tests`).
