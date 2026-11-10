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
# The Rust gate has GATE_MINUTES to judge and is then stopped, and this script ends within a minute of that, so that
# it cannot hold up a build whose verdict gate.py has given. What a stopped gate had started in a session of its
# own, as it starts its tools and test commands, is not stopped with it and may run on after this script. The limit
# is fixed, so a project whose budgets (--mutation-budget, --base-run-budget, gate.toml's [budgets]) or whose own
# test commands take the gate past it gets no comparison. The gate's standard error comes through as it is printed.
#
# SIGHUP and SIGTERM stop the gate with the script, which exits 143. SIGINT is, for the gate, what it was for this
# script: ignored when a Jenkins step runs it, and the gate then judges on through one sent to the script or its
# process group; else Ctrl-C, on which the gate stops what it started and the script exits 130.
#
# The differences that are meant are listed in docs/port-differences.md of the gate's repository,
# https://github.com/DavidSiegl/octoflow-gate/blob/main/docs/port-differences.md. Exits 0 once quality-diff.txt is
# written, whatever it holds, and 2, saying why on standard error, when the arguments are wrong, <dir> lacks gate.py's
# reports or a file cannot be written. Needs Linux's /proc, GNU coreutils' timeout, and awk, diff and sh on PATH.
set -u

# How long the Rust gate may take, in minutes: a fixed bound below a build's own limit, which the gate's budgets are
# not held to (the header says whom it cuts off).
GATE_MINUTES=20
# Seconds a stopped gate has to end before it is killed.
KILL_AFTER=30
# Seconds this script waits, once the gate has ended, for the rest of what it and its tools printed, as the gate
# waits on a tool's (KILL_WAIT in crates/gate/src/process.rs).
SAID_WAIT=5

[ $# -ge 1 ] || { echo "usage: shadow-run.sh <dir> GATE_ARGS..." >&2; exit 2; }
dir=$1
shift
gate=$(dirname "$0")/gate-linux-x86_64

for name in quality.md quality.json; do
    [ -r "$dir/$name" ] || { echo "shadow-run.sh: $dir has no $name of gate.py's to compare with" >&2; exit 2; }
done
rm -f "$dir/quality-rust.md" "$dir/quality-rust.json" "$dir/quality-diff.txt" ||
    { echo "shadow-run.sh: cannot remove an earlier run's files from $dir" >&2; exit 2; }
# Holds `said`, what the Rust gate printed on standard error, and `err`, the pipe it prints that into.
work=$(mktemp -d) || { echo "shadow-run.sh: cannot make a temporary directory" >&2; exit 2; }
# The process ids of timeout, which runs the gate, and of the tee that keeps what the gate prints; empty while
# there is none to stop.
gate_pid=
said_pid=
trap '[ -z "$said_pid" ] || kill "$said_pid" 2>/dev/null; rm -rf "$work"' EXIT

# Pass the signal named $1 on to timeout, which passes it on to the gate, and exit with $2. A shell that is told to
# stop runs no EXIT trap unless it exits by its own hand. The gate runs in the background and the script waits for
# it, as a shell holds a trap back until a command in the foreground has ended.
stopped() {
    [ -z "$gate_pid" ] || kill -s "$1" "$gate_pid" 2>/dev/null
    exit "$2"
}
trap 'stopped HUP 143' HUP
trap 'stopped TERM 143' TERM
# On Ctrl-C the gate stops what it started, and the terminal is given back when it has.
trap '[ -z "$gate_pid" ] || { kill -s INT "$gate_pid"; wait "$gate_pid"; } 2>/dev/null; exit 130' INT

# What timeout runs: the gate as this script was started, for SIGINT. timeout starts its command with SIGINT at its
# default, also when it was itself started with SIGINT ignored. The last hexadecimal digit of SigIgn in proc(5)'s
# status holds signals 1 to 4, SIGINT being 2.
ignored=$(awk '/^SigIgn:/ { print substr($2, length($2)) }' "/proc/$$/status") ||
    { echo "shadow-run.sh: cannot read /proc/$$/status for the signals this script ignores" >&2; exit 2; }
case $ignored in
[2367abef]) start='trap "" INT; exec "$@"' ;;
*) start='exec "$@"' ;;
esac

# The gate's standard error goes to this script's as it comes, and into `said`. tee is not the gate's pipeline, as
# it reads until everything that holds the gate's standard error has ended, which a tool of a stopped gate may not
# for long: it has SAID_WAIT after the gate.
mkfifo "$work/err" || { echo "shadow-run.sh: cannot make a pipe for what the Rust gate prints" >&2; exit 2; }
tee "$work/said" <"$work/err" >&2 &
said_pid=$!
# --out and --json come last, so that they win over any among GATE_ARGS. timeout makes a process group of its own
# for itself and the gate, so a signal to this script's group reaches them through the traps alone.
timeout --kill-after="$KILL_AFTER" "${GATE_MINUTES}m" sh -c "$start" sh "$gate" "$@" \
    --out "$dir/quality-rust.md" --json "$dir/quality-rust.json" 2>"$work/err" &
gate_pid=$!
wait "$gate_pid"
status=$?
gate_pid=
(sleep "$SAID_WAIT" && kill "$said_pid") >/dev/null 2>&1 &
cutter=$!
# The shell would else report a tee that was cut off as "Terminated".
wait "$said_pid" 2>/dev/null
said_status=$?
said_pid=
kill "$cutter" 2>/dev/null
# 143 is tee cut off after SAID_WAIT, having kept what the gate itself printed.
[ "$said_status" -eq 0 ] || [ "$said_status" -eq 143 ] ||
    { echo "shadow-run.sh: cannot keep what the Rust gate printed" >&2; exit 2; }

{
    case $status in
    0 | 1)
        # diff exits 1 for files that differ, which is what it is asked to show, and 2 when it could not read one.
        for format in md json; do
            diff -u "$dir/quality.$format" "$dir/quality-rust.$format"
            [ $? -le 1 ] || echo "shadow-run.sh: diff could not compare quality.$format with quality-rust.$format"
        done
        ;;
    124)
        # timeout's exit status for a program it stopped. One it had to kill gives 137, as does a gate that anything
        # else killed, so that is told as the exit status it is, below.
        echo "The Rust gate was stopped after $GATE_MINUTES minutes without a verdict. It said:"
        cat "$work/said"
        ;;
    *)
        echo "The Rust gate could not judge the change (exit status $status). It said:"
        cat "$work/said"
        ;;
    esac
} >"$dir/quality-diff.txt" || { echo "shadow-run.sh: cannot write $dir/quality-diff.txt" >&2; exit 2; }
