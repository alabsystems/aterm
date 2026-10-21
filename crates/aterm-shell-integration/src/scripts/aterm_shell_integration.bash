#!/bin/bash
# aterm_shell_integration.bash - Shell integration for aTerm
#
# Copyright 2026 Andrew Yates
# Author: Andrew Yates
# Licensed under the Apache License, Version 2.0
#
# Source this file in your ~/.bashrc or ~/.bash_profile:
#   test -e ~/.config/aterm/shell_integration.bash && source ~/.config/aterm/shell_integration.bash
#
# Features enabled:
# - Directory tracking (OSC 7): tab title updates, "Open Terminal Here" support
# - Command tracking (OSC 133): command history indexing, timing, notifications
# - The managed dirs, LIVE: an already-running session shell resolves `claude`/`codex`
#   to atpkg's <prefix>/agents twins (and cargo/rustc to <prefix>/reroute) the moment
#   atpkg lays them — no new tab, no `exec bash` (owner ask 2026-09-16; see "LIVE" below)
# - A LOADER and a BODY (2026-09-26): the shell that is ALREADY RUNNING takes a newer
#   build's integration at its next prompt, when the host that owns it points it there
#   (see "THE LOADER" below)
# - The tty settings, REPAIRED (2026-09-26): a raw-mode program that exits non-zero
#   without restoring them no longer leaves Ctrl-C and Enter broken for every later
#   command (see "The tty settings after a failed command" below)
#
# Compatible with: bash 3.2+

# Only run in interactive shells
[[ $- != *i* ]] && return

# Not traced: see THE TRACE, just below.
{ __aterm_load_flags=$-; set +xv; } 2>/dev/null

# ─── THE TRACE (2026-10-03) — the zsh script's twin ───
#
# A user's XTRACE (`set -x` at the prompt, or in an rc) traced every line aterm
# runs around a command, where bare bash traces only the command: about 165
# lines of DEBUG-trap preexec and PROMPT_COMMAND per command, and in them the
# capability nonce, in plain text, about 18 times per command, on the screen and
# in the scrollback (measured on bash 3.2.57 and 5.2.37: 668 trace lines and the
# nonce 72 times over four commands; bare bash 4 and 0). An rc's `set -x` traced
# the load too, `__aterm_shell_nonce=<nonce>` first, and so did a typed upgrade
# under one, with the new key. VERBOSE (`set -v`) echoed the command
# substitutions aterm runs (`__aterm_urlencode "$cwd"`). bash 3.2 has no
# `local -`, and `set +x` is itself traced — but on the stderr the command runs
# with, so `{ set +xv; } 2>/dev/null` turns both off and prints nothing, and
# turning XTRACE on is never traced. So every function the shell runs on aterm's
# behalf — the DEBUG trap's, the PROMPT_COMMAND trampoline, the prompt handler
# (for a loader from before this), what PS1 calls, the documented helper — OPENS
# by taking `$-` and turning both off in that group, and CLOSES by putting back
# what it took (`__aterm_flags_put`, whose own `return` is traced only into a
# silenced stderr). The user's own code that aterm runs — a scalar
# PROMPT_COMMAND, a DEBUG trap set before aterm's — runs with the user's flags
# put back first, so it is traced as in bare bash. The load does the same: both
# off on the line above, put back wherever this file returns, so an rc's
# `set -x`, or a typed upgrade under one, traces nothing of it but its first
# line, `[[ $- != *i* ]]`. What remains is bash's own trace of the hooks'
# calls — the DEBUG trap's `__aterm_preexec <$_>` before each command,
# PROMPT_COMMAND's `__aterm_prompt_command` (and an array one's last element,
# `__aterm_prompt_end`) and, on bash 5.3+, PS0's `__aterm_ps0` — and an `eval`
# line before a scalar PROMPT_COMMAND of the user's; none carries the nonce.
# Under BASH_XTRACEFD (bash 4.1+) the trace is not on stderr, so each function's
# opening `local` and `set +xv` and its closing `return` reach it too: about 15
# lines a command, no nonce (measured on bash 5.2.37; 290 and the nonce 21 times
# before). A loader
# from before this, frozen in a running shell, still traces its trampoline's own
# lines (no nonce); the body it takes does not.

# ─── The multiplexer boundary (screen / tmux) ───
#
# aterm hosts ONE session per PTY. Run screen or tmux in that PTY and the
# multiplexer owns it: every pane it draws lives inside the SAME aterm session,
# and aterm has no name for a pane. Two things break at that boundary, and both
# used to break SILENTLY:
#
#  1. $ATERM_PARENT_SESSION_ID is an ordinary exported variable, so it rides into
#     every pane shell unchanged — and `aterm ctl`'s flagless self-location then
#     resolves it to the session HOSTING the multiplexer. A flagless call typed
#     in a pane drove the OUTER terminal, said OK, and moved the wrong session.
#  2. The loader guard below is exported too, so a pane shell finds it already
#     set and returns before defining a single hook. No OSC 133 mark is ever
#     emitted from inside the multiplexer (and neither screen nor tmux forwards
#     an unknown OSC outward anyway), so command blocks, exit codes and cwd
#     tracking are ABSENT for the duration — not empty, absent.
#
# This block does not try to fix either — a pane is genuinely not an aterm
# session — it makes them VISIBLE. It MARKS the crossing ($ATERM_MUX, plus the
# outer sid so a tool can name what a flagless call would have hit), and says so
# once. `aterm ctl` reads the marks and refuses an implicitly self-targeting call
# rather than driving the wrong terminal.
#
# $ATERM_PARENT_SESSION_ID is deliberately left ALONE: it is also what provisions
# a nested aterm's parent capability edges, and the outer session really is the
# parent of anything launched from a pane. Marking costs nothing; unsetting would
# quietly disarm recursion provisioning to fix a targeting bug.
#
# The guard is what makes the detection trustworthy WHERE IT RUNS: aterm's spawn
# seam forces $ATERM_SHELL_INTEGRATION_INSTALLED to the EMPTY string for every
# session it starts, so a NON-empty value proves we did not come straight from
# aterm. That tells a real pane shell apart from an aterm window that was
# launched FROM a pane and merely inherited $TMUX/$STY.
#
# HOW OFTEN IT RUNS is the part worth saying plainly, because the answer is "in
# a pane, usually never". aterm delivers this file with `bash --rcfile <path>` —
# argv, chosen once, by whoever starts the shell. A pane shell is started by
# screen or tmux, so it gets no --rcfile and DOES NOT SOURCE THIS FILE AT ALL.
# Measured in a real GNU screen 4.09.01 window under a headless aterm on Linux:
# the pane had STY=…, TERM=screen.xterm-256color and the inherited guard, and
# $ATERM_MUX still came out EMPTY with no hook defined — the block below had not
# run. Nothing here can change that: no environment variable makes an
# INTERACTIVE bash source a file ($BASH_ENV is the non-interactive one), and the
# user's rc files are not ours to edit. So this block fires only where the file
# is genuinely sourced in a pane — a hand-installed `source …` line as the header
# above documents, or fish, whose vendor conf.d rides $XDG_DATA_DIRS into every
# pane — and `aterm ctl` carries the boundary the rest of the time.
#
# What DOES run in every session aterm starts is the tail of this file, past the
# guard, and that is where the detection now originates: $ATERM_MUX_BASE records
# the multiplexer environment THIS session shell was born into. See the export
# below.
__aterm_mux=""
if [[ -n "${TMUX:-}" ]]; then
    __aterm_mux="tmux"
elif [[ -n "${STY:-}" ]]; then
    __aterm_mux="screen"
else
    # tmux's default TERM is screen-256color, so TERM alone names the family,
    # not the program; the markers above are consulted first for that reason.
    case "${TERM:-}" in
        tmux|tmux-*|tmux.*)       __aterm_mux="tmux" ;;
        screen|screen-*|screen.*) __aterm_mux="screen" ;;
    esac
fi

# A user's ATERM_MUX=0 (or off/no/none/false) turns the whole check off:
# keep it, and say nothing.
case "${ATERM_MUX:-}" in 0|off|no|none|false) __aterm_mux="" ;; esac

# THE IN-PLACE UPGRADE (2026-09-26). The guard below is exported, so a NON-empty
# value normally proves this shell did not come straight from aterm (a pane — see
# above). There is one other way to arrive here with it set: THIS VERY SHELL
# sourcing the file again — the live agent upgrade's relaunch line sources a newer
# build's loader into a shell spawned before loaders existed (`typed_rekey` in
# aterm-shell-integration). That shell, and only that shell, already holds
# `$__aterm_shell_nonce` AND the prompt hook its integration defined,
# `__aterm_prompt_command` (every build's since 0.61): a function, never exported,
# so no pane or child shell can have inherited it. The nonce alone is no proof —
# the typed line assigns it in a NESTED shell too (the zsh script says why). For
# the shell that holds both the guard is its own, and the load goes on as an
# upgrade in place: everything below is idempotent, and what must run once per
# shell (the package blocks, the PROMPT_COMMAND and DEBUG-trap wiring) is skipped
# where it already ran.
#
# An upgrade in place first notes, for the body's AN OLDER SCRIPT'S
# `~/.aterm/bin`, the script that loaded this shell FRESH — before this load
# writes over what says so: the loader in `__aterm_si_dir` when its own load was
# the fresh one (`__aterm_fresh_load` 1), `-` for a shell from before loaders
# (neither is set), nothing it can name otherwise (a loader that itself came in
# place). Once per shell, while the body has not settled it.
if [[ -n "${ATERM_SHELL_INTEGRATION_INSTALLED:-}" && -n "${__aterm_shell_nonce+x}" ]] &&
   declare -F __aterm_prompt_command >/dev/null; then
    if [[ -z "${__aterm_old_bin_off+x}${__aterm_bin_from+x}" ]]; then
        if [[ -z "${__aterm_fresh_load+x}${__aterm_si_dir+x}" ]]; then
            __aterm_bin_from=-
        elif [[ "${__aterm_fresh_load-}" == 1 && -n "${__aterm_si_dir-}" ]]; then
            __aterm_bin_from="$__aterm_si_dir/aterm_shell_integration.bash"
        else
            __aterm_bin_from=
        fi
    fi
    __aterm_fresh_load=0
    ATERM_SHELL_INTEGRATION_INSTALLED=
else
    __aterm_fresh_load=1
fi

# Skip if already loaded — marking the boundary on the way out when the
# inherited guard means we crossed one.
if [[ -n "${ATERM_SHELL_INTEGRATION_INSTALLED:-}" ]]; then
    if [[ -n "$__aterm_mux" ]]; then
        export ATERM_MUX="$__aterm_mux"
        if [[ -n "${ATERM_PARENT_SESSION_ID:-}" ]]; then
            export ATERM_MUX_OUTER_SESSION_ID="$ATERM_PARENT_SESSION_ID"
            # Say it ONCE per multiplexer session — not once per pane, which is
            # the same true sentence six times before lunch. The stamp is keyed
            # by the multiplexer's own id ($TMUX / $STY), so every pane of one
            # screen or tmux shares it.
            if [[ "${ATERM_MUX_NOTICE:-1}" != "0" ]]; then
                __aterm_mux_id="${TMUX:-${STY:-$TERM}}"
                __aterm_mux_dir="${XDG_RUNTIME_DIR:-${TMPDIR:-/tmp}}/aterm/mux-notice"
                __aterm_mux_stamp="$__aterm_mux_dir/${__aterm_mux}-${__aterm_mux_id//[!A-Za-z0-9._-]/_}"
                if [[ ! -e "$__aterm_mux_stamp" ]] &&
                   mkdir -p "$__aterm_mux_dir" 2>/dev/null &&
                   : >"$__aterm_mux_stamp" 2>/dev/null; then
                    printf 'aterm: inside %s — no command blocks, exit codes or cwd tracking in these panes (`aterm ctl mux` explains; ATERM_MUX_NOTICE=0 silences)\n' "$__aterm_mux" >&2
                fi
                unset __aterm_mux_id __aterm_mux_dir __aterm_mux_stamp
            fi
        fi
    fi
    unset __aterm_mux
    # The user's flags, as the load found them (see THE TRACE).
    case $__aterm_load_flags in *v*) set -v ;; esac
    case $__aterm_load_flags in *x*) set -x ;; esac
    { return; } 2>/dev/null
fi
export ATERM_SHELL_INTEGRATION_INSTALLED=1
# Past the guard, so aterm started this shell ITSELF. Record the multiplexer
# environment this session shell was born into. This is the one detection input
# that comes from a place which ACTUALLY RUNS for every session, and it reaches a
# pane the only way anything can: ordinary environment inheritance. A pane's own
# $TMUX/$STY are the multiplexer's and no longer match this base — that mismatch
# IS the crossing — while an aterm window merely launched FROM a pane re-runs
# this file and re-stamps the base as its own, so it matches and is not refused.
# Same question the guard was invented to answer, asked where the answer exists.
# It also closes what TERM cannot: a tmux set to default-terminal
# "xterm-256color" is indistinguishable from an aterm window to TERM, and plainly
# a pane to this. `aterm ctl` reads it as $ATERM_MUX_BASE.
# The SPAWN SEAM stamps this for every session (aterm-gui's
# provision_child_identity_env), including sessions whose shell never sources
# this file — so an inherited pane stamp cannot masquerade as a fresh session's
# own. Keep the write only as the fallback for a host that starts a shell
# without that seam (an embedder, a hand-run integration): set it if unset,
# never overwrite the seam's answer with a value read after the pane was entered.
: "${ATERM_MUX_BASE:="${TMUX-}|${STY-}"}"
export ATERM_MUX_BASE
# Any ATERM_MUX inherited from the pane we were launched out of describes a
# multiplexer this session is not inside. Clear it, or every window opened from
# a tmux pane would inherit a refusal it does not deserve.
unset ATERM_MUX ATERM_MUX_OUTER_SESSION_ID __aterm_mux

# ─── THE LOADER (2026-09-26) ───
#
# Everything from here to the BODY marker, and the wiring after the body's end
# marker, is the LOADER: what a shell runs once and keeps for its whole life — the
# nonce and the two host channels captured and scrubbed, the PROMPT_COMMAND
# trampoline, the DEBUG-trap and PROMPT_COMMAND wiring. Everything between the two
# markers is the BODY — every mark, every prompt behaviour, the managed-PATH
# machinery — written a second time, alone, as `aterm_shell_integration_body.bash`
# in the same content-addressed folder, so a shell that is already running can
# source a NEWER build's body at its next prompt. The zsh script's "THE LOADER"
# says why and how; this is its twin. The one bash difference: the body is sourced
# from inside the PROMPT_COMMAND trampoline, with `__aterm_in_prompt_cmd` already
# set, so the DEBUG trap takes none of its lines for a user's command.

# Capture the capability nonce into a shell-local so we can immediately
# drop it from the environment (#8015). Leaving ATERM_SHELL_NONCE in the
# exported env lets every child process (env, ssh SendEnv, docker, cron,
# tmux children, ...) read the 64-hex secret that would be used to bypass
# the #7960 nonce-enforcement defense. Capture first, then unset/unexport
# BEFORE any prompt hook fires so subprocesses never inherit it.
#
# If the env var is missing or empty at source-time, __aterm_shell_nonce
# stays empty and __aterm_id_suffix falls through to the unnonced form
# (pre-nonce compatibility for hosts that have not yet authorized a
# nonce). This matches the documented fallback: the host's OSC 133/633
# handler drops sequences missing/with a wrong id= only when
# `TerminalModes::require_shell_integration_nonce` is enabled.
#
# In an upgrade in place (see above) the environment holds no nonce any more — it
# was scrubbed at this shell's first load — so the one the shell holds is kept: in
# a typed upgrade, the key its line assigned just before sourcing this file.
__aterm_shell_nonce="${ATERM_SHELL_NONCE:-${__aterm_shell_nonce:-}}"
unset ATERM_SHELL_NONCE

# Precomputed capability-nonce suffix for OSC 133/633 emissions.
# The nonce is captured exactly once (above) and the env var is unset on the
# very next line, so this string changes only through the re-key channel below,
# which rewrites it together with the nonce. Computing it here
# lets the marker emitters below expand a plain parameter instead of running
# `$(__aterm_id_suffix)`, which forks a subshell. That mattered: the prompt
# path fires five markers per command cycle (133;D + 133;A + 133;B from the
# prompt hooks, 633;E + 133;C from the DEBUG-trap preexec), i.e. five forks of
# pure dead time between Enter and the command starting. Byte-identical output
# — same ";id=<hex>" spelling, same empty-string fallback when unnonced.
# Not exported, exactly like $__aterm_shell_nonce itself, so #8015 (no nonce
# inheritance by subprocesses) is preserved.
__aterm_id_suffix_str=""
if [[ -n "$__aterm_shell_nonce" ]]; then
    __aterm_id_suffix_str=";id=${__aterm_shell_nonce}"
fi

# THE RE-KEY CHANNEL (2026-09-24) — the zsh script's twin, for the same reason:
# a seamless update that could not carry this shell's nonce left its marks
# dropped for good (`status integration=degraded`). The host names a per-session
# file at spawn ($ATERM_REKEY_PATH, in its 0700 control dir) and, to re-key an
# adopted shell, writes a fresh 64-hex nonce there (0600, exclusive, never
# through a symlink) and authorizes it; the next PROMPT_COMMAND takes it. One
# fork-free `[[ -f ]]` per prompt while nothing waits. Captured and scrubbed
# like the nonce; the key never appears in typed text, scrollback or history.
# PS1's 133;B suffix is re-derived from $__aterm_shell_nonce every prompt, so
# it follows too.
__aterm_rekey_path="${ATERM_REKEY_PATH:-${__aterm_rekey_path:-}}"
unset ATERM_REKEY_PATH

# THE BODY POINTER (the zsh script's "THE LOADER"), captured and scrubbed exactly
# like the re-key path: no child process learns it. An upgrade in place keeps the
# one the shell already has, or takes the one its relaunch line hands it.
__aterm_body_pointer="${ATERM_INTEGRATION_POINTER:-${__aterm_body_pointer:-}}"
unset ATERM_INTEGRATION_POINTER

# Where this file was loaded from — `<root>/<address>/`, the content-addressed
# folder the host prepared — and so which body is running: `<address>` when the
# folder is named by one (16 lowercase hex digits), empty for a hand-installed
# copy, which signs no revision. `BASH_SOURCE` names the file being sourced.
__aterm_si_dir="${BASH_SOURCE[0]%/*}"
__aterm_si_root="${__aterm_si_dir%/*}"
__aterm_body_rev="${__aterm_si_dir##*/}"
if [[ ${#__aterm_body_rev} -ne 16 || "$__aterm_body_rev" == *[!0-9a-f]* ]]; then
    __aterm_body_rev=""
fi

# A fresh load settles the body's AN OLDER SCRIPT'S `~/.aterm/bin` at once: this
# loader puts no `~/.aterm/bin` on PATH, so no entry there is an integration's
# and the body has nothing to look for.
if (( __aterm_fresh_load )); then
    __aterm_old_bin_off=1
fi

# Source package shell hooks — once per shell: an upgrade in place already ran
# them (the LIVE block re-sources the atpkg hook whenever it changes).
# No `~/.aterm/bin` goes on PATH: nothing creates or manages that folder, and an
# entry there ahead of everything would let whatever lands in it shadow every
# command in every session. atpkg's managed bin/ is APPENDED, by its own hook.
if (( __aterm_fresh_load )) && [ -d "$HOME/.aterm/shell.d" ]; then
    for f in "$HOME/.aterm/shell.d"/*.bash "$HOME/.aterm/shell.d"/*.sh; do
        [ -f "$f" ] && . "$f"
    done
fi

# Store the real PROMPT_COMMAND before we modify it.
# Detect array vs scalar to preserve bash 5.1+ array-style PROMPT_COMMAND.
# Once per shell: in an upgrade in place PROMPT_COMMAND is already ours, and
# capturing it as "the user's" would make the trampoline eval itself.
#
# Taken as an array when it holds an element at index 1, read without a fork: the
# `$(declare -p PROMPT_COMMAND)` this used to be cost every new bash 1.5 ms before
# its first prompt (measured 2026-10-16, bash 3.2.57). An array with nothing at
# index 1 (one element, or a sparse one) runs the scalar way with nothing lost:
# the trampoline replaces element 0 (a scalar assignment to an array) and evals
# what element 0 held, and every later element still runs after it (bash 5.1+),
# where `__aterm_preexec` skips it as a PROMPT_COMMAND element. Before 5.1 bash
# runs element 0 alone, so for a one-element array the scalar way is the only one
# that still runs the user's element.
if (( __aterm_fresh_load )); then
    __aterm_prompt_cmd_is_array=0
    if [[ -n ${PROMPT_COMMAND[1]+x} ]]; then
        __aterm_prompt_cmd_is_array=1
    fi
    __aterm_original_prompt_command="${PROMPT_COMMAND:-}"
fi

# THE BODY CHECK, first in every PROMPT_COMMAND (the trampoline below): a newer
# body waits when the host has written the pointer. Fork-free while nothing waits
# — one `[[ -f ]]` — and the read, the removal (the one fork) and the source happen
# only when a pointer waits. The revision moves only when the source succeeded: a
# body that failed to load is still reported as the one before it.
__aterm_body_check() {
    [[ -n "$__aterm_body_pointer" && -f "$__aterm_body_pointer" ]] || return 0
    local __aterm_addr="" __aterm_file=""
    { IFS= read -r __aterm_addr < "$__aterm_body_pointer"; } 2>/dev/null
    command rm -f -- "$__aterm_body_pointer"
    # Exactly 16 lowercase hex digits — a folder address — or nothing changes.
    [[ ${#__aterm_addr} -eq 16 && "$__aterm_addr" != *[!0-9a-f]* ]] || return 0
    [ "$__aterm_addr" != "$__aterm_body_rev" ] || return 0
    __aterm_file="$__aterm_si_root/$__aterm_addr/aterm_shell_integration_body.bash"
    [[ -f "$__aterm_file" && -O "$__aterm_file" ]] || return 0
    . "$__aterm_file" && __aterm_body_rev="$__aterm_addr"
    return 0
}

# THE TRAMPOLINE: the PROMPT_COMMAND entry, and the one piece of the prompt path
# that never changes. The in-prompt flag is raised FIRST, so a body sourced by the
# check is never taken for a user's command by the DEBUG trap; the body sourced is
# the body whose prompt handler runs in the SAME prompt. Its first command takes
# `$?`, the user's `$_` and flags, and turns XTRACE and VERBOSE off, untraced (see
# THE TRACE); the handler is handed all three, may change `__aterm_flags` (the
# user's PROMPT_COMMAND it runs may), and the flags go back here, last — whatever
# body ran, an older one included. It returns 0, as the handler does: bash puts
# `$?` back after PROMPT_COMMAND anyway, and a status it returned was a failed
# command to the user's ERR trap and `set -e` (see the handler's end).
__aterm_prompt_command() {
    { local __aterm_status=$? __aterm_lastarg=$_ __aterm_flags=$-; set +xv; } 2>/dev/null
    __aterm_in_prompt_cmd=1
    __aterm_body_check
    __aterm_body_prompt_command "$__aterm_status" "$__aterm_flags" "$__aterm_lastarg"
    case $__aterm_flags in *v*) set -v ;; esac
    case $__aterm_flags in *x*) set -x ;; esac
}

# THE PS0 TRAMPOLINE (bash 5.3+, the body's "PS0" says why): what PS0 names, and
# like the prompt's trampoline the piece that never changes. It calls the body's
# hook only where the body defines one, so a body without it (an older build's,
# taken at a prompt) leaves PS0 printing nothing, never `command not found` at
# every line. The hook runs where `set -e` is ignored and the flags are off
# untraced (see THE TRACE); it returns 0.
__aterm_ps0() {
    { local __aterm_flags=$-; set +xv; } 2>/dev/null
    if declare -F __aterm_body_ps0 >/dev/null; then
        __aterm_body_ps0 || :
    fi
    case $__aterm_flags in *v*) set -v ;; esac
    case $__aterm_flags in *x*) set -x ;; esac
}

# @@ATERM-INTEGRATION-BODY-BEGIN@@ — everything from here to the END marker is re-sourced live

# Not traced (see THE TRACE): a loader from before it sources this body at a
# switch with the user's XTRACE on. Put back at the body's end.
{ __aterm_body_flags=$-; set +xv; } 2>/dev/null

# Enable agy's fast prompt editor by default (including in adopted shells when
# this body is live-reloaded at the next prompt) so U+202F narrow no-break
# spaces in macOS screenshot filenames do not desynchronize Bubble Tea's
# textarea visual cursor.
: "${AGY_CLI_FAST_PROMPT_EDITOR=true}"
export AGY_CLI_FAST_PROMPT_EDITOR

# The user's XTRACE and VERBOSE, as `$1` (a `$-`) holds them: put back where a
# function of aterm's that turned them off returns, or runs the user's own code.
# It returns `$2` (0 when not given), and its last argument is what `$_` reads
# after it: the caller that runs the user's code next (`|| eval`, `&& eval`)
# hands that code the `$?` and `$_` bare bash would. Turning either flag on is
# not traced, and the `return` after it is traced only into a silenced stderr.
__aterm_flags_put() {
    case $1 in *v*) set -v ;; esac
    case $1 in *x*) set -x ;; esac
    { return "${2:-0}"; } 2>/dev/null
}

# ─── AN OLDER SCRIPT'S `~/.aterm/bin`, taken off (2026-10-19) ───
#
# Once per shell, in a shell an older script loaded that this body reaches in
# place (`__aterm_old_bin_off` unset; a fresh load settles it in the loader):
# `aterm_shell_integration_old_bin.bash`, beside this file in its folder, says
# what goes and when, and takes it off. Its own file (2026-10-20) because no
# shell this build starts ever runs it, yet every one paid here to parse it:
# 55 lines and two function definitions, about 0.95 M of the 78 M instructions
# to the first prompt (2026-10-20, bash 3.2.57, a PTY, the captured aterm spawn
# env, interleaved, n=100).
# The folder is the one `BASH_SOURCE` names, this loader's or the body file's
# alone; a copy with no such file beside it takes nothing off, the side that
# never takes a user's own entry. The file is sourced only when it is this
# user's (`-O`), as THE BODY CHECK sources a body: the body's own code, kept
# beside it, is held to the body's rule.
if [[ -z "${__aterm_old_bin_off-}" ]]; then
    __aterm_old_bin_off=1
    __aterm_old_bin_file="${BASH_SOURCE[0]%/*}/aterm_shell_integration_old_bin.bash"
    if [[ -f "$__aterm_old_bin_file" && -O "$__aterm_old_bin_file" ]]; then
        . "$__aterm_old_bin_file"
    fi
    unset __aterm_old_bin_file __aterm_bin_from
fi

# ─── The reroute directory, FIRST — and the agents directory beside it ───
#
# $ATERM_REROUTE_DIR is set by aterm's spawn seam: the session-scoped directory of
# stubs for the upstream Rust names (`aterm help reroute`), which the seam already
# put FIRST on the PATH it handed this shell. The user's own rc files run AFTER
# that injected environment (`. ~/.cargo/env` in a ~/.bashrc prepends ~/.cargo/bin),
# and so may the package blocks just above. Measured 2026-09-07: ~/.cargo/bin at
# position 17, ahead of the managed store at 19, so a bare `cargo` ran upstream
# Rust silently. An ORDER failure — which is why this is move-to-front (every
# existing occurrence removed, then prepended), never skip-if-present. This file
# is sourced last, after the user's rc, so one assert here is final. Inert outside
# a session: the variable is unset, or the directory (Windows lays none) does not
# exist.
#
# Pure parameter expansion, no fork and no IFS surgery: framing PATH as `:$PATH:`
# makes every entry `:entry:`-delimited, so /opt/x never matches /opt/xy; the
# quoted pattern keeps `*`/`[` in the directory literal; the loop catches two
# adjacent copies (the first pass consumes the colon between them); and an EMPTY
# entry ("here", to a POSIX shell) — the user's — survives because exactly the one
# framing colon is stripped from each end.
#
# $ATPKG_AGENTS — exported by the atpkg shell.d hook sourced just above — names
# <prefix>/agents, which holds ONLY the agent shims aterm keeps current (claude,
# codex, agy, muse; owner decisions 2026-09-10 and 2026-10-15). It fails the same
# way: measured 2026-09-10 on m27 at PATH position 14, behind /opt/homebrew/bin and
# ~/.local/bin, so `codex` ran a brew cask that could run no command and `claude` an
# older native install. It is moved to the front first, then the reroute directory,
# so PATH reads reroute, agents, … — the spawn seam's own order.
__aterm_path_front() {
    # Byte by byte (`LC_ALL=C`, as `__aterm_urlencode`): in a UTF-8 locale bash
    # 3.2 (macOS's /bin/bash) matches a pattern in wide characters, converting
    # the string again for every place it tries, so the substitution below was
    # quadratic in PATH's length. Measured 2026-10-20 (bash 3.2.57, a PATH of 13
    # entries and 561 bytes, as a captured spawn env holds it; 400 calls): 1.06
    # ms a call, 0.22 ms in bytes; 1.65 and 0.33 ms with 23 entries and 771
    # bytes. Every fresh bash calls it once before its first prompt, as the
    # reroute directory comes back to the front from where /etc/profile's
    # path_helper put it. A quoted literal matches the same entries either way.
    local LC_ALL=C
    local __aterm_dir="$1" __aterm_p=":$PATH:" __aterm_q
    # Fixpoint by POSIX equality, not by `[[ == pattern ]]`: under
    # `shopt -s nocasematch` (a user's .bashrc runs before this file) the
    # pattern test is case-insensitive while the replacement is not, and a
    # case-variant spelling of the directory made the old loop spin forever
    # before the prompt (2026-09-07 review).
    while :; do
        __aterm_q="${__aterm_p//":$__aterm_dir:"/:}"
        [ "$__aterm_q" = "$__aterm_p" ] && break
        __aterm_p="$__aterm_q"
    done
    # Only a framed remainder of exactly `:` means nothing is left; deciding on
    # the stripped string dropped a sole empty entry (review finding 2026-09-10).
    case "$__aterm_p" in
        :) export PATH="$__aterm_dir" ;;
        *)
            __aterm_p="${__aterm_p#:}"
            __aterm_p="${__aterm_p%:}"
            export PATH="$__aterm_dir:$__aterm_p"
            ;;
    esac
}
#
# It also records, in $__aterm_managed_want (colon-joined, in order), the dirs it
# put in front, which is what the per-prompt hot path below compares the head of
# PATH against, and in $__aterm_managed_agents_on / $__aterm_managed_reroute_on
# whether each dir WAS there to front. A dir that was absent is re-probed by the
# hot path (one `-d` per prompt, only while it stays absent — review finding
# 2026-09-16: a hook that predates agents/ set $ATPKG_AGENTS, the `-d` here failed
# once, and the shell never looked again) and fronted the moment it appears. The
# `-d` stats of the steady state live HERE, on the change path, never on the
# per-prompt one.
__aterm_managed_want=""
__aterm_managed_agents_on=0
__aterm_managed_reroute_on=0
__aterm_reroute_path_front() {
    __aterm_managed_want=""
    __aterm_managed_agents_on=0
    __aterm_managed_reroute_on=0
    if [[ -n "${ATPKG_AGENTS:-}" && -d "$ATPKG_AGENTS" ]]; then
        __aterm_path_front "$ATPKG_AGENTS"
        __aterm_managed_want="$ATPKG_AGENTS"
        __aterm_managed_agents_on=1
    fi
    if [[ -n "${ATERM_REROUTE_DIR:-}" && -d "$ATERM_REROUTE_DIR" ]]; then
        __aterm_path_front "$ATERM_REROUTE_DIR"
        __aterm_managed_want="$ATERM_REROUTE_DIR${__aterm_managed_want:+:$__aterm_managed_want}"
        __aterm_managed_reroute_on=1
    fi
}
__aterm_reroute_path_front

# ─── LIVE: the tab that is ALREADY OPEN picks the managed dirs up the moment atpkg lays them ───
#
# Owner, 2026-09-16, looking at a status row that read "✓ Claude Code 2.1.273 ·
# Codex 0.154.0 — aterm-managed, current   what `claude` and `codex` run in new
# tabs": "HEY! this is a bad experience. aterm atpkg DID install the latest but it
# didn't make them available for me. instead, it is telling me to open a new tab.
# NO! all the latest and best MUST WORK IN THE SAME TAB with live update! fix this
# and this message and audit that this is the actual behavior."
#
# What was measured in that tab: its shell (pid 1784) was spawned at 10:44:24 by
# the PREVIOUS app build and ADOPTED across the seamless update — the running app
# (0.86.0, pid 1868) started at 10:44:32 — and <prefix>/agents plus the shell.d
# hooks were created at 10:46 by the new build's first pass. The one assert above
# fires ONCE, at load, gated on $ATPKG_AGENTS / $ATERM_REROUTE_DIR being set and
# the directories existing AT THAT INSTANT; that shell had neither variable and no
# directory to find, so `which -a claude` read ~/.local/bin/claude first and the
# only way to the build atpkg had just installed was a new tab. The same freeze
# hits EVERY fresh machine: the first tab opens before the seed pass creates agents/.
#
# The fix is a per-prompt AND per-command re-assert — from PROMPT_COMMAND and from
# the DEBUG-trap preexec, because a command typed at an idle prompt after the dirs
# appear runs BEFORE the next prompt (measured: a PATH assigned in the DEBUG trap
# is what that very command resolves through) — in four steps, all builtin-only
# (no `$(...)`, no backticks, no external stat/dirname/readlink; pinned by a grep
# test):
#
#  1. THE HOOK IS THE SOURCE OF TRUTH when the environment is missing or stale.
#     ~/.aterm/shell.d/00-atpkg.bash is what atpkg generates (crates/atpkg/src/hooks.rs;
#     the spelling is pinned from that crate's side): it exports $ATPKG_AGENTS and
#     $ATPKG_BIN, moves agents/ to the front and appends bin/, and it is idempotent.
#     It is (re)sourced when the TEXT on disk is not the text last sourced — it
#     appeared (a shell spawned before the file existed), or atpkg rewrote it on a
#     later pass. bash has no builtin that reads an mtime or an inode, so unlike
#     zsh (one zstat) the copy is compared by CONTENT: `read` — a builtin, on a
#     builtin redirection — walks the file's lines into a string, and the string
#     is compared to the one recorded at the last source. Measured 2026-09-16
#     (bash 3.2.57, the 11-line hook, 10000 calls): 108 µs per call; a fork of
#     /usr/bin/true is 1278 µs, and this same prompt already forks a `$(...)`
#     for OSC 7 and another for 633;E, so the read is under a tenth of what a
#     command costs here. The `-nt` stamp alternative (3 µs, two stats) was
#     rejected for the STATE it carries: a per-shell file this shell must mint
#     symlink-safely in a possibly shared /tmp, own, and delete at exit — a
#     leak for every shell killed without one. A hook that predates R1 (no
#     `export ATPKG_AGENTS`) is sourced ONCE per copy, not once per prompt. An
#     absent hook costs one failed open per call and sources nothing; the empty
#     text is recorded so a hook that appears later is seen as new.
#  2. A DIR THAT WAS ABSENT when the front was last laid is probed again — one `-d`
#     per prompt, only in that degraded state — and fronted when it appears: a hook
#     that names an agents/ atpkg has not created yet, or a session whose seam
#     exported no $ATERM_REROUTE_DIR. Nothing is assigned while it stays absent.
#  3. THE ORDER. $__aterm_managed_want holds the dirs that must lead PATH; the hot
#     path checks that PATH begins with exactly that head (a literal `case`
#     pattern) and assigns ONLY on a mismatch — assigning PATH flushes bash's
#     command hash, which is exactly what a change needs (`claude` re-resolves to
#     the twin) and pure waste otherwise.
#  4. THE TWIN WATCH. bash hashes a command's path on first use, and a hashed name
#     is never searched again while the file exists — so once agents/ leads PATH
#     and `claude` has run the foreign copy, a twin that lands LATER (a fresh
#     machine: agents/ is created at launch, the twin only once the managed program
#     is installed — the exact window in which the owner typed `claude`) would keep
#     losing to the hashed path for the life of the shell (measured 2026-09-16,
#     bash 3.2.57 and zsh 5.9). `hash -d claude codex agy muse` forgets exactly
#     those names every call — a builtin, no syscall; its `2>/dev/null` (a name that
#     was never hashed is an error) is a builtin redirection, not a fork — so every
#     agent name walks PATH afresh and finds the twin the moment it exists. An
#     unhashed name earlier in the list does not stop the later ones being
#     forgotten (measured 2026-10-15 on bash 3.2.57; bash 5.3's builtins/hash.def
#     loops the same way). The names are atpkg::stub::AGENT_PROGRAMS, the twins
#     agents/ holds (owner decisions 2026-09-10 and 2026-10-15); a bash glob over
#     agents/ would be generic but is not safe under a user's `failglob`/`noglob`,
#     so the names are spelled. zsh watches the directory's listing instead and
#     needs no names.
#
# $ATERM_REROUTE_DIR is derived for a shell that predates it — the sibling
# `<dir of $ATPKG_AGENTS>/reroute`, when it is a directory and $__ATERM_REROUTE_PASSTHROUGH is
# not engaged (set, non-empty and not "0": atpkg::reroute::engaged) — so the final
# order is reroute, agents, everything else, bin/ last (the hook appends it).
#
# Gated on BEING INSIDE AN ATERM SESSION ($ATERM_CHILD=1, which the spawn seam sets
# for every child, or $ATERM_SESSION_ID) — NOT on $ATERM_REROUTE_DIR, which is
# precisely what the adopted shell lacks. Inert everywhere else.
__aterm_atpkg_hook="$HOME/.aterm/shell.d/00-atpkg.bash"
__aterm_managed_live=0
if [[ -n "${ATERM_CHILD:-}" || -n "${ATERM_SESSION_ID:-}" ]]; then
    __aterm_managed_live=1
fi

# Leaves the hook's text in $__aterm_atpkg_hook_now, or the empty string when it
# is absent (the failed open is the shell's own, silenced by the redirection
# that precedes it — no fork). Builtin `read` on a builtin redirection; the
# `|| [[ -n ... ]]` keeps a final line without a newline. Its own global, not
# $REPLY: the hot path runs between a user's `read` and the line that consumes
# $REPLY, and must not clobber it. `local` with a value, so `set -u` holds. The
# failed open sits on the left of `|| :`: bash 5 counts it a failed command, so
# in a session with no hook a user's `set -e` closed the shell at the prompt after
# it, and `set -E` fired the ERR trap at every prompt and command (measured on
# bash 5.2.37; bash 3.2.57 lets it pass).
__aterm_atpkg_hook_now=""
__aterm_atpkg_hook_read() {
    __aterm_atpkg_hook_now=""
    local __aterm_line=""
    while IFS= read -r __aterm_line || [[ -n "$__aterm_line" ]]; do
        __aterm_atpkg_hook_now+="$__aterm_line"$'\n'
    done 2>/dev/null < "$__aterm_atpkg_hook" || :
    return 0
}
# The copy the shell.d loop above sourced at load is the copy last sourced —
# whatever it exported (a pre-R1 hook exports no $ATPKG_AGENTS, and is still not
# sourced again until atpkg rewrites it). Recorded ONCE per shell: a body
# re-sourced live keeps the record, so a hook rewritten just before is still
# sourced at the next prompt.
if [[ -z "${__aterm_atpkg_hook_seen+x}" ]]; then
    __aterm_atpkg_hook_seen=""
    if (( __aterm_managed_live )); then
        __aterm_atpkg_hook_read
        __aterm_atpkg_hook_seen="$__aterm_atpkg_hook_now"
    fi
fi

# Exports $ATERM_REROUTE_DIR (status 0) or leaves it alone (status 1).
__aterm_managed_derive_reroute() {
    [[ -z "${ATERM_REROUTE_DIR:-}" && -n "${ATPKG_AGENTS:-}" ]] || return 1
    case "${__ATERM_REROUTE_PASSTHROUGH:-}" in
        ''|0) ;;
        *) return 1 ;;
    esac
    local __aterm_dir="${ATPKG_AGENTS%/*}/reroute"
    [[ -d "$__aterm_dir" ]] || return 1
    export ATERM_REROUTE_DIR="$__aterm_dir"
}
# A session shell whose seam exported no $ATERM_REROUTE_DIR but whose rc block
# sourced the hook derives it now, so the load-time order is final too.
if (( __aterm_managed_live )) && __aterm_managed_derive_reroute; then
    __aterm_reroute_path_front
fi

# The hot path: every PROMPT_COMMAND and every preexec. Builtin-only — see above.
__aterm_managed_path_live() {
    (( __aterm_managed_live )) || return 0
    # 1. The hook: sourced when the text on disk is not the text last sourced.
    #    POSIX `[ ]` equality, not `[[ != ]]` — the latter reads its right side as
    #    a pattern (and case-insensitively under `nocasematch`). An ABSENT hook
    #    falls through: steps 2 and 3 keep the reroute dir in front regardless
    #    (review finding 2026-09-16: an early return here left a PATH prepend at
    #    the prompt shadowing the reroute stubs for the shell's life).
    #    This step and step 4 run at every prompt and before every command, so
    #    neither leaves a failed command: the derivation says no (status 1)
    #    wherever the session already has its reroute dir, and `hash -d` fails
    #    for a name not hashed — at every prompt once any command has been —
    #    and to a user's `set -e` either closed the shell (the hook rewritten
    #    by an atpkg install or update, or one `ls` in a tab with agents/), and
    #    `set -E` fired the ERR trap (measured on bash 3.2.57 and 5.2.37).
    __aterm_atpkg_hook_read
    if [ "$__aterm_atpkg_hook_now" != "$__aterm_atpkg_hook_seen" ]; then
        __aterm_atpkg_hook_seen="$__aterm_atpkg_hook_now"
        if [[ -n "$__aterm_atpkg_hook_now" ]]; then
            . "$__aterm_atpkg_hook"
            __aterm_managed_derive_reroute || :
            __aterm_reroute_path_front
            return 0
        fi
    fi
    # 2. A dir that was absent when the front was last laid: probe it again.
    local __aterm_refront=0
    if (( ! __aterm_managed_agents_on )) && [[ -n "${ATPKG_AGENTS:-}" && -d "$ATPKG_AGENTS" ]]; then
        __aterm_refront=1
    fi
    if (( ! __aterm_managed_reroute_on )); then
        if [[ -n "${ATERM_REROUTE_DIR:-}" ]]; then
            [[ -d "$ATERM_REROUTE_DIR" ]] && __aterm_refront=1
        elif __aterm_managed_derive_reroute; then
            __aterm_refront=1
        fi
    fi
    if (( __aterm_refront )); then
        __aterm_reroute_path_front
        return 0
    fi
    # 3. The order: assign only on a mismatch. A quoted `case` pattern is literal,
    #    so a directory named with `*` or `[` still compares by equality.
    if [[ -n "$__aterm_managed_want" ]]; then
        case "$PATH" in
            "$__aterm_managed_want") ;;
            "$__aterm_managed_want":*) ;;
            *)
                __aterm_reroute_path_front
                return 0
                ;;
        esac
    fi
    # 4. The twin watch: the managed agent names always walk PATH afresh.
    if (( __aterm_managed_agents_on )); then
        hash -d claude codex agy muse 2>/dev/null || :
    fi
    return 0
}

# Track last command for OSC 133;D. KEPT across a live re-source (as is the flag
# below): the body is re-sourced from the PROMPT_COMMAND trampoline, where a
# command that was running still owes its 133;D and the flag is raised.
__aterm_last_command="${__aterm_last_command:-}"
# When that command started (`__aterm_line_ran_a_second`), kept the same way.
__aterm_line_t0="${__aterm_line_t0:-}"
# Guard: suppress DEBUG trap capture during PROMPT_COMMAND execution.
# Without this, commands from the user's original PROMPT_COMMAND (starship,
# pyenv, nvm, etc.) would be captured as if they were user commands.
__aterm_in_prompt_cmd="${__aterm_in_prompt_cmd:-0}"

# ─── The marks go to the terminal (2026-10-16) ───
#
# The DEBUG trap fires for the first simple command INSIDE a compound command
# typed at the prompt, and by then bash has applied the compound's redirections:
# for `{ echo a; echo b; } > f`, `if …; fi > f`, `for …; done > f` or `while …;
# done <<< x > f` the preexec ran with stdout on f, and 633;E (the line and the
# capability nonce), the running title and 133;C went into the user's file
# ahead of its data, while the terminal never saw the block start (measured on
# bash 5.2.21: 419 bytes in f for 20 of output, the block left `entering`). A
# simple command's redirections come after its trap, so `echo a > f` was clean;
# a pipeline's group runs in a subshell the trap does not reach. A shell whose
# stdout the user moved for good (`exec >log`) put every mark in the log.
#
# So a mark goes to stdout while stdout is a terminal, and otherwise to the
# terminal the shell is on: stderr when that still is one (the prompt's own
# stream, no open), else /dev/tty, opened per mark with its failure silenced (no
# controlling terminal loses the mark, never writes it into a file). Builtins
# only, no fork; the hot path is one isatty per mark. Only a shell whose stdout
# WAS a terminal when this loaded does that: one started on pipes (a test's, an
# embedder's) writes to stdout as it always did, and never reaches a terminal it
# was not handed. The answer is kept across a live re-source, which runs inside
# PROMPT_COMMAND, where an `exec >log` holds stdout.
if [[ -z ${__aterm_osc_tty+x} ]]; then
    if [[ -t 1 ]]; then __aterm_osc_tty=1; else __aterm_osc_tty=0; fi
fi

# OSC escape sequences
__aterm_osc() {
    if [[ -t 1 || ${__aterm_osc_tty-} != 1 ]]; then
        printf '\033]%s\a' "$1"
    elif [[ -t 2 ]]; then
        printf '\033]%s\a' "$1" >&2
    else
        { printf '\033]%s\a' "$1" >/dev/tty; } 2>/dev/null || :
    fi
}

# The re-key channel's check (the path is captured by the loader above).
__aterm_rekey_check() {
    [[ -n "$__aterm_rekey_path" && -f "$__aterm_rekey_path" ]] || return 0
    local key=""
    { IFS= read -r key < "$__aterm_rekey_path"; } 2>/dev/null
    command rm -f -- "$__aterm_rekey_path"
    # Exactly 64 lowercase hex digits, or nothing changes.
    [[ ${#key} -eq 64 && "$key" != *[!0-9a-f]* ]] || return 0
    __aterm_shell_nonce="$key"
    __aterm_id_suffix_str=";id=${key}"
}

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Expands to ";id=<64-hex>" when the captured nonce is non-empty, or to
# the empty string otherwise. Reads from the captured local — never from
# the environment — so the nonce is not inherited by subprocesses.
# Kept as the documented helper / external entry point; the hot emitters
# below use $__aterm_id_suffix_str instead to avoid a fork per marker. Not
# traced (see THE TRACE): under its caller's `set -x` it printed the nonce.
__aterm_id_suffix() {
    { local __aterm_flags=$-; set +xv; } 2>/dev/null
    if [[ -n "$__aterm_shell_nonce" ]]; then
        printf ';id=%s' "$__aterm_shell_nonce"
    fi
    __aterm_flags_put "$__aterm_flags"
}

# Percent-encode a string for use in file:// URIs (RFC 3986), into REPLY.
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; all others
# are encoded byte-by-byte as %XX. Setting LC_ALL=C ensures multi-byte
# UTF-8 characters are split into individual bytes for correct encoding.
#
# Runs once per prompt (via __aterm_report_cwd), so it is fork-free by
# construction: `printf -v` (bash 3.1+, i.e. inside the documented 3.2+
# floor) writes into a variable instead of spawning a `$(printf ...)`
# subshell per encoded byte. A path with a single space used to cost a
# fork; a 4-byte emoji cost four. The answer is REPLY, not output: a
# caller's `$(...)` around it was itself a fork at every prompt. The caller
# declares `local REPLY` (bash scopes dynamically), so a user's REPLY — set
# by `read` at one prompt, used at the next — is never touched.
__aterm_urlencode() {
    local LC_ALL=C
    # Fast path: no byte needs encoding, so the loop would copy the string
    # verbatim. Skip it. (The class is exactly the loop's pass-through class,
    # so this is the same decision the loop would make for every byte.)
    if [[ "$1" != *[^a-zA-Z0-9_.~/-]* ]]; then
        REPLY="$1"
        return
    fi
    local string="$1" i char out="" byte
    for ((i = 0; i < ${#string}; i++)); do
        char="${string:i:1}"
        case "$char" in
            [a-zA-Z0-9_.~/-]) out+="$char" ;;
            # The `& 0xFF` mask is LOAD-BEARING here, not decoration: bash's
            # `printf '%d' "'<byte>"` yields a SIGNED value for bytes >= 0x80
            # (0xC3 reads as -61), and every non-ASCII byte of a UTF-8 path
            # reaches this branch. Without the mask a path like `Ünïcødé/`
            # would encode as %FFFFFFFFFFFFFFC3… and corrupt the OSC 7 URI.
            *) printf -v byte '%d' "'$char"
               printf -v byte '%%%02X' "$(( byte & 0xFF ))"
               out+="$byte" ;;
        esac
    done
    REPLY="$out"
}

# Git Bash, MSYS2 and Cygwin run on Windows with a POSIX view of it: $PWD is
# `/c/Users//x`, `/cygdrive/c/x` or `/usr/bin`, none of which names a Windows
# directory, so aterm could neither show it nor open a tab there. There OSC 7
# carries the RFC 8089 drive form the pwsh integration sends, `/C:/Users//x`,
# with the colon percent-encoded like any byte outside the class above.
case "$OSTYPE" in
    msys* | cygwin*) __aterm_win_posix=1 ;;
    *) __aterm_win_posix= ;;
esac
__aterm_win_cwd_from=
__aterm_win_cwd_to=

# ─── The line as typed (2026-10-16) ───
#
# The DEBUG trap fires before each SIMPLE command, and `$BASH_COMMAND` is that
# one simple command, alias-expanded and re-printed. Taken at the line's first
# trap it was the block's command and the tab title: `sleep 2; (exit 7)` was
# recorded as `sleep 2`, `true && sleep 3 | cat` showed `true` while it ran,
# `{ echo a; echo b; } | wc -l` was `wc -l`, and with Ubuntu's stock aliases a
# plain `ls /` was `ls --color=auto /` (measured on bash 5.2.21). zsh's preexec
# and fish's are handed the whole line. bash keeps it in one place: the history
# entry it saved before running it. So the first trap takes `history 1` — when
# the line WAS saved: the prompt marks `$HISTCMD` once nothing of the user's
# runs before the line (a `history -a; history -c; history -r` in their
# PROMPT_COMMAND moves it), and a line bash saved runs at that number, while one
# it did not (ignorespace, ignoredups, `set +o history`, HISTSIZE=0) runs below
# it. The mark is taken at the end of the prompt handler when PROMPT_COMMAND is
# the trampoline alone (a scalar of the user's runs inside the handler), and by
# THE LAST ELEMENT (below) when it is an array; anything else after the
# trampoline leaves no mark. The first trap spends it, and the next prompt
# clears it first. Without a mark, and where a fork costs too much, it is
# `$BASH_COMMAND` as before.
#
# Reading the entry takes a command substitution: a fork (0.8 ms in a bash tab
# on Linux) on the way from Enter to the command, except on bash 5.3+, whose
# `${ ...; }` runs in the shell. Under Git Bash, MSYS2 and Cygwin a fork costs
# tens of milliseconds (16 ms a `$( )` in Git Bash 5.3.9, where `${ ...; }` takes
# 0.5 ms; measured 2026-10-17), so there it stays `$BASH_COMMAND` before 5.3.
# Git for Windows ships 5.3, whose typed `ls /nope` had been recorded as its
# alias, `ls -F --color=auto --show-control-chars /nope`.
__aterm_hist_mark="${__aterm_hist_mark:-}"
if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 3) )); then
    # Inside eval: an older bash cannot parse `${ ...; }` at all.
    eval '__aterm_history_line() { REPLY=${ HISTTIMEFORMAT= builtin history 1; }; }'
elif [[ -n "$__aterm_win_posix" ]]; then
    __aterm_history_line() { return 1; }
else
    __aterm_history_line() { REPLY=$(HISTTIMEFORMAT= builtin history 1); }
fi

# bash before 5 is not judged by `$HISTCMD` alone: 3.2's read 1 at every prompt
# and in every trap, whether bash saved the line or not (measured on 3.2.57,
# macOS's /bin/bash), so an ignorespace line ran "at the mark" too and was
# reported, and titled, as the entry before it. There the mark also keeps the
# entry the list ends in, and the first trap takes the history entry only when
# it is another: a line bash saved is a new entry, one it did not leaves the
# list as the prompt saw it. That costs a fork at each prompt, on bash before 5
# only (4.x was not measured, and is judged the same way).
__aterm_hist_seen="${__aterm_hist_seen:-}"
if (( BASH_VERSINFO[0] < 5 )); then
    __aterm_hist_mark_take() {
        local REPLY
        __aterm_hist_mark=${HISTCMD-}
        __aterm_hist_seen=
        __aterm_history_line && __aterm_hist_seen=$REPLY
    }
else
    __aterm_hist_mark_take() { __aterm_hist_mark=${HISTCMD-}; }
fi

# The command line the first trap reports, into REPLY: the history entry when
# bash saved this line (see above), its number and the `*` of an edited entry
# cut, else `$BASH_COMMAND`.
__aterm_typed_line() {
    local __aterm_mark=$__aterm_hist_mark __aterm_seen=${__aterm_hist_seen-}
    __aterm_hist_mark=
    __aterm_hist_seen=
    if [[ "$__aterm_mark" == [1-9]* && "${HISTCMD-}" == "$__aterm_mark" ]] &&
        __aterm_history_line && [[ "$REPLY" != "$__aterm_seen" ]]; then
        REPLY="${REPLY#*[0-9][* ] }"
        [[ -n "$REPLY" ]] && return 0
    fi
    REPLY="$BASH_COMMAND"
}

# ─── THE LAST ELEMENT (2026-10-16) — an array PROMPT_COMMAND ───
#
# bash 5.1+ runs every element of an array PROMPT_COMMAND, each as its own
# top-level command, and aterm's trampoline is the first: the user's elements
# ran after the prompt handler had lowered the in-prompt flag. The DEBUG trap
# skips an element it fires for whole, but an element of several commands
# (`__x=1; __y=2`, `history -a; history -c; history -r`) fires it for each, and
# the first was taken for a typed line at every prompt, an empty Enter's too:
# a block of its own, and the line typed next unreported, its slot taken. A
# `$HISTCMD` mark taken in the handler was stale by then, too: the element that
# re-reads the history file moved it, and an ignorespace line typed after it
# was reported, and titled, as the line another tab had appended (measured on
# bash 5.2.21).
#
# So the array ends in `__aterm_prompt_end`, after the user's elements: the
# handler leaves the flag raised when this prompt's array ends in it, and it
# lowers the flag and takes the mark. bash runs a copy of the array taken
# before the first element, so the handler reads what this prompt runs; when
# the array does not end in it (an element appended since, or the shell's
# first prompt after a live upgrade), the handler lowers the flag as before,
# leaves no mark, and puts it last for the next prompt. Only an armed one acts.
# A Ctrl-C in a user element cuts the rest of the array, this element too, but
# bash then runs PROMPT_COMMAND again before the prompt (measured), so the flag
# is never left raised at one. bash before 5.1 runs only the first element of
# an array, and gets none. Its name is the wiring's, never renamed, like the
# trampoline's.
__aterm_pc_arrays=0
if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
    __aterm_pc_arrays=1
    # Whether PROMPT_COMMAND is an array of more than one element, so that
    # bash runs more after the trampoline. Nounset-safe in this order: under
    # `set -u` bash 5.2
    # calls a scalar's `${#X[@]}` unbound and an empty array's `${X@a}`, and a
    # scalar has no `[-1]`. Inside eval, as the history read above: bash 3.2
    # has no `@a`.
    eval '__aterm_pc_more() {
        [[ -n ${PROMPT_COMMAND[@]+x} && ${PROMPT_COMMAND@a} == *a* ]] &&
            (( ${#PROMPT_COMMAND[@]} > 1 ))
    }'
else
    # One element runs (bash before 5.1 reads an array's first alone).
    __aterm_pc_more() { return 1; }
fi
__aterm_prompt_end_armed=0

__aterm_prompt_end() {
    { local __aterm_flags=$-; set +xv; } 2>/dev/null
    if (( __aterm_prompt_end_armed )); then
        __aterm_prompt_end_armed=0
        __aterm_hist_mark_take
        __aterm_in_prompt_cmd=0
    fi
    __aterm_flags_put "$__aterm_flags"
}

# `__aterm_prompt_end` last in PROMPT_COMMAND (an array), and only there; the
# user's elements keep their indices.
__aterm_prompt_end_last() {
    local __aterm_i
    for __aterm_i in "${!PROMPT_COMMAND[@]}"; do
        if [[ "${PROMPT_COMMAND[__aterm_i]}" == __aterm_prompt_end ]]; then
            unset 'PROMPT_COMMAND[__aterm_i]'
        fi
    done
    PROMPT_COMMAND+=(__aterm_prompt_end)
}

# Set __aterm_win_cwd_to to the Windows form of POSIX path $1 (a variable, not
# output, so the per-prompt caller forks nothing for it).
__aterm_win_cwd() {
    local LC_ALL=C p="$1" d w
    [[ "$p" == "$__aterm_win_cwd_from" ]] && return
    __aterm_win_cwd_from="$p"
    # MSYS mounts drive X at /x: rewrite that by expansion alone.
    if [[ "$OSTYPE" == msys* && ("$p" == /[a-zA-Z] || "$p" == /[a-zA-Z]/*) ]]; then
        d="${p:1:1}"
        case "$d" in
            [a-z])
                w=abcdefghijklmnopqrstuvwxyz
                w="${w%%"$d"*}"
                d=ABCDEFGHIJKLMNOPQRSTUVWXYZ
                d="${d:${#w}:1}"
                ;;
        esac
        __aterm_win_cwd_to="/$d:${p:2}"
        return
    fi
    # Anything else (/usr/bin, /tmp, Cygwin's /cygdrive/c) goes through the
    # shell's own mount table, once per directory.
    w=$(cygpath -m -- "$p" 2>/dev/null)
    case "$w" in
        [a-zA-Z]:*) __aterm_win_cwd_to="/$w" ;;
        //*) __aterm_win_cwd_to="$w" ;;
        *) __aterm_win_cwd_to="$p" ;;
    esac
}

# Report current working directory (OSC 7)
__aterm_report_cwd() {
    local cwd="$PWD"
    if [[ -n "$__aterm_win_posix" ]]; then
        __aterm_win_cwd "$cwd"
        cwd="$__aterm_win_cwd_to"
    fi
    local REPLY
    __aterm_urlencode "$cwd"
    # SIGNED first, then plain (see the zsh script's __aterm_report_cwd).
    local uri="file://${HOSTNAME:-$(hostname)}${REPLY}"
    if [[ -n "$__aterm_id_suffix_str" ]]; then
        __aterm_osc "633;P;AtermCwd=${uri}${__aterm_id_suffix_str}"
    fi
    __aterm_osc "7;${uri}"
}

# Mark prompt start (OSC 133;A)
__aterm_mark_prompt_start() {
    __aterm_osc "133;A${__aterm_id_suffix_str}"
}

# Mark command line start (OSC 133;B) - after prompt, before user input
__aterm_mark_command_start() {
    __aterm_osc "133;B${__aterm_id_suffix_str}"
}

# Mark command execution start (OSC 133;C)
__aterm_mark_exec_start() {
    __aterm_osc "133;C${__aterm_id_suffix_str}"
}

# Mark command completion (OSC 133;D;exitcode)
# Takes exit status as $1 (caller must pass it — $? inside a function
# body reflects the previous statement, not the original command).
__aterm_mark_exec_finish() {
    __aterm_osc "133;D;${1}${__aterm_id_suffix_str}"
    __aterm_last_command=""
}

# Encode a string for OSC 633;E (VS Code convention), into REPLY (see
# `__aterm_urlencode`: the caller's `local REPLY` keeps the user's).
# Backslash-hex encodes semicolons, backslashes, and bytes <= 0x20.
#
# Runs once per user command, between Enter and the command actually
# starting, so it is fork-free. Space is split out of the old
# `[[:cntrl:]]|' '` arm because it is unconditionally 0x20 — a literal
# beats a subshell, and spaces are the only member of that arm a real
# command line ever contains. Control bytes keep the computed form but
# use `printf -v` instead of a `$(printf ...)` subshell.
__aterm_encode_cmd() {
    local LC_ALL=C
    local string="$1" i char result="" hex
    for ((i = 0; i < ${#string}; i++)); do
        char="${string:i:1}"
        case "$char" in
            \\) result+="\\\\" ;;
            \;) result+="\\x3b" ;;
            ' ') result+="\\x20" ;;
            # Mask retained verbatim from the pre-fork-free form so the byte
            # arithmetic is identical for every input (under LC_ALL=C this arm
            # only ever sees 0x00-0x1F/0x7F, where the mask is a no-op).
            [[:cntrl:]]) printf -v hex '%d' "'$char"
                         printf -v hex '\\x%02x' "$(( hex & 0xFF ))"
                         result+="$hex" ;;
            *) result+="$char" ;;
        esac
    done
    REPLY="$result"
}

# The body's revision, SIGNED (633;P, the VS Code property mark): the folder
# address this body was taken from, so the host can tell a shell running its own
# body from one running an older build's (`status integration_rev=`). Emitted
# before every 133;A — a builtin printf, nothing when the revision is unknown (a
# hand-installed copy).
__aterm_mark_integration_rev() {
    [[ -n "$__aterm_body_rev" ]] || return 0
    __aterm_osc "633;P;AtermIntegration=${__aterm_body_rev}${__aterm_id_suffix_str}"
}

# Capture command before execution. The DEBUG trap's entry, which fires before
# each command: XTRACE and VERBOSE off first, and put back as it returns (see THE
# TRACE), around the work below. It returns 0: under a user's `extdebug` a DEBUG
# trap that returns non-zero skips the command.
#
# The trap hands it the user's `$_` (its command is `__aterm_preexec "$_"`): bash
# sets `$_` to the last argument of the simple command that ran last, and before
# each of the user's commands that is the trap's own, so its last argument is
# what `$_` reads after it — bash-preexec's technique. Called bare, the trap left
# `$_` reading `__aterm_preexec` before every command, and `mkdir -p d && cd
# "$_"` said `cd: __aterm_preexec: No such file or directory` (measured on bash
# 3.2.57 and 5.2.37; bare bash enters d). A shell whose loader set the bare trap
# keeps it — the wiring is the loader's, once per shell — and calls this with no
# argument; its next tab has the fixed one.
#
# The work is handed the function the trap fired in, the shell's own
# `FUNCNAME[1]` — only under function tracing (`T` in `$-`: `set -o functrace`,
# or bash 4+'s `extdebug`), the one mode in which the trap fires inside a
# function. Without it a function that calls this one wraps the trap — what
# starship's init does on bash < 4.4 when it finds a DEBUG trap set: it sets the
# trap to a function of its own, which evals the one it found. Taken for "the
# trap fired in a function", every command typed after such a wrapper went
# unreported (no 633;E or 133;C; measured with starship's layering typed at a
# prompt, on bash 3.2.57 and 5.2.37).
__aterm_preexec() {
    { local __aterm_flags=$-; set +xv; } 2>/dev/null
    local __aterm_in_fn=""
    case $__aterm_flags in *T*) __aterm_in_fn=${FUNCNAME[1]-} ;; esac
    # Past the command's first simple command there is nothing to capture, so
    # only a chained handler is left to run.
    if [[ -n "$__aterm_prev_debug_handler" || -z "$__aterm_last_command" ]]; then
        __aterm_preexec_run "${1-}" "$__aterm_in_fn"
    fi
    # Put back only a flag it turned off: the trap fires before every simple
    # command at the top level (each pass of a loop typed at the prompt), and
    # the silenced `return` costs a redirection. `case` returns 0 (extdebug).
    case $__aterm_flags in *[xv]*) __aterm_flags_put "$__aterm_flags" ;; esac
}

__aterm_preexec_run() {
    # Always chain the previous DEBUG trap handler first, before any early
    # returns, so pre-existing handlers (starship, pyenv, etc.) always run —
    # with the user's flags, `$?` 0 and the user's `$_`, and re-taking the
    # flags after it, which it may change.
    if [[ -n "$__aterm_prev_debug_handler" ]]; then
        __aterm_flags_put "$__aterm_flags" 0 "$1" && eval "$__aterm_prev_debug_handler"
        { __aterm_flags=$-; set +xv; } 2>/dev/null
    fi

    # Skip if this is from PROMPT_COMMAND (ours or the user's original).
    # PROMPT_COMMAND may be an ARRAY (bash 5.1+): bash runs EVERY element as
    # its own top-level command AFTER ours returned — the in-prompt flag is
    # already clear by then, and a scalar compare sees only element 0. A
    # sibling integration's precmd (__vte_prompt_command, starship, ...)
    # would then be captured as the user's command: its 133;C fires at the
    # prompt (out of phase, dropped) and __aterm_last_command stays occupied,
    # so the REAL command never emits 633;E/133;C — no block ever reaches
    # Executing and a driver's verified submit can never attribute a press.
    # Match ALL elements; a scalar expands as one element, covering both.
    (( __aterm_in_prompt_cmd )) && return
    local __aterm_pc_elem
    for __aterm_pc_elem in "${PROMPT_COMMAND[@]}"; do
        [[ "$BASH_COMMAND" == "$__aterm_pc_elem" ]] && return
    done
    [[ "$BASH_COMMAND" == "__aterm_"* ]] && return
    # Nor a line of a function: the line the user typed runs at the top level,
    # and the trap fires inside a function (`$2`) only under the user's `set -o
    # functrace` (`set -T`; bash-preexec's subshell mode sets it, and bash 4+'s
    # `extdebug` turns it on), for a function the shell is already running. There
    # it fired for the lines of aterm's prompt handler after it lowers the
    # in-prompt flag, and of the trampoline before it raises it: every prompt
    # reported `return $last_status` as the user's command, its 133;C before the
    # prompt's 133;B, and the command typed at it got no 633;E or 133;C at all
    # (measured on bash 3.2.57 and 5.2.37). Any function, not only aterm's: one
    # an element of an array PROMPT_COMMAND runs is no command typed either.
    [[ -n "$2" ]] && return

    # Only capture the first command (not subshells)
    if [[ -z "$__aterm_last_command" ]]; then
        # The line as typed (see above), not this one simple command of it.
        local REPLY
        __aterm_typed_line
        __aterm_line_start "$REPLY"
    fi
}

# A line's start, for the line `$1`: the managed dirs, the line's 633;E, its
# running title and its 133;C. The first trap's work (`__aterm_preexec_run`), or
# PS0's where bash saved the line (see "PS0" below).
__aterm_line_start() {
    # The managed dirs, live — BEFORE this command resolves: a `claude` typed
    # at a prompt that was drawn before atpkg laid agents/ must already run
    # the twin (see "LIVE" above).
    __aterm_managed_path_live
    __aterm_last_command="$1"
    local REPLY
    # Report command text for session memory (OSC 633;E)
    __aterm_encode_cmd "$__aterm_last_command"
    __aterm_osc "633;E;${REPLY}${__aterm_id_suffix_str}"
    # Set tab title to running command (OSC 0): see "The command title" below.
    if [[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]; then
        local cmd="$__aterm_last_command"
        (( ${#cmd} <= 256 )) || cmd="${cmd:0:255}…"
        __aterm_osc "0;${cmd//[[:cntrl:]]/}"
    fi
    # When the line started, for the tty probe at its end (see "The tty settings
    # after a failed command"): microseconds where bash keeps a real-time clock
    # (5.0+), whole seconds before.
    if [[ -n "${EPOCHREALTIME-}" ]]; then
        __aterm_line_t0=${EPOCHREALTIME//[!0-9]/}
    else
        __aterm_line_t0=s$SECONDS
    fi
    __aterm_mark_exec_start
}

# ─── The command title (2026-10-18) ───
#
# The tab's title while a line runs is the line as typed, its control characters
# stripped, at most 256 characters: a longer line is cut to its first 255 and ends
# with `…`. The cut used to be a bare 64 characters, and the tab, which keeps a
# command line's last argument when it middle-cuts one (a file or a path), then
# showed the cut-off end as if it were the file: `vim … crates/aterm-gui/src/plat`
# for a 70-character `vim` line (the 2026-10-18 judges' J4F-1). The mark tells
# the tab the end is gone, and it cuts such a title at its end. 256 characters
# stay under the terminal's 1024-byte title cap in any script. The pwsh, zsh and
# fish scripts cut the same way.

# ─── PS0: the line's start where the DEBUG trap never fires (2026-10-17) ───
#
# The trap fires before a SIMPLE command at the top level, and a line that opens
# with a subshell runs none there: `(exit 3)` and `( cd sub && make )` emitted
# no 633;E and no 133;C, so their block never started and the exit code was
# lost (`ctl blocks`: `entering exit=-`, measured on Git Bash 5.3.9; the trap
# rule is bash's, not Windows'). bash 4.4+ expands PS0 once for each line it
# reads, after reading it and before running it, and never for an empty line or
# a lone comment; bash 5.3's `${ ...; }` runs that expansion in the shell itself,
# with no fork (0.5 ms a line in Git Bash), and changes neither `$?` nor `$_`
# (measured). So on bash 5.3+ PS0 opens with `${ __aterm_ps0; }` (the loader's
# trampoline, wired once per shell), and this does the first trap's work for a
# line bash saved in its history, whatever its first command: the line is the
# history entry at the prompt's mark, as the trap takes it ("The line as
# typed"), and the trap then finds the work done. A line bash did not save
# (ignorespace, ignoredups, `set +o history`) is still the trap's: PS0 cannot
# read it. Before 5.3 PS0 would cost a fork a line, run in a subshell that
# could keep no state, and is not used. A PS0 the user sets later replaces the
# hook; the trap then covers what it did before.
__aterm_body_ps0() {
    (( __aterm_in_prompt_cmd )) && return 0
    # Before the line runs, `$HISTCMD` is the number the NEXT line would take:
    # one past the mark for a line bash saved, the mark itself for one it did
    # not (the trap, while the line runs, reads the mark; measured on 5.3.9).
    [[ -z "$__aterm_last_command" && "$__aterm_hist_mark" == [1-9]* &&
        "$__aterm_hist_mark" != *[!0-9]* &&
        "${HISTCMD-}" == "$((__aterm_hist_mark + 1))" ]] || return 0
    local REPLY
    __aterm_history_line || return 0
    REPLY="${REPLY#*[0-9][* ] }"
    [[ -n "$REPLY" ]] || return 0
    __aterm_hist_mark=
    __aterm_hist_seen=
    __aterm_line_start "$REPLY"
    return 0
}

# ─── The tty settings after a failed command (2026-09-26) ───
#
# The zsh script's twin ("The tty settings the shell keeps" there has the whole
# story), narrower because bash already does half of it. bash saves the tty
# before a foreground job and PUTS IT BACK when the job dies by a signal — but
# after a job that EXITS it adopts whatever the job left. So a raw-mode program
# that exits without restoring (`tty.setraw` and an uncaught error, `os._exit`)
# leaves every later command `-icanon -isig -iexten -icrnl -ixon -opost`: Ctrl-C
# arrives as a literal ^C byte and interrupts nothing, and Enter ends no line
# (`read x` hangs). Measured 2026-09-26, bash 3.2.57 `--norc` and with this file
# sourced, in a pty: a python `tty.setraw` child SIGKILLed or a node
# `setRawMode` child SIGKILLed left bash fine (Ctrl-C ended `sleep 3` at
# 0.46 s); the same python child ending in `os._exit(1)` or `os._exit(0)` left it
# broken (Ctrl-C ended `sleep 3` only at 3.03–3.09 s, `read -r x` never returned).
#
# bash has no `ttyctl -f`, so this is the REPAIR alone: after a command that
# FAILED, one `stty -a` fork, and — only when the tty is in a dead raw program's
# state (ISIG off: every raw mode clears it, and nobody keeps it off at a prompt
# on purpose, since it disables Ctrl-C and Ctrl-Z) — a second one turning back on
# what raw modes turn off: `isig icanon iexten echo icrnl opost`. IXON is
# deliberately NOT touched: a user's own `stty -ixon` clears it too and cannot be
# told apart, so it survives. A successful command costs nothing when it ended
# within a second (measured with this code: exit 1 repaired, Ctrl-C at 0.48 s).
#
# A command that ran a second or more is probed whatever its status (2026-10-18):
# a full-screen program killed from outside — vim ended by Task Manager or
# `taskkill /F` in Git Bash — reports 0 under MSYS2, and the tty it left raw
# echoed nothing typed until `stty sane` (the 2026-10-18 verifier's
# v4-gitbash-killed-app-echo-repair-gated-on-status: only a failing command
# after it happened to repair it). A program the user ran that long can afford
# the probe, one fork: about 50 ms on MSYS2, a few on Linux and macOS. A quick
# command still takes no fork, so a raw program that exits 0 within a second of
# its start is the residual.
__aterm_line_ran_a_second() {
    local __aterm_t0=${__aterm_line_t0-} __aterm_now
    case $__aterm_t0 in
        s[0-9]*) (( SECONDS - ${__aterm_t0#s} >= 1 )) ;;
        [0-9]*)
            __aterm_now=${EPOCHREALTIME-}
            __aterm_now=${__aterm_now//[!0-9]/}
            [[ -n "$__aterm_now" ]] && (( __aterm_now - __aterm_t0 >= 1000000 ))
            ;;
        *) return 1 ;;
    esac
}

__aterm_tty_repair() {
    local __aterm_tty_state=""
    __aterm_tty_state="$(command stty -a 2>/dev/null)" || return 0
    # Newlines to spaces and a space at each end, so ` -isig ` matches the word
    # wherever `stty -a` put it — first on its line (Linux), or after a label.
    __aterm_tty_state=" ${__aterm_tty_state//$'\n'/ } "
    case "$__aterm_tty_state" in
        *" -isig "*) command stty isig icanon iexten echo icrnl opost 2>/dev/null ;;
    esac
    return 0
}

# ─── THE START MARK and THE WARM HOLD (docs/DESIGN-warm-spare-2026-10-16.md) ───
#
# The zsh script's twins. Every fresh integrated bash signs ONE start record at
# its first prompt, after every startup file: the rc folder ($HOME), its flags
# and `cold` or `spare`, in one fork-free write to /dev/tty. bash runs nothing of
# the user's prompt path before the trampoline (the wrapper sources this file
# last, and it wraps PROMPT_COMMAND), so no hook is ever ahead of aterm's
# (`pre:0`), and bash has no `sched` or periodic hooks. A cold shell leaves its
# job count out (`j:-`: counting forks); a warm spare counts its jobs with one
# fork, in its idle time.
__aterm_start_mark() {
    local REPLY="$HOME" __aterm_j=-
    if [[ $1 == spare ]]; then
        local __aterm_jl
        __aterm_jl=$(jobs -p 2>/dev/null)
        local -a __aterm_ja=($__aterm_jl)
        __aterm_j=${#__aterm_ja[@]}
    fi
    case $REPLY in *[\\\;\ [:cntrl:]]*) __aterm_encode_cmd "$REPLY" ;; esac
    printf '\033]633;P;AtermStartRc=%s%s\a\033]633;P;AtermStartFlags=pre:0,s:0,j:%s,r:0%s\a\033]633;P;AtermStart=%s%s\a' \
        "$REPLY" "$__aterm_id_suffix_str" "$__aterm_j" "$__aterm_id_suffix_str" \
        "$1" "$__aterm_id_suffix_str" >/dev/tty 2>/dev/null
    return 0
}

# A WARM spare's hold, at the top of its first prompt handler: its startup files
# have run unseen, and nothing of the prompt has. HISTFILE is unset while held, so
# a discard's hang-up writes no history; released by `go`, it is put back and
# the list read again as a cold start reads it (the sizes applied, then
# `history -c; history -r`: bash 3.2 has no fork-free stat to tell whether the
# file moved, and a reload of an unmoved file reads the same list), and
# `$SECONDS` carries on from the rc's run. Not read again where the startup
# files left a list of their own (`history -s`, noted at the fresh load below):
# bash then read no file at its start. A readonly history variable (a
# hardened /etc/profile) stays as it is: its unset fails quietly, and its
# assignment fails alone in an `eval` instead of ending the prompt.
# Nothing here touches the tty, so what is typed before the release waits there
# for readline.
__aterm_spare_hold() {
    local __aterm_rc=$SECONDS __aterm_go="" __aterm_n=0 __aterm_own=${__aterm_spare_rc_list-}
    local __aterm_hf_set=${HISTFILE+x} __aterm_hf=${HISTFILE-}
    unset HISTFILE 2>/dev/null || :
    __aterm_start_mark spare
    while :; do
        if IFS= read -r __aterm_go < "$__aterm_spare_fifo"; then
            [[ $__aterm_go == go ]] && break
        else
            __aterm_n=$((__aterm_n + 1))
            { [[ $__aterm_n -lt 50 ]] && [[ -p $__aterm_spare_fifo ]]; } || exit 0
        fi
    done 2>/dev/null
    unset __aterm_spare_mode __aterm_spare_fifo __aterm_spare_rc_list
    if [[ -n $__aterm_hf_set ]]; then
        [[ -n ${HISTFILE+x} ]] || HISTFILE=$__aterm_hf
        if [[ -z $__aterm_own && -f $HISTFILE ]]; then
            # As bash's own load: the list's size, then the file cut to
            # HISTFILESIZE (each assignment applies it), then the read.
            [[ -z ${HISTSIZE+x} ]] || eval 'HISTSIZE=$HISTSIZE' 2>/dev/null || :
            [[ -z ${HISTFILESIZE+x} ]] || eval 'HISTFILESIZE=$HISTFILESIZE' 2>/dev/null || :
            history -c
            history -r
        fi
    fi
    SECONDS=$__aterm_rc
    return 0
}

# PROMPT_COMMAND handler - runs before each prompt, from the loader's trampoline,
# which hands it the status the command before it left, the user's flags with
# XTRACE and VERBOSE already off, and the user's `$_`; the flags are the
# trampoline's `__aterm_flags`, which this re-takes after the user's own
# PROMPT_COMMAND and the trampoline puts back (see THE TRACE). A loader from
# before it hands the status alone, with the user's flags still on: they are
# taken and turned off here, and put back at the end.
__aterm_body_prompt_command() {
    { [[ -n ${2+x} ]] || local __aterm_flags=$-; set +xv; } 2>/dev/null
    local last_status=$1 __aterm_lastarg=${3-}
    __aterm_last_exit=$last_status
    __aterm_in_prompt_cmd=1
    # No mark from an earlier prompt (see "The line as typed").
    __aterm_hist_mark=
    __aterm_hist_seen=
    __aterm_prompt_end_armed=0

    # The first prompt of a fresh load signs the start record; a warm spare
    # waits there first (see THE WARM HOLD).
    if [[ -n "${__aterm_start_pending:-}" ]]; then
        unset __aterm_start_pending
        if [[ "${__aterm_spare_mode:-}" == warm ]]; then
            __aterm_spare_hold
        else
            __aterm_start_mark cold
        fi
    fi

    # A waiting re-key first, so every mark this prompt emits carries it.
    __aterm_rekey_check

    # The tty settings (see "The tty settings after a failed command" above): one
    # probe after a failed command or one that ran a second, nothing after a
    # quick success.
    [[ "$last_status" == 0 ]] && ! __aterm_line_ran_a_second || __aterm_tty_repair
    __aterm_line_t0=

    # The managed dirs, live (see "LIVE" above): one probe, an assign only on change.
    __aterm_managed_path_live

    # If we had a command, mark it finished
    if [[ -n "$__aterm_last_command" ]]; then
        __aterm_mark_exec_finish $last_status
    fi

    # Report current directory
    __aterm_report_cwd

    # Set tab title to abbreviated CWD (OSC 0).
    # Match HOME with trailing / to avoid false prefix matches
    # (e.g., /Users//foo matching /Users//foobar).
    local __aterm_tab_title="$PWD"
    if [[ "$PWD" == "$HOME" ]]; then
        __aterm_tab_title="~"
    elif [[ "$PWD" == "$HOME"/* ]]; then
        __aterm_tab_title="~${PWD#$HOME}"
    fi
    # Strip control characters: a crafted directory name (Unix dir names may
    # contain any byte except '/' and NUL) could otherwise inject BEL/ESC and
    # smuggle a nested OSC (e.g. clipboard write) out of the title. Mirrors the
    # command-title path's ${cmd//[[:cntrl:]]/} guard.
    if [[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]; then
        __aterm_osc "0;${__aterm_tab_title//[[:cntrl:]]/}"
    fi

    # The revision this prompt runs, then the prompt start.
    __aterm_mark_integration_rev
    __aterm_mark_prompt_start

    # Run original PROMPT_COMMAND if any (scalar case only).
    # When PROMPT_COMMAND is an array, bash chains array elements automatically;
    # we prepended ourselves at index 0, so the rest run without eval.
    # It reads the `$?` and `$_` the command left, and runs with the user's
    # flags, as in bare bash (see THE TRACE). A status other than 0 is handed on
    # as a function's return on the left of `||`, which is no failed command to
    # a user's ERR trap or `set -e` — the zsh script's `__aterm_line_init_pass`.
    # The `( exit $last_status )` here was one: the ERR trap fired for it under
    # `set -E`, and after a command that failed where bash does not stop
    # (`false && true`) `set -e` closed the shell (measured on bash 3.2.57 and
    # 5.2.37).
    if ! (( __aterm_prompt_cmd_is_array )) && [[ -n "$__aterm_original_prompt_command" ]]; then
        if (( last_status )); then
            __aterm_flags_put "$__aterm_flags" "$last_status" "$__aterm_lastarg" ||
                eval "$__aterm_original_prompt_command"
        else
            __aterm_flags_put "$__aterm_flags" 0 "$__aterm_lastarg" &&
                eval "$__aterm_original_prompt_command"
        fi
        { __aterm_flags=$-; set +xv; } 2>/dev/null
    fi

    # One-shot prompt setup. Runs after the original PROMPT_COMMAND so it
    # survives frameworks (starship, oh-my-bash) that set PS1 at init.
    if [[ -n "${__aterm_pending_prompt_setup:-}" ]]; then
        __aterm_set_prompt
        unset __aterm_pending_prompt_setup
    fi

    # Embed OSC 133;B at the end of PS1 so it fires after prompt text
    # renders (correct protocol position). Emitting 133;B directly from
    # PROMPT_COMMAND would place it before the prompt — a protocol violation.
    # Custom prompts (__aterm_set_prompt) already embed 133;B in PS1.
    # Re-derive the suffix on every PROMPT_COMMAND from the captured
    # shell-local (#8015 — the env var is unset immediately after source
    # time, so all subsequent reads come from $__aterm_shell_nonce).
    if [[ -z "${__aterm_prompt_has_mark_b:-}" ]]; then
        local __aterm_b_suffix=""
        # Signed, it says it stands where the buffer starts (`aterm_input=start`):
        # it ends the prompt, and readline draws the line after it (aterm's
        # typing prediction places Home, End, Ctrl-A, Ctrl-E and Delete only on a
        # line started at a mark that says so).
        [[ -n "$__aterm_shell_nonce" ]] && __aterm_b_suffix=";id=${__aterm_shell_nonce};aterm_input=start"
        local __aterm_b="\[\033]133;B${__aterm_b_suffix}\a\]"
        # Strip every previous variant (with or without a nonce suffix) so
        # nonce rotation does not leave stale id= tails on PS1. Only a suffix
        # that is a whole mark goes: the last `\[\033]133;B`, then `\a\]`,
        # `;id=<hex>\a\]` or `;id=<hex>;aterm_input=start\a\]` (the word this
        # body signs, older bodies none) and nothing else. Found with QUOTED globs and cut by
        # LENGTH, never `${PS1%…}` or a regex: unquoted, the mark's backslashes
        # read as glob escapes; a regex matches nothing in a PS1 holding a byte
        # the tab's UTF-8 locale cannot decode (a Latin-1 `Gruß`); and bash 5's
        # `%` empties a PS1 that starts with such a byte before a backslash.
        # Each of the first two left PS1 gaining a mark at every prompt (review
        # findings 2026-10-01). A loop, so a shell that grew them under an older
        # body sheds them all at its next prompt; each pass removes a non-empty
        # suffix, so it ends.
        local __aterm_b_lead='\[\033]133;B' __aterm_b_end='\a\]'
        local __aterm_b_word=';aterm_input=start'
        local __aterm_b_tail __aterm_b_id
        while [[ "$PS1" == *"$__aterm_b_lead"* ]]; do
            __aterm_b_tail="${PS1##*"$__aterm_b_lead"}"
            if [[ "$__aterm_b_tail" != "$__aterm_b_end" ]]; then
                [[ "$__aterm_b_tail" == ";id="?*"$__aterm_b_end" ]] || break
                __aterm_b_id="${__aterm_b_tail:4:${#__aterm_b_tail}-4-${#__aterm_b_end}}"
                # The word after the id, cut by length like the rest.
                [[ "$__aterm_b_id" == *"$__aterm_b_word" ]] &&
                    __aterm_b_id="${__aterm_b_id:0:${#__aterm_b_id}-${#__aterm_b_word}}"
                [[ -n "$__aterm_b_id" && "$__aterm_b_id" != *[!0-9a-fA-F]* ]] || break
            fi
            PS1="${PS1:0:${#PS1}-${#__aterm_b_lead}-${#__aterm_b_tail}}"
        done
        PS1="${PS1}${__aterm_b}"
    fi

    # PS1 now carries the nonce (either 133;B above or __aterm_set_prompt's), so
    # it must not ride into a child's environment (#8015). A PS1 the user's rc
    # exported, or one bash imported from its own environment, keeps the export
    # attribute through every reassignment; drop it here, every prompt, after
    # the user's PROMPT_COMMAND and before any command they type, so a later
    # `export PS1` is undone too. A builtin, no fork; bash 3.2 has it; on an
    # unset PS1 it creates nothing.
    export -n PS1

    # It returns 0, whatever the command's status. PROMPT_COMMAND's status is a
    # command's to a user's ERR trap, which fired again at the prompt after
    # every failed command (bare bash: once, for the command; twice more under
    # `set -E`), and to `set -e`, which closed the shell at the prompt after a
    # failure bash itself lets pass (`false && true`). Nothing needs it: bash
    # puts `$?` back after PROMPT_COMMAND — and after each element of an array
    # one, which run after it — so PS1 reads the command's (measured on bash
    # 3.2.57 and 5.2.37).
    # The number the next line runs at if bash saves it (see "The line as
    # typed"): taken here, after a scalar of the user's, when nothing runs
    # after this; with more elements of an array to run, by the last element
    # (see THE LAST ELEMENT), which then lowers the flag.
    if __aterm_pc_more; then
        if [[ "${PROMPT_COMMAND[-1]}" == __aterm_prompt_end ]]; then
            __aterm_prompt_end_armed=1
        else
            __aterm_prompt_end_last
        fi
    elif [[ "${PROMPT_COMMAND-}" == __aterm_prompt_command ]]; then
        __aterm_hist_mark_take
    fi
    (( __aterm_prompt_end_armed )) || __aterm_in_prompt_cmd=0
    [[ -n ${2+x} ]] || __aterm_flags_put "$__aterm_flags"
}

# ─── Prompt Override ───
# When ATERM_PROMPT_STYLE is set, override PS1 using palette-indexed colors.
# Colors are in PS1 proper (where \[...\] is processed); $() outputs plain text.
__aterm_set_prompt() {
    local style="${ATERM_PROMPT_STYLE:-none}"
    [[ "$style" == "none" ]] && return

    local hc="${ATERM_PROMPT_HOST_COLOR:-2}"
    local pc="${ATERM_PROMPT_PATH_COLOR:-4}"
    local gc="${ATERM_PROMPT_GIT_COLOR:-3}"
    local sc="${ATERM_PROMPT_SEP_COLOR:-8}"

    local h="\[\033[38;5;${hc}m\]"
    local p="\[\033[38;5;${pc}m\]"
    local g="\[\033[38;5;${gc}m\]"
    local s="\[\033[38;5;${sc}m\]"
    local r="\[\033[0m\]"

    local git_seg="${g}\$(__aterm_git_segment)${r}"
    local err="\$(__aterm_err_prompt)"
    # Embed OSC 133;B at the end of PS1 so it fires after the prompt is
    # drawn (correct position). Without this, 133;B from PROMPT_COMMAND
    # fires before PS1 renders, placing the marker too early.
    # Capability-nonce suffix (#7987, #8015): emit `;id=<hex>` when the
    # captured shell-local nonce is non-empty. Read from the local, never
    # the env var — the env var is unset at source time to prevent leaks
    # into subprocesses.
    local mark_b_id=""
    # Signed, it also says it stands where the buffer starts (see the PS1
    # suffix above).
    [[ -n "$__aterm_shell_nonce" ]] && mark_b_id=";id=${__aterm_shell_nonce};aterm_input=start"
    local mark_b="\[\033]133;B${mark_b_id}\a\]"

    case "$style" in
        minimal)
            PS1="${p}\W${r} ${err}${mark_b}"
            ;;
        standard)
            PS1="${h}\u@\h${s}:${p}\w${r}${git_seg} ${err}${mark_b}"
            ;;
        powerline)
            PS1="${h}\u@\h${r} ${s}${r} ${p}\w${r}${git_seg} ${s}${r} ${err}${mark_b}"
            ;;
    esac
    __aterm_prompt_has_mark_b=1
}

# Error-aware prompt character: separator color on success, error color on failure.
# Uses \001/\002 (raw \[/\]) since this is called via $() inside PS1. Like the
# segment below, it runs in the subshell the `$()` makes, so XTRACE and VERBOSE
# go off for it alone (see THE TRACE).
__aterm_err_prompt() {
    { set +xv; } 2>/dev/null
    if [[ ${__aterm_last_exit:-0} -ne 0 ]]; then
        printf '\001\033[38;5;%sm\002$\001\033[0m\002 ' "${ATERM_PROMPT_ERROR_COLOR:-1}"
    else
        printf '\001\033[38;5;%sm\002$\001\033[0m\002 ' "${ATERM_PROMPT_SEP_COLOR:-8}"
    fi
}

__aterm_git_segment() {
    { set +xv; } 2>/dev/null
    local branch
    branch=$(command git rev-parse --abbrev-ref HEAD 2>/dev/null) || return
    [[ -n "$branch" ]] && printf ' (%s)' "$branch"
}

# Defer prompt setup to first PROMPT_COMMAND so it survives frameworks
# (starship, oh-my-bash) that overwrite PS1 during their initialization.
# Every reference to these two is `${…:-}`-guarded and they start EMPTY, so a
# user's `set -u` (typed, or in .bashrc) neither aborts PROMPT_COMMAND — which
# left `__aterm_in_prompt_cmd=1` stuck and the DEBUG-trap preexec dead for the
# life of the shell (review finding 2026-09-16) — nor prints at every prompt.
#
# Once per shell, like the state above: a body re-sourced live must neither
# forget that PS1 already carries its 133;B nor rebuild a prompt style that ran.
if [[ -z "${__aterm_prompt_has_mark_b+x}" ]]; then
    __aterm_pending_prompt_setup=""
    __aterm_prompt_has_mark_b=""
    if [[ -n "${ATERM_PROMPT_STYLE:-}" && "${ATERM_PROMPT_STYLE:-}" != "none" ]]; then
        __aterm_pending_prompt_setup=1
    fi
fi

# ─── Key Bindings ───
# Bind xterm-style modifier+arrow sequences for readline.
# Without these, sequences like \e[1;3C (Alt+Right) leak as literal text.
__aterm_setup_keybindings() {
    # Alt+Arrow: word navigation
    bind '"\e[1;3C": forward-word'       # Alt+Right
    bind '"\e[1;3D": backward-word'      # Alt+Left
    # Ctrl+Arrow: word navigation
    bind '"\e[1;5C": forward-word'       # Ctrl+Right
    bind '"\e[1;5D": backward-word'      # Ctrl+Left
    # Home/End
    bind '"\e[H": beginning-of-line'     # Home
    bind '"\e[F": end-of-line'           # End
    bind '"\e[1~": beginning-of-line'    # Home (alternate)
    bind '"\e[4~": end-of-line'          # End (alternate)
    # Delete
    bind '"\e[3~": delete-char'          # Delete/Fn+Backspace
    # Shift+Arrow: history navigation
    bind '"\e[1;2A": previous-history'   # Shift+Up
    bind '"\e[1;2B": next-history'       # Shift+Down
}
# Called ONCE per shell, by the loader (after the body's end marker): a body
# re-sourced live leaves the user's bindings alone.

# The body ends in success: the loader moves its revision only when the source
# returned 0. The flags it found go back last (see THE TRACE); that returns 0.
__aterm_flags_put "$__aterm_body_flags"

# @@ATERM-INTEGRATION-BODY-END@@

# ─── THE LOADER, continued: the wiring, once per shell ───
#
# An upgrade in place SIGNS its revision now, not at the next prompt — the zsh
# script's twin, for the same reason: the typed line that upgrades a shell from
# before loaders goes on to relaunch its agent, and until the shell's next
# prompt the host would name it `integration_rev=frozen`. Here, above the trap:
# on a fresh load every line after it would be taken for a user's command.
if (( ! __aterm_fresh_load )); then
    __aterm_mark_integration_rev
fi

# The key bindings, ONCE per shell: a fresh load only — the zsh script's twin
# (review finding 2026-09-26). A body re-sourced live, binding them again at a
# prompt, took back any binding the user had made for these keys since, in every
# live tab at every update.
if (( __aterm_fresh_load )); then
    __aterm_setup_keybindings 2>/dev/null
fi

# Save the DEBUG trap the startup files set, for chaining. Once per shell: in
# an upgrade in place the trap is already ours, and the handler it chains was
# saved at the first load.
#
# THE RC'S DEBUG TRAP (2026-10-17). This file is sourced, and bash hides the
# DEBUG trap from a sourced file, so the `$(trap -p DEBUG)` that stood here saw
# no trap the user's rc set (bash-preexec, atuin) — and forked for nothing on
# the path of every new tab (0.7-1.4 ms). Both wrappers now hand it over
# (`bash_debug_trap_handover!`): in `<re-key path>.trap` (DEBUG_TRAP_SUFFIX),
# read here without a fork — `read -d ''` takes every line of a multi-line
# handler — or, for a shell with no re-key path, in `__aterm_rc_debug_trap`.
# A copy sourced by hand gets what the `.` shows it (a trap set under
# functrace), through the fork as before. `trap -p` quotes for re-input, so
# its line is read back as words, and the handler is the third, exactly as
# set (stripping the outer quotes left `'\''` escapes in it). The read answers
# 1 at the file's end, so it is the first command of an `||` list: no user's
# ERR trap hears it, and a `.bashrc`'s `set -e` closed the shell on it (review,
# 2026-10-17).
if (( __aterm_fresh_load )); then
    __aterm_prev_debug_handler=""
    __aterm_tmp=""
    if [[ -z "${__aterm_rc_debug_handed-}" ]]; then
        __aterm_tmp=$(trap -p DEBUG 2>/dev/null)
    elif [[ -n "${__aterm_rc_debug_trap+x}" ]]; then
        __aterm_tmp=$__aterm_rc_debug_trap
    elif [[ -n "$__aterm_rekey_path" ]]; then
        { IFS= read -r -d '' __aterm_tmp <"$__aterm_rekey_path.trap"; } 2>/dev/null || :
    fi
    if [[ "$__aterm_tmp" == trap\ --\ * ]] && eval "__aterm_tmp=($__aterm_tmp)" 2>/dev/null; then
        __aterm_prev_debug_handler=${__aterm_tmp[2]-}
    fi
    unset __aterm_tmp __aterm_rc_debug_handed __aterm_rc_debug_trap
fi

# The start record is owed once per shell, at its first prompt (see THE START
# MARK): a fresh load only. A body re-sourced live, or a typed upgrade, owes none.
if (( __aterm_fresh_load )); then
    __aterm_start_pending=1
    # bash reads its history file after the startup files only when they saved
    # no line of their own (`history -s`): a warm spare whose list is not empty
    # here keeps it at its release (THE WARM HOLD). A fork, in a spare only,
    # before anyone asked for it.
    if [[ ${__aterm_spare_mode-} == warm && -n $(builtin history 1 2>/dev/null) ]]; then
        __aterm_spare_rc_list=1
    fi
fi

# Install the integration. The trap goes LAST: the DEBUG trap is live from the
# very next top-level command, and inside a sourced script that next command is
# the remainder of THIS file — with the trap first, the PROMPT_COMMAND wiring
# below was captured as a "user command" at load (a bogus 633;E + 133;C + tab
# title, and __aterm_last_command left occupied before the first real prompt).
# Once per shell, like the handler above: an upgrade in place is already wired.
if (( __aterm_fresh_load )); then
    if (( __aterm_prompt_cmd_is_array )); then
        PROMPT_COMMAND=("__aterm_prompt_command" "${PROMPT_COMMAND[@]}")
        # The first prompt's array already ends in it (see THE LAST ELEMENT).
        if (( __aterm_pc_arrays )); then
            PROMPT_COMMAND+=(__aterm_prompt_end)
        fi
    else
        PROMPT_COMMAND="__aterm_prompt_command"
    fi
    # The line's start from PS0 on bash 5.3+ (the body's "PS0"), ahead of the
    # user's own PS0, which is kept whole after it. Never exported: a bash
    # started from this one has no `__aterm_ps0` to call, and one before 5.3
    # cannot expand `${ ...; }`.
    if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 3) )) &&
        [[ "${PS0-}" != *'__aterm_ps0'* ]]; then
        PS0='${ __aterm_ps0; }'"${PS0-}"
        export -n PS0
    fi
    # The user's `$_` handed through (see `__aterm_preexec`).
    trap '__aterm_preexec "$_"' DEBUG
fi

# The user's flags, as the load found them (see THE TRACE). A function of
# aterm's, so the trap just set does not take the line for a user's command.
__aterm_flags_put "$__aterm_load_flags"
