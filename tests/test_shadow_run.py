"""ci/shadow-run.sh, which runs the Rust gate after gate.py and writes how their reports differ.

A stub stands in for the Rust gate: it records its arguments and writes the reports the test gives it.
"""

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

    def shorten_limit(self, minutes):
        """Give the release's shadow-run.sh a time limit of minutes in place of its own."""
        with open(self.script) as f:
            text = f.read()
        self.assertIn("\nGATE_MINUTES=20\n", text)
        with open(self.script, "w") as f:
            f.write(text.replace("\nGATE_MINUTES=20\n", f"\nGATE_MINUTES={minutes}\n"))

    def write(self, name, text):
        """Write text to the file name in the reports' directory."""
        with open(os.path.join(self.dir, name), "w") as f:
            f.write(text)

    def read(self, name):
        """The text of the file name in the reports' directory."""
        with open(os.path.join(self.dir, name)) as f:
            return f.read()

    def shadow(self, *args, gate_exit=0, md="GATE: OK\n", json='{\n  "gate": "OK"\n}\n', said="", path=None,
               sigint=signal.SIG_DFL):
        """Run shadow-run.sh on the reports' directory with args, the stub gate exiting gate_exit after writing md and
        json and printing said, with path as PATH when given and sigint as what SIGINT does to the script (the tests
        themselves may run with it ignored, as in a Jenkins build); returns (exit status, standard error)."""
        env = {**os.environ, "GATE_LOG": self.log, "FAKE_STATUS": str(gate_exit), "FAKE_MD": md, "FAKE_JSON": json,
               "FAKE_SAID": said, "PATH": path or os.environ["PATH"]}
        run = subprocess.run([self.script, self.dir, *args], env=env,  # noqa: S603
                             preexec_fn=lambda: signal.signal(signal.SIGINT, sigint),
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

    def test_a_gate_that_could_not_judge_is_said_with_its_words(self):
        self.write("quality-rust.md", "an earlier run's\n")
        shadow_exit, stderr = self.shadow(gate_exit=2, said="gate: main names no commit\n")
        self.assertEqual((shadow_exit, stderr), (0, "gate: main names no commit\n"))
        self.assertEqual(self.read("quality-diff.txt"),
                         "The Rust gate could not judge the change (exit status 2). It said:\n"
                         "gate: main names no commit\n")
        self.assertFalse(os.path.exists(os.path.join(self.dir, "quality-rust.md")))

    def test_a_gate_stopped_at_its_time_limit_is_said_with_what_it_had_printed(self):
        # A timeout that prints for the gate and reports it stopped, in place of waiting out the limit.
        bin_dir = os.path.join(self.tmp.name, "bin")
        os.mkdir(bin_dir)
        stopper = os.path.join(bin_dir, "timeout")
        with open(stopper, "w") as f:
            f.write("#!/bin/sh\necho 'gate: running mutmut' >&2\nexit 124\n")
        os.chmod(stopper, 0o755)  # noqa: S103
        shadow_exit, stderr = self.shadow(path=bin_dir + os.pathsep + os.environ["PATH"])
        self.assertEqual((shadow_exit, stderr), (0, "gate: running mutmut\n"))
        self.assertEqual(self.read("quality-diff.txt"),
                         "The Rust gate was stopped after 20 minutes without a verdict. It said:\n"
                         "gate: running mutmut\n")

    def test_a_gate_past_its_time_limit_is_stopped_though_what_it_started_runs_on(self):
        # What the gate started is in a session of its own, as the Rust gate's tools are, and holds its standard error.
        self.shorten_limit("0.02")
        self.put_gate("#!/bin/sh\necho 'gate: running the tests' >&2\nsetsid sleep 20 >&2 &\nexec sleep 20\n")
        started = time.monotonic()
        shadow_exit, stderr = self.shadow()
        self.assertLess(time.monotonic() - started, 15)
        self.assertEqual((shadow_exit, stderr), (0, "gate: running the tests\n"))
        self.assertEqual(self.read("quality-diff.txt"),
                         "The Rust gate was stopped after 0.02 minutes without a verdict. It said:\n"
                         "gate: running the tests\n")

    def test_a_gate_that_was_killed_is_not_said_to_have_been_stopped_at_the_limit(self):
        self.put_gate("#!/bin/sh\nkill -KILL $$\n")
        self.assertEqual(self.shadow()[0], 0)
        self.assertIn("The Rust gate could not judge the change (exit status 137). It said:\n",
                      self.read("quality-diff.txt"))

    def test_the_gate_has_sigint_ignored_only_when_the_script_was_started_so(self):
        # The second bit of the last hexadecimal digit of SigIgn is SIGINT's.
        self.put_gate("#!/bin/sh\nawk '/^SigIgn:/ { print substr($2, length($2)) }' /proc/$$/status >&2\nexit 2\n")
        _, as_a_jenkins_step = self.shadow(sigint=signal.SIG_IGN)
        _, at_a_terminal = self.shadow()
        self.assertEqual((int(as_a_jenkins_step, 16) & 2, int(at_a_terminal, 16) & 2), (2, 0))

    @pytest.mark.real_sleep
    def test_ctrl_c_reaches_the_gate_and_ends_the_script(self):
        # Ctrl-C goes to the terminal's foreground process group, which the script's session stands in for.
        self.put_gate("#!/bin/sh\ntrap 'trap \"\" INT; echo interrupted >> \"$GATE_LOG\"; exit 130' INT\n"
                      "echo started >> \"$GATE_LOG\"\nsleep 20 &\nwait\n")
        run = subprocess.Popen([self.script, self.dir], env={**os.environ, "GATE_LOG": self.log},  # noqa: S603
                               preexec_fn=lambda: signal.signal(signal.SIGINT, signal.SIG_DFL),  # noqa: PLW1509
                               start_new_session=True, stderr=subprocess.DEVNULL)
        self.addCleanup(run.kill)
        while not os.path.exists(self.log):
            self.assertIsNone(run.poll())
            time.sleep(0.05)
        os.killpg(run.pid, signal.SIGINT)
        self.assertEqual(run.wait(timeout=15), 130)
        with open(self.log) as f:
            self.assertEqual(f.read(), "started\ninterrupted\n")

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
