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
# - Live managed PATH (`<prefix>/reroute` and `<prefix>/agents`)
# - Live body reload across updates via $ATERM_INTEGRATION_POINTER
# - Frozen tty settings (`ttyctl -f`) with raw-mode crash recovery
#
# Compatible with: zsh 5.0+

# Only run in interactive shells
[[ -o interactive ]] || return

# Suppress user XTRACE (`set -x` / `setopt xtrace`) inside aterm's own functions
# and during script load so prompt hooks do not flood output or leak
# $__aterm_shell_nonce. zsh prints a traced command only after expanding its
# words, so `: ${options[xtrace]::=off}` disables XTRACE before anything is
# printed, and zsh restores XTRACE on function return. The `(#*special)` guard
# avoids assigning when a caller declared a local `options` variable (e.g. for
# `zparseopts -A`), where a subscript assignment would fail or mutate the local.
typeset -g __aterm_load_xtrace=${${(M)${(t)options}:#*special}:+${options[xtrace]}}${${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}:+}

# ─── Multiplexer boundary (screen / tmux) ───
#
# Inside screen or tmux, multiple panes share one aterm PTY:
# 1. $ATERM_PARENT_SESSION_ID is inherited by every pane, so a flagless
#    `aterm ctl` call would otherwise drive the outer session.
# 2. Neither screen nor tmux forwards OSC 133 marks outward, so command blocks,
#    exit codes, and cwd tracking are unavailable in panes.
#
# When this script is sourced inside a pane (inherited
# $ATERM_SHELL_INTEGRATION_INSTALLED is non-empty, whereas aterm's spawn seam
# resets it to "" for every session it launches directly), export $ATERM_MUX and
# $ATERM_MUX_OUTER_SESSION_ID so `aterm ctl` can refuse implicit self-targeting
# without unsetting $ATERM_PARENT_SESSION_ID (which nested aterm instances need
# for capability provisioning). Sessions that never source this file in a pane
# are still detected via $ATERM_MUX_BASE below.
__aterm_mux=""
if [[ -n "${TMUX:-}" ]]; then
    __aterm_mux="tmux"
elif [[ -n "${STY:-}" ]]; then
    __aterm_mux="screen"
else
    # Consult $TMUX/$STY before $TERM because tmux defaults to screen-256color.
    case "${TERM:-}" in
        tmux|tmux-*|tmux.*)       __aterm_mux="tmux" ;;
        screen|screen-*|screen.*) __aterm_mux="screen" ;;
    esac
fi

case "${ATERM_MUX:-}" in 0|off|no|none|false) __aterm_mux="" ;; esac

# In-place upgrade: if this exact shell is re-sourcing the integration (both
# `$__aterm_shell_nonce` and the non-inheritable `__aterm_precmd` function are
# present), record the origin script for `aterm_shell_integration_old_bin.zsh`
# and clear the guard so idempotent setup runs again while one-shot steps
# (`__aterm_fresh_load=0`) are skipped. Checking `__aterm_precmd` in addition to
# the nonce prevents a nested subshell from upgrading in place.
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

# Skip if already loaded, marking the multiplexer boundary when crossed.
if [[ -n "${ATERM_SHELL_INTEGRATION_INSTALLED:-}" ]]; then
    if [[ -n "$__aterm_mux" ]]; then
        export ATERM_MUX="$__aterm_mux"
        if [[ -n "${ATERM_PARENT_SESSION_ID:-}" ]]; then
            export ATERM_MUX_OUTER_SESSION_ID="$ATERM_PARENT_SESSION_ID"
            # Emit the notice at most once per multiplexer session ($TMUX / $STY).
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
# Fallback for hosts that did not stamp $ATERM_MUX_BASE at spawn: record the
# multiplexer environment this session shell started in so `aterm ctl` can
# detect when a child later enters tmux/screen.
: "${ATERM_MUX_BASE:="${TMUX-}|${STY-}"}"
export ATERM_MUX_BASE
# Clear any multiplexer markers inherited from a parent pane when a new aterm
# session is launched from inside tmux/screen.
unset ATERM_MUX ATERM_MUX_OUTER_SESSION_ID __aterm_mux

# ─── Loader ───
#
# The loader runs once per shell (capturing and scrubbing the nonce, re-key
# path, and body pointer, and registering hooks). The section between the BODY
# markers is also written standalone as `aterm_shell_integration_body.zsh` in
# `<root>/<16-hex-addr>/` so a running shell can hot-reload a newer build's body
# at its next prompt when the host writes `<addr>` to $ATERM_INTEGRATION_POINTER.

# Capture the capability nonce into a shell-local variable and immediately
# unset $ATERM_SHELL_NONCE so child processes cannot read it from the environment.
# When unset or empty, emissions fall back to unnonced sequences; on an in-place
# upgrade, preserve the existing shell-local nonce.
typeset -g __aterm_shell_nonce="${ATERM_SHELL_NONCE:-${__aterm_shell_nonce:-}}"
unset ATERM_SHELL_NONCE

# Precompute the `;id=<hex>` suffix so marker emitters expand a parameter
# without forking a subshell per marker.
typeset -g __aterm_id_suffix_str=""
if [[ -n "$__aterm_shell_nonce" ]]; then
    __aterm_id_suffix_str=";id=${__aterm_shell_nonce}"
fi

# Per-session nonce re-key file ($ATERM_REKEY_PATH in the 0700 control dir),
# used to re-authorize a shell adopted across a seamless update. Captured and
# scrubbed from the environment so child processes cannot read the path.
typeset -g __aterm_rekey_path="${ATERM_REKEY_PATH:-${__aterm_rekey_path:-}}"
unset ATERM_REKEY_PATH

# Per-session live-body pointer path ($ATERM_INTEGRATION_POINTER), captured and
# scrubbed like the re-key path.
typeset -g __aterm_body_pointer="${ATERM_INTEGRATION_POINTER:-${__aterm_body_pointer:-}}"
unset ATERM_INTEGRATION_POINTER

# Content-addressed folder (`<root>/<16-hex-addr>/`) this script was sourced
# from (`%x`). Hand-installed copies outside a 16-hex directory leave
# `__aterm_body_rev` empty and sign no revision.
typeset -g __aterm_si_dir="${${(%):-%x}:h}"
typeset -g __aterm_si_root="${__aterm_si_dir:h}"
typeset -g __aterm_body_rev="${__aterm_si_dir:t}"
if [[ ${#__aterm_body_rev} -ne 16 || "$__aterm_body_rev" == *[^0-9a-f]* ]]; then
    __aterm_body_rev=""
fi

# A fresh load never places `~/.aterm/bin` on PATH, so skip the legacy cleanup.
if (( __aterm_fresh_load )); then
    typeset -g __aterm_old_bin_off=1
fi

# Source package shell hooks once per shell. `(N)` (NULL_GLOB) prevents zsh's
# default NOMATCH from aborting the script when a glob matches nothing.
if (( __aterm_fresh_load )) && [ -d "$HOME/.aterm/shell.d" ]; then
    for f in "$HOME/.aterm/shell.d"/*.zsh(N) "$HOME/.aterm/shell.d"/*.sh(N); do
        [ -f "$f" ] && . "$f"
    done
fi

# Check whether the host wrote a new 16-hex body address to
# $__aterm_body_pointer, and source `<root>/<addr>/aterm_shell_integration_body.zsh`
# under `emulate -L zsh` with `verbose` off if owned by the current user (`-O`).
__aterm_body_check() {
    [[ -n "$__aterm_body_pointer" && -f "$__aterm_body_pointer" ]] || return 0
    emulate -L zsh
    builtin unsetopt verbose
    local __aterm_addr="" __aterm_file=""
    { IFS= read -r __aterm_addr < "$__aterm_body_pointer" } 2>/dev/null
    command rm -f -- "$__aterm_body_pointer"
    [[ ${#__aterm_addr} -eq 16 && "$__aterm_addr" != *[^0-9a-f]* ]] || return 0
    [[ "$__aterm_addr" != "$__aterm_body_rev" ]] || return 0
    __aterm_file="$__aterm_si_root/$__aterm_addr/aterm_shell_integration_body.zsh"
    [[ -f "$__aterm_file" && -O "$__aterm_file" ]] || return 0
    . "$__aterm_file" && __aterm_body_rev="$__aterm_addr"
    return 0
}

# Precmd trampoline: captures `$?` and disables XTRACE in one expansion, checks
# for a newer body, and runs the body's precmd. Always returns 0 so a user's
# ZERR trap does not fire inside precmd after a failed command.
__aterm_precmd() {
    local __aterm_status=$?${${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}:+}
    __aterm_body_check
    __aterm_body_precmd "$__aterm_status"
    return 0
}

autoload -Uz add-zsh-hook

# @@ATERM-INTEGRATION-BODY-BEGIN@@ — everything from here to the END marker is re-sourced live

: ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}

# Enable agy's fast prompt editor by default so U+202F narrow no-break spaces
# in macOS screenshot filenames do not desynchronize Bubble Tea's cursor.
: "${AGY_CLI_FAST_PROMPT_EDITOR=true}"
export AGY_CLI_FAST_PROMPT_EDITOR

# One-time cleanup of `~/.aterm/bin` entries added by older scripts in shells
# upgraded in place.
if (( ! ${+__aterm_old_bin_off} )); then
    typeset -g __aterm_old_bin_off=1
    typeset -g __aterm_old_bin_file="${${(%):-%x}:h}/aterm_shell_integration_old_bin.zsh"
    if [[ -f "$__aterm_old_bin_file" && -O "$__aterm_old_bin_file" ]]; then
        . "$__aterm_old_bin_file"
    fi
    unset __aterm_old_bin_file __aterm_bin_from
fi

# ─── Managed PATH (`$ATERM_REROUTE_DIR` and `$ATPKG_AGENTS`) ───
#
# Keep `$ATERM_REROUTE_DIR` (session-scoped Rust toolchain stubs) and
# `$ATPKG_AGENTS` (`<prefix>/agents` shims) at the front of PATH ahead of entries
# prepended by `/etc/zprofile` (`path_helper`) or `~/.zshrc` (`~/.cargo/env`).
# `${(@)path:#…}` removes existing occurrences by literal equality while
# preserving empty PATH entries (`(@)`), and `emulate -L zsh` isolates the array
# operations from user `ksh_arrays`, `sh_word_split`, `glob_subst`, and
# `warn_create_global`.
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

# Per-prompt and per-command live PATH maintenance (builtin-only, gated on
# `$ATERM_CHILD` or `$ATERM_SESSION_ID`):
# 1. Re-source `~/.aterm/shell.d/00-atpkg.zsh` when its `mtime:inode` (`zstat`,
#    loaded lazily as `b:zstat` once the hook exists) changes.
# 2. Re-probe managed directories that did not exist when PATH was last fronted.
# 3. Re-front PATH only on a prefix mismatch (assigning `$path` flushes zsh's
#    command hash).
# 4. Watch `$ATPKG_AGENTS` entries via glob and run `rehash` when a twin is
#    added or removed so previously hashed foreign commands re-resolve.
typeset -g __aterm_atpkg_hook="$HOME/.aterm/shell.d/00-atpkg.zsh"
typeset -gi __aterm_managed_live=0
typeset -gi __aterm_have_zstat=0
typeset -gi __aterm_zstat_loaded="${__aterm_zstat_loaded:-0}"
if [[ -n "${ATERM_CHILD:-}" || -n "${ATERM_SESSION_ID:-}" ]]; then
    __aterm_managed_live=1
    __aterm_have_zstat=1
fi

# Store "<mtime>:<inode>" of the hook in $__aterm_atpkg_hook_now (never $REPLY,
# which precmd must not clobber).
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
if (( ! ${+__aterm_atpkg_hook_seen} )); then
    typeset -g __aterm_atpkg_hook_seen=""
    if (( __aterm_managed_live )); then
        __aterm_atpkg_hook_stamp
        __aterm_atpkg_hook_seen="$__aterm_atpkg_hook_now"
    fi
fi

# Derive `$ATERM_REROUTE_DIR` as `<parent of $ATPKG_AGENTS>/reroute` for adopted
# shells spawned without it, unless `$__ATERM_REROUTE_PASSTHROUGH` is engaged.
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
if (( __aterm_managed_live )) && __aterm_managed_derive_reroute; then
    __aterm_reroute_path_front
fi

__aterm_managed_path_live() {
    emulate -L zsh
    (( __aterm_managed_live )) || return 0
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
    local -i __aterm_n=$#__aterm_managed_want
    if (( __aterm_n )) && [[ "${(j.:.)path[1,__aterm_n]}" != "${(j.:.)__aterm_managed_want}" ]]; then
        __aterm_reroute_path_front
        return 0
    fi
    if (( __aterm_managed_agents_on )); then
        __aterm_managed_agents_listing
        if [[ "$__aterm_managed_agents_now" != "$__aterm_managed_agents_seen" ]]; then
            __aterm_managed_agents_seen="$__aterm_managed_agents_now"
            rehash
        fi
    fi
    return 0
}

# Preserve command state across live body reloads; use `typeset -g` in functions
# that run without `emulate` so user `WARN_NESTED_VAR` does not warn.
typeset -g __aterm_in_command="${__aterm_in_command:-0}"
typeset -g __aterm_report_host="${HOST:-${HOSTNAME:-localhost}}"

# Use `printf '\033]%s\a'` rather than `print -n`, which expands `\e` / `\a` /
# `\xNN` escapes inside `$1` and would allow OSC break-out from encoded command
# lines or titles.
__aterm_osc() {
    printf '\033]%s\a' "$1"
}

# Check and consume a pending nonce re-key file.
__aterm_rekey_check() {
    emulate -L zsh
    [[ -n "$__aterm_rekey_path" && -f "$__aterm_rekey_path" ]] || return 0
    local key=""
    { IFS= read -r key < "$__aterm_rekey_path" } 2>/dev/null
    command rm -f -- "$__aterm_rekey_path"
    [[ ${#key} -eq 64 && "$key" != *[^0-9a-f]* ]] || return 0
    __aterm_shell_nonce="$key"
    __aterm_id_suffix_str=";id=${key}"
}

# Print `;id=<64-hex>` when the captured nonce is non-empty. Hot emitters read
# `$__aterm_id_suffix_str` directly to avoid a subshell fork.
__aterm_id_suffix() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    if [[ -n "$__aterm_shell_nonce" ]]; then
        print -rn -- ";id=${__aterm_shell_nonce}"
    fi
}

# Percent-encode `$1` for `file://` URIs (RFC 3986) into caller's `local REPLY`,
# fork-free (`printf -v`). `LC_ALL=C` splits multi-byte UTF-8 into individual
# bytes, and `emulate -L zsh` ensures 1-based indexing regardless of user
# `KSH_ARRAYS`.
__aterm_urlencode() {
    emulate -L zsh
    local LC_ALL=C
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

# Report current working directory: signed `633;P;AtermCwd=` first (so the host
# ignores unsigned background-job OSC 7 reports before `133;A`), followed by
# standard unnonced OSC 7.
__aterm_report_cwd() {
    local REPLY
    __aterm_urlencode "$PWD"
    local uri="file://${__aterm_report_host}${REPLY}"
    if [[ -n "$__aterm_id_suffix_str" ]]; then
        __aterm_osc "633;P;AtermCwd=${uri}${__aterm_id_suffix_str}"
    fi
    __aterm_osc "7;${uri}"
}

__aterm_mark_prompt_start() {
    __aterm_osc "133;A${__aterm_id_suffix_str}"
}

# Mark command line start (OSC 133;B). Sent from zle-line-init after zle draws
# the prompt; `aterm_input=start` is attached only when `$BUFFER` is empty (as
# as opposed to pre-filled via `push-line` or `print -z`) so predictive echo
# knows the cursor stands at the start of the buffer.
__aterm_mark_command_start() {
    if [[ -n "$__aterm_id_suffix_str" ]] && zle && (( ! ${#BUFFER} )); then
        __aterm_osc "133;B${__aterm_id_suffix_str};aterm_input=start"
    else
        __aterm_osc "133;B${__aterm_id_suffix_str}"
    fi
}

__aterm_mark_exec_start() {
    __aterm_osc "133;C${__aterm_id_suffix_str}"
}

__aterm_mark_exec_finish() {
    __aterm_osc "133;D;$1${__aterm_id_suffix_str}"
}

# Report the active body revision (`633;P;AtermIntegration=<16-hex>`).
__aterm_mark_integration_rev() {
    [[ -n "$__aterm_body_rev" ]] || return 0
    __aterm_osc "633;P;AtermIntegration=${__aterm_body_rev}${__aterm_id_suffix_str}"
}

# ─── TTY state protection ───
#
# zsh adopts whatever termios state the last foreground job left behind, so a
# raw-mode program that dies (SIGKILL, `_exit`) without restoring termios leaves
# `-isig -iexten -icrnl -ixon -opost` active for subsequent commands.
#
# 1. Repair: if `stty -a` reports `-isig`, restore `isig icanon iexten echo
#    icrnl opost` (leaving `ixon` untouched in case the user set `stty -ixon`).
#    On a fresh shell, the initial `stty -a` runs asynchronously in a process
#    substitution watched by `zle -F` (`__aterm_tty_probe_ready`) so new tabs do
#    not block on a fork/exec before the first prompt.
# 2. Freeze: `ttyctl -f` makes zsh restore its saved termios after every job,
#    while `stty`, `reset`, and `tset` are wrapped to thaw (`ttyctl -u`), run,
#    and re-freeze in an `always` block (skipped if the user already aliased or
#    defined any of those names, in which case repair runs after failed jobs).
typeset -gi __aterm_tty_armed="${__aterm_tty_armed:-0}"
typeset -gi __aterm_tty_frozen="${__aterm_tty_frozen:-0}"
typeset -gi __aterm_tty_probe_fd="${__aterm_tty_probe_fd:-0}"
typeset -gi __aterm_tty_owed="${__aterm_tty_owed:-0}"

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

__aterm_tty_take() {
    zle -F $__aterm_tty_probe_fd 2>/dev/null
    read -r -u $__aterm_tty_probe_fd REPLY || true
    exec {__aterm_tty_probe_fd}<&-
    __aterm_tty_probe_fd=0
}

# Asynchronous callback (`zle -F`) for the first-prompt tty probe. If the rc
# left the tty raw (`-isig`), `zle -I` releases zle's termios, `ttyctl -u`
# unfreezes so zle adopts the repaired settings on return, and `__aterm_tty_owed`
# re-freezes at the next precmd/preexec.
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

# Run a tty-modifying command while thawed, then re-freeze in an `always` block
# so neither `setopt err_return` nor SIGINT leaves the tty thawed with
# `__aterm_tty_frozen=1`.
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

# Arm tty protection once per shell; `function <name>` avoids alias expansion
# when a body is re-sourced live.
__aterm_tty_arm() {
    emulate -L zsh
    __aterm_tty_armed=1
    if (( __aterm_fresh_load )); then
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

# Body precmd: runs without `emulate` so `unsetopt verbose` persists for the
# scheduled line-init event when needed.
__aterm_body_precmd() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    local last_status=$1

    __aterm_rekey_check

    (( ! __aterm_tty_probe_fd )) || __aterm_tty_repair
    (( ! __aterm_tty_owed )) || { __aterm_tty_owed=0; builtin ttyctl -f }
    (( __aterm_tty_armed )) || __aterm_tty_arm
    (( __aterm_tty_frozen || last_status == 0 )) || __aterm_tty_repair

    __aterm_line_init_wire "$last_status"
    (( ! __aterm_line_init_hush )) || builtin unsetopt verbose

    __aterm_managed_path_live

    if (( __aterm_in_command )); then
        __aterm_mark_exec_finish $last_status
        typeset -g __aterm_in_command=0
    fi

    __aterm_report_cwd

    # Abbreviate $HOME prefix in tab title (OSC 0) and strip control bytes so a
    # directory name cannot inject BEL/ESC.
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

    return 0
}

# Encode `$1` for OSC 633;E into caller's `local REPLY` (backslash-hex encoding
# `\`, `;`, space, and control bytes, fork-free via `printf -v`).
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

__aterm_preexec() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    typeset -g __aterm_in_command=1

    (( ! __aterm_tty_probe_fd )) || __aterm_tty_repair
    (( ! __aterm_tty_owed )) || { __aterm_tty_owed=0; builtin ttyctl -f }

    __aterm_managed_path_live

    local REPLY
    __aterm_encode_cmd "$1"
    __aterm_osc "633;E;${REPLY}${__aterm_id_suffix_str}"

    # Set tab title to running command (OSC 0), stripped of control bytes and
    # capped at 256 chars (`…` suffix).
    if [[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]; then
        local cmd="$1"
        (( ${#cmd} <= 256 )) || cmd="${cmd:0:255}…"
        __aterm_osc "0;${cmd//[[:cntrl:]]/}"
    fi

    __aterm_mark_exec_start
}

# ─── Prompt Override ───
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
# Bind xterm-style modifier+arrow and navigation sequences so they do not leak
# as literal text at the prompt.
__aterm_setup_keybindings() {
    bindkey '\e[1;3C' forward-word       # Alt+Right
    bindkey '\e[1;3D' backward-word      # Alt+Left
    bindkey '\e[1;5C' forward-word       # Ctrl+Right
    bindkey '\e[1;5D' backward-word      # Ctrl+Left
    bindkey '\e[H' beginning-of-line     # Home
    bindkey '\e[F' end-of-line           # End
    bindkey '\e[1~' beginning-of-line    # Home (alternate)
    bindkey '\e[4~' end-of-line          # End (alternate)
    bindkey '\e[3~' delete-char          # Delete/Fn+Backspace
    bindkey '\e[1;2A' up-line-or-history    # Shift+Up
    bindkey '\e[1;2B' down-line-or-history  # Shift+Down
}

# ─── OSC 133;B (`zle-line-init`) ───
#
# Emit OSC 133;B after the prompt is drawn and pass the entry `$?` through to
# any chained `__aterm_orig_zle_line_init` widget. Always return 0 when no
# chained widget exists so `add-zle-hook-widget` continues to later hooks.
__aterm_zle_line_init() {
    local -i __aterm_reached=$?
    if (( ! ${__aterm_line_init_marked:-0} )); then
        typeset -g __aterm_line_init_marked=1
        __aterm_mark_command_start
    fi
    typeset -g __aterm_line_init_via="${widgets[zle-line-init]-}"
    (( ${+widgets[__aterm_orig_zle_line_init]} )) || return 0
    __aterm_line_init_pass $__aterm_reached __aterm_orig_zle_line_init
}

# Invoke widget `$2` with `$?` restored to `$1` without triggering ZERR/ERR_RETURN.
__aterm_line_init_pass() {
    if (( ! $1 )); then
        zle "$2"
    else
        __aterm_line_init_status "$1" || zle "$2"
    fi
}
__aterm_line_init_status() { return "$1"; }

# When a user rc or plugin (e.g. oh-my-zsh, zsh-vi-mode) replaces `zle-line-init`
# instead of chaining it, `__aterm_line_init_wire` schedules a `+0` event that
# runs after all precmd hooks, temporarily fronts `__aterm_line_init_lead` (or
# registers with `add-zle-hook-widget` if `azhw` owns `zle-line-init`), emits
# 133;B, restores the widget table (`__aterm_line_init_aside`), and invokes the
# underlying widget while preserving `$?`, `$_`, `$pipestatus` (for status 0/1),
# `PRINT_EXIT_VALUE`, and `VERBOSE`.
typeset -g __aterm_line_init_via="${__aterm_line_init_via-}"
typeset -gi __aterm_line_init_marked=0
typeset -gi __aterm_line_init_led="${__aterm_line_init_led:-0}"
typeset -g __aterm_line_init_u="" __aterm_line_init_pev=""
typeset -gi __aterm_line_init_hush="${__aterm_line_init_hush:-0}"

__aterm_line_init_lead() {
    local -i __aterm_reached=$?
    if (( ! ${__aterm_line_init_marked:-0} )); then
        typeset -g __aterm_line_init_marked=1
        __aterm_mark_command_start
    fi
    if [[ "${widgets[zle-line-init]-}" == user:__aterm_line_init_lead ]]; then
        __aterm_line_init_aside
        [[ -n "${widgets[zle-line-init]-}" ]] || return 0
        __aterm_line_init_pass $__aterm_reached zle-line-init
        return 0
    fi
    local -i __aterm_status=0
    if (( ${+widgets[__aterm_line_init_under]} )); then
        __aterm_line_init_pass $__aterm_reached __aterm_line_init_under
        __aterm_status=$?
    fi
    __aterm_line_init_aside
    return $__aterm_status
}

# Restore every widget alias pointing to `__aterm_line_init_lead` (including
# aliases taken by powerlevel10k during prompt expansion) back to
# `__aterm_line_init_under`.
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

__aterm_line_init_wire() {
    [[ -o zle ]] || return 0
    local -i __aterm_verbose=${__aterm_line_init_hush:-0}
    [[ -o verbose ]] && __aterm_verbose=1
    emulate -L zsh
    __aterm_line_init_hush=0
    __aterm_line_init_marked=0
    local __aterm_via="$__aterm_line_init_via"
    __aterm_line_init_via=""
    (( ! __aterm_line_init_led )) || __aterm_line_init_aside
    local __aterm_now="${widgets[zle-line-init]-}"
    [[ "$__aterm_now" == user:__aterm_zle_line_init ||
       ( -n "$__aterm_now" && "$__aterm_now" == "$__aterm_via" ) ]] && return 0
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

# ─── Start mark and warm-spare hold ───
#
# Emit `AtermStartRc`, `AtermStartFlags`, and `AtermStart` (`cold` or `spare`)
# to `/dev/tty` (bypassing instant-prompt stdout capture) at the first prompt.
__aterm_start_mark() {
    emulate -L zsh
    local REPLY="" __aterm_s=0 __aterm_r=0
    local -i __aterm_pre=0 __aterm_i=${precmd_functions[(ie)__aterm_first_precmd]}
    (( ${+functions[precmd]} )) && __aterm_pre=1
    (( __aterm_i <= ${#precmd_functions} )) && (( __aterm_pre += __aterm_i - 1 ))
    [[ ${modules[zsh/sched]-} != loaded ]] || __aterm_s=${#zsh_scheduled_events}
    (( ${+PERIOD} && ( ${+functions[periodic]} || ${#periodic_functions} ) )) && __aterm_r=1
    REPLY="${ZDOTDIR:-$HOME}"
    [[ $REPLY != *[\\\;\ [:cntrl:]]* ]] || __aterm_encode_cmd "$REPLY"
    printf '\033]633;P;AtermStartRc=%s%s\a\033]633;P;AtermStartFlags=pre:%d,s:%d,j:%d,r:%d%s\a\033]633;P;AtermStart=%s%s\a' \
        "$REPLY" "$__aterm_id_suffix_str" $__aterm_pre $__aterm_s ${#jobstates} $__aterm_r \
        "$__aterm_id_suffix_str" "$1" "$__aterm_id_suffix_str" >/dev/tty 2>/dev/null
    return 0
}

# Hold a warm spare at its first precmd until released via `go` on
# `$__aterm_spare_fifo`, keeping `SAVEHIST=0` while held and recording whether
# `$HISTFILE` changed during the wait.
__aterm_spare_hold() {
    : ${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}
    emulate -L zsh
    local __aterm_rc=$SECONDS __aterm_hs0="" __aterm_go="" __aterm_ss=${SAVEHIST-}
    local -i __aterm_n=0 __aterm_ss_set=${+SAVEHIST}
    [[ ${(t)SAVEHIST} != *readonly* ]] || __aterm_ss_set=2
    local -A __aterm_h
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
    local -i __aterm_i=${precmd_functions[(ie)__aterm_first_precmd]}
    typeset -ga __aterm_spare_hooks
    __aterm_spare_hooks=( ${precmd_functions[1,__aterm_i-1]} )
    (( ! ${+functions[precmd]} )) || __aterm_spare_hooks[1,0]=( precmd )
    [[ ${(t)SECONDS} == *readonly* ]] || SECONDS=$__aterm_rc
    return 0
}

# Reload history if `$HISTFILE` changed during the hold and re-run precmd hooks
# registered ahead of aterm's with the startup `$?`.
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

# One-shot first precmd after all rc files have run: emit the start mark (or
# hold a warm spare), apply `ATERM_PROMPT_STYLE`, re-front managed PATH dirs,
# and unregister itself.
__aterm_first_precmd() {
    local __aterm_st=$?${${${(M)${(t)options}:#*special}:+${options[xtrace]::=off}}:+}
    if [[ ${__aterm_spare_mode-} == warm ]]; then
        __aterm_spare_hold
        __aterm_spare_rerun "$__aterm_st"
    else
        __aterm_start_mark cold
    fi
    if [[ -n "${ATERM_PROMPT_STYLE:-}" && "${ATERM_PROMPT_STYLE:-}" != "none" ]]; then
        __aterm_set_prompt
    fi

    __aterm_reroute_path_front

    add-zsh-hook -d precmd __aterm_first_precmd
    return 0
}

# Ensure the sourced body returns 0 so `__aterm_body_check` updates `__aterm_body_rev`.
true

# @@ATERM-INTEGRATION-BODY-END@@

# ─── Loader wiring (once per shell) ───
#
# Wire `zle-line-init`, `__aterm_first_precmd`, and keybindings only on a fresh
# load (`__aterm_fresh_load=1`): re-aliasing `zle-line-init` after user plugins
# have wrapped `__aterm_zle_line_init` would create a recursion cycle, and
# re-running keybindings would overwrite user bindings from `~/.zshrc`.
if (( __aterm_fresh_load )); then
    if (( ${+widgets[zle-line-init]} )) && [[ "${widgets[zle-line-init]}" != "user:__aterm_zle_line_init" ]]; then
        zle -A zle-line-init __aterm_orig_zle_line_init
    fi
    zle -N zle-line-init __aterm_zle_line_init
fi

if (( __aterm_fresh_load )); then
    add-zsh-hook precmd __aterm_first_precmd
fi
add-zsh-hook precmd __aterm_precmd
add-zsh-hook preexec __aterm_preexec

if (( __aterm_fresh_load )); then
    __aterm_setup_keybindings
fi

# On an in-place upgrade, sign the active revision immediately rather than
# waiting for the next prompt (the upgrade line may relaunch an agent at once).
if (( ! __aterm_fresh_load )); then
    __aterm_mark_integration_rev
fi

[[ $__aterm_load_xtrace != on ]] || builtin setopt xtrace
