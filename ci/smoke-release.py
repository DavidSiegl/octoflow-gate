"""Check a release tarball that ci/package.sh built, before Jenkins's Release stage publishes it.

    uv run --frozen python ci/smoke-release.py <tarball>

Unpacked into an empty directory, the release must:

- answer `gate.py --help` with exit status 0;
- hold a gate-rust helper whose output for a fixture of gate-rust's tests, gate-rust/tests/fixtures/fns.rs, the
  release's own gate.py accepts: one JSON document of the schema it reads;
- judge a fixture project, whose gate.toml lists one `[[test]]` command, GATE: OK with that command's test read, and
  write a quality.json that the release's schema/quality-v1.json accepts.

Exits 0 when all hold; otherwise names the first that does not on standard error and exits 1. It runs gate.py with
the Python that runs it, and needs jsonschema, a dev dependency, and git.
"""

import importlib.util
import json
import os
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

import jsonschema

REPO = Path(__file__).resolve().parent.parent
RUST_FIXTURE = REPO / "gate-rust" / "tests" / "fixtures" / "fns.rs"

# The fixture project's one test command: it writes a JUnit report of one passing test and exits 0.
TEST_COMMAND = ("open('junit.xml', 'w').write('<testsuites><testsuite><testcase classname=\"smoke\" name=\"test_ok\"/>"
                "</testsuite></testsuites>')")


class SmokeFailure(Exception):
    """A check the release failed, its message saying which and why."""


def run(args, cwd):
    """The completed run of args in cwd, its output captured as text, whatever its exit status."""
    return subprocess.run(args, cwd=cwd, capture_output=True, text=True, check=False)  # noqa: S603


def check_help(release: Path) -> None:
    """Fail unless the unpacked release at release answers `gate.py --help` with exit status 0."""
    proc = run([sys.executable, str(release / "gate.py"), "--help"], release)
    if proc.returncode != 0:
        raise SmokeFailure(f"`gate.py --help` exited {proc.returncode}: {proc.stderr.strip()}")


def check_rust_helper(release: Path) -> None:
    """Fail unless the release's own gate.py, imported, reads its helper's output for RUST_FIXTURE as one document of
    its schema, listing at least one fn: the same reader that judges a change's Rust."""
    spec = importlib.util.spec_from_file_location("released_gate", release / "gate.py")
    if spec is None or spec.loader is None:
        raise SmokeFailure("the release's gate.py cannot be imported")
    gate = importlib.util.module_from_spec(spec)
    # Its dataclasses look their module up there while they are defined.
    sys.modules[spec.name] = gate
    spec.loader.exec_module(gate)
    docs, why = gate._rust_documents(str(release / "gate-rust-linux-x86_64"), [str(RUST_FIXTURE)], str(release))
    if why is not None:
        raise SmokeFailure(f"gate.py rejects the helper's output for {RUST_FIXTURE.name}: {why}")
    if not docs[0].get("fns"):
        raise SmokeFailure(f"the helper found no fn in {RUST_FIXTURE.name}")


def check_fixture_project(release: Path, project: Path) -> None:
    """Fail unless the release's gate.py, run in project, an empty directory it makes a git repository of, judges a
    change GATE: OK by gate.toml's one test command, TEST_COMMAND, and writes a quality.json that the release's schema
    accepts and whose failing_tests is [], which it is only when that command's JUnit report was read."""
    def git(*args):
        proc = run(["git", "-c", "user.name=smoke", "-c", "user.email=smoke@localhost", "-c", "commit.gpgSign=false",
                    *args], project)
        if proc.returncode != 0:
            raise SmokeFailure(f"`git {' '.join(args)}` failed in the fixture project: {proc.stderr.strip()}")
        return proc.stdout.strip()

    git("init", "--quiet", "--initial-branch=main")
    (project / "pyproject.toml").write_text('[project]\nname = "smoke"\nversion = "0"\n')
    (project / "gate.toml").write_text(f'[[test]]\nname = "smoke"\ncommand = [{json.dumps(sys.executable)}, "-c", '
                                       f'{json.dumps(TEST_COMMAND)}]\njunit = ["junit.xml"]\n')
    (project / ".gitignore").write_text("junit.xml\nquality.md\nquality.json\n")
    git("add", ".")
    git("commit", "--quiet", "-m", "base")
    base = git("rev-parse", "HEAD")
    (project / "notes.txt").write_text("a change\n")
    git("add", "notes.txt")

    proc = run([sys.executable, str(release / "gate.py"), "--base", base, "--out", "quality.md"], project)
    if proc.returncode != 0:
        raise SmokeFailure(f"the fixture project was judged with exit status {proc.returncode}, not 0 for GATE: OK: "
                           f"{proc.stderr.strip() or proc.stdout.strip()}")
    try:
        report = json.loads((project / "quality.json").read_text())
        schema = json.loads((release / "schema" / "quality-v1.json").read_text())
    except (OSError, ValueError) as e:
        raise SmokeFailure(f"cannot read the fixture project's quality.json or the release's schema: {e}") from e
    try:
        jsonschema.validate(report, schema, format_checker=jsonschema.Draft202012Validator.FORMAT_CHECKER)
    except jsonschema.ValidationError as e:
        raise SmokeFailure(f"the fixture project's quality.json does not follow schema/quality-v1.json: "
                           f"{e.message}") from e
    if report["failing_tests"] != []:
        raise SmokeFailure(f"the fixture project's test report was not read: failing_tests is "
                           f"{json.dumps(report['failing_tests'])}, not []")


def smoke(tarball: Path) -> None:
    """Run every check on tarball, raising SmokeFailure for the first that fails."""
    with tempfile.TemporaryDirectory() as tmp:
        release, project = Path(tmp, "release"), Path(tmp, "project")
        release.mkdir()
        project.mkdir()
        try:
            with tarfile.open(tarball) as tar:
                tar.extractall(release, filter="data")
        except (OSError, tarfile.TarError) as e:
            raise SmokeFailure(f"cannot unpack {tarball}: {e}") from e
        check_help(release)
        check_rust_helper(release)
        check_fixture_project(release, project)


def main() -> int:
    """Smoke-test the tarball the command line names; the exit status the module's docstring gives."""
    if len(sys.argv) != 2:
        print("usage: smoke-release.py <tarball>", file=sys.stderr)
        return 2
    try:
        smoke(Path(sys.argv[1]))
    except SmokeFailure as e:
        print(f"smoke-release: {e}", file=sys.stderr)
        return 1
    print(f"smoke-release: {os.path.basename(sys.argv[1])} passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
