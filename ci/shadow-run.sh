#!/bin/sh
# The shadow run of the Rust gate (gate-standalone/17): judge with the `gate-linux-x86_64` beside this script a change
# that gate.py has already judged, and write how the two reports differ. gate.py's verdict stands; this one decides
# nothing, and a build archives what it writes for a person to read. A release ships it as `shadow-run.sh`, beside
# both gates, until the Rust gate judges alone.
#
#   shadow-run.sh <dir> GATE_ARGS...
#
# <dir> holds quality.md and quality.json as `gate.py --out <dir>/quality.md GATE_ARGS...` wrote them. GATE_ARGS are
# the arguments gate.py was given, which the Rust gate takes as they are (it ignores --rust-syntax; an --out or
# --json among them gives way to this script's), and `--python PATH` when the judged project's Python is not PATH's
# python3. A caller whose gate.py ran gate.toml's [[test]] commands gives their reports as --tests and --coverage,
# so that the tests do not run twice, and makes no shadow run when gate.py reported a command it could not judge
# ("The test command ..." among quality.json's failed conditions): given the reports, the Rust gate cannot know
# of it, and the difference would be the run's, not the gates'. It is run from the working tree gate.py judged, as
# gate.py was.
#
# Writes into <dir>, replacing what an earlier run left:
#
#   quality-rust.md, quality-rust.json   the Rust gate's report, when it could judge the change
#   quality-diff.txt                     empty when the two reports agree; else `diff -u` of the quality files and of
#                                        the JSON, gate.py's first; or, when the Rust gate could not judge, a line
#                                        saying so with its exit status
#
# The Rust gate is this script's foreground command: its standard error is the script's, where it says why it could
# not judge, and nothing limits its time, so one that hangs holds its caller until that stops it. On Ctrl-C the gate
# stops what it started and the script, having waited for it, exits 130 and writes no quality-diff.txt; neither
# happens where SIGINT is ignored, as in a Jenkins step. SIGHUP or SIGTERM ends the script at once, and the gate with
# it when it went to both, as an aborted build's does; what the gate had started then runs on.
#
# The differences that are meant are listed in docs/port-differences.md of the gate's repository,
# https://github.com/DavidSiegl/octoflow-gate/blob/main/docs/port-differences.md. Exits 0 once quality-diff.txt is
# written, whatever it holds, and 2, saying why on standard error, when the arguments are wrong, <dir> lacks gate.py's
# reports or a file cannot be written. Needs `diff`.
set -u

[ $# -ge 1 ] || { echo "usage: shadow-run.sh <dir> GATE_ARGS..." >&2; exit 2; }
dir=$1
shift
gate=$(dirname "$0")/gate-linux-x86_64

for name in quality.md quality.json; do
    [ -r "$dir/$name" ] || { echo "shadow-run.sh: $dir has no $name of gate.py's to compare with" >&2; exit 2; }
done
rm -f "$dir/quality-rust.md" "$dir/quality-rust.json" "$dir/quality-diff.txt" ||
    { echo "shadow-run.sh: cannot remove an earlier run's files from $dir" >&2; exit 2; }
# Without a trap the shell would die of Ctrl-C at once, the gate still stopping what it started; with one it runs the
# trap when its foreground command has ended.
trap 'exit 130' INT

# --out and --json come last, so that they win over any among GATE_ARGS.
"$gate" "$@" --out "$dir/quality-rust.md" --json "$dir/quality-rust.json"
status=$?

{
    case $status in
    0 | 1)
        # diff exits 1 for files that differ, which is what it is asked to show, and 2 when it could not read one.
        for format in md json; do
            diff -u "$dir/quality.$format" "$dir/quality-rust.$format"
            [ $? -le 1 ] || echo "shadow-run.sh: diff could not compare quality.$format with quality-rust.$format"
        done
        ;;
    *)
        echo "The Rust gate could not judge the change (exit status $status); its standard error says why."
        ;;
    esac
} >"$dir/quality-diff.txt" || { echo "shadow-run.sh: cannot write $dir/quality-diff.txt" >&2; exit 2; }
