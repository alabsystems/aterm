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
#
# Compatible with: bash 3.2+

# Only run in interactive shells
[[ $- != *i* ]] && return

# Skip if already loaded
[[ -n "$ATERM_SHELL_INTEGRATION_INSTALLED" ]] && return
export ATERM_SHELL_INTEGRATION_INSTALLED=1

# Package bin directory
if [ -d "$HOME/.aterm/bin" ]; then
    export PATH="$HOME/.aterm/bin:$PATH"
fi

# Source package shell hooks
if [ -d "$HOME/.aterm/shell.d" ]; then
    for f in "$HOME/.aterm/shell.d"/*.bash "$HOME/.aterm/shell.d"/*.sh; do
        [ -f "$f" ] && . "$f"
    done
fi

# Package suite version
export ATERM_SUITE_VERSION="${ATERM_SUITE_VERSION:-}"

# Store the real PROMPT_COMMAND before we modify it.
# Detect array vs scalar to preserve bash 5.1+ array-style PROMPT_COMMAND.
__aterm_prompt_cmd_is_array=0
if [[ "$(declare -p PROMPT_COMMAND 2>/dev/null)" == "declare -a"* ]]; then
    __aterm_prompt_cmd_is_array=1
fi
__aterm_original_prompt_command="${PROMPT_COMMAND:-}"

# Track last command for OSC 133;D
__aterm_last_command=""
# Guard: suppress DEBUG trap capture during PROMPT_COMMAND execution.
# Without this, commands from the user's original PROMPT_COMMAND (starship,
# pyenv, nvm, etc.) would be captured as if they were user commands.
__aterm_in_prompt_cmd=0

# OSC escape sequences
__aterm_osc() {
    printf '\033]%s\a' "$1"
}

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
__aterm_shell_nonce="${ATERM_SHELL_NONCE:-}"
unset ATERM_SHELL_NONCE

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Expands to ";id=<64-hex>" when the captured nonce is non-empty, or to
# the empty string otherwise. Reads from the captured local — never from
# the environment — so the nonce is not inherited by subprocesses.
__aterm_id_suffix() {
    if [[ -n "$__aterm_shell_nonce" ]]; then
        printf ';id=%s' "$__aterm_shell_nonce"
    fi
}

# Percent-encode a string for use in file:// URIs (RFC 3986).
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; all others
# are encoded byte-by-byte as %XX. Setting LC_ALL=C ensures multi-byte
# UTF-8 characters are split into individual bytes for correct encoding.
__aterm_urlencode() {
    local LC_ALL=C
    local string="$1" i char
    for ((i = 0; i < ${#string}; i++)); do
        char="${string:i:1}"
        case "$char" in
            [a-zA-Z0-9_.~/-]) printf '%s' "$char" ;;
            *) printf '%%%02X' "$(( $(printf '%d' "'$char") & 0xFF ))" ;;
        esac
    done
}

# Report current working directory (OSC 7)
__aterm_report_cwd() {
    local cwd
    cwd=$(__aterm_urlencode "$PWD")
    __aterm_osc "7;file://${HOSTNAME:-$(hostname)}${cwd}"
}

# Mark prompt start (OSC 133;A)
__aterm_mark_prompt_start() {
    __aterm_osc "133;A$(__aterm_id_suffix)"
}

# Mark command line start (OSC 133;B) - after prompt, before user input
__aterm_mark_command_start() {
    __aterm_osc "133;B$(__aterm_id_suffix)"
}

# Mark command execution start (OSC 133;C)
__aterm_mark_exec_start() {
    __aterm_osc "133;C$(__aterm_id_suffix)"
}

# Mark command completion (OSC 133;D;exitcode)
# Takes exit status as $1 (caller must pass it — $? inside a function
# body reflects the previous statement, not the original command).
__aterm_mark_exec_finish() {
    __aterm_osc "133;D;${1}$(__aterm_id_suffix)"
    __aterm_last_command=""
}

# Encode a string for OSC 633;E (VS Code convention).
# Backslash-hex encodes semicolons, backslashes, and bytes <= 0x20.
__aterm_encode_cmd() {
    local LC_ALL=C
    local string="$1" i char result=""
    for ((i = 0; i < ${#string}; i++)); do
        char="${string:i:1}"
        case "$char" in
            \\) result+="\\\\" ;;
            \;) result+="\\x3b" ;;
            [[:cntrl:]]|' ') result+=$(printf '\\x%02x' "$(( $(printf '%d' "'$char") & 0xFF ))") ;;
            *) result+="$char" ;;
        esac
    done
    printf '%s' "$result"
}

# Capture command before execution
# Uses DEBUG trap which fires before each command
__aterm_preexec() {
    # Always chain the previous DEBUG trap handler first, before any early
    # returns, so pre-existing handlers (starship, pyenv, etc.) always run.
    [[ -n "$__aterm_prev_debug_handler" ]] && eval "$__aterm_prev_debug_handler"

    # Skip if this is from PROMPT_COMMAND (ours or the user's original)
    (( __aterm_in_prompt_cmd )) && return
    [[ "$BASH_COMMAND" == "$PROMPT_COMMAND" ]] && return
    [[ "$BASH_COMMAND" == "__aterm_"* ]] && return

    # Only capture the first command (not subshells)
    if [[ -z "$__aterm_last_command" ]]; then
        __aterm_last_command="$BASH_COMMAND"
        # Report command text for session memory (OSC 633;E)
        __aterm_osc "633;E;$(__aterm_encode_cmd "$BASH_COMMAND")$(__aterm_id_suffix)"
        # Set tab title to running command (OSC 0).
        # Truncate to first 64 chars and strip control characters.
        local cmd="${BASH_COMMAND:0:64}"
        cmd="${cmd//[[:cntrl:]]/}"
        if [[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]; then
            __aterm_osc "0;$cmd"
        fi
        __aterm_mark_exec_start
    fi
}

# PROMPT_COMMAND handler - runs before each prompt
__aterm_prompt_command() {
    local last_status=$?
    __aterm_last_exit=$last_status
    __aterm_in_prompt_cmd=1

    # If we had a command, mark it finished
    if [[ -n "$__aterm_last_command" ]]; then
        __aterm_mark_exec_finish $last_status
    fi

    # One-shot banner (between 133;D and 133;A — outside the semantic
    # prompt region so terminal parsers don't treat it as prompt text).
    if [[ -n "$__aterm_pending_banner" ]]; then
        printf '%s' "$__aterm_pending_banner" | base64 -d
        unset __aterm_pending_banner
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

    # Mark prompt start
    __aterm_mark_prompt_start

    # Run original PROMPT_COMMAND if any (scalar case only).
    # When PROMPT_COMMAND is an array, bash chains array elements automatically;
    # we prepended ourselves at index 0, so the rest run without eval.
    # Restore $? so the user's prompt sees the real exit status.
    if ! (( __aterm_prompt_cmd_is_array )) && [[ -n "$__aterm_original_prompt_command" ]]; then
        ( exit $last_status )
        eval "$__aterm_original_prompt_command"
    fi

    # One-shot prompt setup. Runs after the original PROMPT_COMMAND so it
    # survives frameworks (starship, oh-my-bash) that set PS1 at init.
    if [[ -n "$__aterm_pending_prompt_setup" ]]; then
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
    if [[ -z "$__aterm_prompt_has_mark_b" ]]; then
        local __aterm_b_suffix=""
        [[ -n "$__aterm_shell_nonce" ]] && __aterm_b_suffix=";id=${__aterm_shell_nonce}"
        local __aterm_b="\[\033]133;B${__aterm_b_suffix}\a\]"
        # Strip any previous variant (with or without a nonce suffix) so
        # nonce rotation does not leave stale id= tails on PS1.
        local __aterm_b_re='\\\[\\033\]133;B(;id=[0-9a-fA-F]+)?\\a\\\]$'
        if [[ "$PS1" =~ $__aterm_b_re ]]; then
            PS1="${PS1%${BASH_REMATCH[0]}}"
        fi
        PS1="${PS1}${__aterm_b}"
    fi

    __aterm_in_prompt_cmd=0
    return $last_status
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
    [[ -n "$__aterm_shell_nonce" ]] && mark_b_id=";id=${__aterm_shell_nonce}"
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
# Uses \001/\002 (raw \[/\]) since this is called via $() inside PS1.
__aterm_err_prompt() {
    if [[ ${__aterm_last_exit:-0} -ne 0 ]]; then
        printf '\001\033[38;5;%sm\002$\001\033[0m\002 ' "${ATERM_PROMPT_ERROR_COLOR:-1}"
    else
        printf '\001\033[38;5;%sm\002$\001\033[0m\002 ' "${ATERM_PROMPT_SEP_COLOR:-8}"
    fi
}

__aterm_git_segment() {
    local branch
    branch=$(command git rev-parse --abbrev-ref HEAD 2>/dev/null) || return
    [[ -n "$branch" ]] && printf ' (%s)' "$branch"
}

# Defer prompt setup to first PROMPT_COMMAND so it survives frameworks
# (starship, oh-my-bash) that overwrite PS1 during their initialization.
if [[ -n "$ATERM_PROMPT_STYLE" && "$ATERM_PROMPT_STYLE" != "none" ]]; then
    __aterm_pending_prompt_setup=1
fi

# Stash startup banner for deferred printing on first PROMPT_COMMAND.
# Printing now would be erased if the user's PROMPT_COMMAND (starship,
# oh-my-bash, etc.) clears or redraws the screen on first invocation.
if [[ -n "$ATERM_BANNER_B64" ]]; then
    __aterm_pending_banner="$ATERM_BANNER_B64"
    unset ATERM_BANNER_B64
    if [[ -n "${BASH_EXECUTION_STRING:-}" ]]; then
        printf '%s' "$__aterm_pending_banner" | base64 -d
        unset __aterm_pending_banner
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
__aterm_setup_keybindings 2>/dev/null

# Save any existing DEBUG trap handler for chaining.
# trap -p DEBUG outputs: trap -- 'handler' DEBUG
__aterm_prev_debug_handler=""
__aterm_tmp=$(trap -p DEBUG 2>/dev/null)
if [[ "$__aterm_tmp" == trap\ --\ * ]]; then
    __aterm_prev_debug_handler="${__aterm_tmp#trap -- \'}"
    __aterm_prev_debug_handler="${__aterm_prev_debug_handler%\' DEBUG}"
fi
unset __aterm_tmp

# Install the integration
trap '__aterm_preexec' DEBUG
if (( __aterm_prompt_cmd_is_array )); then
    PROMPT_COMMAND=("__aterm_prompt_command" "${PROMPT_COMMAND[@]}")
else
    PROMPT_COMMAND="__aterm_prompt_command"
fi
