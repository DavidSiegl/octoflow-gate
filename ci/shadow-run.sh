#!/bin/sh
# The shadow run of the Rust gate (gate-standalone/17): judge with the `gate-linux-x86_64` beside this script a change
# that gate.py has already judged, and write how the two reports differ. gate.py's verdict stands; this one decides
# nothing, and a build archives what it writes for a person to read. A release ships it as `shadow-run.sh`, beside
# both gates, until the Rust gate judges alone.
#
#   shadow-run.sh <dir> GATE_ARGS...
#
# <dir> holds quality.md and quality.json as `gate.py --out <dir>/quality.md GATE_ARGS...` wrote them. GATE_ARGS are
# the arguments gate.py was given but for --out and --json, which the Rust gate takes as they are (it ignores
# --rust-syntax), and `--python PATH` when the judged project's Python is not PATH's python3. A caller whose gate.py
# ran gate.toml's [[test]] commands gives their reports as --tests and --coverage, so that the tests do not run
# twice. It is run from the working tree gate.py judged, as gate.py was.
#
# Writes into <dir>, replacing what an earlier run left:
#
#   quality-rust.md, quality-rust.json   the Rust gate's report, when it could judge the change
#   quality-diff.txt                     empty when the two reports agree; else `diff -u` of the quality files and of
#                                        the JSON, gate.py's first; or, when the Rust gate could not judge, a line
#                                        saying so with its exit status and what it printed on standard error
#
# docs/port-differences.md lists the differences that are meant. Exits 0 once quality-diff.txt is written, whatever
# it holds, and 2, saying why on standard error, when the arguments are wrong, <dir> lacks gate.py's reports or a file
# cannot be written. Needs diff.
set -u

[ $# -ge 1 ] || { echo "usage: shadow-run.sh <dir> GATE_ARGS..." >&2; exit 2; }
dir=$1
shift
gate=$(dirname "$0")/gate-linux-x86_64

for report in quality.md quality.json; do
    [ -r "$dir/$report" ] || { echo "shadow-run.sh: $dir has no $report of gate.py's to compare with" >&2; exit 2; }
done
rm -f "$dir/quality-rust.md" "$dir/quality-rust.json" "$dir/quality-diff.txt" ||
    { echo "shadow-run.sh: cannot remove an earlier run's files from $dir" >&2; exit 2; }
said=$(mktemp) || { echo "shadow-run.sh: cannot make a temporary file" >&2; exit 2; }
trap 'rm -f "$said"' EXIT

status=0
"$gate" "$@" --out "$dir/quality-rust.md" 2>"$said" || status=$?
cat "$said" >&2

{
    case $status in
    0 | 1)
        # diff exits 1 for files that differ, which is what it is asked to show, and 2 when it could not read one.
        for report in md json; do
            diff -u "$dir/quality.$report" "$dir/quality-rust.$report"
            [ $? -le 1 ] || echo "shadow-run.sh: diff could not compare quality.$report with quality-rust.$report"
        done
        ;;
    *)
        echo "The Rust gate could not judge the change (exit status $status). It said:"
        cat "$said"
        ;;
    esac
} >"$dir/quality-diff.txt" || { echo "shadow-run.sh: cannot write $dir/quality-diff.txt" >&2; exit 2; }
