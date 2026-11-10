"""Which tests a coverage.py data file records running given lines, read through coverage's API, not its SQLite schema.

The Rust gate runs this with `python -I -c` (python_mutants.rs). argv[1] is the data file, argv[2] JSON
[[path, first, last], ...], each path real and absolute. It prints JSON {"tests": n, "busiest": m, "reached": [...]}:
n the tests the file records a context of, m the most of them it records running any one line, and for each span the
sorted pytest node ids of the tests it records running a line from first to last, both included, or null when the
file did not measure the span's path. A test's contexts are pytest-cov's `<node id>|setup`, `|run` and `|teardown`; a
measured path relative in the file is taken from the current directory. A missing file, and any error of coverage's,
exit with status 1 and say so on standard error.
"""

import json, os, sys
from coverage import CoverageData
if not os.path.isfile(sys.argv[1]):
    sys.exit(f"no such file: {sys.argv[1]}")
recorded = CoverageData(basename=sys.argv[1])
recorded.read()
by_path = {}
for measured in recorded.measured_files():
    by_path[os.path.realpath(measured)] = {
        line: {c.rpartition("|")[0] for c in contexts if c.rpartition("|")[2] in ("setup", "run", "teardown")}
        for line, contexts in recorded.contexts_by_lineno(measured).items()}
by_line = [tests for lines in by_path.values() for tests in lines.values()]
reached = []
for path, first, last in json.loads(sys.argv[2]):
    lines = by_path.get(path)
    reached.append(None if lines is None else
                   sorted(set().union(*(tests for line, tests in lines.items() if first <= line <= last))))
print(json.dumps({"tests": len(set().union(*by_line)), "busiest": max(map(len, by_line), default=0),
                  "reached": reached}))
