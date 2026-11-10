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
#                                        the JSON, gate.py's first; or, when the Rust gate could not judge or was
#                                        stopped, a line saying so and what it printed on standard error
#
# The Rust gate has GATE_MINUTES to judge and is then stopped, so that it cannot hold up a build whose verdict
# gate.py has given; what it had started may outlive it by the time those take to end. Its standard error comes
# through as it is printed.
#
# The differences that are meant are listed in docs/port-differences.md of the gate's repository,
# https://github.com/DavidSiegl/octoflow-gate/blob/main/docs/port-differences.md. Exits 0 once quality-diff.txt is
# written, whatever it holds, and 2, saying why on standard error, when the arguments are wrong, <dir> lacks gate.py's
# reports or a file cannot be written. Needs diff, tee and timeout.
set -u

# How long the Rust gate may take, in minutes: above what its checks' budgets add up to, below a build's own limit.
GATE_MINUTES=20

[ $# -ge 1 ] || { echo "usage: shadow-run.sh <dir> GATE_ARGS..." >&2; exit 2; }
dir=$1
shift
gate=$(dirname "$0")/gate-linux-x86_64

for name in quality.md quality.json; do
    [ -r "$dir/$name" ] || { echo "shadow-run.sh: $dir has no $name of gate.py's to compare with" >&2; exit 2; }
done
rm -f "$dir/quality-rust.md" "$dir/quality-rust.json" "$dir/quality-diff.txt" ||
    { echo "shadow-run.sh: cannot remove an earlier run's files from $dir" >&2; exit 2; }
# Holds `said`, what the Rust gate printed on standard error, and `status`, its exit status.
work=$(mktemp -d) || { echo "shadow-run.sh: cannot make a temporary directory" >&2; exit 2; }
trap 'rm -rf "$work"' EXIT
# A shell that is told to stop runs no EXIT trap unless it exits by its own hand.
trap 'exit 143' HUP TERM

# --out and --json come last, so that they win over any among GATE_ARGS. The gate's standard error goes to this
# script's as it comes and into `said`; its standard output goes round the pipe, through descriptor 3; and its exit
# status goes through a file, a pipeline having only its last command's.
{
    {
        timeout --kill-after=30 "${GATE_MINUTES}m" "$gate" "$@" \
            --out "$dir/quality-rust.md" --json "$dir/quality-rust.json" 2>&1 >&3 3>&-
        echo $? >"$work/status"
    } | tee "$work/said" >&2
} 3>&1 || { echo "shadow-run.sh: cannot keep what the Rust gate printed" >&2; exit 2; }
status=$(cat "$work/status") || { echo "shadow-run.sh: cannot read the Rust gate's exit status" >&2; exit 2; }

{
    case $status in
    0 | 1)
        # diff exits 1 for files that differ, which is what it is asked to show, and 2 when it could not read one.
        for format in md json; do
            diff -u "$dir/quality.$format" "$dir/quality-rust.$format"
            [ $? -le 1 ] || echo "shadow-run.sh: diff could not compare quality.$format with quality-rust.$format"
        done
        ;;
    124 | 137)
        # timeout's exit statuses for a program it stopped, and for one it had to kill.
        echo "The Rust gate was stopped after $GATE_MINUTES minutes without a verdict. It said:"
        cat "$work/said"
        ;;
    *)
        echo "The Rust gate could not judge the change (exit status $status). It said:"
        cat "$work/said"
        ;;
    esac
} >"$dir/quality-diff.txt" || { echo "shadow-run.sh: cannot write $dir/quality-diff.txt" >&2; exit 2; }
