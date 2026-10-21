#!/bin/zsh
# aterm_shell_integration.zsh - Shell integration for aTerm
#
# Copyright 2026 Andrew Yates
# Author: Andrew Yates
# Licensed under the Apache License, Version 2.0
#
# Source this file in your ~/.zshrc:
#   test -e ~/.config/aterm/shell_integration.zsh && source ~/.config/aterm/shell_integration.zsh
#
# Features enabled:
# - Directory tracking (OSC 7): tab title updates, "Open Terminal Here" support
# - Command tracking (OSC 133): command history indexing, timing, notifications
# - The managed dirs, LIVE: an already-running session shell resolves `claude`/`codex`
#   to atpkg's <prefix>/agents twins (and cargo/rustc to <prefix>/reroute) the moment
#   atpkg lays them — no new tab, no `exec zsh` (owner ask 2026-09-16; see "LIVE" below)
# - A LOADER and a BODY (2026-09-26): the shell that is ALREADY RUNNING takes a newer
#   build's integration at its next prompt, when the host that owns it points it there
#   (see "THE LOADER" below)
# - The tty settings, KEPT (2026-09-26): a raw-mode program that dies without restoring
#   them no longer leaves Ctrl-C and Enter broken for every later command (`ttyctl -f`,
#   with `stty`/`reset`/`tset` thawing it — see "The tty settings the shell keeps"
#   below)
#
# Compatible with: zsh 5.0+

# Only run in interactive shells
[[ -o interactive ]] || return

# ─── THE TRACE (2026-10-02) ───
#
# A user's XTRACE (`set -x` at the prompt, `setopt xtrace` in an rc) traced every
# line aterm runs around a command, where bare zsh traces only the command: 80 to
# 660 lines of precmd, preexec and scheduled event per command, and in them the
# capability nonce, in plain text, 4 to 10 times per command, on the screen and in
# the scrollback (measured on zsh 5.9). An `unsetopt xtrace` is itself traced, and
# `emulate` keeps XTRACE as it finds it. But zsh prints a traced command only after
# expanding its words, so a command whose own expansion turns XTRACE off is never
# printed — `: ${options[xtrace]::=off}` — and zsh puts XTRACE back as it found it
# at every function's return. So every function the shell runs on aterm's behalf
# OPENS with that line, before anything else: the hooks, the scheduled event's
# function, what the prompt calls, the tty wrappers and what they call, the
# documented helper; the trampoline takes `$?` in the same command (`${…:+}`
# expands to nothing). Nothing of aterm's is traced from there on, while the
# user's commands, and the precmd hooks after aterm's, are traced as in bare zsh.
# The load does the same: XTRACE off here, put back as it was wherever this file
# returns, so a `.zshenv` that sets it, or a typed upgrade under `set -x`, traces
# none of it (the nonce is captured below) but the line above. A widget is never
# traced (zle turns XTRACE off around each). What remains is the scheduled
# event's own command — one line, three after a status of 2 or more — where an rc
# replaced zle-line-init (see THE LEAD): zsh prints it before it runs, and the
# event keeps the user's `$_` and `$pipestatus`, which the simple command that
# turns XTRACE off would replace.
#
# The line is spelled
#     : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
# so it assigns only where `options` is zsh's own. A function of the user's that
# declares a local `options` (`local options`, `local -A options` for `zparseopts
# -A`) hides zsh's from everything it calls, and the tty wrappers are called from
# such functions: the bare assignment failed there (`options: assignment to
# invalid subscript range`), which abandoned the user's whole function before its
# `stty` ran, or wrote an `xtrace` key into the user's table (measured on zsh
# 5.9). There aterm's lines are traced, as they were before.
typeset -g __aterm_load_xtrace=${${(M)${(t)options}:#*special}:+${options[xtrace]}}${${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}:+}

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
# a pane, usually never". aterm delivers this file by pointing $ZDOTDIR at its
# own cache dir for the shell IT starts, and the wrapper .zshrc there unsets
# $ZDOTDIR again so the user's own tooling sees their real one. A pane shell is
# started by screen or tmux, so it inherits no $ZDOTDIR and DOES NOT SOURCE THIS
# FILE AT ALL. (The bash half was measured in a real GNU screen 4.09.01 window
# under a headless aterm: STY, a screen TERM and the inherited guard all set,
# $ATERM_MUX still EMPTY, no hook defined. zsh's injection is the stricter of
# the two — it erases its own trail on purpose.) So this block fires only where
# the file is genuinely sourced in a pane — a hand-installed `source …` line as
# the header above documents — and `aterm ctl` carries the boundary otherwise.
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
# aterm-shell-integration), or a user's rc that sources this file is re-read. That
# shell, and only that shell, already holds `$__aterm_shell_nonce` AND the precmd
# hook its integration defined, `__aterm_precmd` (every build's since 0.61): a
# function, which no pane or child shell inherits. The nonce alone is no proof
# (review finding 2026-10-01): the typed line assigns it in whichever shell runs
# it — a NESTED one too (a `bash` typed in the tab, a `nix develop`), which
# inherited the guard and ran no integration — and the next live agent upgrade
# typed its line there again, upgrading that shell in place: hooks wired, PATH
# re-fronted, a revision signed for a tab whose own shell was still frozen. For
# the shell that holds both the guard is its own, and the load goes on as an
# upgrade in place: everything below is idempotent, and what must run once per
# shell (the package blocks, the hook and widget wiring) is skipped where it
# already ran.
#
# An upgrade in place first notes, for the body's AN OLDER SCRIPT'S
# `~/.aterm/bin`, the script that loaded this shell FRESH — before this load
# writes over what says so: the loader in `__aterm_si_dir` when its own load was
# the fresh one (`__aterm_fresh_load` 1), `-` for a shell from before loaders
# (neither is set), nothing it can name otherwise (a loader that itself came in
# place). Once per shell, while the body has not settled it.
if [[ -n "${ATERM_SHELL_INTEGRATION_INSTALLED:-}" ]] &&
   (( ${+__aterm_shell_nonce} && ${+functions[__aterm_precmd]} )); then
    if (( ! ${+__aterm_old_bin_off} && ! ${+__aterm_bin_from} )); then
        if (( ! ${+__aterm_fresh_load} && ! ${+__aterm_si_dir} )); then
            typeset -g __aterm_bin_from=-
        elif [[ "${__aterm_fresh_load-}" == 1 && -n "${__aterm_si_dir-}" ]]; then
            typeset -g __aterm_bin_from="$__aterm_si_dir/aterm_shell_integration.zsh"
        else
            typeset -g __aterm_bin_from=
        fi
    fi
    typeset -gi __aterm_fresh_load=0
    ATERM_SHELL_INTEGRATION_INSTALLED=
else
    typeset -gi __aterm_fresh_load=1
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
    [[ $__aterm_load_xtrace != on ]] || builtin setopt xtrace
    return
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
# nonce and the two host channels captured and scrubbed, the one precmd trampoline,
# the hook registrations. Everything between the two markers is the BODY: every
# mark, every prompt behaviour, the managed-PATH machinery — and it is written a
# second time, alone, as `aterm_shell_integration_body.zsh` in the same
# content-addressed folder, so a shell that is already running can source a NEWER
# build's body at its next prompt.
#
# WHY (gap 20 of the 2026-09-24 audit): an integration script used to be fixed at
# spawn. Only the nonce (the re-key channel) and PATH order were live, so every fix
# to the marks or anything else reached only NEW tabs — and the long-lived agent
# tabs (2 and 14 days old on the owner's Mac) kept the old code, with no status
# word saying so.
#
# HOW A NEWER BODY ARRIVES: the host names a per-session POINTER file at spawn
# ($ATERM_INTEGRATION_POINTER, `<control dir>/integration/<sid>` in its 0700
# control dir), captured and scrubbed below like the re-key path. A successor that
# adopts this shell across an update writes its own script folder's 16-hex address
# there (0600, created exclusively, never through a symlink), and the trampoline
# takes it at the next prompt: one fork-free `[[ -f ]]` while nothing waits; when a
# pointer waits, the read, the removal and one `source`. The body is taken ONLY
# from `<root>/<address>/aterm_shell_integration_body.zsh`, where `<root>` is the
# folder THIS file was loaded from — the host's own content-addressed cache — and
# only a 16-hex address is accepted, so no pointer can steer a source anywhere
# else. A pointer naming the body already running, a missing body, or one not owned
# by this user changes nothing.
#
# WHAT THE HOST SEES: the body signs `633;P;AtermIntegration=<address>` before each
# prompt's 133;A, and `status integration_rev=` compares it with the address the
# host ships (`current`, `stale:<address>`, or `frozen` for a shell from before
# loaders, which no pointer can reach).

# Capture the capability nonce into a shell-local so we can immediately
# drop it from the environment (#8015). Leaving ATERM_SHELL_NONCE in the
# exported env lets every child process (env, ssh SendEnv, docker, cron,
# tmux children, ...) read the 64-hex secret that would be used to bypass
# the #7960 nonce-enforcement defense. Capture first, then unset BEFORE
# any prompt hook fires so subprocesses never inherit it.
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
typeset -g __aterm_shell_nonce="${ATERM_SHELL_NONCE:-${__aterm_shell_nonce:-}}"
unset ATERM_SHELL_NONCE

# Precomputed capability-nonce suffix for OSC 133/633 emissions.
# The nonce is captured exactly once (above) and the env var is unset on the
# very next line, so this string changes only through the re-key channel below,
# which rewrites it together with the nonce. Computing it here
# lets the marker emitters below expand a plain parameter instead of running
# `$(__aterm_id_suffix)`, which forks a subshell. That mattered: the prompt
# path fires five markers per command cycle (133;D + 133;A from precmd, 133;B
# from zle-line-init, 633;E + 133;C from preexec), i.e. five forks of pure
# dead time around every command. Byte-identical output — same ";id=<hex>"
# spelling, same empty-string fallback when unnonced. `typeset -g` (not
# `export`), exactly like $__aterm_shell_nonce itself, so #8015 (no nonce
# inheritance by subprocesses) is preserved.
typeset -g __aterm_id_suffix_str=""
if [[ -n "$__aterm_shell_nonce" ]]; then
    __aterm_id_suffix_str=";id=${__aterm_shell_nonce}"
fi

# THE RE-KEY CHANNEL (2026-09-24) — the one way the nonce above changes after
# source time. A seamless update that could not carry a shell's nonce (a parent
# from before 0.92, a session adopted without its screen) left every mark it
# emits dropped for the rest of its life (`status integration=degraded`, both
# live tabs on the owner's Mac that day), and only closing the tab cured it.
# The host names a per-session file at spawn ($ATERM_REKEY_PATH, in its 0700
# control dir); to re-key an adopted shell it writes a fresh 64-hex nonce there
# (0600, created exclusively, never through a symlink) and authorizes it, and
# THIS shell takes it at its next prompt. While nothing is pending the cost is
# one fork-free `[[ -f ]]` per prompt; the read and the removal happen only
# when a key waits. Captured and scrubbed from the environment like the nonce,
# so no child process learns the path; the key itself never appears in typed
# text, scrollback or history.
typeset -g __aterm_rekey_path="${ATERM_REKEY_PATH:-${__aterm_rekey_path:-}}"
unset ATERM_REKEY_PATH

# THE BODY POINTER (see "THE LOADER" above), captured and scrubbed exactly like the
# re-key path: no child process learns it. An upgrade in place keeps the one the
# shell already has, or takes the one its relaunch line hands it.
typeset -g __aterm_body_pointer="${ATERM_INTEGRATION_POINTER:-${__aterm_body_pointer:-}}"
unset ATERM_INTEGRATION_POINTER

# Where this file was loaded from — `<root>/<address>/`, the content-addressed
# folder the host prepared — and so which body is running: `<address>` when the
# folder is named by one (16 lowercase hex digits), empty for a hand-installed
# copy, which signs no revision. `%x` is the file being sourced, never the
# caller's. The pointer's bodies are taken from `<root>` only.
typeset -g __aterm_si_dir="${${(%):-%x}:h}"
typeset -g __aterm_si_root="${__aterm_si_dir:h}"
typeset -g __aterm_body_rev="${__aterm_si_dir:t}"
if [[ ${#__aterm_body_rev} -ne 16 || "$__aterm_body_rev" == *[^0-9a-f]* ]]; then
    __aterm_body_rev=""
fi

# A fresh load settles the body's AN OLDER SCRIPT'S `~/.aterm/bin` at once: this
# loader puts no `~/.aterm/bin` on PATH, so no entry there is an integration's
# and the body has nothing to look for.
if (( __aterm_fresh_load )); then
    typeset -g __aterm_old_bin_off=1
fi

# Source package shell hooks — once per shell: an upgrade in place already ran
# them (the LIVE block re-sources the atpkg hook whenever it changes).
# No `~/.aterm/bin` goes on PATH: nothing creates or manages that folder, and an
# entry there ahead of everything would let whatever lands in it shadow every
# command in every session. atpkg's managed bin/ is APPENDED, by its own hook.
#
# The `(N)` NULL_GLOB qualifier is REQUIRED: without it
# zsh's default NOMATCH raises "no matches found" the instant a glob matches nothing
# (e.g. shell.d holds only `*.zsh` hooks and no `*.sh`) and ABORTS this whole sourced
# script — killing every OSC 7 (cwd) / OSC 133 (command-block) hook defined below, and
# printing an error as the first line of every session. Per-glob `(N)` expands an
# empty match to nothing instead. (bash's unmatched-glob-stays-literal + the `[ -f ]`
# guard makes the bash script safe without this.)
if (( __aterm_fresh_load )) && [ -d "$HOME/.aterm/shell.d" ]; then
    for f in "$HOME/.aterm/shell.d"/*.zsh(N) "$HOME/.aterm/shell.d"/*.sh(N); do
        [ -f "$f" ] && . "$f"
    done
fi

# THE BODY CHECK, first in every precmd (the trampoline below): a newer body waits
# when the host has written the pointer. Fork-free while nothing waits — one
# `[[ -f ]]` — and the read, the removal (the one fork) and the source happen only
# when a pointer waits. The body is sourced under `emulate -L zsh`, so a user's
# ksh_arrays / sh_word_split / warn_create_global cannot bend its top level, and
# the revision moves only when the source succeeded: a body that failed to load is
# still reported as the one before it. And with VERBOSE off, which `emulate`
# without -R leaves as it finds it: VERBOSE echoes every line a file sourced,
# which put the whole body on the user's screen at every switch (measured on zsh
# 5.9: ~1,000 lines); LOCAL_OPTIONS puts it back at the return.
__aterm_body_check() {
    [[ -n "$__aterm_body_pointer" && -f "$__aterm_body_pointer" ]] || return 0
    emulate -L zsh
    builtin unsetopt verbose
    local __aterm_addr="" __aterm_file=""
    { IFS= read -r __aterm_addr < "$__aterm_body_pointer" } 2>/dev/null
    command rm -f -- "$__aterm_body_pointer"
    # Exactly 16 lowercase hex digits — a folder address — or nothing changes.
    [[ ${#__aterm_addr} -eq 16 && "$__aterm_addr" != *[^0-9a-f]* ]] || return 0
    [[ "$__aterm_addr" != "$__aterm_body_rev" ]] || return 0
    __aterm_file="$__aterm_si_root/$__aterm_addr/aterm_shell_integration_body.zsh"
    [[ -f "$__aterm_file" && -O "$__aterm_file" ]] || return 0
    . "$__aterm_file" && __aterm_body_rev="$__aterm_addr"
    return 0
}

# THE TRAMPOLINE: the one precmd hook, and the one piece of the prompt path that
# never changes. A body sourced by the check is the body whose precmd runs in the
# SAME prompt, so the prompt that takes a new body already emits its marks.
# It returns 0, and so does the body's precmd: a hook's status is not `$?` for
# the hooks after it, the prompt or the next command (zsh restores it after
# every hook), and a function that returns non-zero is a failed command to a
# user's ZERR trap, which fired twice inside these after every failed command
# (measured on zsh 5.9; bare zsh fires it once, for the command). Its first
# command takes `$?` and turns XTRACE off, and is not traced (see THE TRACE).
__aterm_precmd() {
    local __aterm_status=$?${${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}:+}
    __aterm_body_check
    __aterm_body_precmd "$__aterm_status"
    return 0
}

# Install hooks using zsh hook arrays (the registrations are at the end of the
# file, after the body has defined what they name). Autoloaded here, before any
# `add-zsh-hook` a function below spells.
autoload -Uz add-zsh-hook

# @@ATERM-INTEGRATION-BODY-BEGIN@@ — everything from here to the END marker is re-sourced live

# Not traced (see THE TRACE): a loader from before it sources this body at a switch
# with the user's XTRACE on, and its body check puts that back at its return.
: ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}

# Enable agy's fast prompt editor by default (including in adopted shells when
# this body is live-reloaded at the next prompt) so U+202F narrow no-break
# spaces in macOS screenshot filenames do not desynchronize Bubble Tea's
# textarea visual cursor.
: "${AGY_CLI_FAST_PROMPT_EDITOR=true}"
export AGY_CLI_FAST_PROMPT_EDITOR

# ─── AN OLDER SCRIPT'S `~/.aterm/bin`, taken off (2026-10-19) ───
#
# Once per shell, in a shell an older script loaded that this body reaches in
# place (`__aterm_old_bin_off` unset; a fresh load settles it in the loader):
# `aterm_shell_integration_old_bin.zsh`, beside this file in its folder, says
# what goes and when, and takes it off. Its own file (2026-10-20), as in the
# bash script, where every fresh shell paid to parse it; compiled here with the
# body it cost a fresh zsh little, and the three scripts keep one shape. The
# folder is the one `%x` names (as `__aterm_si_dir`), this loader's or the body
# file's alone; a copy with no such file beside it takes nothing off. Sourced
# only when it is this user's (`-O`), as THE BODY CHECK sources a body.
if (( ! ${+__aterm_old_bin_off} )); then
    typeset -g __aterm_old_bin_off=1
    typeset -g __aterm_old_bin_file="${${(%):-%x}:h}/aterm_shell_integration_old_bin.zsh"
    if [[ -f "$__aterm_old_bin_file" && -O "$__aterm_old_bin_file" ]]; then
        . "$__aterm_old_bin_file"
    fi
    unset __aterm_old_bin_file __aterm_bin_from
fi

# ─── The reroute directory, FIRST — and the agents directory beside it ───
#
# $ATERM_REROUTE_DIR is set by aterm's spawn seam: the session-scoped directory of
# stubs for the upstream Rust names (`aterm help reroute`), which the seam already
# put FIRST on the PATH it handed this shell. That position is not final. This file
# runs from the wrapper .zshenv — BEFORE /etc/zprofile (path_helper rebuilds PATH
# from /etc/paths) and BEFORE ~/.zshrc (`. ~/.cargo/env` prepends ~/.cargo/bin) —
# and the package blocks just above may prepend too. Measured 2026-09-07:
# ~/.cargo/bin at position 17, ahead of the managed store at 19, so a bare `cargo`
# ran upstream Rust silently. An ORDER failure — which is why this is move-to-front
# (every existing occurrence removed, then prepended), never skip-if-present.
# Asserted here, after the package blocks, and again from __aterm_first_precmd,
# which runs after every rc file has had its say. Inert outside a session: the
# variable is unset, or the directory (Windows lays none) does not exist.
#
# $ATPKG_AGENTS — exported by the atpkg shell.d hook sourced just above — names
# <prefix>/agents, which holds ONLY the agent shims aterm keeps current (claude,
# codex, agy, muse; owner decisions 2026-09-10 and 2026-10-15). It fails the same
# way, for the same reasons: measured 2026-09-10 on m27 at PATH position 14, behind
# /opt/homebrew/bin (path_helper) and ~/.local/bin (~/.zshrc), so `codex` ran a brew
# cask that could run no command and `claude` an older native install. It is moved
# to the front first, then the reroute directory, so PATH reads reroute, agents, … —
# the spawn seam's own order; the two hold disjoint names, so what matters is that
# both precede everything else.
#
# `${(@)path:#…}`: `:#` matches the expanded value LITERALLY (no GLOB_SUBST), so a
# directory named with `[` or `*` is still removed by equality; `(@)` in quotes
# keeps an EMPTY entry ("here", to a POSIX shell) — the user's — from being dropped.
#
# It also records, in $__aterm_managed_want, the dirs it put in front (in order),
# which is what the per-prompt hot path below compares the head of $path against,
# and in $__aterm_managed_agents_on / $__aterm_managed_reroute_on whether each dir
# WAS there to front. A dir that was absent is re-probed by the hot path (one `-d`
# per prompt, only while it stays absent — review finding 2026-09-16: a hook that
# predates agents/ set $ATPKG_AGENTS, the `-d` here failed once, and the shell
# never looked again) and fronted the moment it appears. The `-d` stats of the
# steady state live HERE, on the change path, never on the per-prompt one.
#
# `emulate -L zsh` opens every function of this block: a user's rc may `setopt
# ksh_arrays` (subscripts from 0 — `path[1,n]` read the wrong elements, and
# `${(@)path:#…}` collapsed to element 0, so PATH was truncated to the two managed
# dirs at every prompt; measured 2026-09-16), `sh_word_split`, `glob_subst`, or
# `warn_create_global` (sourcing the hook from inside a function then printed four
# "created globally" lines at the prompt). `emulate -L` is a builtin, local to the
# function, and restores every option on return.
#
# $__aterm_managed_agents_listing leaves the names inside <prefix>/agents, joined
# by ":", in $__aterm_managed_agents_now: one readdir, in-process — a bare glob
# stats nothing, `(N)` makes an empty directory the empty string, and atpkg's
# dot-prefixed temp files are not matched. It is the TWIN WATCH of the hot path
# (step 4 below); the front records what it saw so the first prompt after a twin
# lands is the one that rehashes.
typeset -g __aterm_managed_agents_now=""
__aterm_managed_agents_listing() {
    emulate -L zsh
    __aterm_managed_agents_now=""
    [[ -n "${ATPKG_AGENTS:-}" ]] || return 0
    local -a __aterm_ls
    __aterm_ls=("$ATPKG_AGENTS"/*(N))
    __aterm_managed_agents_now="${(j.:.)__aterm_ls}"
}
typeset -ga __aterm_managed_want
typeset -gi __aterm_managed_agents_on=0
typeset -gi __aterm_managed_reroute_on=0
typeset -g __aterm_managed_agents_seen=""
__aterm_reroute_path_front() {
    emulate -L zsh
    __aterm_managed_want=()
    __aterm_managed_agents_on=0
    __aterm_managed_reroute_on=0
    if [[ -n "${ATPKG_AGENTS:-}" && -d "$ATPKG_AGENTS" ]]; then
        path=("$ATPKG_AGENTS" "${(@)path:#$ATPKG_AGENTS}")
        __aterm_managed_want=("$ATPKG_AGENTS")
        __aterm_managed_agents_on=1
        __aterm_managed_agents_listing
        __aterm_managed_agents_seen="$__aterm_managed_agents_now"
    fi
    if [[ -n "${ATERM_REROUTE_DIR:-}" && -d "$ATERM_REROUTE_DIR" ]]; then
        path=("$ATERM_REROUTE_DIR" "${(@)path:#$ATERM_REROUTE_DIR}")
        __aterm_managed_want=("$ATERM_REROUTE_DIR" "${__aterm_managed_want[@]}")
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
# What was measured in that tab: its zsh (pid 1784) was spawned at 10:44:24 by the
# PREVIOUS app build and ADOPTED across the seamless update — the running app
# (0.86.0, pid 1868) started at 10:44:32 — and <prefix>/agents plus the shell.d
# hooks were created at 10:46 by the new build's first pass. Nothing above runs
# again in a shell that is already up: the load-time assert and the first-precmd
# one both fire ONCE, gated on $ATPKG_AGENTS / $ATERM_REROUTE_DIR being set and the
# directories existing AT THAT INSTANT, and that shell had neither variable and no
# directory to find. So `which -a claude` read ~/.local/bin/claude first, `codex`
# resolved to a brew cask that hung two minutes on `--version`, and the only way to
# the build atpkg had just installed was a new tab. The same freeze hits EVERY fresh
# machine: the first tab opens before the seed pass creates agents/.
#
# The fix is a per-prompt AND per-command re-assert — preexec matters because a
# command typed at an idle prompt after the dirs appear runs BEFORE the next precmd
# — in four steps, all builtin-only (no `$(...)`, no backticks, no external
# stat/dirname/readlink; pinned by a grep test):
#
#  1. THE HOOK IS THE SOURCE OF TRUTH when the environment is missing or stale.
#     ~/.aterm/shell.d/00-atpkg.zsh is what atpkg generates (crates/atpkg/src/hooks.rs;
#     the spelling is pinned from that crate's side): it exports $ATPKG_AGENTS and
#     $ATPKG_BIN, moves agents/ to the front and appends bin/, and it is idempotent.
#     It is (re)sourced when the copy on disk is not the copy last sourced — it
#     appeared (a shell spawned before the file existed), or atpkg rewrote it
#     temp+rename on a later pass, so mtime OR inode moved. The stamp is read with
#     `zstat` (zsh/stat, loaded as the one builtin `b:zstat` so the module never
#     shadows /usr/bin/stat): ONE stat syscall, in-process, and its `2>/dev/null` is
#     a builtin redirection, not a fork. A hook that predates R1 (no `export
#     ATPKG_AGENTS`) is sourced ONCE per copy, not once per prompt (review finding
#     2026-09-16). Without the module (a minimal zsh) the fallback probes `-f` and
#     sources only while $ATPKG_AGENTS is unset (bash and fish compare the hook's
#     TEXT instead — their step 1; this rare fallback keeps the cheaper rule and
#     picks a REWRITTEN hook up in the next tab).
#  2. A DIR THAT WAS ABSENT when the front was last laid is probed again — one `-d`
#     per prompt, only in that degraded state — and fronted when it appears: a hook
#     that names an agents/ atpkg has not created yet, or a session whose seam
#     exported no $ATERM_REROUTE_DIR. Nothing is assigned while it stays absent.
#  3. THE ORDER. $__aterm_managed_want holds the dirs that must lead $path; the hot
#     path compares the head of $path against it by string equality and assigns
#     ONLY on a mismatch — assigning $path flushes zsh's command hash, which is
#     exactly what a change needs (`claude` re-resolves to the twin) and pure
#     waste otherwise.
#  4. THE TWIN WATCH. zsh hashes a command's path on first use, and a hashed name is
#     never searched again while the file exists — so once agents/ leads $path and
#     `claude` has run the foreign copy, a twin that lands LATER (a fresh machine:
#     agents/ is created at launch, the twin only once the managed program is
#     installed — the exact window in which the owner typed `claude`) would keep
#     losing to the hashed path for the life of the shell (measured 2026-09-16, zsh
#     5.9 and bash 3.2.57). The names inside agents/ are listed each call (one
#     readdir, no stat, no fork) and compared to the listing recorded when the dir
#     was fronted; on a change — a twin laid, or removed — `rehash` empties the
#     table and the next lookup walks $path again. A twin RE-laid under the same
#     name changes nothing here and needs nothing: the hashed path IS the twin.
#
# Per prompt and per command, steady state: one zstat, one readdir, no assignment.
# Measured 2026-09-16 (zsh 5.9, 10000 calls): ~30 µs per call before the twin
# watch; a fork of /usr/bin/true costs ~1400 µs.
#
# $ATERM_REROUTE_DIR is derived for a shell that predates it — the sibling
# `<dir of $ATPKG_AGENTS>/reroute`, when it is a directory and $__ATERM_REROUTE_PASSTHROUGH is
# not engaged (set, non-empty and not "0": atpkg::reroute::engaged) — so the final
# order is reroute, agents, everything else, bin/ last (the hook appends it).
#
# Gated on BEING INSIDE AN ATERM SESSION ($ATERM_CHILD=1, which the spawn seam sets
# for every child, or $ATERM_SESSION_ID) — NOT on $ATERM_REROUTE_DIR, which is
# precisely what the adopted shell lacks. Inert everywhere else.
#
# zsh/stat is loaded at the first stamp of a hook that EXISTS, not at load: an
# absent hook's stamp is the empty string with the module or without it, and
# loading it cost a fresh session shell 0.36 ms before its first prompt (measured
# 2026-10-16, zsh 5.9) — every first tab of a fresh machine, which opens before
# atpkg lays the hook. $__aterm_have_zstat says the stamp is zstat's (loaded, or
# to be loaded); a zsh whose module will not load drops to the `-f` fallback.
typeset -g __aterm_atpkg_hook="$HOME/.aterm/shell.d/00-atpkg.zsh"
typeset -gi __aterm_managed_live=0
typeset -gi __aterm_have_zstat=0
typeset -gi __aterm_zstat_loaded="${__aterm_zstat_loaded:-0}"
if [[ -n "${ATERM_CHILD:-}" || -n "${ATERM_SESSION_ID:-}" ]]; then
    __aterm_managed_live=1
    __aterm_have_zstat=1
fi

# Leaves "<mtime>:<inode>" of the hook on disk in $__aterm_atpkg_hook_now, or the
# empty string when it is absent (or zsh/stat is unavailable). One stat syscall,
# no fork. Its own global, not $REPLY: precmd runs between a user's `read` and
# the line that consumes $REPLY, and must not clobber it.
typeset -g __aterm_atpkg_hook_now=""
__aterm_atpkg_hook_stamp() {
    emulate -L zsh
    __aterm_atpkg_hook_now=""
    (( __aterm_have_zstat )) || return 0
    if (( ! __aterm_zstat_loaded )); then
        [[ -e "$__aterm_atpkg_hook" ]] || return 0
        if ! zmodload -F zsh/stat b:zstat 2>/dev/null; then
            __aterm_have_zstat=0
            return 0
        fi
        __aterm_zstat_loaded=1
    fi
    local -A __aterm_st
    zstat -H __aterm_st -- "$__aterm_atpkg_hook" 2>/dev/null || return 0
    __aterm_atpkg_hook_now="$__aterm_st[mtime]:$__aterm_st[inode]"
}
# The copy the shell.d loop above sourced at load is the copy last sourced —
# whatever it exported (a pre-R1 hook exports no $ATPKG_AGENTS, and is still not
# sourced again until atpkg rewrites it). Recorded ONCE per shell: a body
# re-sourced live keeps the record, so a hook rewritten just before is still
# sourced at the next prompt.
if (( ! ${+__aterm_atpkg_hook_seen} )); then
    typeset -g __aterm_atpkg_hook_seen=""
    if (( __aterm_managed_live )); then
        __aterm_atpkg_hook_stamp
        __aterm_atpkg_hook_seen="$__aterm_atpkg_hook_now"
    fi
fi

# Exports $ATERM_REROUTE_DIR (status 0) or leaves it alone (status 1).
__aterm_managed_derive_reroute() {
    emulate -L zsh
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

# The hot path: every precmd and every preexec. Builtin-only — see above.
__aterm_managed_path_live() {
    emulate -L zsh
    (( __aterm_managed_live )) || return 0
    # 1. The hook: sourced when the copy on disk is not the copy last sourced.
    # Stamped first, and the stamp's verdict on zsh/stat read after: a module that
    # will not load at the first hook's stamp sends that same call to the `-f`
    # fallback, not the next prompt's.
    if (( __aterm_have_zstat )); then
        __aterm_atpkg_hook_stamp
    fi
    if (( __aterm_have_zstat )); then
        if [[ -n "$__aterm_atpkg_hook_now" && "$__aterm_atpkg_hook_now" != "$__aterm_atpkg_hook_seen" ]]; then
            __aterm_atpkg_hook_seen="$__aterm_atpkg_hook_now"
            . "$__aterm_atpkg_hook"
            __aterm_managed_derive_reroute
            __aterm_reroute_path_front
            return 0
        fi
    elif [[ -z "${ATPKG_AGENTS:-}" && -f "$__aterm_atpkg_hook" ]]; then
        . "$__aterm_atpkg_hook"
        __aterm_managed_derive_reroute
        __aterm_reroute_path_front
        return 0
    fi
    # 2. A dir that was absent when the front was last laid: probe it again.
    local -i __aterm_refront=0
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
    # 3. The order: assign only on a mismatch.
    local -i __aterm_n=$#__aterm_managed_want
    if (( __aterm_n )) && [[ "${(j.:.)path[1,__aterm_n]}" != "${(j.:.)__aterm_managed_want}" ]]; then
        __aterm_reroute_path_front
        return 0
    fi
    # 4. The twin watch: a name appearing in (or leaving) agents/ empties the hash.
    if (( __aterm_managed_agents_on )); then
        __aterm_managed_agents_listing
        if [[ "$__aterm_managed_agents_now" != "$__aterm_managed_agents_seen" ]]; then
            __aterm_managed_agents_seen="$__aterm_managed_agents_now"
            rehash
        fi
    fi
    return 0
}

# State tracking. KEPT across a live re-source: the body is re-sourced from the
# precmd trampoline, and a command that was running must still get its 133;D.
# The functions that run with the user's options (the precmd, the zle-line-init
# widgets: no `emulate`) set a global with `typeset -g`: a plain
# assignment there is what a user's WARN_NESTED_VAR warns at, at every command
# and in the middle of the line editor's prompt.
typeset -g __aterm_in_command="${__aterm_in_command:-0}"
typeset -g __aterm_report_host="${HOST:-${HOSTNAME:-localhost}}"

# OSC escape sequences.
#
# `printf '%s'` — NOT `print -n` — because zsh's `print` without `-r` expands
# escape sequences in its ARGUMENT, and the argument here is the whole
# already-expanded payload. That silently undid every escape the callers below
# construct. Verified on the wire: `__aterm_encode_cmd` correctly turned a
# command line `a<ESC>b<BEL>c;d e` into `a\x1bb\x07c\x3bd\x20e`, and `print -n`
# converted those escapes straight back into RAW 0x1b / 0x07 bytes inside the
# OSC 633;E payload — the embedded BEL terminates the OSC string early and the
# remaining bytes are parsed as fresh input, which is exactly the OSC break-out
# the encoder exists to prevent.
#
# The OSC 0 title path was worse. `${title//[[:cntrl:]]/}` strips control BYTES,
# but a command whose LITERAL text reads `echo \e]52;c;aGVsbG8=\a` contains no
# control bytes for that guard to strip — `print` then manufactured the ESC and
# BEL itself, smuggling a live OSC 52 clipboard write out of the tab title.
#
# `printf` never interprets a `%s` argument, which is why the bash script — which
# always spelled it `printf '\033]%s\a' "$1"` — was never affected; this is now
# the identical spelling. zsh's `printf` is a builtin, so the frame still costs
# no fork. `print -rn --` is NOT a sufficient fix on its own: `-r` would also
# stop the leading `\e` and trailing `\a` of the frame itself from being
# interpreted, emitting a literal backslash-e instead of an OSC introducer.
__aterm_osc() {
    printf '\033]%s\a' "$1"
}

# The re-key channel's check (the path is captured by the loader above).
__aterm_rekey_check() {
    emulate -L zsh
    [[ -n "$__aterm_rekey_path" && -f "$__aterm_rekey_path" ]] || return 0
    local key=""
    { IFS= read -r key < "$__aterm_rekey_path" } 2>/dev/null
    command rm -f -- "$__aterm_rekey_path"
    # Exactly 64 lowercase hex digits, or nothing changes.
    [[ ${#key} -eq 64 && "$key" != *[^0-9a-f]* ]] || return 0
    __aterm_shell_nonce="$key"
    __aterm_id_suffix_str=";id=${key}"
}

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Expands to ";id=<64-hex>" when the captured nonce is non-empty, or to
# the empty string otherwise. Reads from the captured local — never from
# the environment — so the nonce is not inherited by subprocesses.
# Kept as the documented helper / external entry point; the hot emitters
# below use $__aterm_id_suffix_str instead to avoid a fork per marker. Not
# traced (see THE TRACE): its caller's `set -x` printed the nonce.
__aterm_id_suffix() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    if [[ -n "$__aterm_shell_nonce" ]]; then
        print -rn -- ";id=${__aterm_shell_nonce}"
    fi
}

# Percent-encode a string for use in file:// URIs (RFC 3986), into REPLY.
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; all others
# are encoded byte-by-byte as %XX. LC_ALL=C ensures multi-byte UTF-8
# characters are split into individual bytes for correct encoding.
#
# Runs once per prompt (via __aterm_report_cwd), so it is fork-free by
# construction: `printf -v` writes into a variable instead of spawning a
# `$(printf ...)` subshell per encoded byte. A path with a single space used
# to cost a fork; a 4-byte emoji cost four. The answer is REPLY, not output:
# a caller's `$(...)` around it was itself a fork at every prompt (0.6 ms,
# measured on zsh 5.9). The caller declares `local REPLY`, so a user's REPLY
# — set by `read` at one prompt, used at the next — is never touched. No
# `& 0xFF` mask is needed (or present, historically): unlike bash, zsh's
# `printf '%d' "'<byte>"` returns the UNSIGNED byte value (195 for 0xC3), so
# `%02X` is already correct.
#
# Under `emulate -L zsh`, like `__aterm_encode_cmd`: the loop reads
# `${string[$i]}` from 1, and a user's KSH_ARRAYS (subscripts from 0) dropped the
# first byte and read one past the last — `file://<host>` then ran into the path,
# and the `%00` it ended with is a cwd the engine refuses, so the tab's cwd stayed
# at the directory before (measured on zsh 5.9).
__aterm_urlencode() {
    emulate -L zsh
    local LC_ALL=C
    # Fast path: no byte needs encoding, so the loop would copy the string
    # verbatim. Skip it. (The class is exactly the loop's pass-through class,
    # so this is the same decision the loop would make for every byte.)
    if [[ "$1" != *[^a-zA-Z0-9_.~/-]* ]]; then
        REPLY="$1"
        return
    fi
    local string="$1" i char encoded="" hex
    for ((i = 1; i <= ${#string}; i++)); do
        char="${string[$i]}"
        case "$char" in
            [a-zA-Z0-9_.~/-]) encoded+="$char" ;;
            *) printf -v hex '%%%02X' "'$char"; encoded+="$hex" ;;
        esac
    done
    REPLY="$encoded"
}

# Report current working directory (OSC 7). With a nonce, the same URI first
# goes SIGNED like the marks as the property `633;P;AtermCwd=` (the encoder
# above always escapes `;`): the shell's own word, so aterm drops a background
# job's report that lands between it and the 133;A. The OSC 7 carries no nonce:
# an aterm from before the property ignores the property and stores what the
# OSC 7 names (docs/DESIGN-cwd-reports-2026-10-19.md).
__aterm_report_cwd() {
    local REPLY
    __aterm_urlencode "$PWD"
    local uri="file://${__aterm_report_host}${REPLY}"
    if [[ -n "$__aterm_id_suffix_str" ]]; then
        __aterm_osc "633;P;AtermCwd=${uri}${__aterm_id_suffix_str}"
    fi
    __aterm_osc "7;${uri}"
}

# Mark prompt start (OSC 133;A)
__aterm_mark_prompt_start() {
    __aterm_osc "133;A${__aterm_id_suffix_str}"
}

# Mark command line start (OSC 133;B). Sent from zle-line-init, after zle drew
# the line: over an EMPTY buffer the cursor stands where the buffer starts, and
# the mark says so (`aterm_input=start`, signed). A line pre-filled from the
# buffer stack (push-line, accept-and-hold, accept-line-and-down-history,
# `print -z`) is drawn first, so the mark lands after its text (measured on zsh
# 5.9: `P> echo hi` with the mark at column 10), and it says nothing of where
# the buffer starts. aterm's typing prediction places Home, End, Ctrl-A, Ctrl-E
# and Delete only on a line started at a mark that says so
# (`Terminal::input_region_start`).
__aterm_mark_command_start() {
    if [[ -n "$__aterm_id_suffix_str" ]] && zle && (( ! ${#BUFFER} )); then
        __aterm_osc "133;B${__aterm_id_suffix_str};aterm_input=start"
    else
        __aterm_osc "133;B${__aterm_id_suffix_str}"
    fi
}

# Mark command execution start (OSC 133;C)
__aterm_mark_exec_start() {
    __aterm_osc "133;C${__aterm_id_suffix_str}"
}

# Mark command completion (OSC 133;D;exitcode)
__aterm_mark_exec_finish() {
    __aterm_osc "133;D;$1${__aterm_id_suffix_str}"
}

# The body's revision, SIGNED (633;P, the VS Code property mark): the folder
# address this body was taken from, so the host can tell a shell running its own
# body from one running an older build's (`status integration_rev=`). Emitted
# before every 133;A — a builtin printf, nothing when the revision is unknown (a
# hand-installed copy) — so a successor that adopted this shell learns it at the
# first prompt after the update, whatever it carried.
__aterm_mark_integration_rev() {
    [[ -n "$__aterm_body_rev" ]] || return 0
    __aterm_osc "633;P;AtermIntegration=${__aterm_body_rev}${__aterm_id_suffix_str}"
}

# ─── The tty settings the shell keeps (2026-09-26) ───
#
# THE DEFECT. zsh ADOPTS whatever terminal settings the last foreground job left
# behind: when a job ends, an unfrozen zsh re-reads the tty and runs every later
# command under what it found. A raw-mode program that dies without restoring
# them — SIGKILLed, or exiting after `tty.setraw` without a `finally` — therefore
# leaves every command after it with `-isig -iexten -icrnl -ixon -opost`: Ctrl-C
# no longer interrupts anything (it arrives as a literal ^C byte), Enter sends a
# bare CR that ends no line (`read x`, a password prompt: "hello^M", and only
# Ctrl-J finishes it), and output staircases. The zle prompt itself looks fine —
# zle sets its own modes to edit — which is why it reads as "Ctrl-C is broken in
# this tab" and nothing else. Measured 2026-09-26 on zsh 5.9, this file sourced,
# in a pty: a python `tty.setraw` child that died by SIGKILL, `os._exit(1)` or
# `os._exit(0)`, and a node `setRawMode` child SIGKILLed (the Claude Code shape),
# all left it; Ctrl-C sent 0.4 s into `sleep 3` ended it at 3.04–3.09 s. And it
# was seen for real: a SIGKILLed Claude Code 2.1.283 in a headless aterm left the
# integrated login zsh exactly so ("sleep STILL RUNNING 1s after ctrl+c",
# `read x` returning `hello\r` only on Ctrl-J). The terminal-mode handback
# (f7757d04b) repairs what the dead program told the TERMINAL; this is what it
# told the TTY DRIVER, which only the shell that owns the tty can keep straight —
# a repair from the pty master does not stick, because zsh re-applies the state
# it adopted after the next job.
#
# THE FIX, in two parts, armed once per shell at the first prompt this body runs
# (`__aterm_tty_arm`, from `__aterm_body_precmd`): after every rc file has had its
# say, and — through the loader's body pointer — also in a shell that was already
# running, possibly already broken, when this body reached it.
#
#  1. REPAIR FIRST: when the tty is in a dead raw program's state (ISIG off —
#     every raw mode clears it, and nobody keeps it off at a prompt on purpose,
#     since it disables Ctrl-C and Ctrl-Z), turn back on what raw modes turn off:
#     `isig icanon iexten echo icrnl opost`. IXON is deliberately NOT touched: a
#     raw mode clears it too, but so does a user's own `stty -ixon`, and which of
#     the two cleared it cannot be told apart — so a deliberate `stty -ixon`
#     survives. Freezing a tab that is already broken would lock the broken state
#     in; that is why the repair comes first.
#  2. FREEZE (`ttyctl -f`, zsh's own mechanism for exactly this): from now on the
#     shell does not adopt a job's tty changes; it puts back what it had after
#     every job, exactly — IXON included. Fork-free at every prompt. What a user
#     changes ON PURPOSE must still stick, so `stty`, `reset` and `tset` become
#     thin functions that thaw the tty, run the real command, and freeze what it
#     left (`__aterm_tty_thaw_run`) — typed at the prompt, from an alias, or in a
#     sourced file alike. What a freeze does undo is a change made by a CHILD
#     process on its own (a script running `stty`, `command stty`, `/bin/stty`):
#     `ttyctl -u` thaws for the rest of the shell.
#
# Measured with exactly this code (2026-09-26, zsh 5.9, pty): all five dying
# children above leave `isig iexten icrnl ixon opost` and Ctrl-C ends `sleep 3`
# in 0.46–0.54 s; `read -r x` ends on Enter; `stty -ixon` typed before the kill
# survives it; a shell that was ALREADY broken when the body arrived is repaired
# at that prompt and stays repaired across the next kill; `reset` still works.
#
# RESPECTING THE USER. The freeze and the wrappers are skipped — the shell keeps
# zsh's adopting behaviour — when the user's rc defines ANY of `stty`, `reset`,
# `tset` as a function or an alias: their definition would bypass the thaw, and a
# frozen tty would then silently undo what it does. (That is also the way to keep
# zsh's own behaviour on purpose; there is deliberately no environment switch —
# the owner's rule of 2026-09-22 is one batteries-included default, not knobs.
# `ttyctl -u` at a prompt thaws the tty for the rest of that shell.) Such a shell
# still gets the repair: after every command that FAILED (status non-zero — a
# SIGKILL is 137), one `stty -a` fork, and the repair only when the raw signature
# is there. What that fallback cannot catch is a raw program that exits 0 without
# restoring; the freeze catches that too.
#
# A FRESH SHELL'S FIRST PROMPT does not wait for the read (2026-10-15): the
# `stty -a` fork and exec cost that prompt 1.4 ms (measured on zsh 5.9), on the
# path of every new tab. There the read starts in the background (a process
# substitution: the prompt pays the fork alone), the freeze and the wrappers
# are armed at once, and the repair waits for the read until the first
# command's preexec — or the next prompt, when the line was empty. What it
# read is what the shell kept: the line editor leaves ISIG, the flag the repair
# reads, as it found it. Found broken — an rc ran a raw program that died — the
# tty is repaired THROUGH the thaw (`__aterm_tty_thaw_run`), so the shell keeps
# the repair and freezes that, before the first command runs. A Ctrl-C at
# that first prompt cannot wait for a command: THE FIRST PROMPT'S CTRL-C
# below takes the read as soon as it answers. An upgrade in
# place (`__aterm_fresh_load` 0) reads at once, as before: that shell may be
# broken right now. A body taken through the pointer finds the shell armed by
# the body before it, except in a shell whose earlier body had no arm (builds
# of 2026-09-26 between the loader and the tty fix): its loader's
# `__aterm_fresh_load` is still 1, so it defers like a fresh shell, and its
# next command still runs repaired.
#
# Kept across a live re-source: a newer body arriving through the pointer finds
# the shell already armed and changes nothing about it.
typeset -gi __aterm_tty_armed="${__aterm_tty_armed:-0}"
typeset -gi __aterm_tty_frozen="${__aterm_tty_frozen:-0}"
# The descriptor of the read a fresh shell's first prompt left running, or 0.
typeset -gi __aterm_tty_probe_fd="${__aterm_tty_probe_fd:-0}"
# 1 once THE FIRST PROMPT'S CTRL-C repaired the tty from inside the line
# editor, thawed: the freeze is owed to the next prompt or command.
typeset -gi __aterm_tty_owed="${__aterm_tty_owed:-0}"

# Whether the tty is in a dead raw program's state (part 1 above): `raw` in
# REPLY when it is, else nothing. One fork to read it; silent when stdin is
# not a terminal.
__aterm_tty_read() {
    emulate -L zsh
    REPLY=""
    local __aterm_tty_state=""
    __aterm_tty_state="$(command stty -a 2>/dev/null)" || return 0
    local -a __aterm_tty_words
    __aterm_tty_words=(${=__aterm_tty_state})
    (( ! ${__aterm_tty_words[(Ie)-isig]} )) || REPLY=raw
    return 0
}

# The repair (part 1 above): the tty read now — or what the read a fresh
# shell's first prompt started answered, taken now — and a second fork only
# when it is in a dead raw program's state; through the thaw when the tty is
# frozen, so the shell keeps what it set.
__aterm_tty_repair() {
    emulate -L zsh
    local REPLY=""
    if (( __aterm_tty_probe_fd )); then
        __aterm_tty_take
    else
        __aterm_tty_read
    fi
    [[ "$REPLY" == raw ]] || return 0
    if (( __aterm_tty_frozen )); then
        __aterm_tty_thaw_run stty isig icanon iexten echo icrnl opost 2>/dev/null
    else
        command stty isig icanon iexten echo icrnl opost 2>/dev/null
    fi
    return 0
}

# The first prompt's read, taken into the caller's REPLY: up to its end — the
# probe has exited by then — where `read` answers 1, the first command of an
# `||` list, so no user's ZERR trap hears it. The editor's watch on it goes too.
__aterm_tty_take() {
    zle -F $__aterm_tty_probe_fd 2>/dev/null
    read -r -u $__aterm_tty_probe_fd REPLY || true
    exec {__aterm_tty_probe_fd}<&-
    __aterm_tty_probe_fd=0
}

# THE FIRST PROMPT'S CTRL-C (2026-10-17). Until the deferred read is taken, a
# fresh shell's first prompt runs in the tty its rc left. A raw one has ISIG
# off, so Ctrl-C there reached the line editor as a byte, which beeped (emacs)
# or inserted a ^C (viins) and kept the line: measured, zsh 5.9, `echo LOST`,
# Ctrl-C, `echo KEPT` ran `echo LOSTecho KEPT`. Waiting for the read before
# the prompt would put its fork and exec back on every new tab, so the line
# editor watches it instead (`zle -F`) and takes it the moment it answers,
# about a millisecond after the prompt is drawn; a sane tty costs nothing
# more. Answered `raw`, the repair runs right there, from inside the editor,
# which takes three steps (zsh 5.9's zle_main.c, jobs.c): `zle -I` hands the
# tty back in the state the shell keeps and asks the editor to keep whatever
# it finds when it takes the tty again — but only while the tty is not
# frozen (a job run inside the editor is never adopted) — so the freeze is
# lifted, `stty` puts back what raw modes turn off (its stdin the tty: the
# editor's is not), and the editor redraws the prompt in place on the
# repaired tty. Ctrl-C there is then the shell's own, `$?` 130 and all. The
# freeze is owed to the next prompt or command (`__aterm_tty_owed`). A
# command typed before the read answers takes it at its preexec, as before.
# Not traced (see THE TRACE): the editor calls it under the user's options.
__aterm_tty_probe_ready() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    local REPLY=""
    __aterm_tty_take
    [[ "$REPLY" == raw ]] || return 0
    zle -I
    if (( __aterm_tty_frozen )); then
        builtin ttyctl -u
        __aterm_tty_owed=1
    fi
    command stty isig icanon iexten echo icrnl opost 2>/dev/null <$TTY
    return 0
}

# Run a tty-changing command with the tty THAWED, so the shell adopts what it
# leaves, then freeze that (part 2 above). `builtin`/`command` so no alias or
# function of the user's, and not these wrappers themselves, is reached.
#
# The re-freeze MUST run on every way out, because `__aterm_tty_frozen` stays 1
# and so keeps the fallback repair off: a wrapper that leaves the tty thawed
# brings back the whole defect, silently, for the rest of the shell. Two ways out
# skipped it until the review of 2026-09-26, both measured in a pty:
#  - a user's `setopt err_return`: this function ran under the USER's options,
#    so a failing command (`stty bogusflag`, a typo) returned before `ttyctl -f`.
#    `ttyctl` then said "tty is not frozen" with `__aterm_tty_frozen=1`, and a
#    raw child's `exit 1` left `-isig -iexten -icrnl -opost -ixon` (Ctrl-C into
#    `sleep 3` took 3.03-4.04 s). `emulate -L zsh` closes that.
#  - Ctrl-C while the command runs (`tset`'s "TERM = (unknown)?" question,
#    `reset`'s pause): zsh abandons the rest of a function whose foreground child
#    died by SIGINT, `emulate` or not. The `always` block runs on that path too.
# The command's own status is still what the wrapper returns (130 for the ^C).
# Not traced (see THE TRACE): the user's `stty` line is, as in bare zsh.
__aterm_tty_thaw_run() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    local -i __aterm_tty_rc=0
    {
        builtin ttyctl -u
        command "$@"
        __aterm_tty_rc=$?
    } always {
        builtin ttyctl -f
    }
    return $__aterm_tty_rc
}

# Once per shell, at the first prompt this body runs; a fresh shell's repair
# follows at its first command (see "A FRESH SHELL'S FIRST PROMPT" above). The
# wrappers are spelled with the `function` keyword: a user's alias for the name
# would otherwise be expanded when a live body is parsed and define something
# else entirely. They open with THE TRACE's line: under `set -x`, bare zsh
# traces the `stty` line and nothing for it, where the wrapper traced its own
# call too.
__aterm_tty_arm() {
    emulate -L zsh
    __aterm_tty_armed=1
    if (( __aterm_fresh_load )); then
        # The probe answers in a word, so taking it is one short read; the
        # editor takes it when it does (THE FIRST PROMPT'S CTRL-C).
        exec {__aterm_tty_probe_fd}< <(__aterm_tty_read; print -rn -- "$REPLY")
        zle -F $__aterm_tty_probe_fd __aterm_tty_probe_ready 2>/dev/null
    fi
    (( __aterm_tty_probe_fd )) || __aterm_tty_repair
    local __aterm_tty_name
    for __aterm_tty_name in stty reset tset; do
        (( ${+aliases[$__aterm_tty_name]} )) && return 0
        if (( ${+functions[$__aterm_tty_name]} )) &&
           [[ "${functions[$__aterm_tty_name]}" != *__aterm_tty_thaw_run* ]]; then
            return 0
        fi
    done
    function stty {
        : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
        __aterm_tty_thaw_run stty "$@"
    }
    function reset {
        : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
        __aterm_tty_thaw_run reset "$@"
    }
    function tset {
        : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
        __aterm_tty_thaw_run tset "$@"
    }
    builtin ttyctl -f
    __aterm_tty_frozen=1
    return 0
}

# precmd - runs before each prompt, from the loader's trampoline, which hands it
# the status the command before it left. XTRACE off first (see THE TRACE): a
# loader from before it calls this with the user's on. No `emulate`: the VERBOSE
# it turns off below stays off for its caller.
__aterm_body_precmd() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    local last_status=$1

    # A waiting re-key first, so every mark this prompt emits carries it.
    __aterm_rekey_check

    # The tty settings (see "The tty settings the shell keeps" above): a fresh
    # shell's first-prompt read taken (an empty line ran no preexec), armed once;
    # after that, fork-free while frozen, and one probe after a failed command
    # where the user's own definitions ruled the freeze out.
    (( ! __aterm_tty_probe_fd )) || __aterm_tty_repair
    (( ! __aterm_tty_owed )) || { __aterm_tty_owed=0; builtin ttyctl -f }
    (( __aterm_tty_armed )) || __aterm_tty_arm
    (( __aterm_tty_frozen || last_status == 0 )) || __aterm_tty_repair

    # 133;B leads zle-line-init again where something took it (see "THE LEAD").
    __aterm_line_init_wire "$last_status"
    # VERBOSE echoes a scheduled event's text before it runs (bare zsh echoes
    # nothing between two prompts): off here, at the caller's level — the wire
    # runs under `emulate -L` — and on again from the event's first `[[ ]]`. The
    # precmd hooks after this one run with it off: a file one of them sources is
    # not echoed (no function body or `eval` is echoed either way).
    (( ! __aterm_line_init_hush )) || builtin unsetopt verbose

    # The managed dirs, live (see "LIVE" above): one stat, one readdir, an assign
    # (or a rehash) only on a change.
    __aterm_managed_path_live

    # If we were in a command, mark it finished
    if (( __aterm_in_command )); then
        __aterm_mark_exec_finish $last_status
        typeset -g __aterm_in_command=0
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

    # 0, not the command's status (see THE TRAMPOLINE).
    return 0
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
# use `printf -v` instead of a `$(printf ...)` subshell. Under `emulate -L zsh`
# (see `__aterm_urlencode`): a user's KSH_ARRAYS cut the first letter off every
# command line 633;E reported.
__aterm_encode_cmd() {
    emulate -L zsh
    local LC_ALL=C
    local string="$1" i char encoded="" hex
    for ((i = 1; i <= ${#string}; i++)); do
        char="${string[$i]}"
        case "$char" in
            \\) encoded+="\\\\" ;;
            \;) encoded+="\\x3b" ;;
            ' ') encoded+="\\x20" ;;
            [[:cntrl:]]) printf -v hex '\\x%02x' "'$char"; encoded+="$hex" ;;
            *) encoded+="$char" ;;
        esac
    done
    REPLY="$encoded"
}

# preexec - runs before command execution. XTRACE off first (see THE TRACE), then
# zsh's own options: nothing here is the user's to change.
__aterm_preexec() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    typeset -g __aterm_in_command=1

    # A fresh shell's tty, repaired before its first command runs (see "A FRESH
    # SHELL'S FIRST PROMPT"), and frozen again where the first prompt's repair
    # left it thawed (THE FIRST PROMPT'S CTRL-C).
    (( ! __aterm_tty_probe_fd )) || __aterm_tty_repair
    (( ! __aterm_tty_owed )) || { __aterm_tty_owed=0; builtin ttyctl -f }

    # The managed dirs, live — BEFORE this command resolves: a `claude` typed at a
    # prompt that was drawn before atpkg laid agents/ must already run the twin.
    __aterm_managed_path_live

    # Report command text for session memory (OSC 633;E)
    local REPLY
    __aterm_encode_cmd "$1"
    __aterm_osc "633;E;${REPLY}${__aterm_id_suffix_str}"

    # Set tab title to running command (OSC 0): the line, control characters
    # stripped, at most 256 characters; a longer one is its first 255 and `…`, so
    # the tab knows its end is cut off (the bash script's "The command title").
    if [[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]; then
        local cmd="$1"
        (( ${#cmd} <= 256 )) || cmd="${cmd:0:255}…"
        __aterm_osc "0;${cmd//[[:cntrl:]]/}"
    fi

    # Mark execution start
    __aterm_mark_exec_start
}

# ─── Prompt Override ───
# When ATERM_PROMPT_STYLE is set, override PS1 using palette-indexed colors.
# Git branch is evaluated dynamically via PROMPT_SUBST (updates on cd).
__aterm_set_prompt() {
    local style="${ATERM_PROMPT_STYLE:-none}"
    [[ "$style" == "none" ]] && return

    setopt PROMPT_SUBST

    local hc="${ATERM_PROMPT_HOST_COLOR:-2}"
    local pc="${ATERM_PROMPT_PATH_COLOR:-4}"
    local gc="${ATERM_PROMPT_GIT_COLOR:-3}"
    local ec="${ATERM_PROMPT_ERROR_COLOR:-1}"
    local sc="${ATERM_PROMPT_SEP_COLOR:-8}"

    local h="%F{$hc}" p="%F{$pc}" g="%F{$gc}" e="%F{$ec}" s="%F{$sc}" r="%f"
    local err="%(?.${s}.${e})"

    case "$style" in
        minimal)
            PROMPT="${p}%1~${r} ${err}\$${r} "
            ;;
        standard)
            PROMPT=''"${h}%n@%m${s}:${p}%~${r}"' $(__aterm_git_segment '"${g}"' '"${r}"') '"${err}\$${r} "
            ;;
        powerline)
            PROMPT=''"${h}%n@%m${r} ${s}${r} ${p}%~${r}"' $(__aterm_git_segment '"${g}"' '"${r}"') '"${s}${r} ${err}\$${r} "
            ;;
    esac
}

__aterm_git_segment() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    local branch
    branch=$(command git rev-parse --abbrev-ref HEAD 2>/dev/null) || return
    [[ -n "$branch" ]] && print -n "${1}(${branch//\%/%%})${2}"
}

# ─── Key Bindings ───
# Bind xterm-style modifier+arrow sequences so they work at the prompt.
# Without these, sequences like \e[1;3C (Alt+Right) leak as literal text.
__aterm_setup_keybindings() {
    # Alt+Arrow: word navigation
    bindkey '\e[1;3C' forward-word       # Alt+Right
    bindkey '\e[1;3D' backward-word      # Alt+Left
    # Ctrl+Arrow: word navigation (alternative modifier)
    bindkey '\e[1;5C' forward-word       # Ctrl+Right
    bindkey '\e[1;5D' backward-word      # Ctrl+Left
    # Home/End
    bindkey '\e[H' beginning-of-line     # Home
    bindkey '\e[F' end-of-line           # End
    bindkey '\e[1~' beginning-of-line    # Home (alternate)
    bindkey '\e[4~' end-of-line          # End (alternate)
    # Delete
    bindkey '\e[3~' delete-char          # Delete/Fn+Backspace
    # Shift+Arrow: selection (if zsh supports it, otherwise history)
    bindkey '\e[1;2A' up-line-or-history    # Shift+Up
    bindkey '\e[1;2B' down-line-or-history  # Shift+Down
}
# Called ONCE per shell, by the loader (the end of this file): a body re-sourced
# live leaves the user's bindings alone.

# ─── OSC 133;B (end of prompt / start of user input) ───
# Emitted via zle-line-init so it fires after the prompt is fully drawn.
# Placing it in preexec is too late (user has already typed their command).
# The loader wires `__aterm_zle_line_init` once (end of file) — from the wrapper
# .zshenv, BEFORE ~/.zshrc. Where the lead (below) marked the line already, it
# only passes the line on. With nothing to pass it to it returns 0, not the status
# of a test: add-zle-hook-widget's dispatcher (`zle "$hook" … || return`) stopped
# at aterm's widget, its first hook, and never ran the user's own line-init hooks
# after it (measured 2026-10-01, zsh 5.9). What it passes the line to is reached
# with the `$?` it was reached with (`__aterm_line_init_pass`).
__aterm_zle_line_init() {
    local -i __aterm_reached=$?
    if (( ! ${__aterm_line_init_marked:-0} )); then
        typeset -g __aterm_line_init_marked=1
        __aterm_mark_command_start
    fi
    # What holds the front reached this widget: it leads with 133;B by itself.
    typeset -g __aterm_line_init_via="${widgets[zle-line-init]-}"
    (( ${+widgets[__aterm_orig_zle_line_init]} )) || return 0
    __aterm_line_init_pass $__aterm_reached __aterm_orig_zle_line_init
}

# `zle $2`, entered with `$?` = `$1`. zle hands zle-line-init the status of the
# command before the line, which a widget reads as `$?` until it runs a command
# of its own — and aterm's widgets ran theirs first, so the widget they pass the
# line to read 0 (measured on zsh 5.9: after `false`, a zle-line-init that
# prints `$?` printed 1 in bare zsh and 0 behind aterm's). The `if` leaves 0 for
# a 0; any other status is a function's return, which on the left of `||` is no
# failed command to a ZERR trap or ERR_RETURN. Run with the user's options.
__aterm_line_init_pass() {
    if (( ! $1 )); then
        zle "$2"
    else
        __aterm_line_init_status "$1" || zle "$2"
    fi
}
__aterm_line_init_status() { return "$1"; }

# THE LEAD (2026-10-01). An rc that defines its own zle-line-init with `zle -N`
# REPLACES the loader's widget instead of chaining it: oh-my-zsh's
# lib/key-bindings.zsh does exactly that (`echoti smkx`, whenever terminfo has
# smkx — every aterm tab), and so do vi-mode cursor snippets. Measured 2026-10-01
# (zsh 5.9, oh-my-zsh 4d4cfc2, its default template): 133;A at every prompt and
# never a 133;B, so the engine — which takes 133;C only after a B — kept no block
# and no exit code for any command (`ctl blocks`: every block `prompt exit=-`),
# while `status` read `integration=on`.
#
# So 133;B goes in FRONT of whatever holds zle-line-init — for the one line it
# marks, and only once every precmd hook has run. The precmd schedules
# `__aterm_line_init_late` (`sched +0`: zsh runs a due event after the precmd
# hooks, before it expands the prompt and reads the line), which puts
# `__aterm_line_init_lead` in front and keeps what was there as
# `__aterm_line_init_under`; the lead marks the line, puts the table back as it
# was and runs that widget AS zle-line-init. No rc or precmd hook finds the lead
# in the table (what reads it while the prompt is expanded: see the two notes
# below), and every widget runs once per line. A lead that stayed in front, put
# back by the precmd, was seen by the plugins that read the table:
#  - zsh-vi-mode, which at its first prompt wraps a widget listed under another
#    function's name and replaces one listed under its own, wrapped the lead
#    where it replaces oh-my-zsh's widget: `echoti smkx` kept running at every
#    line while oh-my-zsh's `rmkx` (zle-line-finish, which zsh-vi-mode did
#    replace) did not, so the cursor-key and keypad application modes stayed set
#    while every command ran (measured 2026-10-01 with `ctl modes`; bare zsh
#    sets neither);
#  - a plugin that wraps zle-line-init again whenever its wrapper lost the front,
#    under a NEW name each time (zsh-autosuggestions, once `zle-*` is out of its
#    ignore list), wrapped the lead again at every prompt, and each line ran every
#    wrapper so far until FUNCNEST stopped it (measured: from prompt 139 on).
# (The PROMPT string is no place for the mark: measured the same day,
# powerlevel10k rebuilds PROMPT in a precmd hook that runs after aterm's, and a
# 133;B appended to it never reached a single prompt.)
#
# The event is scheduled only where the loader's widget would not mark the line:
# what holds the front is neither that widget nor the one that reached it on the
# line before (`__aterm_line_init_via`: powerlevel10k alone, zsh-vi-mode alone,
# add-zle-hook-widget, an rc that wraps the loader's widget — none schedules it
# after their first prompt). Where it runs it changes nothing a command could
# read and prints nothing (measured on zsh 5.9): after a status of 0 or 1 it is an
# arithmetic command calling `__aterm_line_init_late` as a math function, which
# keeps `$?`, `$_` and `$pipestatus`; after any other status it is a call
# returning that status, and `$pipestatus` after a PIPELINE so ended reads `($?)`
# — every command sets it, and only a pipeline that reproduced it, forking once
# per element, could put it back. That call runs with PRINT_EXIT_VALUE off: zsh reports
# every top-level pipeline that ends non-zero while a user's rc has it on, so the
# call reported the status a second time, above the prompt (`zsh: exit 3`), and a
# function cannot turn it off for its caller (zsh puts it back at every return).
# The event turns it off and back with `${name::=value}` inside `[[ ]]` — which
# runs no command, so `$_` stays the user's — around the call, inside `{ }
# always { }`, whose status is the call's. The arithmetic command, and that
# block, are the first command of an `&&`/`||` list whose second never runs, so
# neither fires a ZERR trap or ERR_EXIT. A DEBUG trap sees their commands.
#
# Nothing here re-points `__aterm_orig_zle_line_init`, the loader's: re-pointing
# that alias made a loop (measured: ~250 marks and FUNCNEST errors at every
# prompt). A body from before the lead (a rollback, an older build adopting the
# shell through the pointer) finds the table exactly as the rc left it.

# The widget that held the front when the loader's widget last ran in a line —
# read, and forgotten, by the next precmd: a widget that stops reaching it costs
# one line its mark, not every line after.
typeset -g __aterm_line_init_via="${__aterm_line_init_via-}"
# Whether this prompt's line is marked (cleared at every precmd): a line reaches
# the loader's widget after the lead marked it, and a `vared` run by a command
# reaches it too.
typeset -gi __aterm_line_init_marked=0
# Whether the event put the lead in the table and it has not stepped aside yet.
typeset -gi __aterm_line_init_led="${__aterm_line_init_led:-0}"
# The event's own (see "THE LEAD"): the user's `$_` and PRINT_EXIT_VALUE while
# the call runs.
typeset -g __aterm_line_init_u="" __aterm_line_init_pev=""
# Whether the precmd turned the user's VERBOSE off for a pending event, which
# turns it back on (see `__aterm_line_init_wire`).
typeset -gi __aterm_line_init_hush="${__aterm_line_init_hush:-0}"

# The lead runs no `emulate`: the widget it hands the line to runs with the
# user's options, as it would without aterm, and with the `$?` the lead was
# reached with (`__aterm_line_init_pass`). A widget called with `zle <name>`
# keeps the caller's `$WIDGET`, so it is the table that says how it was reached.
__aterm_line_init_lead() {
    local -i __aterm_reached=$?
    if (( ! ${__aterm_line_init_marked:-0} )); then
        typeset -g __aterm_line_init_marked=1
        __aterm_mark_command_start
    fi
    if [[ "${widgets[zle-line-init]-}" == user:__aterm_line_init_lead ]]; then
        # zsh called it: zle-line-init goes back to what it took the front from,
        # which then runs as zle-line-init. zle reads no status from it, and a
        # non-zero one would be a second failed command to a user's ZERR trap,
        # after the widget's own.
        __aterm_line_init_aside
        [[ -n "${widgets[zle-line-init]-}" ]] || return 0
        __aterm_line_init_pass $__aterm_reached zle-line-init
        return 0
    fi
    # A wrapper put in front while it led calls it: it runs what it took the
    # front from, then steps aside.
    local -i __aterm_status=0
    if (( ${+widgets[__aterm_line_init_under]} )); then
        __aterm_line_init_pass $__aterm_reached __aterm_line_init_under
        __aterm_status=$?
    fi
    __aterm_line_init_aside
    return $__aterm_status
}

# The table as it is without aterm: every name that holds the lead — zle-line-init,
# and the alias a wrapper keeps of it when it wrapped zle-line-init while the lead
# held it (powerlevel10k does, while it expands its first prompt: after the event,
# before the line) — holds again the widget the lead took the front from, or
# nothing where it took the front from nothing. (Measured on zsh 5.9 with
# oh-my-zsh and powerlevel10k: a lead that put back only zle-line-init dropped
# powerlevel10k's widget from the table at the first line, and the alias went on
# naming the lead.)
__aterm_line_init_aside() {
    emulate -L zsh
    __aterm_line_init_led=0
    local __aterm_w
    for __aterm_w in ${(k)widgets[(R)user:__aterm_line_init_lead]}; do
        if (( ${+widgets[__aterm_line_init_under]} )); then
            zle -A __aterm_line_init_under "$__aterm_w"
        else
            zle -D "$__aterm_w"
        fi
    done
    (( ! ${+widgets[__aterm_line_init_under]} )) || zle -D __aterm_line_init_under
}

# The scheduled event: the lead takes the front, unless the loader's widget holds
# it now. Called as a command it returns the status it was handed; as a math
# function (see above) its value is not used. Where a precmd hook failed with an
# error, zsh skips the due events and zle runs them as it starts reading the
# line, after zle-line-init: there it marks the line itself (measured on zsh 5.9:
# a lead put in front then reached only a continuation line).
#
# add-zle-hook-widget's dispatcher (zsh's own hook mechanism) is never led: 133;B
# becomes one of its hooks instead — the loader's widget, added once where the
# dispatcher does not list it already. A plugin that reads the table while the
# lead holds the front chooses by what it finds there: powerlevel10k, expanding
# its first prompt, registers through add-zle-hook-widget only where the
# dispatcher holds the front, and otherwise wraps zle-line-init. In front of the
# dispatcher, the lead made it wrap, and the next add-zle-hook-widget — from a
# precmd hook — listed powerlevel10k's widget as a hook of the dispatcher it
# wraps: every line looped until FUNCNEST stopped it (measured on zsh 5.9 with
# oh-my-zsh, powerlevel10k and a hook added at the third prompt; bare zsh never
# builds that chain). Not traced (see THE TRACE): the event's own line is.
__aterm_line_init_late() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    if zle; then
        if (( ! __aterm_line_init_marked )); then
            __aterm_line_init_marked=1
            __aterm_mark_command_start
        fi
        return ${1:-0}
    fi
    local __aterm_now="${widgets[zle-line-init]-}"
    case "$__aterm_now" in
        (user:__aterm_zle_line_init|user:__aterm_line_init_lead) ;;
        (user:azhw:zle-line-init)
            local -a __aterm_hooks
            zstyle -a zle-line-init widgets __aterm_hooks
            if (( ! ${__aterm_hooks[(I)<->:(user:|)__aterm_zle_line_init]} &&
                  ${+functions[add-zle-hook-widget]} )); then
                zle -N __aterm_zle_line_init
                add-zle-hook-widget line-init __aterm_zle_line_init
            fi
            ;;
        (*)
            if [[ -n "$__aterm_now" ]]; then
                zle -A zle-line-init __aterm_line_init_under
            elif (( ${+widgets[__aterm_line_init_under]} )); then
                zle -D __aterm_line_init_under
            fi
            zle -N zle-line-init __aterm_line_init_lead
            __aterm_line_init_led=1
            ;;
    esac
    return ${1:-0}
}
functions -M __aterm_line_init_late 0 0

# From the precmd, handed the status the command before it left. One string
# compare where nothing needs the lead, and nothing at all without a line editor
# (`unsetopt zle`), where no line is ever initialised.
__aterm_line_init_wire() {
    [[ -o zle ]] || return 0
    # The user's VERBOSE, read before `emulate`: on, or off only because a
    # pending event of ours (removed below) was to turn it back on.
    local -i __aterm_verbose=${__aterm_line_init_hush:-0}
    [[ -o verbose ]] && __aterm_verbose=1
    emulate -L zsh
    __aterm_line_init_hush=0
    __aterm_line_init_marked=0
    local __aterm_via="$__aterm_line_init_via"
    __aterm_line_init_via=""
    # A lead whose line never came steps aside first.
    (( ! __aterm_line_init_led )) || __aterm_line_init_aside
    local __aterm_now="${widgets[zle-line-init]-}"
    [[ "$__aterm_now" == user:__aterm_zle_line_init ||
       ( -n "$__aterm_now" && "$__aterm_now" == "$__aterm_via" ) ]] && return 0
    # One still waiting (see `__aterm_line_init_late`) was written for an older
    # status.
    local -i __aterm_old=${zsh_scheduled_events[(I)*__aterm_line_init_late*]}
    (( __aterm_old )) && builtin sched -$__aterm_old
    local -i __aterm_s=${1:-0}
    local __aterm_event
    case $__aterm_s in
        0) __aterm_event='(( __aterm_line_init_late(), 1 )) || builtin :' ;;
        1) __aterm_event='(( __aterm_line_init_late(), 0 )) && builtin :' ;;
        *)
            __aterm_event='[[ -n ${__aterm_line_init_u::=$_}${__aterm_line_init_pev::=${options[printexitvalue]}}${options[printexitvalue]::=off} ]]'
            __aterm_event+=' && { __aterm_line_init_late '$__aterm_s' "$__aterm_line_init_u" && builtin :; }'
            __aterm_event+=' always { [[ -n ${options[printexitvalue]::=$__aterm_line_init_pev}${__aterm_line_init_u::=} ]]; } && builtin :'
            ;;
    esac
    (( ! __aterm_verbose )) ||
        __aterm_event='[[ -n ${options[verbose]::=on}${__aterm_line_init_hush::=0} ]] && '$__aterm_event
    if builtin sched +0 "$__aterm_event" 2>/dev/null; then
        __aterm_line_init_hush=$__aterm_verbose
    else
        __aterm_line_init_late
    fi
}

# ─── THE START MARK and THE WARM HOLD (docs/DESIGN-warm-spare-2026-10-16.md) ───
#
# Every fresh integrated zsh signs ONE start record at its first prompt hook,
# after every startup file: the final rc folder (`${ZDOTDIR:-$HOME}`, a ZDOTDIR
# the user's .zshenv set included), the hooks that ran ahead of aterm's, pending
# `sched` events, jobs and periodic hooks, then `cold` or `spare`. The host learns
# from the cold ones whether an rc generation is clean enough to run hidden in a
# warm spare. One fork-free write to /dev/tty, so an instant prompt that holds
# stdout cannot capture it; under `emulate -L zsh`, reading nothing it cannot read
# unset.
__aterm_start_mark() {
    emulate -L zsh
    local REPLY="" __aterm_s=0 __aterm_r=0
    local -i __aterm_pre=0 __aterm_i=${precmd_functions[(ie)__aterm_first_precmd]}
    (( ${+functions[precmd]} )) && __aterm_pre=1
    (( __aterm_i <= ${#precmd_functions} )) && (( __aterm_pre += __aterm_i - 1 ))
    # The sched module is loaded once anything scheduled: no event otherwise.
    [[ ${modules[zsh/sched]-} != loaded ]] || __aterm_s=${#zsh_scheduled_events}
    (( ${+PERIOD} && ( ${+functions[periodic]} || ${#periodic_functions} ) )) && __aterm_r=1
    # A folder with nothing to escape (the common case) skips the byte loop.
    REPLY="${ZDOTDIR:-$HOME}"
    [[ $REPLY != *[\\\;\ [:cntrl:]]* ]] || __aterm_encode_cmd "$REPLY"
    printf '\033]633;P;AtermStartRc=%s%s\a\033]633;P;AtermStartFlags=pre:%d,s:%d,j:%d,r:%d%s\a\033]633;P;AtermStart=%s%s\a' \
        "$REPLY" "$__aterm_id_suffix_str" $__aterm_pre $__aterm_s ${#jobstates} $__aterm_r \
        "$__aterm_id_suffix_str" "$1" "$__aterm_id_suffix_str" >/dev/tty 2>/dev/null
    return 0
}

# A WARM spare's hold: its startup files have run (unseen, told `run` at the
# parked hold), and it waits here, at the top of its first prompt hook, before any
# prompt work, clock, git segment or history-dependent expansion. A discard's
# hang-up writes no history (SAVEHIST is 0 while held, unless the rc made it
# readonly: then it is left as it is). Released by `go`, it
# carries `$SECONDS` on from the rc's run and leaves `__aterm_spare_reload` set
# when the history file moved meanwhile. Nothing here touches the tty, so what
# is typed before the release waits there for zle.
__aterm_spare_hold() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    local __aterm_rc=$SECONDS __aterm_hs0="" __aterm_go="" __aterm_ss=${SAVEHIST-}
    local -i __aterm_n=0 __aterm_ss_set=${+SAVEHIST}
    [[ ${(t)SAVEHIST} != *readonly* ]] || __aterm_ss_set=2
    local -A __aterm_h
    # The history stamp needs zsh/stat LOADED, which a session shell defers to
    # its first atpkg hook (`__aterm_atpkg_hook_stamp`): loaded here, in the
    # spare's idle time, or every release would read the history file again.
    (( __aterm_zstat_loaded )) || { zmodload -F zsh/stat b:zstat 2>/dev/null && __aterm_zstat_loaded=1 }
    if [[ -n ${HISTFILE-} ]] && (( __aterm_zstat_loaded )) &&
       zstat -H __aterm_h -- "$HISTFILE" 2>/dev/null; then
        __aterm_hs0="${__aterm_h[mtime]}:${__aterm_h[size]}:${__aterm_h[inode]}"
    fi
    (( __aterm_ss_set == 2 )) || SAVEHIST=0
    __aterm_start_mark spare
    while :; do
        if IFS= read -r __aterm_go < "$__aterm_spare_fifo"; then
            [[ $__aterm_go == go ]] && break
        elif (( ++__aterm_n >= 50 )) || [[ ! -p $__aterm_spare_fifo ]]; then
            exit 0
        fi
    done 2>/dev/null
    unset __aterm_spare_mode __aterm_spare_fifo
    case $__aterm_ss_set in
        1) SAVEHIST=$__aterm_ss ;;
        0) unset SAVEHIST ;;
    esac
    typeset -g __aterm_spare_reload=""
    if [[ -n ${HISTFILE-} ]]; then
        __aterm_h=()
        if [[ -z $__aterm_hs0 ]] || ! zstat -H __aterm_h -- "$HISTFILE" 2>/dev/null ||
           [[ "${__aterm_h[mtime]}:${__aterm_h[size]}:${__aterm_h[inode]}" != $__aterm_hs0 ]]; then
            __aterm_spare_reload=1
        fi
    fi
    # The hooks the rc put ahead of aterm's, in the order zsh ran them.
    local -i __aterm_i=${precmd_functions[(ie)__aterm_first_precmd]}
    typeset -ga __aterm_spare_hooks
    __aterm_spare_hooks=( ${precmd_functions[1,__aterm_i-1]} )
    (( ! ${+functions[precmd]} )) || __aterm_spare_hooks[1,0]=( precmd )
    [[ ${(t)SECONDS} == *readonly* ]] || SECONDS=$__aterm_rc
    return 0
}

# After the release, with the user's options: the history read again exactly as
# a cold start reads it when the file moved during the hold (the stale list is
# pushed under with SAVEHIST 0, so it is never written, and a SAVEHIST the rc
# unset is unset again after; not where HISTFILE or
# SAVEHIST is readonly, which `fc -p` and that 0 would assign), then the precmd hooks
# that ran ahead of aterm's at spare time run again, silenced, each with the `$?`
# the rc left — what an Enter on an empty line does — so direnv, mise and their
# like reflect the moment the tab was asked for.
__aterm_spare_rerun() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    if [[ -n ${__aterm_spare_reload-} && ${(t)HISTFILE-}${(t)SAVEHIST-} != *readonly* ]]; then
        local __aterm_hf=$HISTFILE __aterm_hz=$HISTSIZE __aterm_hss=${SAVEHIST-} __aterm_hss_set=${+SAVEHIST}
        SAVEHIST=0
        fc -p "$__aterm_hf" "$__aterm_hz" "$__aterm_hss"
        [[ $__aterm_hss_set == 1 ]] || unset SAVEHIST
    fi
    unset __aterm_spare_reload
    local __aterm_f
    for __aterm_f in "${__aterm_spare_hooks[@]}"; do
        if (( ! $1 )); then
            "$__aterm_f" >/dev/null 2>&1
        else
            __aterm_line_init_status "$1" || "$__aterm_f" >/dev/null 2>&1
        fi
    done
    unset __aterm_spare_hooks
    return 0
}

# ─── Deferred First-Precmd Setup ───
# Runs once on the very first precmd after the shell has fully initialized
# and processed SIGWINCH from the initial terminal resize. Signs the start
# record (a warm spare holds there first), handles the prompt override, then
# uninstalls itself. XTRACE off first (see THE TRACE), in the command that takes
# `$?`; no `emulate`, as the prompt override sets PROMPT_SUBST for the user and
# the hooks re-run after a hold run with the user's options.
__aterm_first_precmd() {
    local __aterm_st=$?${${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}:+}
    if [[ ${__aterm_spare_mode-} == warm ]]; then
        __aterm_spare_hold
        __aterm_spare_rerun "$__aterm_st"
    else
        __aterm_start_mark cold
    fi
    # Apply prompt override if requested
    # `${…:-}`: under a user's `setopt nounset` the bare form errored at every
    # prompt and this one-shot never uninstalled itself (review note 2026-09-16).
    if [[ -n "${ATERM_PROMPT_STYLE:-}" && "${ATERM_PROMPT_STYLE:-}" != "none" ]]; then
        __aterm_set_prompt
    fi

    # The reroute and agents directories, FIRST — unconditionally, once: /etc/zprofile
    # and ~/.zshrc have both run by now (see __aterm_reroute_path_front for why the
    # load-time assert above is not final), and a ~/.zshrc carrying atpkg's rc block
    # may have set $ATPKG_AGENTS itself, so $__aterm_managed_want is recomputed here.
    # From this prompt on, __aterm_precmd/__aterm_preexec keep it live (the "LIVE"
    # block above) without assigning $path unless the order is actually wrong.
    __aterm_reroute_path_front

    add-zsh-hook -d precmd __aterm_first_precmd
    # 0, not the status the rc left (see THE TRAMPOLINE).
    return 0
}

# The body ends in success: the loader moves its revision only when the source
# returned 0.
true

# @@ATERM-INTEGRATION-BODY-END@@

# ─── THE LOADER, continued: the wiring, once per shell ───
#
# OSC 133;B rides zle-line-init. A user's own zle-line-init is kept and chained —
# on a FRESH load only (review finding 2026-09-26). By an upgrade in place the
# user's rc has run, and a plugin may have wrapped our widget (`add-zle-hook-widget`
# keeps it as the first of its hooks; powerlevel10k and older
# zsh-syntax-highlighting wrap it too): re-wiring then aliased the plugin's widget
# — which calls ours — as `__aterm_orig_zle_line_init` and installed ours in
# front of it, which calls that alias. A loop: measured on zsh 5.9, ~250 133;B
# marks and "recursion limit exceeded" at every prompt of the upgraded shell.
# Nothing needs re-wiring here: a widget names its FUNCTION, so whatever chain the
# shell has reaches the new body's `__aterm_zle_line_init` as it stands — and
# where an rc took zle-line-init from it, the body's precmd puts 133;B back in
# front under its own names (see "THE LEAD"), never through this alias.
if (( __aterm_fresh_load )); then
    if (( ${+widgets[zle-line-init]} )) && [[ "${widgets[zle-line-init]}" != "user:__aterm_zle_line_init" ]]; then
        zle -A zle-line-init __aterm_orig_zle_line_init
    fi
    zle -N zle-line-init __aterm_zle_line_init
fi

# Install hooks using zsh hook arrays (`add-zsh-hook` never adds a name twice).
# __aterm_first_precmd is registered first so the prompt override lands before
# __aterm_precmd emits OSC 133;A (prompt start marker) — on a fresh load only: in
# an upgrade in place it has run and removed itself long ago.
if (( __aterm_fresh_load )); then
    add-zsh-hook precmd __aterm_first_precmd
fi
add-zsh-hook precmd __aterm_precmd
add-zsh-hook preexec __aterm_preexec

# The key bindings, ONCE per shell: a fresh load only (review finding
# 2026-09-26). This file runs before the user's own rc (zsh reads it from the
# ZDOTDIR .zshenv), so a binding of theirs for one of these keys wins; a body
# re-sourced live — a newer build's through the pointer, or an upgrade in place
# — binding them again at a prompt took those keys back from the user in every
# live tab at every update.
if (( __aterm_fresh_load )); then
    __aterm_setup_keybindings
fi

# An upgrade in place SIGNS its revision now, not at the next prompt: the typed
# line that upgrades a shell from before loaders goes on to relaunch its agent,
# which holds the terminal for hours, and until the shell's next prompt the host
# would name it `integration_rev=frozen` — and an update in between would carry
# no revision to point it by. (Signed with the key the shell holds — the one the
# typed line assigned before it sourced this file, so a degraded tab's engine
# verifies it; a fresh load signs at its first prompt.)
if (( ! __aterm_fresh_load )); then
    __aterm_mark_integration_rev
fi

# The user's XTRACE, as the load found it (see THE TRACE).
[[ $__aterm_load_xtrace != on ]] || builtin setopt xtrace
