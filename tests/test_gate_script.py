"""scripts/gate, the shell script CI runs: how it picks BASE and the judge before it runs anything."""

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
