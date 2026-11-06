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
