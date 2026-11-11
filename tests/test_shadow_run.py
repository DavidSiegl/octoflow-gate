"""ci/shadow-run.sh, which runs the Rust gate after gate.py and writes how their reports differ.

A stub stands in for the Rust gate: it records its arguments and writes the reports the test gives it.
"""

import contextlib
import os
import shutil
import signal
import subprocess
import tempfile
import time
import unittest

import conftest
import pytest

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
        self.put_gate(GATE_STUB)
        self.dir = os.path.join(self.tmp.name, "reports")
        os.mkdir(self.dir)
        self.log = os.path.join(self.tmp.name, "gate.log")
        self.write("quality.md", "GATE: OK\n")
        self.write("quality.json", '{\n  "gate": "OK"\n}\n')

    def put_gate(self, text):
        """Make the shell script text the release's Rust gate."""
        gate = os.path.join(self.release, "gate-linux-x86_64")
        with open(gate, "w") as f:
            f.write(text)
        os.chmod(gate, 0o755)  # noqa: S103

    def start_in_a_session(self):
        """Start shadow-run.sh on the reports' directory in a session of its own, so that a signal to its process
        group is what a terminal's would be, with SIGINT at its default whatever the tests run with; returns it once
        the stub gate has written GATE_LOG."""
        run = subprocess.Popen([self.script, self.dir],  # noqa: S603
                               env={**os.environ, "GATE_LOG": self.log},
                               preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL),  # noqa: PLW1509
                               start_new_session=True, stderr=subprocess.DEVNULL)
        self.addCleanup(run.wait)
        self.addCleanup(self.kill_group, run.pid)
        self.wait_for(lambda: os.path.exists(self.log), run)
        return run

    @staticmethod
    def kill_group(pgid):
        """Kill what is left of the process group pgid, if anything."""
        with contextlib.suppress(ProcessLookupError):
            os.killpg(pgid, signal.SIGKILL)

    def wait_for(self, happened, run):
        """Wait until happened() is true, failing after 10 seconds, or when the process run has ended before."""
        deadline = time.monotonic() + 10
        while not happened():
            self.assertIsNone(run.poll())
            self.assertLess(time.monotonic(), deadline)
            time.sleep(0.05)

    def logged(self):
        """What the stub gate has written to GATE_LOG."""
        with open(self.log) as f:
            return f.read()

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
                             ["--base", "main", "--tests", "a report.xml", "--out", f"{self.dir}/quality-rust.md",
                              "--json", f"{self.dir}/quality-rust.json"])

    def test_an_out_or_json_among_the_arguments_gives_way_so_gate_py_s_reports_stay(self):
        # The Rust gate takes the last of each; a forwarded --json would else name gate.py's own quality.json.
        self.shadow("--out", f"{self.dir}/quality.md", "--json", f"{self.dir}/quality.json")
        with open(self.log) as f:
            self.assertEqual(f.read().splitlines()[-4:], ["--out", f"{self.dir}/quality-rust.md",
                                                          "--json", f"{self.dir}/quality-rust.json"])

    def test_a_verdict_that_differs_is_shown_in_both_reports_and_fails_nothing(self):
        shadow_exit, _ = self.shadow(gate_exit=1, md="GATE: ERROR\n", json='{\n  "gate": "ERROR"\n}\n')
        self.assertEqual(shadow_exit, 0)
        diff = self.read("quality-diff.txt")
        self.assertIn("-GATE: OK\n+GATE: ERROR\n", diff)
        self.assertIn('-  "gate": "OK"\n+  "gate": "ERROR"\n', diff)

    def test_a_gate_that_could_not_judge_is_said_and_its_words_come_through(self):
        self.write("quality-rust.md", "an earlier run's\n")
        shadow_exit, stderr = self.shadow(gate_exit=2, said="gate: main names no commit\n")
        self.assertEqual((shadow_exit, stderr), (0, "gate: main names no commit\n"))
        self.assertEqual(self.read("quality-diff.txt"),
                         "The Rust gate could not judge the change (exit status 2); its standard error says why.\n")
        self.assertFalse(os.path.exists(os.path.join(self.dir, "quality-rust.md")))

    def test_a_gate_that_was_killed_is_said_with_the_exit_status_a_shell_gives_it(self):
        self.put_gate("#!/bin/sh\nkill -KILL $$\n")
        self.assertEqual(self.shadow()[0], 0)
        self.assertIn("(exit status 137)", self.read("quality-diff.txt"))

    @pytest.mark.real_sleep
    def test_ctrl_c_ends_the_script_once_the_gate_has_stopped(self):
        # The stub takes a moment to stop, as a gate that stops its tools does.
        self.put_gate("#!/bin/sh\ntrap 'trap \"\" INT; sleep 0.5; echo interrupted >> \"$GATE_LOG\"; exit 130' INT\n"
                      "echo started >> \"$GATE_LOG\"\nsleep 20 &\nwait\n")
        run = self.start_in_a_session()
        os.killpg(run.pid, signal.SIGINT)
        self.assertEqual(run.wait(timeout=10), 130)
        self.assertEqual(self.logged(), "started\ninterrupted\n")
        self.assertFalse(os.path.exists(os.path.join(self.dir, "quality-diff.txt")))

    def test_a_gate_that_cannot_be_started_is_said_too(self):
        os.remove(os.path.join(self.release, "gate-linux-x86_64"))
        shadow_exit, _ = self.shadow()
        self.assertEqual(shadow_exit, 0)
        self.assertIn("(exit status 127)", self.read("quality-diff.txt"))

    def test_without_gate_py_s_reports_nothing_runs(self):
        os.remove(os.path.join(self.dir, "quality.json"))
        refused = f"shadow-run.sh: {self.dir} has no quality.json of gate.py's to compare with\n"
        self.assertEqual(self.shadow(), (2, refused))
        self.assertFalse(os.path.exists(self.log))

    def test_no_directory_is_a_usage_error(self):
        run = subprocess.run([self.script], capture_output=True, text=True, check=False)  # noqa: S603
        self.assertEqual((run.returncode, run.stderr), (2, "usage: shadow-run.sh <dir> GATE_ARGS...\n"))
