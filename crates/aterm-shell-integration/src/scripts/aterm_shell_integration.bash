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
# - Live managed PATH: an already-running session shell resolves `claude`/`codex`
#   to atpkg's <prefix>/agents twins (and cargo/rustc to <prefix>/reroute) as soon
#   as atpkg lays them
# - Loader and body split: a running shell reloads a newer build's integration
#   body at its next prompt when the host updates the body pointer
# - TTY repair: restores terminal modes after a command that exits in raw mode
#
# Compatible with: bash 3.2+

# Only run in interactive shells
[[ $- != *i* ]] && return

# Suppress xtrace/verbose during integration load.
{ __aterm_load_flags=$-; set +xv; } 2>/dev/null

# ─── Trace suppression ───
#
# User XTRACE (`set -x`) or VERBOSE (`set -v`) would otherwise trace every line
# of the DEBUG-trap preexec and PROMPT_COMMAND hooks and leak `$__aterm_shell_nonce`
# into scrollback. bash 3.2 has no `local -`, and `set +x` is itself traced on
# stderr, so `{ set +xv; } 2>/dev/null` silences both without output, and
# `__aterm_flags_put` restores the user's flags on return (its `return` is traced
# only into silenced stderr). User code chained by aterm (scalar PROMPT_COMMAND
# or prior DEBUG trap) runs with the user's flags restored first. Under
# BASH_XTRACEFD (bash 4.1+), only the opening `local`/`set +xv` and closing
# `return` reach the trace fd, without the nonce.

# ─── Multiplexer boundary (screen / tmux) ───
#
# aterm hosts one session per PTY. Inside screen or tmux:
#  1. $ATERM_PARENT_SESSION_ID is inherited by pane shells, so flagless
#     `aterm ctl` self-location would target the outer session hosting the mux.
#  2. $ATERM_SHELL_INTEGRATION_INSTALLED is inherited too, so pane shells return
#     at the guard below without installing OSC 133/7 hooks (and screen/tmux drop
#     unknown OSCs anyway).
#
# When this script is sourced inside a mux (e.g., via a manual `.bashrc` source
# line) with $ATERM_SHELL_INTEGRATION_INSTALLED already set, mark the boundary
# with $ATERM_MUX and $ATERM_MUX_OUTER_SESSION_ID so `aterm ctl` can refuse
# implicit self-targeting. $ATERM_PARENT_SESSION_ID is left intact so nested
# aterm instances launched from a pane still provision parent capability edges.
# Because aterm injects bash via `--rcfile` (which screen/tmux pane shells do not
# receive), `aterm ctl` primarily detects mux entry by comparing live $TMUX/$STY
# against $ATERM_MUX_BASE recorded below at session startup.
__aterm_mux=""
if [[ -n "${TMUX:-}" ]]; then
    __aterm_mux="tmux"
elif [[ -n "${STY:-}" ]]; then
    __aterm_mux="screen"
else
    # tmux's default TERM is screen-256color, so check $TMUX/$STY first.
    case "${TERM:-}" in
        tmux|tmux-*|tmux.*)       __aterm_mux="tmux" ;;
        screen|screen-*|screen.*) __aterm_mux="screen" ;;
    esac
fi

# ATERM_MUX=0 (or off/no/none/false) disables the mux check.
case "${ATERM_MUX:-}" in 0|off|no|none|false) __aterm_mux="" ;; esac

# In-place upgrade: if this shell already holds both `$__aterm_shell_nonce` and
# the unexported `__aterm_prompt_command` function (`typed_rekey_with_loader`),
# allow re-sourcing in place while skipping one-time hook/package setup.
# Record the script that originally loaded this shell in `__aterm_bin_from` so
# `aterm_shell_integration_old_bin.bash` can strip any legacy `~/.aterm/bin` PATH entry.
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

# Skip if already loaded; mark the mux boundary if inherited across one.
if [[ -n "${ATERM_SHELL_INTEGRATION_INSTALLED:-}" ]]; then
    if [[ -n "$__aterm_mux" ]]; then
        export ATERM_MUX="$__aterm_mux"
        if [[ -n "${ATERM_PARENT_SESSION_ID:-}" ]]; then
            export ATERM_MUX_OUTER_SESSION_ID="$ATERM_PARENT_SESSION_ID"
            # Emit notice at most once per multiplexer session (keyed by $TMUX/$STY).
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
    case $__aterm_load_flags in *v*) set -v ;; esac
    case $__aterm_load_flags in *x*) set -x ;; esac
    { return; } 2>/dev/null
fi
export ATERM_SHELL_INTEGRATION_INSTALLED=1
# Fallback for hosts that do not pre-populate ATERM_MUX_BASE at spawn: record the
# mux environment this session shell started in so `aterm ctl` can detect panes.
: "${ATERM_MUX_BASE:="${TMUX-}|${STY-}"}"
export ATERM_MUX_BASE
# Clear any ATERM_MUX inherited from a mux pane that launched a new aterm window.
unset ATERM_MUX ATERM_MUX_OUTER_SESSION_ID __aterm_mux

# ─── Loader ───
#
# The loader runs once per shell: it captures and scrubs the nonce and host
# pointer paths, defines the PROMPT_COMMAND and PS0 trampolines, and wires
# traps/prompts after the body marker. Between the BODY markers is the reloadable
# body (`aterm_shell_integration_body.bash`), re-sourced inside the trampoline
# with `__aterm_in_prompt_cmd=1` when the host writes a new folder address to
# `$__aterm_body_pointer`.

# Capture the capability nonce into a shell-local and unset ATERM_SHELL_NONCE
# immediately so child processes cannot inherit the 64-hex secret.
# When unset (or during an in-place upgrade after the env var was scrubbed),
# preserve any existing `$__aterm_shell_nonce` or fall back to unnonced marks.
__aterm_shell_nonce="${ATERM_SHELL_NONCE:-${__aterm_shell_nonce:-}}"
unset ATERM_SHELL_NONCE

# Precompute the `;id=<hex>` suffix so per-prompt/preexec emitters expand a
# parameter without forking `$(__aterm_id_suffix)`.
__aterm_id_suffix_str=""
if [[ -n "$__aterm_shell_nonce" ]]; then
    __aterm_id_suffix_str=";id=${__aterm_shell_nonce}"
fi

# Per-session re-key file path (in the host's 0700 control dir), captured and
# scrubbed from the environment. Checked once per prompt via `[[ -f ]]`.
__aterm_rekey_path="${ATERM_REKEY_PATH:-${__aterm_rekey_path:-}}"
unset ATERM_REKEY_PATH

# Per-session body pointer path, captured and scrubbed like the re-key path.
__aterm_body_pointer="${ATERM_INTEGRATION_POINTER:-${__aterm_body_pointer:-}}"
unset ATERM_INTEGRATION_POINTER

# Resolve the content-addressed folder (`<root>/<16-hex>/`) from `BASH_SOURCE`.
# Hand-installed copies outside a 16-hex folder leave `__aterm_body_rev` empty.
__aterm_si_dir="${BASH_SOURCE[0]%/*}"
__aterm_si_root="${__aterm_si_dir%/*}"
__aterm_body_rev="${__aterm_si_dir##*/}"
if [[ ${#__aterm_body_rev} -ne 16 || "$__aterm_body_rev" == *[!0-9a-f]* ]]; then
    __aterm_body_rev=""
fi

# Fresh loads never add `~/.aterm/bin` to PATH, so skip the legacy PATH cleanup.
if (( __aterm_fresh_load )); then
    __aterm_old_bin_off=1
fi

# Source package shell hooks once per shell.
if (( __aterm_fresh_load )) && [ -d "$HOME/.aterm/shell.d" ]; then
    for f in "$HOME/.aterm/shell.d"/*.bash "$HOME/.aterm/shell.d"/*.sh; do
        [ -f "$f" ] && . "$f"
    done
fi

# Save the user's PROMPT_COMMAND before modifying it (once per shell).
# Check index 1 without a subshell (`declare -p` costs ~1.5 ms on bash 3.2):
# a single-element array works via the scalar path because replacing element 0
# and evaling the saved value still preserves it.
if (( __aterm_fresh_load )); then
    __aterm_prompt_cmd_is_array=0
    if [[ -n ${PROMPT_COMMAND[1]+x} ]]; then
        __aterm_prompt_cmd_is_array=1
    fi
    __aterm_original_prompt_command="${PROMPT_COMMAND:-}"
fi

# Check for a newer body pointer at the start of PROMPT_COMMAND.
__aterm_body_check() {
    [[ -n "$__aterm_body_pointer" && -f "$__aterm_body_pointer" ]] || return 0
    local __aterm_addr="" __aterm_file=""
    { IFS= read -r __aterm_addr < "$__aterm_body_pointer"; } 2>/dev/null
    command rm -f -- "$__aterm_body_pointer"
    [[ ${#__aterm_addr} -eq 16 && "$__aterm_addr" != *[!0-9a-f]* ]] || return 0
    [ "$__aterm_addr" != "$__aterm_body_rev" ] || return 0
    __aterm_file="$__aterm_si_root/$__aterm_addr/aterm_shell_integration_body.bash"
    [[ -f "$__aterm_file" && -O "$__aterm_file" ]] || return 0
    . "$__aterm_file" && __aterm_body_rev="$__aterm_addr"
    return 0
}

# Permanent PROMPT_COMMAND entry point. Captures `$?`, `$_`, and `$-` while
# silencing xtrace/verbose, raises `__aterm_in_prompt_cmd` before checking for
# a body reload so the DEBUG trap ignores sourced body lines, and restores flags.
__aterm_prompt_command() {
    { local __aterm_status=$? __aterm_lastarg=$_ __aterm_flags=$-; set +xv; } 2>/dev/null
    __aterm_in_prompt_cmd=1
    __aterm_body_check
    __aterm_body_prompt_command "$__aterm_status" "$__aterm_flags" "$__aterm_lastarg"
    case $__aterm_flags in *v*) set -v ;; esac
    case $__aterm_flags in *x*) set -x ;; esac
}

# Permanent PS0 entry point (bash 5.3+). Guards `__aterm_body_ps0` with
# `declare -F` so bodies without it do not fail on PS0 expansion.
__aterm_ps0() {
    { local __aterm_flags=$-; set +xv; } 2>/dev/null
    if declare -F __aterm_body_ps0 >/dev/null; then
        __aterm_body_ps0 || :
    fi
    case $__aterm_flags in *v*) set -v ;; esac
    case $__aterm_flags in *x*) set -x ;; esac
}

# @@ATERM-INTEGRATION-BODY-BEGIN@@ — everything from here to the END marker is re-sourced live

# Suppress xtrace/verbose while re-sourcing the body (restored at the end).
{ __aterm_body_flags=$-; set +xv; } 2>/dev/null

# Enable agy's fast prompt editor by default (including in adopted shells when
# this body is live-reloaded at the next prompt) so U+202F narrow no-break
# spaces in macOS screenshot filenames do not desynchronize Bubble Tea's
# textarea visual cursor.
: "${AGY_CLI_FAST_PROMPT_EDITOR=true}"
export AGY_CLI_FAST_PROMPT_EDITOR

# Restore user's XTRACE and VERBOSE from `$1` (captured `$-`), returning `$2`
# (default 0) and setting `$_` to the last argument so chained user code sees
# the original `$?` and `$_`.
__aterm_flags_put() {
    case $1 in *v*) set -v ;; esac
    case $1 in *x*) set -x ;; esac
    { return "${2:-0}"; } 2>/dev/null
}

# Remove any legacy `~/.aterm/bin` PATH entry once per shell on in-place upgrade
# (`__aterm_old_bin_off` is already set on fresh loads). Kept in a separate file
# so fresh shells do not pay to parse it.
if [[ -z "${__aterm_old_bin_off-}" ]]; then
    __aterm_old_bin_off=1
    __aterm_old_bin_file="${BASH_SOURCE[0]%/*}/aterm_shell_integration_old_bin.bash"
    if [[ -f "$__aterm_old_bin_file" && -O "$__aterm_old_bin_file" ]]; then
        . "$__aterm_old_bin_file"
    fi
    unset __aterm_old_bin_file __aterm_bin_from
fi

# ─── Managed PATH directories ($ATERM_REROUTE_DIR and $ATPKG_AGENTS) ───
#
# Move `$ATPKG_AGENTS` and `$ATERM_REROUTE_DIR` to the front of PATH (reroute
# first, then agents) so user rc files (`. ~/.cargo/env`, homebrew, etc.) cannot
# shadow the session's Rust reroute stubs or atpkg's managed agent twins.
#
# Framing PATH as `:$PATH:` with quoted parameter expansion avoids forks and IFS
# changes, prevents prefix collisions (`/opt/x` vs `/opt/xy`), collapses adjacent
# duplicates, and preserves empty PATH entries (`:`).
__aterm_path_front() {
    # `LC_ALL=C` avoids quadratic wide-char conversion in bash 3.2 pattern matching
    # (0.22 ms vs 1.06 ms per call on a 13-entry PATH).
    local LC_ALL=C
    local __aterm_dir="$1" __aterm_p=":$PATH:" __aterm_q
    # POSIX `[ = ]` equality avoids infinite loops under `shopt -s nocasematch`
    # when PATH contains a case-variant spelling of `$__aterm_dir`.
    while :; do
        __aterm_q="${__aterm_p//":$__aterm_dir:"/:}"
        [ "$__aterm_q" = "$__aterm_p" ] && break
        __aterm_p="$__aterm_q"
    done
    # Only a framed remainder of `:` means PATH is otherwise empty (preserving a
    # sole empty entry `::`).
    case "$__aterm_p" in
        :) export PATH="$__aterm_dir" ;;
        *)
            __aterm_p="${__aterm_p#:}"
            __aterm_p="${__aterm_p%:}"
            export PATH="$__aterm_dir:$__aterm_p"
            ;;
    esac
}
# Record the expected PATH prefix in `$__aterm_managed_want` and whether each
# managed dir existed at the last check (`__aterm_managed_agents_on` /
# `__aterm_managed_reroute_on`) so the per-prompt hot path avoids `-d` stats in
# the steady state.
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

# ─── Live managed PATH updates ───
#
# Re-assert managed PATH directories on every PROMPT_COMMAND and DEBUG-trap
# preexec (builtin-only, no subshells or external commands) so already-open tabs
# pick up `<prefix>/agents` and `<prefix>/reroute` as soon as atpkg creates or
# updates them:
#  1. Re-source `~/.aterm/shell.d/00-atpkg.bash` when its on-disk content changes
#     (compared via builtin `read`, ~108 us on bash 3.2).
#  2. Re-probe any managed dir that was absent when last checked.
#  3. Re-front managed dirs only when `$PATH` no longer starts with
#     `$__aterm_managed_want` (avoiding unnecessary command-hash flushes).
#  4. Run `hash -d claude codex agy muse` so newly installed agent twins are not
#     shadowed by previously hashed foreign binaries.
__aterm_atpkg_hook="$HOME/.aterm/shell.d/00-atpkg.bash"
__aterm_managed_live=0
if [[ -n "${ATERM_CHILD:-}" || -n "${ATERM_SESSION_ID:-}" ]]; then
    __aterm_managed_live=1
fi

# Read the hook's contents into `$__aterm_atpkg_hook_now` (empty if absent)
# without clobbering `$REPLY` or triggering `set -e` / `ERR` traps on bash 5 when
# the file does not exist (`|| :`).
__aterm_atpkg_hook_now=""
__aterm_atpkg_hook_read() {
    __aterm_atpkg_hook_now=""
    local __aterm_line=""
    while IFS= read -r __aterm_line || [[ -n "$__aterm_line" ]]; do
        __aterm_atpkg_hook_now+="$__aterm_line"$'\n'
    done 2>/dev/null < "$__aterm_atpkg_hook" || :
    return 0
}
# Record the hook contents sourced at initial load (once per shell across live
# body reloads).
if [[ -z "${__aterm_atpkg_hook_seen+x}" ]]; then
    __aterm_atpkg_hook_seen=""
    if (( __aterm_managed_live )); then
        __aterm_atpkg_hook_read
        __aterm_atpkg_hook_seen="$__aterm_atpkg_hook_now"
    fi
fi

# Derive `$ATERM_REROUTE_DIR` from `<dir of $ATPKG_AGENTS>/reroute` when unset
# and passthrough is not engaged.
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
if (( __aterm_managed_live )) && __aterm_managed_derive_reroute; then
    __aterm_reroute_path_front
fi

# Per-prompt and per-command managed PATH check (builtin-only).
__aterm_managed_path_live() {
    (( __aterm_managed_live )) || return 0
    # 1. Re-source hook if its content changed (`[ != ]` avoids pattern/nocasematch).
    #    Guard non-zero statuses with `|| :` so `set -e` / `set -E` are unaffected.
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
    # 2. Re-probe any managed dir that was previously absent.
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
    # 3. Restore PATH order if `$PATH` no longer starts with `$__aterm_managed_want`.
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
    # 4. Evict cached paths for managed agent binaries so new twins resolve immediately.
    if (( __aterm_managed_agents_on )); then
        hash -d claude codex agy muse 2>/dev/null || :
    fi
    return 0
}

# Preserve command tracking state across live body reloads inside PROMPT_COMMAND.
__aterm_last_command="${__aterm_last_command:-}"
__aterm_line_t0="${__aterm_line_t0:-}"
# Guard: suppress DEBUG trap capture during PROMPT_COMMAND execution.
__aterm_in_prompt_cmd="${__aterm_in_prompt_cmd:-0}"

# ─── Terminal stream routing for OSC marks ───
#
# For compound commands with redirections (`{ ...; } > f`, `for ...; done > f`),
# bash applies the redirection before firing the DEBUG trap for the first simple
# command inside, so stdout points at `f`. Route marks to stdout when it is a
# tty (or when the shell started on pipes in tests/embedders), otherwise to
# stderr if it is a tty, or `/dev/tty`.
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

# Check for a new nonce written to `$__aterm_rekey_path`.
__aterm_rekey_check() {
    [[ -n "$__aterm_rekey_path" && -f "$__aterm_rekey_path" ]] || return 0
    local key=""
    { IFS= read -r key < "$__aterm_rekey_path"; } 2>/dev/null
    command rm -f -- "$__aterm_rekey_path"
    [[ ${#key} -eq 64 && "$key" != *[!0-9a-f]* ]] || return 0
    __aterm_shell_nonce="$key"
    __aterm_id_suffix_str=";id=${key}"
}

# Capability-nonce suffix helper for external callers; internal emitters use
# `$__aterm_id_suffix_str` directly to avoid a subshell fork.
__aterm_id_suffix() {
    { local __aterm_flags=$-; set +xv; } 2>/dev/null
    if [[ -n "$__aterm_shell_nonce" ]]; then
        printf ';id=%s' "$__aterm_shell_nonce"
    fi
    __aterm_flags_put "$__aterm_flags"
}

# Percent-encode `$1` for `file://` URIs (RFC 3986) into `REPLY` without
# subshell forks (`printf -v`). `LC_ALL=C` splits multi-byte UTF-8 into raw bytes.
__aterm_urlencode() {
    local LC_ALL=C
    if [[ "$1" != *[^a-zA-Z0-9_.~/-]* ]]; then
        REPLY="$1"
        return
    fi
    local string="$1" i char out="" byte
    for ((i = 0; i < ${#string}; i++)); do
        char="${string:i:1}"
        case "$char" in
            [a-zA-Z0-9_.~/-]) out+="$char" ;;
            # Mask with `0xFF` because bash `printf '%d' "'<byte>"` sign-extends
            # bytes >= 0x80.
            *) printf -v byte '%d' "'$char"
               printf -v byte '%%%02X' "$(( byte & 0xFF ))"
               out+="$byte" ;;
        esac
    done
    REPLY="$out"
}

# Under Git Bash / MSYS2 / Cygwin, translate POSIX `$PWD` (`/c/Users//x`,
# `/cygdrive/c/x`) to RFC 8089 Windows drive form (`/C:/Users//x`) for OSC 7.
case "$OSTYPE" in
    msys* | cygwin*) __aterm_win_posix=1 ;;
    *) __aterm_win_posix= ;;
esac
__aterm_win_cwd_from=
__aterm_win_cwd_to=

# ─── Full command line capture ───
#
# `$BASH_COMMAND` in the DEBUG trap holds only the current simple command (with
# aliases expanded). To record the full line as typed, capture `$HISTCMD` at the
# end of the prompt and read `history 1` on the first trap when the line was
# saved to history; fall back to `$BASH_COMMAND` when history was skipped
# (`ignorespace`, `ignoredups`, `set +o history`) or on pre-5.3 Windows POSIX
# shells where a `$(...)` fork costs ~16 ms (bash 5.3+ uses `${ ...; }` in-process).
#
# No builtin puts a history entry in a variable, and bash 5.3 keeps a `${ ...; }`
# substitution's output in a file it makes and unlinks at once (a memfd on Linux)
# in `$TMPDIR`, else in the system folder it found first and remembers (`/tmp`).
# Git Bash's `/tmp` is MSYS2's shared `usertemp` mount, the TEMP of the
# session's first MSYS2 process, and where that folder is gone bash cannot make
# the file: the read fails (`function_substitute: cannot open anonymous file`)
# and the line runs with no block. So the read names a folder it saw just now,
# `$TMPDIR` or else `/tmp`, and forks `$(...)`, which needs none, only where
# neither is there. On Linux bash asks for a memfd before any folder
# (`anonopen`), so the read there checks none: a check would fork every line
# where `/tmp` is read-only and `$TMPDIR` unset (a read-only container), and a
# read that still fails does so inside PS0's `${| ...; }` or the DEBUG trap,
# where the line runs on.
__aterm_hist_mark="${__aterm_hist_mark:-}"
if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 3) )); then
    # Inside eval so older bash versions do not fail parsing `${ ...; }`.
    if [[ $OSTYPE == linux* ]]; then
        eval '__aterm_history_line() { REPLY=${ HISTTIMEFORMAT= builtin history 1; }; }'
    else
        eval '__aterm_history_line() {
            if [[ -d ${TMPDIR-} && -w ${TMPDIR-} ]]; then
                REPLY=${ HISTTIMEFORMAT= builtin history 1; }
            elif [[ -d /tmp && -w /tmp ]]; then
                local TMPDIR=/tmp
                REPLY=${ HISTTIMEFORMAT= builtin history 1; }
            else
                REPLY=$(HISTTIMEFORMAT= builtin history 1)
            fi
        }'
    fi
elif [[ -n "$__aterm_win_posix" ]]; then
    __aterm_history_line() { return 1; }
else
    __aterm_history_line() { REPLY=$(HISTTIMEFORMAT= builtin history 1); }
fi

# On bash < 5, `$HISTCMD` reads 1 in traps regardless of whether the line was
# saved (bash 3.2.57), so also snapshot the last history entry at the prompt and
# require it to differ on the first trap.
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

# Return the typed command line in `REPLY`: the history entry (with its index
# and edit `*` stripped) when bash saved the line, or `$BASH_COMMAND` otherwise.
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

# ─── Array PROMPT_COMMAND trailer (`__aterm_prompt_end`) ───
#
# On bash 5.1+, each element of an array `PROMPT_COMMAND` runs as a top-level
# command after our trampoline at index 0. Keep `__aterm_in_prompt_cmd=1` across
# the user's elements and lower it (while taking `$HISTCMD`) in `__aterm_prompt_end`
# at the end of the array so multi-command elements do not trigger the DEBUG trap
# or advance `$HISTCMD` after the mark is taken.
__aterm_pc_arrays=0
if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 1) )); then
    __aterm_pc_arrays=1
    # Check whether `PROMPT_COMMAND` is an array with >1 element (`set -u` safe;
    # inside `eval` because bash 3.2 lacks `${X@a}`).
    eval '__aterm_pc_more() {
        [[ -n ${PROMPT_COMMAND[@]+x} && ${PROMPT_COMMAND@a} == *a* ]] &&
            (( ${#PROMPT_COMMAND[@]} > 1 ))
    }'
else
    # bash < 5.1 only executes index 0 of an array `PROMPT_COMMAND`.
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

# Place `__aterm_prompt_end` at the end of the `PROMPT_COMMAND` array without
# shifting the indices of user elements.
__aterm_prompt_end_last() {
    local __aterm_i
    for __aterm_i in "${!PROMPT_COMMAND[@]}"; do
        if [[ "${PROMPT_COMMAND[__aterm_i]}" == __aterm_prompt_end ]]; then
            unset 'PROMPT_COMMAND[__aterm_i]'
        fi
    done
    PROMPT_COMMAND+=(__aterm_prompt_end)
}

# Convert POSIX path `$1` to its Windows form in `__aterm_win_cwd_to` (cached by
# `__aterm_win_cwd_from`).
__aterm_win_cwd() {
    local LC_ALL=C p="$1" d w
    [[ "$p" == "$__aterm_win_cwd_from" ]] && return
    __aterm_win_cwd_from="$p"
    # Fast path for MSYS `/x/...` drive mounts without forking `cygpath`.
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
    # Resolve `/usr/bin`, `/tmp`, or Cygwin `/cygdrive/c` via `cygpath -m`.
    w=$(cygpath -m -- "$p" 2>/dev/null)
    case "$w" in
        [a-zA-Z]:*) __aterm_win_cwd_to="/$w" ;;
        //*) __aterm_win_cwd_to="$w" ;;
        *) __aterm_win_cwd_to="$p" ;;
    esac
}

# Report current working directory (signed OSC 633;P;AtermCwd + plain OSC 7).
__aterm_report_cwd() {
    local cwd="$PWD"
    if [[ -n "$__aterm_win_posix" ]]; then
        __aterm_win_cwd "$cwd"
        cwd="$__aterm_win_cwd_to"
    fi
    local REPLY
    __aterm_urlencode "$cwd"
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
__aterm_mark_exec_finish() {
    __aterm_osc "133;D;${1}${__aterm_id_suffix_str}"
    __aterm_last_command=""
}

# Backslash-hex encode semicolons, backslashes, and bytes <= 0x20 for OSC 633;E
# into `REPLY` without subshell forks.
__aterm_encode_cmd() {
    local LC_ALL=C
    local string="$1" i char result="" hex
    for ((i = 0; i < ${#string}; i++)); do
        char="${string:i:1}"
        case "$char" in
            \\) result+="\\\\" ;;
            \;) result+="\\x3b" ;;
            ' ') result+="\\x20" ;;
            [[:cntrl:]]) printf -v hex '%d' "'$char"
                         printf -v hex '\\x%02x' "$(( hex & 0xFF ))"
                         result+="$hex" ;;
            *) result+="$char" ;;
        esac
    done
    REPLY="$result"
}

# Emit the active integration body's content-addressed folder revision (633;P).
__aterm_mark_integration_rev() {
    [[ -n "$__aterm_body_rev" ]] || return 0
    __aterm_osc "633;P;AtermIntegration=${__aterm_body_rev}${__aterm_id_suffix_str}"
}

# DEBUG trap entry point (`trap '__aterm_preexec "$_"' DEBUG`).
# Passing `"$_"` as the last argument preserves the user's `$_` across the trap.
# Returns 0 so `extdebug` does not skip the user's command. Only inspects
# `FUNCNAME[1]` when `functrace`/`extdebug` (`T` in `$-`) is active, so wrapper
# functions installed by frameworks (e.g. starship on bash < 4.4) do not suppress
# command capture.
__aterm_preexec() {
    { local __aterm_flags=$-; set +xv; } 2>/dev/null
    local __aterm_in_fn=""
    case $__aterm_flags in *T*) __aterm_in_fn=${FUNCNAME[1]-} ;; esac
    if [[ -n "$__aterm_prev_debug_handler" || -z "$__aterm_last_command" ]]; then
        __aterm_preexec_run "${1-}" "$__aterm_in_fn"
    fi
    case $__aterm_flags in *[xv]*) __aterm_flags_put "$__aterm_flags" ;; esac
}

__aterm_preexec_run() {
    # Chain any pre-existing DEBUG trap handler with the user's flags and `$_`.
    if [[ -n "$__aterm_prev_debug_handler" ]]; then
        __aterm_flags_put "$__aterm_flags" 0 "$1" && eval "$__aterm_prev_debug_handler"
        { __aterm_flags=$-; set +xv; } 2>/dev/null
    fi

    # Skip commands run inside PROMPT_COMMAND (scalar or array elements), our
    # own helpers, or inside a function under `set -T` / `extdebug` (`$2`).
    (( __aterm_in_prompt_cmd )) && return
    local __aterm_pc_elem
    for __aterm_pc_elem in "${PROMPT_COMMAND[@]}"; do
        [[ "$BASH_COMMAND" == "$__aterm_pc_elem" ]] && return
    done
    [[ "$BASH_COMMAND" == "__aterm_"* ]] && return
    [[ -n "$2" ]] && return

    # Capture only the first simple command of the line.
    if [[ -z "$__aterm_last_command" ]]; then
        local REPLY
        __aterm_typed_line
        __aterm_line_start "$REPLY"
    fi
}

# Emit command-start marks (633;E, OSC 0 title, and 133;C) for command line `$1`.
__aterm_line_start() {
    __aterm_managed_path_live
    __aterm_last_command="$1"
    local REPLY
    __aterm_encode_cmd "$__aterm_last_command"
    __aterm_osc "633;E;${REPLY}${__aterm_id_suffix_str}"
    # Truncate running-command title to 256 chars with a trailing ellipsis so the
    # tab title middle-truncator recognizes that the end was already clipped.
    if [[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]; then
        local cmd="$__aterm_last_command"
        (( ${#cmd} <= 256 )) || cmd="${cmd:0:255}…"
        __aterm_osc "0;${cmd//[[:cntrl:]]/}"
    fi
    if [[ -n "${EPOCHREALTIME-}" ]]; then
        __aterm_line_t0=${EPOCHREALTIME//[!0-9]/}
    else
        __aterm_line_t0=s$SECONDS
    fi
    __aterm_mark_exec_start
}

# PS0 hook (bash 5.3+ via `${| __aterm_ps0; }`): fires before a line runs,
# capturing lines that start with a subshell (`(exit 3)`) where the top-level
# DEBUG trap never fires. Before the line runs, `$HISTCMD` is `mark + 1` if bash
# saved the line to history. `${| ...; }` expands to `$REPLY` and keeps no
# output, so bash makes no file for it. A loader of v0.107.0-v0.108.0 wrote the
# hook as `${ ...; }`, whose output bash keeps in a file (`__aterm_history_line`
# says where): where that folder had gone, bash dropped every line typed and ran
# none of it. A running shell's PS0 takes the new form from this body.
if [[ ${PS0-} == *'${ __aterm_ps0; }'* ]]; then
    PS0=${PS0/'${ __aterm_ps0; }'/'${| __aterm_ps0; }'}
fi
__aterm_body_ps0() {
    (( __aterm_in_prompt_cmd )) && return 0
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

# ─── TTY mode repair ───
#
# bash restores tty settings when a foreground job dies by signal, but adopts
# the job's tty state on normal exit (or when a full-screen app is killed via
# `taskkill /F` under MSYS2, which reports exit 0). After any failed command or
# any command that ran for >= 1 s, check `stty -a` and restore
# `isig icanon iexten echo icrnl opost` if `-isig` is set (leaving `ixon` alone).
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
    __aterm_tty_state=" ${__aterm_tty_state//$'\n'/ } "
    case "$__aterm_tty_state" in
        *" -isig "*) command stty isig icanon iexten echo icrnl opost 2>/dev/null ;;
    esac
    return 0
}

# ─── Start record and warm-spare hold ───
#
# Emit `AtermStartRc`, `AtermStartFlags`, and `AtermStart` (`cold` or `spare`)
# once at the first prompt after startup files finish.
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

# Hold a warm spare at its first prompt until the host writes `go` to its FIFO.
# Unsets `HISTFILE` while held so a discarded spare writes no history, and
# reloads history on release unless the startup files populated their own list.
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
            [[ -z ${HISTSIZE+x} ]] || eval 'HISTSIZE=$HISTSIZE' 2>/dev/null || :
            [[ -z ${HISTFILESIZE+x} ]] || eval 'HISTFILESIZE=$HISTFILESIZE' 2>/dev/null || :
            history -c
            history -r
        fi
    fi
    SECONDS=$__aterm_rc
    return 0
}

# Body PROMPT_COMMAND handler called by `__aterm_prompt_command`.
__aterm_body_prompt_command() {
    { [[ -n ${2+x} ]] || local __aterm_flags=$-; set +xv; } 2>/dev/null
    local last_status=$1 __aterm_lastarg=${3-}
    __aterm_last_exit=$last_status
    __aterm_in_prompt_cmd=1
    __aterm_hist_mark=
    __aterm_hist_seen=
    __aterm_prompt_end_armed=0

    if [[ -n "${__aterm_start_pending:-}" ]]; then
        unset __aterm_start_pending
        if [[ "${__aterm_spare_mode:-}" == warm ]]; then
            __aterm_spare_hold
        else
            __aterm_start_mark cold
        fi
    fi

    __aterm_rekey_check

    [[ "$last_status" == 0 ]] && ! __aterm_line_ran_a_second || __aterm_tty_repair
    __aterm_line_t0=

    __aterm_managed_path_live

    if [[ -n "$__aterm_last_command" ]]; then
        __aterm_mark_exec_finish $last_status
    fi

    __aterm_report_cwd

    # Set tab title to abbreviated CWD (OSC 0), stripping control characters.
    local __aterm_tab_title="$PWD"
    if [[ "$PWD" == "$HOME" ]]; then
        __aterm_tab_title="~"
    elif [[ "$PWD" == "$HOME"/* ]]; then
        __aterm_tab_title="~${PWD#$HOME}"
    fi
    if [[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]; then
        __aterm_osc "0;${__aterm_tab_title//[[:cntrl:]]/}"
    fi

    __aterm_mark_integration_rev
    __aterm_mark_prompt_start

    # Run scalar original PROMPT_COMMAND with original `$?`, `$_`, and flags,
    # passing non-zero status via `||` so `set -e` and `ERR` traps do not fire.
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

    # One-shot prompt setup after original PROMPT_COMMAND so it survives
    # frameworks (starship, oh-my-bash) that set PS1 at init.
    if [[ -n "${__aterm_pending_prompt_setup:-}" ]]; then
        __aterm_set_prompt
        unset __aterm_pending_prompt_setup
    fi

    # Append OSC 133;B to PS1 so it fires after prompt text renders.
    # Strip any prior 133;B suffixes by length using quoted globs (avoiding
    # `${PS1%...}` and regex bugs on non-UTF-8 bytes in PS1).
    if [[ -z "${__aterm_prompt_has_mark_b:-}" ]]; then
        local __aterm_b_suffix=""
        [[ -n "$__aterm_shell_nonce" ]] && __aterm_b_suffix=";id=${__aterm_shell_nonce};aterm_input=start"
        local __aterm_b="\[\033]133;B${__aterm_b_suffix}\a\]"
        local __aterm_b_lead='\[\033]133;B' __aterm_b_end='\a\]'
        local __aterm_b_word=';aterm_input=start'
        local __aterm_b_tail __aterm_b_id
        while [[ "$PS1" == *"$__aterm_b_lead"* ]]; do
            __aterm_b_tail="${PS1##*"$__aterm_b_lead"}"
            if [[ "$__aterm_b_tail" != "$__aterm_b_end" ]]; then
                [[ "$__aterm_b_tail" == ";id="?*"$__aterm_b_end" ]] || break
                __aterm_b_id="${__aterm_b_tail:4:${#__aterm_b_tail}-4-${#__aterm_b_end}}"
                [[ "$__aterm_b_id" == *"$__aterm_b_word" ]] &&
                    __aterm_b_id="${__aterm_b_id:0:${#__aterm_b_id}-${#__aterm_b_word}}"
                [[ -n "$__aterm_b_id" && "$__aterm_b_id" != *[!0-9a-fA-F]* ]] || break
            fi
            PS1="${PS1:0:${#PS1}-${#__aterm_b_lead}-${#__aterm_b_tail}}"
        done
        PS1="${PS1}${__aterm_b}"
    fi

    # Unexport PS1 so child processes cannot inherit the embedded nonce.
    export -n PS1

    # Arm `__aterm_prompt_end` if array elements remain, or capture `$HISTCMD` now.
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
    local mark_b_id=""
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

# Error-aware prompt character (`\001`/`\002` raw markers for `$()` inside PS1).
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

# Initialize prompt setup flags once per shell (`set -u` safe).
if [[ -z "${__aterm_prompt_has_mark_b+x}" ]]; then
    __aterm_pending_prompt_setup=""
    __aterm_prompt_has_mark_b=""
    if [[ -n "${ATERM_PROMPT_STYLE:-}" && "${ATERM_PROMPT_STYLE:-}" != "none" ]]; then
        __aterm_pending_prompt_setup=1
    fi
fi

# ─── Key Bindings ───
# Bind xterm-style modifier+arrow sequences for readline.
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

__aterm_flags_put "$__aterm_body_flags"

# @@ATERM-INTEGRATION-BODY-END@@

# ─── Loader wiring (once per shell) ───
#
# On an in-place upgrade, emit the new integration revision immediately before
# relaunching an agent.
if (( ! __aterm_fresh_load )); then
    __aterm_mark_integration_rev
fi

if (( __aterm_fresh_load )); then
    __aterm_setup_keybindings 2>/dev/null
fi

# Capture the DEBUG trap set by user startup files for chaining.
# Because bash hides the DEBUG trap inside a sourced file, our wrappers hand it
# over in `<re-key path>.trap` or `$__aterm_rc_debug_trap` (`bash_debug_trap_handover!`);
# fall back to `trap -p DEBUG` when sourced manually.
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

if (( __aterm_fresh_load )); then
    __aterm_start_pending=1
    if [[ ${__aterm_spare_mode-} == warm && -n $(builtin history 1 2>/dev/null) ]]; then
        __aterm_spare_rc_list=1
    fi
fi

# Wire PROMPT_COMMAND, PS0 (bash 5.3+), and the DEBUG trap last (so wiring lines
# are not captured as user commands).
if (( __aterm_fresh_load )); then
    if (( __aterm_prompt_cmd_is_array )); then
        PROMPT_COMMAND=("__aterm_prompt_command" "${PROMPT_COMMAND[@]}")
        if (( __aterm_pc_arrays )); then
            PROMPT_COMMAND+=(__aterm_prompt_end)
        fi
    else
        PROMPT_COMMAND="__aterm_prompt_command"
    fi
    if (( BASH_VERSINFO[0] > 5 || (BASH_VERSINFO[0] == 5 && BASH_VERSINFO[1] >= 3) )) &&
        [[ "${PS0-}" != *'__aterm_ps0'* ]]; then
        PS0='${| __aterm_ps0; }'"${PS0-}"
        export -n PS0
    fi
    trap '__aterm_preexec "$_"' DEBUG
fi

__aterm_flags_put "$__aterm_load_flags"
