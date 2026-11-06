"""Check a release tarball that ci/package.sh built, before Jenkins's Release stage publishes it.

    uv run --frozen python ci/smoke-release.py <tarball>

Unpacked into an empty directory, the release must:

- answer `gate.py --help` with exit status 0;
- judge a fixture project as a project runs it, `gate.py --rust-syntax gate-rust-linux-x86_64`: its gate.toml lists
  one `[[test]]` command, and its change adds a Rust function over the cognitive-complexity limit. The gate must find
  that function, which it can only through the helper's documents, read in full, and fail on that alone; read the
  command's JUnit report; and write a quality.json that the release's schema/quality-v1.json accepts.

The helper's output is checked only as gate.py reads it; its structure field by field is gate-rust/tests/schema.rs's,
run on the same source by the build's Gate stage. Exits 0 when all hold; otherwise names the first that does not on
standard error and exits 1. It runs gate.py with the Python that runs it, and needs jsonschema, a dev dependency,
and git.
"""

import json
import subprocess
import sys
import tarfile
import tempfile
from pathlib import Path

import jsonschema

# The fixture project's one test command: it writes a JUnit report of one passing test and exits 0.
TEST_COMMAND = ("open('junit.xml', 'w').write('<testsuites><testsuite><testcase classname=\"smoke\" name=\"test_ok\"/>"
                "</testsuite></testsuites>')")

# The fixture project's change: a new Rust function of cognitive complexity 21 (1 + 2 + ... + 6), over the limit of 15.
TANGLED_RS = """\
pub fn tangled(a: u32) -> u32 {
    let mut n = 0;
    if a > 0 {
        if a > 1 {
            if a > 2 {
                if a > 3 {
                    if a > 4 {
                        if a > 5 {
                            n += 1;
                        }
                    }
                }
            }
        }
    }
    n
}
"""

# What the gate must find in that change, as quality.json lists a finding.
EXPECTED_FINDING = {"path": "tangled.rs", "line": 1, "check": "cognitive-complexity"}
# The one failed condition the fixture project's change must raise, in gate.py's words for a check's findings; any
# other, such as a check that could not run, fails the smoke check.
EXPECTED_CONDITION = "1 findings of the `cognitive-complexity` check; each is listed below."


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


def make_fixture_project(project: Path) -> str:
    """Make project, an empty directory, a git repository whose one commit holds gate.toml with TEST_COMMAND, and
    whose working tree adds tangled.rs; return that commit."""
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
    (project / "tangled.rs").write_text(TANGLED_RS)
    git("add", "tangled.rs")
    return git("rev-parse", "HEAD")


def check_fixture_project(release: Path, project: Path) -> None:
    """Fail unless the release's gate.py, with its helper, judges make_fixture_project's change GATE: ERROR for
    EXPECTED_FINDING alone, with no failing test read from the test command's report, and writes a quality.json that
    the release's schema accepts."""
    base = make_fixture_project(project)
    proc = run([sys.executable, str(release / "gate.py"), "--base", base,
                "--rust-syntax", str(release / "gate-rust-linux-x86_64"), "--out", "quality.md"], project)
    said = proc.stderr.strip() or proc.stdout.strip()
    if proc.returncode != 1:
        raise SmokeFailure(f"the fixture project was judged with exit status {proc.returncode}, not 1 for GATE: ERROR: "
                           f"{said}")
    try:
        report = json.loads((project / "quality.json").read_text())
        schema = json.loads((release / "schema" / "quality-v1.json").read_text())
    except (OSError, ValueError) as e:
        raise SmokeFailure(f"cannot read the fixture project's quality.json or the release's schema: {e}; gate.py "
                           f"said: {said}") from e
    try:
        jsonschema.validate(report, schema, format_checker=jsonschema.Draft202012Validator.FORMAT_CHECKER)
    except jsonschema.ValidationError as e:
        raise SmokeFailure(f"the fixture project's quality.json does not follow schema/quality-v1.json: "
                           f"{e.message}") from e
    found = [{key: f[key] for key in EXPECTED_FINDING} for f in report["findings"]]
    if found != [EXPECTED_FINDING]:
        raise SmokeFailure(f"the fixture project's findings are {json.dumps(found)}, not [{json.dumps(EXPECTED_FINDING)}]"
                           f": the helper's output was not read as it should be")
    if report["failed_conditions"] != [EXPECTED_CONDITION]:
        raise SmokeFailure(f"the fixture project's failed conditions are {json.dumps(report['failed_conditions'])}, "
                           f"not [{json.dumps(EXPECTED_CONDITION)}]")
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
    print(f"smoke-release: {Path(sys.argv[1]).name} passed")
    return 0


if __name__ == "__main__":
    sys.exit(main())
