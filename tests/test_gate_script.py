"""scripts/gate, the shell script CI runs: how it picks BASE and the judge before it runs anything, and when it has
the pinned release's Rust gate judge after gate.py."""

import os
import shutil
import subprocess

import conftest
from gitrepo import RepositoryTest

# conftest.REPO, not this file's directory: under mutmut the tests run from mutants/, which has no scripts/.
SCRIPT = os.path.join(conftest.REPO, "scripts", "gate")


class TestGateScript(RepositoryTest):
    BASE_FILES = {"f.py": "x = 1\n"}

    def setUp(self):
        super().setUp()
        os.makedirs(os.path.join(self.repo, "scripts"))
        shutil.copy(SCRIPT, os.path.join(self.repo, "scripts", "gate"))

    def gate(self, *args):
        """Run the repository's scripts/gate as outside a Jenkins build; returns (exit status, standard error)."""
        env = {k: v for k, v in os.environ.items() if k not in ("WORKSPACE", "WORKSPACE_TMP", "CHANGE_TARGET")}
        run = subprocess.run(["bash", "scripts/gate", *args], cwd=self.repo, env=env,  # noqa: S603, S607
                             capture_output=True, text=True, check=False)
        return run.returncode, run.stderr

    def test_the_default_base_is_the_branch_main_never_a_tag_main(self):
        self.git("switch", "--quiet", "-c", "trunk")
        self.git("branch", "--quiet", "-D", "main")
        self.git("tag", "main")
        self.assertEqual(self.gate(), (2, "gate: refs/heads/main is not a commit\n"))

    def test_the_default_judge_is_the_branch_main_never_a_tag_main(self):
        # The tag holds a gate.py the branch lacks: judged by the tag, the run would get past that check.
        self.git("switch", "--quiet", "-c", "trunk")
        self.commit({"gate.py": ""}, "a judge")
        self.git("tag", "main")
        self.assertEqual(self.gate(), (2, "gate: refs/heads/main has no gate.py to judge with\n"))

    def test_a_judge_without_test_commands_is_refused(self):
        # Its gate.py would then run no test, and judge the change without them.
        judge = {"gate.py": "", "pyproject.toml": "", "gate-rust/Cargo.toml": ""}
        self.commit(judge, "a judge without gate.toml")
        self.assertEqual(self.gate(), (2, "gate: refs/heads/main has no gate.toml to judge with\n"))
        self.commit({"gate.toml": '[budgets]\nmutation = 600\n# [[test]]\n'}, "a gate.toml without [[test]]")
        refused = "gate: refs/heads/main's gate.toml lists no [[test]] command, so no test would be judged\n"
        self.assertEqual(self.gate(), (2, refused))

    def test_a_judge_with_a_pin_judges_with_the_release_not_its_own_gate_py(self):
        # Pinned, the judge needs the script that fetches the release, and no gate.py or gate-rust/ of its own.
        self.commit({"ci/gate.pin": f"0.5.0 {'0' * 64}\n"}, "a judge that pins a release")
        self.assertEqual(self.gate(), (2, "gate: refs/heads/main has no ci/fetch-gate.sh to judge with\n"))

    def test_the_release_is_fetched_by_the_judges_script_not_the_changes(self):
        # A change that edits ci/fetch-gate.sh, say to skip the sha256 check, must not fetch the gate it is judged by.
        fetch = "ci/fetch-gate.sh"
        self.put(fetch, "#!/bin/sh\necho 'fetched by the judge' >&2\nexit 1\n")
        os.chmod(os.path.join(self.repo, fetch), 0o755)  # noqa: S103
        self.commit({"ci/gate.pin": f"0.5.0 {'0' * 64}\n", "pyproject.toml": "",
                     "gate.toml": '[[test]]\nname = "t"\ncommand = ["true"]\n'}, "a judge that pins a release")
        self.put(fetch, "#!/bin/sh\necho 'fetched by the change' >&2\nexit 1\n")
        self.assertEqual(self.gate(), (2, "fetched by the judge\n"))

    def test_a_change_whose_gate_toml_its_pinned_release_refuses_is_not_judged(self):
        # Merged, it would be read by that release's gate.py, which refuses a key it does not know: every later change,
        # the one raising the pin included, would then go unjudged.
        fetch = "ci/fetch-gate.sh"
        self.put(fetch, '#!/bin/sh\nmkdir -p "$2"\ncat > "$2/gate.py" <<\'EOF\'\n' + RELEASED_GATE_PY + "EOF\necho 0.5.0\n")
        os.chmod(os.path.join(self.repo, fetch), 0o755)  # noqa: S103
        gate_toml = '[[test]]\nname = "t"\ncommand = ["true"]\n'
        self.commit({"ci/gate.pin": f"0.5.0 {'0' * 64}\n", "pyproject.toml": "", "gate.toml": gate_toml},
                    "a judge that pins a release")
        self.put("gate.toml", "new_key = 1\n" + gate_toml)
        status, stderr = self.gate()
        self.assertEqual(status, 2)
        self.assertIn("gate: gate.toml: unknown key `new_key`; the release ci/gate.pin pins would refuse it", stderr)


    def test_the_change_that_first_pins_a_release_has_its_gate_toml_checked_too(self):
        # A judge from before the pin has no ci/fetch-gate.sh of its own, so the change's fetches the release it pins.
        gate_toml = '[[test]]\nname = "t"\ncommand = ["true"]\n'
        self.commit({"gate.py": "", "gate-rust/Cargo.toml": "", "pyproject.toml": "", "gate.toml": gate_toml},
                    "a judge from before the pin")
        fetch = "ci/fetch-gate.sh"
        self.put(fetch, '#!/bin/sh\nmkdir -p "$2"\ncat > "$2/gate.py" <<\'EOF\'\n' + RELEASED_GATE_PY + "EOF\necho 0.5.0\n")
        os.chmod(os.path.join(self.repo, fetch), 0o755)  # noqa: S103
        self.put("ci/gate.pin", f"0.5.0 {'0' * 64}\n")
        self.put("gate.toml", "new_key = 1\n" + gate_toml)
        status, stderr = self.gate()
        self.assertEqual(status, 2)
        self.assertIn("gate: gate.toml: unknown key `new_key`; the release ci/gate.pin pins would refuse it", stderr)



class TestShadowRun(RepositoryTest):
    """scripts/gate's shadow run of the Rust gate, with stubs for the pinned release and for the tools the script
    asks for before it judges: the release's gate.py writes the verdict the test names, and its shadow-run.sh
    records its arguments."""

    BASE_FILES = {"f.py": "x = 1\n"}

    def setUp(self):
        super().setUp()
        os.makedirs(os.path.join(self.repo, "scripts"))
        shutil.copy(SCRIPT, os.path.join(self.repo, "scripts", "gate"))
        self.bin = os.path.join(self.outside, "bin")
        os.mkdir(self.bin)
        for tool, text in {"uv": UV_STUB, "rustup": "#!/bin/sh\necho clippy-x\necho llvm-tools-x\n",
                           "cargo-llvm-cov": "", "cargo-nextest": "", "cargo-mutants": ""}.items():
            with open(os.path.join(self.bin, tool), "w") as f:
                f.write(text)
            os.chmod(os.path.join(self.bin, tool), 0o755)  # noqa: S103
        self.log = os.path.join(self.outside, "shadow.log")

    def pin(self, release_files):
        """Commit a judge that pins a release whose fetch script unpacks release_files, each name to its text, made
        executable, and whose gate.toml lists two test commands with their reports."""
        unpack = "".join(f'cat > "$2/{name}" <<\'EOF\'\n{text}EOF\nchmod +x "$2/{name}"\n'
                         for name, text in release_files.items())
        self.put("ci/fetch-gate.sh", f'#!/bin/sh\nmkdir -p "$2"\n{unpack}echo 0.6.0\n')
        os.chmod(os.path.join(self.repo, "ci/fetch-gate.sh"), 0o755)  # noqa: S103
        self.commit({"ci/gate.pin": f"0.6.0 {'0' * 64}\n", "pyproject.toml": "", "gate.toml": TWO_TEST_COMMANDS},
                    "a judge that pins a release")

    def gate(self, gate_exit=0, condition="", detail="", shadow_exit=0):
        """Run scripts/gate with the stub tools first on PATH, the release's gate.py exiting gate_exit with condition
        as its report's failed condition and detail as what its failing test said, and its shadow-run.sh exiting
        shadow_exit; returns (exit status, standard error)."""
        env = {k: v for k, v in os.environ.items() if k not in ("WORKSPACE", "WORKSPACE_TMP", "CHANGE_TARGET")}
        env |= {"PATH": self.bin + os.pathsep + env["PATH"], "FAKE_GATE_EXIT": str(gate_exit),
                "FAKE_CONDITION": condition, "FAKE_DETAIL": detail, "FAKE_SHADOW_EXIT": str(shadow_exit),
                "SHADOW_LOG": self.log}
        run = subprocess.run(["bash", "scripts/gate"], cwd=self.repo, env=env,  # noqa: S607
                             capture_output=True, text=True, check=False)
        return run.returncode, run.stderr

    def shadow_args(self):
        """The arguments the release's shadow-run.sh was run with, or None when it was not run."""
        if not os.path.exists(self.log):
            return None
        with open(self.log) as f:
            return f.read().splitlines()

    def test_the_rust_gate_judges_after_gate_py_from_the_reports_of_the_judge_s_test_commands(self):
        self.pin({"gate.py": RELEASED_GATE_PY + JUDGING_GATE_PY, "shadow-run.sh": SHADOW_STUB})
        status, _ = self.gate(gate_exit=1)
        self.assertEqual(status, 1)
        args = self.shadow_args()
        config, helper = args[4], args[8]
        self.assertTrue(config.endswith("/pyproject.toml") and helper.endswith("/release/gate-rust-linux-x86_64"))
        self.assertEqual(args, [".", "--base", self.git("rev-parse", "main").strip(), "--config", config,
                                "--test-contexts", ".coverage", "--rust-syntax", helper, "--python", "python",
                                "--tests", "a.xml", "--coverage", "a cov.xml", "--tests", "b.xml"])

    def test_a_shadow_run_that_fails_leaves_gate_py_s_verdict(self):
        self.pin({"gate.py": RELEASED_GATE_PY + JUDGING_GATE_PY, "shadow-run.sh": SHADOW_STUB})
        status, stderr = self.gate(shadow_exit=2)
        self.assertEqual(status, 0)
        self.assertIn("gate: the shadow run of the Rust gate failed; gate.py's verdict stands\n", stderr)

    def test_without_a_verdict_of_gate_py_s_the_rust_gate_does_not_judge(self):
        self.pin({"gate.py": RELEASED_GATE_PY + JUDGING_GATE_PY, "shadow-run.sh": SHADOW_STUB})
        self.assertEqual(self.gate(gate_exit=2)[0], 2)
        self.assertIsNone(self.shadow_args())

    def test_a_test_command_gate_py_could_not_judge_leaves_the_rust_gate_out(self):
        # Given the reports, the Rust gate would not know of the command's exit status, and would differ for that.
        self.pin({"gate.py": RELEASED_GATE_PY + JUDGING_GATE_PY, "shadow-run.sh": SHADOW_STUB})
        unjudged = ("The test command `a` exited 5, but its JUnit reports list no failing test, so its tests were not "
                    "judged.")
        status, stderr = self.gate(gate_exit=1, condition=unjudged)
        self.assertEqual(status, 1)
        self.assertIn("gate: no shadow run of the Rust gate: gate.py could not judge a test command\n", stderr)
        self.assertIsNone(self.shadow_args())

    def test_a_failing_test_that_quotes_such_a_condition_does_not_leave_the_rust_gate_out(self):
        self.pin({"gate.py": RELEASED_GATE_PY + JUDGING_GATE_PY, "shadow-run.sh": SHADOW_STUB})
        status, stderr = self.gate(gate_exit=1, condition="1 tests fail; each is listed below.",
                                   detail='assert conditions == ["The test command `a` exited 5"]')
        self.assertEqual(status, 1)
        self.assertNotIn("no shadow run", stderr)
        self.assertIsNotNone(self.shadow_args())

    def test_a_release_without_the_rust_gate_is_judged_by_gate_py_alone(self):
        self.pin({"gate.py": RELEASED_GATE_PY + JUDGING_GATE_PY})
        status, stderr = self.gate()
        self.assertEqual(status, 0)
        self.assertNotIn("shadow run", stderr)


# uv as scripts/gate calls it: `sync` does nothing, and `run [--option]... COMMAND` runs COMMAND, python being the
# machine's python3.
UV_STUB = """#!/bin/sh
[ "$1" = sync ] && exit 0
shift
while [ "${1#--}" != "$1" ]; do shift; done
if [ "$1" = python ]; then shift; exec python3 "$@"; fi
exec "$@"
"""

TWO_TEST_COMMANDS = """[[test]]
name = "a"
command = ["true"]
junit = ["a.xml"]
coverage = ["a cov.xml"]

[[test]]
name = "b"
command = ["true"]
junit = ["b.xml"]
"""

# What makes RELEASED_GATE_PY a gate.py that judges: it writes the file after --out, GATE: ERROR when it exits 1, and
# quality.json beside it with FAKE_CONDITION as the failed condition and FAKE_DETAIL as what a failing test said, and
# exits FAKE_GATE_EXIT.
JUDGING_GATE_PY = """
if __name__ == "__main__":
    import json, os, sys
    out = sys.argv[sys.argv.index("--out") + 1]
    status = int(os.environ["FAKE_GATE_EXIT"])
    with open(out, "w") as f:
        f.write("GATE: ERROR\\n" if status == 1 else "GATE: OK\\n")
    with open(out.removesuffix(".md") + ".json", "w") as f:
        json.dump({"failed_conditions": [c for c in [os.environ["FAKE_CONDITION"]] if c],
                   "failing_tests": [{"detail": d} for d in [os.environ["FAKE_DETAIL"]] if d]}, f)
    sys.exit(status)
"""

# The release's shadow-run.sh: records its arguments, one a line, in SHADOW_LOG and exits FAKE_SHADOW_EXIT.
SHADOW_STUB = """#!/bin/sh
printf '%s\\n' "$@" > "$SHADOW_LOG"
exit "$FAKE_SHADOW_EXIT"
"""

# The part of a released gate.py that reads gate.toml, as the fetch stub above unpacks it: it refuses any key but test.
RELEASED_GATE_PY = """import tomllib
class GateError(Exception):
    pass
class GateSettings:
    @classmethod
    def read(cls, path):
        with open(path, "rb") as f:
            for key in tomllib.load(f):
                if key != "test":
                    raise GateError(f"{path}: unknown key `{key}`")
"""
