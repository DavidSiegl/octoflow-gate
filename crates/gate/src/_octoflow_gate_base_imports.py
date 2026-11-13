"""The pytest plugin by which a run of the changed tests at the base tells where its modules came from.

The Rust gate writes this file beside the copy of the base and loads it with `-p` (python_at_base.rs). At the end of
the session it writes, as a JSON object to the file --gate-imports-report names, each module in sys.modules whose
__file__ lies under the directory --gate-imports-root names: its name, and that file's path from there.

Under pytest-xdist each worker, which imports the tests where the controller does not, writes its own report, the
file's name followed by "-" and the worker's id, as xdist gives the worker its config.workerinput; not
PYTEST_XDIST_WORKER, which a pytest the gate runs from inside a worker of its own inherits. The file's directory is
resolved, and the root, so that a link on the way to either changes nothing; the file itself is not, so that a module
that is a link is named by the link. A namespace package has no __file__, and none is written.
"""

from __future__ import annotations

import contextlib
import json
import os
import sys
from typing import TYPE_CHECKING

# Only for mypy: pytest exports these from 7.0 on, and the judged project's pytest may be older.
if TYPE_CHECKING:
    from pytest import Parser, Session


def pytest_addoption(parser: Parser) -> None:
    """Add the two options the gate gives each run at the base."""
    parser.addoption("--gate-imports-root")
    parser.addoption("--gate-imports-report")


def pytest_sessionfinish(session: Session) -> None:
    """Write the report of the modules imported from under the root."""
    root = os.path.realpath(session.config.getoption("gate_imports_root"))
    found: dict[str, str] = {}
    for name, module in list(sys.modules.items()):
        where = None
        # A module that computes its attributes, as a lazy loader's, may raise anything.
        with contextlib.suppress(Exception):
            where = getattr(module, "__file__", None)
        if isinstance(where, str):
            real = os.path.join(os.path.realpath(os.path.dirname(where)), os.path.basename(where))
            if real.startswith(root + os.sep):
                found[name] = os.path.relpath(real, root)
    report = session.config.getoption("gate_imports_report")
    worker = getattr(session.config, "workerinput", {}).get("workerid")
    with open(f"{report}-{worker}" if worker else report, "w", encoding="utf-8") as f:
        json.dump(found, f)
