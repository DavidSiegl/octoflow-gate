"""ci/shadow-run.sh, which runs the Rust gate after gate.py and writes how their reports differ.

A stub stands in for the Rust gate: it records its arguments and writes the reports the test gives it.
"""

import os
import shutil
import subprocess
import tempfile
import unittest

import conftest

# conftest.REPO, not this file's directory: under mutmut the tests run from mutants/, which has no ci/.
SCRIPT = os.path.join(conftest.REPO, "ci", "shadow-run.sh")

# Appends its arguments, one a line, to GATE_LOG. With FAKE_STATUS 0 or 1, writes FAKE_MD to the file after --out and
# FAKE_JSON beside it, as the Rust gate names quality.json; prints FAKE_SAID on standard error; exits FAKE_STATUS.
GATE_STUB = """#!/bin/sh
printf '%s\\n' "$@" >> "$GATE_LOG"
printf '%s' "$FAKE_SAID" >&2
if [ "$FAKE_STATUS" -le 1 ]; then
    while [ $# -gt 0 ]; do
        if [ "$1" = --out ]; then
            printf '%s' "$FAKE_MD" > "$2"
            printf '%s' "$FAKE_JSON" > "${2%.md}.json"
        fi
        shift
    done
fi
exit "$FAKE_STATUS"
"""


class TestShadowRun(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        # The script as a release unpacks it, beside the gate it runs.
        self.release = os.path.join(self.tmp.name, "release")
        os.mkdir(self.release)
        self.script = shutil.copy(SCRIPT, os.path.join(self.release, "shadow-run.sh"))
        gate = os.path.join(self.release, "gate-linux-x86_64")
        with open(gate, "w") as f:
            f.write(GATE_STUB)
        os.chmod(gate, 0o755)  # noqa: S103
        self.dir = os.path.join(self.tmp.name, "reports")
        os.mkdir(self.dir)
        self.log = os.path.join(self.tmp.name, "gate.log")
        self.write("quality.md", "GATE: OK\n")
        self.write("quality.json", '{\n  "gate": "OK"\n}\n')

    def write(self, name, text):
        """Write text to the file name in the reports' directory."""
        with open(os.path.join(self.dir, name), "w") as f:
            f.write(text)

    def read(self, name):
        """The text of the file name in the reports' directory."""
        with open(os.path.join(self.dir, name)) as f:
            return f.read()

    def shadow(self, *args, gate_exit=0, md="GATE: OK\n", json='{\n  "gate": "OK"\n}\n', said=""):
        """Run shadow-run.sh on the reports' directory with args, the stub gate exiting gate_exit after writing md and
        json and printing said; returns (exit status, standard error)."""
        env = {**os.environ, "GATE_LOG": self.log, "FAKE_STATUS": str(gate_exit), "FAKE_MD": md, "FAKE_JSON": json,
               "FAKE_SAID": said}
        run = subprocess.run([self.script, self.dir, *args], env=env,  # noqa: S603
                             capture_output=True, text=True, check=False)
        return run.returncode, run.stderr

    def test_reports_that_agree_leave_an_empty_difference(self):
        self.assertEqual(self.shadow("--base", "main"), (0, ""))
        self.assertEqual(self.read("quality-diff.txt"), "")
        self.assertEqual(self.read("quality-rust.md"), "GATE: OK\n")
        self.assertEqual(self.read("quality-rust.json"), '{\n  "gate": "OK"\n}\n')

    def test_the_gate_gets_the_arguments_and_its_own_out(self):
        self.shadow("--base", "main", "--tests", "a report.xml")
        with open(self.log) as f:
            self.assertEqual(f.read().splitlines(),
                             ["--base", "main", "--tests", "a report.xml", "--out", f"{self.dir}/quality-rust.md"])

    def test_a_verdict_that_differs_is_shown_in_both_reports_and_fails_nothing(self):
        shadow_exit, _ = self.shadow(gate_exit=1, md="GATE: ERROR\n", json='{\n  "gate": "ERROR"\n}\n')
        self.assertEqual(shadow_exit, 0)
        diff = self.read("quality-diff.txt")
        self.assertIn("-GATE: OK\n+GATE: ERROR\n", diff)
        self.assertIn('-  "gate": "OK"\n+  "gate": "ERROR"\n', diff)

    def test_a_gate_that_could_not_judge_is_said_with_its_words(self):
        self.write("quality-rust.md", "an earlier run's\n")
        shadow_exit, stderr = self.shadow(gate_exit=2, said="gate: main names no commit\n")
        self.assertEqual((shadow_exit, stderr), (0, "gate: main names no commit\n"))
        self.assertEqual(self.read("quality-diff.txt"),
                         "The Rust gate could not judge the change (exit status 2). It said:\n"
                         "gate: main names no commit\n")
        self.assertFalse(os.path.exists(os.path.join(self.dir, "quality-rust.md")))

    def test_a_gate_that_cannot_be_started_is_said_too(self):
        os.remove(os.path.join(self.release, "gate-linux-x86_64"))
        shadow_exit, _ = self.shadow()
        self.assertEqual(shadow_exit, 0)
        self.assertIn("The Rust gate could not judge the change (exit status 127). It said:\n",
                      self.read("quality-diff.txt"))

    def test_without_gate_py_s_reports_nothing_runs(self):
        os.remove(os.path.join(self.dir, "quality.json"))
        refused = f"shadow-run.sh: {self.dir} has no quality.json of gate.py's to compare with\n"
        self.assertEqual(self.shadow(), (2, refused))
        self.assertFalse(os.path.exists(self.log))

    def test_no_directory_is_a_usage_error(self):
        run = subprocess.run([self.script], capture_output=True, text=True, check=False)  # noqa: S603
        self.assertEqual((run.returncode, run.stderr), (2, "usage: shadow-run.sh <dir> GATE_ARGS...\n"))
