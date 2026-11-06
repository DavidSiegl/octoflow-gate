"""Settings and guards every test file shares: Hypothesis's profiles, the repository's top directory, no network,
and no real sleeping.

The guards, each a fixture every test runs under, and each for this process only, not for a child process a test
starts:

- No test may open a network connection, so a test that reaches for the real network is a bug, and fails with
  `NetworkUsed`, naming the address. A Unix socket is not the network.
- No test may wait in time.sleep: a real sleep makes the suite slow, and a test that depends on how long one takes
  flaky. The test code is guarded; gate.py and the standard library are not: subprocess sleeps while it waits for a
  child with a timeout, which is how the gate runs its tools.
"""

import os
import pathlib
import socket
import sys
import time

import pytest
from guard_errors import NetworkUsed, RealSleep
from hypothesis import HealthCheck, settings

# How many examples each property test tries. `default` keeps a local `uv run pytest` fast; the gate's pytest command
# (gate.toml) takes `ci`, ten times as many, with --hypothesis-profile, which wins over HYPOTHESIS_PROFILE. No
# deadline: the tests that run git or sh for each example take tens of milliseconds an example, and their speed is not
# what they test.
# differing_executors is off because the gate's mutmut runs the tests more than once in one process, and unittest
# makes a new TestCase for each run, so the second calls each @given method on another self. The check guards against
# state kept on self from one example to the next, and the @given methods of tests/test_properties.py keep none: they
# read self only to assert.
_UNCHECKED = [HealthCheck.differing_executors]
settings.register_profile("default", max_examples=25, deadline=None, suppress_health_check=_UNCHECKED)
settings.register_profile("ci", max_examples=250, deadline=None, suppress_health_check=_UNCHECKED)
settings.load_profile(os.environ.get("HYPOTHESIS_PROFILE", "default"))

# The real calls, taken before any test can replace them, which a guarded call hands on to when it is allowed.
_connect = socket.socket.connect
_connect_ex = socket.socket.connect_ex
_create_connection = socket.create_connection
_sleep = time.sleep

# The top directory of the tree the tests run in, and import gate.py from: the repository, or
# under mutmut its copy of it, mutants/ at the repository's top (mutmut's fixed name; gate.py's _MUTMUT_STATE).
_ROOT = pathlib.Path(__file__).resolve().parent.parent

# The repository's top directory, whose files the tests read as text: the docs, and gate.py unmutated. Under mutmut,
# which runs the tests in _ROOT = mutants/, that is the directory above, the one mutmut copied this file from:
# mutmut's copy of the code holds every mutant beside each function it mutates, and mutants/ holds only what
# [tool.mutmut]'s source_paths and also_copy (pyproject.toml) and mutmut's own defaults (tests/, pyproject.toml, the
# lock files) name, so not README.md. A checkout that is itself named mutants has no tests/conftest.py above it.
_IN_MUTMUT_COPY = _ROOT.name == "mutants" and (_ROOT.parent / "tests" / "conftest.py").is_file()
REPO = _ROOT.parent if _IN_MUTMUT_COPY else _ROOT

# The code whose time.sleep calls the sleep guard refuses: the tests.
_GUARDED_TESTS = _ROOT / "tests"


def pytest_configure(config):
    """Register the marker that lets a test past the sleep guard."""
    config.addinivalue_line(
        "markers",
        "real_sleep: the test waits on something outside this process's control, a child process, by polling it with "
        "time.sleep: what it tests is how a program behaves with real processes and real time. Keep such waits short "
        "and bounded by a deadline.")


@pytest.fixture(autouse=True)
def no_network(monkeypatch):
    """For the length of each test, make socket.socket.connect, connect_ex and socket.create_connection raise
    NetworkUsed for any address but a Unix socket's."""

    def check(family, address):
        if family != socket.AF_UNIX:
            raise NetworkUsed(f"the test connected to {address!r}, and tests connect to no address (tests/conftest.py)")

    def connect(sock, address):
        check(sock.family, address)
        return _connect(sock, address)

    def connect_ex(sock, address):
        check(sock.family, address)
        return _connect_ex(sock, address)

    # Checked before the real one resolves the host name, so that no DNS query goes out either.
    def create_connection(address, *args, **kwargs):
        check(socket.AF_INET, address)
        return _create_connection(address, *args, **kwargs)

    monkeypatch.setattr(socket.socket, "connect", connect)
    monkeypatch.setattr(socket.socket, "connect_ex", connect_ex)
    monkeypatch.setattr(socket, "create_connection", create_connection)


@pytest.fixture(autouse=True)
def no_real_sleep(request, monkeypatch):
    """For the length of each test not marked real_sleep, make time.sleep raise RealSleep when called from a test
    file; called from anywhere else, such as gate.py or the standard library, it sleeps."""
    if request.node.get_closest_marker("real_sleep") is not None:
        return

    def sleep(seconds):
        caller = pathlib.Path(sys._getframe(1).f_code.co_filename).resolve()
        if caller.is_relative_to(_GUARDED_TESTS):
            raise RealSleep(f"time.sleep({seconds!r}) in {caller.name}: a test that must wait on a real process is "
                            f"marked real_sleep (tests/conftest.py)")
        _sleep(seconds)

    monkeypatch.setattr(time, "sleep", sleep)
