#!/usr/bin/env bash
# SPDX-License-Identifier: Apache-2.0
# Copyright 2026 Andrew Yates
#
# handback-fake-aterm.sh — a stand-in `aterm` that plays the SHELL'S JOB
# CONTROL against tools/test-foreground-handback.sh under the schedule a loaded
# machine can deal it (tests/handback_lane_keys.rs).
#
# It models one thing: WHO HOLDS THE TERMINAL. A command line that starts a job
# (`fg`, a line running `sleep 30` — the mouse jobs, the pipelines and the
# recovery row's re-arming job — or the live-holder row's python, which
# prints `parent-back`) hands the terminal to the job; Ctrl-Z or Ctrl-C that
# reaches the job hands it back to the shell (one that reaches the shell is
# absorbed, and the job still takes the terminal), and so does the Enter that
# releases the live holder (it reads the line and kills itself). Each handover lands
# only at the SECOND
# `who` read after it was caused — the adversarial schedule: nothing about it
# is visible to a driver that does not read the holder, however long it waits
# on a quiet screen. That is the 2026-09-27 race, where zsh's `fg` printed the
# job line and a loaded scheduler ran the lane's Ctrl-C before zsh's
# `tcsetpgrp` gave the job the terminal.
#
# It writes a VIOLATION line to "$FAKE_ATERM_STATE/violations" when the lane
#   * sends Ctrl-C/Ctrl-Z while the shell holds the terminal (the key reaches
#     the shell, and the job it was meant for lives on), or the live holder's
#     release (a bare Enter: the shell reads an empty line, and the holder
#     waits on);
#   * presses Enter on a command line while a job holds it (the job reads the
#     line; the shell never runs it);
#   * waits `await match <re>` on a pattern one of its own typed lines
#     matches (the line's echo satisfies the wait before the command ran).
# Every other verb answers at once: `modes` with one dummy row, so the lane's
# mode rows FAIL here — the test reads the violations, not the lane's verdict.
set -u
st=${FAKE_ATERM_STATE:?names the state directory of the fake}

violation() {
    printf 'VIOLATION: %s\n' "$*" >>"$st/violations"
}

holder() { cat "$st/holder"; }

if [ "${1:-}" = --headless ]; then
    case " $* " in *" --help "*)
        echo "--lifeline-fd <n>"
        exit 0
        ;;
    esac
    sock=""
    while [ $# -gt 0 ]; do
        [ "$1" = --control-sock ] && sock=$2
        shift
    done
    echo shell >"$st/holder"
    : >"$st/pending"
    : >"$st/line"
    rm -f "$st/polled"
    echo boot >>"$st/log"
    exec python3 -c 'import socket,sys,time
s=socket.socket(socket.AF_UNIX);s.bind(sys.argv[1]);s.listen(8);time.sleep(600)' "$sock"
fi

[ "${1:-}" = ctl ] || exit 64
shift
# `ctl --sock <path> --timeout <s> <verb> …`
while [ $# -gt 0 ]; do
    case $1 in
        --sock | --timeout) shift 2 ;;
        *) break ;;
    esac
done
verb=${1:-}
shift || true

# One function per verb: bash 3.2 misparses command substitutions inside a
# `case` arm, so the dispatch is a lookup, not a `case`.
v_version() { echo "aterm fake"; }

v_who() {
    local pending pg=100
    pending=$(cat "$st/pending")
    if [ -n "$pending" ]; then
        if [ -e "$st/polled" ]; then
            echo "$pending" >"$st/holder"
            : >"$st/pending"
            rm -f "$st/polled"
        else
            : >"$st/polled"
        fi
    fi
    [ "$(holder)" = job ] && pg=200
    echo "0 s-fake driving=- watchers=0 turns=0 alive nonce=0 fgpgid=$pg"
}

hand_to() {
    echo "$1" >"$st/pending"
    rm -f "$st/polled"
}

v_send() {
    local payload key=""
    payload=$(cat)
    [ "$payload" = "$(printf '\003')" ] && key=Ctrl-C
    [ "$payload" = "$(printf '\032')" ] && key=Ctrl-Z
    [ "$payload" = "$(printf '\r')" ] && key=Enter
    if [ -z "$key" ]; then
        printf '%s' "$payload" >>"$st/line"
        return
    fi
    if [ "$(holder)" != job ]; then
        # The shell absorbs it; the job it was meant for is untouched.
        violation "$key sent while the shell held the terminal"
        return
    fi
    [ "$key" = Enter ] && echo "released the holder" >>"$st/log"
    hand_to shell
}

v_key() {
    local line
    line=$(cat "$st/line")
    : >"$st/line"
    if [ "$(holder)" != shell ]; then
        violation "a command line typed while a job held the terminal: $line"
    fi
    printf '%s\n' "$line" >>"$st/typed"
    printf 'typed %s\n' "$line" >>"$st/log"
    if [ "$line" = fg ] || [ "${line#*sleep 30}" != "$line" ] ||
        [ "${line#*parent-}" != "$line" ]; then
        hand_to job
    fi
}

v_await() {
    local re echoed=""
    if [ "${1:-}" = match ]; then
        re=$2
        printf 'await %s\n' "$re" >>"$st/log"
        [ -e "$st/typed" ] && echoed=$(grep -E -e "$re" "$st/typed" | head -n 1)
        if [ -n "$echoed" ]; then
            violation "await match /$re/ is satisfied by the echo of a typed line: $echoed"
        fi
    fi
    echo "OK ${1:-} 1"
}

v_signal() {
    hand_to shell
    echo "OK signalled pgrp 200 discarded=0"
}

v_modes() { echo "fake=1"; }

if [ "$(type -t "v_$verb")" = function ]; then
    "v_$verb" "$@"
fi
exit 0
