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
#
# Compatible with: zsh 5.0+

# Only run in interactive shells
[[ -o interactive ]] || return

# Skip if already loaded
[[ -n "$ATERM_SHELL_INTEGRATION_INSTALLED" ]] && return
export ATERM_SHELL_INTEGRATION_INSTALLED=1

# Package bin directory
if [ -d "$HOME/.aterm/bin" ]; then
    export PATH="$HOME/.aterm/bin:$PATH"
fi

# Source package shell hooks. The `(N)` NULL_GLOB qualifier is REQUIRED: without it
# zsh's default NOMATCH raises "no matches found" the instant a glob matches nothing
# (e.g. shell.d holds only `*.zsh` hooks and no `*.sh`) and ABORTS this whole sourced
# script — killing every OSC 7 (cwd) / OSC 133 (command-block) hook defined below, and
# printing an error as the first line of every session. Per-glob `(N)` expands an
# empty match to nothing instead. (bash's unmatched-glob-stays-literal + the `[ -f ]`
# guard makes the bash script safe without this.)
if [ -d "$HOME/.aterm/shell.d" ]; then
    for f in "$HOME/.aterm/shell.d"/*.zsh(N) "$HOME/.aterm/shell.d"/*.sh(N); do
        [ -f "$f" ] && . "$f"
    done
fi

# Package suite version
export ATERM_SUITE_VERSION="${ATERM_SUITE_VERSION:-}"

# State tracking
typeset -g __aterm_in_command=0
typeset -g __aterm_report_host="${HOST:-${HOSTNAME:-localhost}}"

# OSC escape sequences
__aterm_osc() {
    print -n "\e]${1}\a"
}

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
typeset -g __aterm_shell_nonce="${ATERM_SHELL_NONCE:-}"
unset ATERM_SHELL_NONCE

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Expands to ";id=<64-hex>" when the captured nonce is non-empty, or to
# the empty string otherwise. Reads from the captured local — never from
# the environment — so the nonce is not inherited by subprocesses.
__aterm_id_suffix() {
    if [[ -n "$__aterm_shell_nonce" ]]; then
        print -rn -- ";id=${__aterm_shell_nonce}"
    fi
}

# Percent-encode a string for use in file:// URIs (RFC 3986).
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; all others
# are encoded byte-by-byte as %XX. LC_ALL=C ensures multi-byte UTF-8
# characters are split into individual bytes for correct encoding.
__aterm_urlencode() {
    local LC_ALL=C
    local string="$1" i char encoded=""
    for ((i = 1; i <= ${#string}; i++)); do
        char="${string[$i]}"
        case "$char" in
            [a-zA-Z0-9_.~/-]) encoded+="$char" ;;
            *) encoded+=$(printf '%%%02X' "'$char") ;;
        esac
    done
    print -rn -- "$encoded"
}

# Report current working directory (OSC 7)
__aterm_report_cwd() {
    local cwd
    cwd=$(__aterm_urlencode "$PWD")
    __aterm_osc "7;file://${__aterm_report_host}${cwd}"
}

# Mark prompt start (OSC 133;A)
__aterm_mark_prompt_start() {
    __aterm_osc "133;A$(__aterm_id_suffix)"
}

# Mark command line start (OSC 133;B)
__aterm_mark_command_start() {
    __aterm_osc "133;B$(__aterm_id_suffix)"
}

# Mark command execution start (OSC 133;C)
__aterm_mark_exec_start() {
    __aterm_osc "133;C$(__aterm_id_suffix)"
}

# Mark command completion (OSC 133;D;exitcode)
__aterm_mark_exec_finish() {
    __aterm_osc "133;D;$1$(__aterm_id_suffix)"
}

# precmd - runs before each prompt
__aterm_precmd() {
    local last_status=$?

    # If we were in a command, mark it finished
    if (( __aterm_in_command )); then
        __aterm_mark_exec_finish $last_status
        __aterm_in_command=0
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

    return $last_status
}

# Encode a string for OSC 633;E (VS Code convention).
# Backslash-hex encodes semicolons, backslashes, and bytes <= 0x20.
__aterm_encode_cmd() {
    local LC_ALL=C
    local string="$1" i char encoded=""
    for ((i = 1; i <= ${#string}; i++)); do
        char="${string[$i]}"
        case "$char" in
            \\) encoded+="\\\\" ;;
            \;) encoded+="\\x3b" ;;
            [[:cntrl:]]|' ') encoded+=$(printf '\\x%02x' "'$char") ;;
            *) encoded+="$char" ;;
        esac
    done
    print -rn -- "$encoded"
}

# preexec - runs before command execution
__aterm_preexec() {
    __aterm_in_command=1

    # Report command text for session memory (OSC 633;E)
    __aterm_osc "633;E;$(__aterm_encode_cmd "$1")$(__aterm_id_suffix)"

    # Set tab title to running command (OSC 0).
    # Truncate to first 64 chars and strip control characters.
    local cmd="${1:0:64}"
    if [[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]; then
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
__aterm_setup_keybindings

# ─── OSC 133;B (end of prompt / start of user input) ───
# Emitted via zle-line-init so it fires after the prompt is fully drawn.
# Placing it in preexec is too late (user has already typed their command).
if (( ${+widgets[zle-line-init]} )); then
    zle -A zle-line-init __aterm_orig_zle_line_init
fi
__aterm_zle_line_init() {
    __aterm_mark_command_start
    (( ${+widgets[__aterm_orig_zle_line_init]} )) && zle __aterm_orig_zle_line_init
}
zle -N zle-line-init __aterm_zle_line_init

# Install hooks using zsh hook arrays.
# __aterm_first_precmd is registered first so the one-shot banner prints
# before __aterm_precmd emits OSC 133;A (prompt start marker). This keeps
# the banner outside the semantic prompt region.
autoload -Uz add-zsh-hook

# ─── Deferred First-Precmd Setup ───
# Runs once on the very first precmd after the shell has fully initialized
# and processed SIGWINCH from the initial terminal resize. Handles prompt
# override and startup banner display, then uninstalls itself.
__aterm_first_precmd() {
    local last_status=$?

    # Apply prompt override if requested
    if [[ -n "$ATERM_PROMPT_STYLE" && "$ATERM_PROMPT_STYLE" != "none" ]]; then
        __aterm_set_prompt
    fi

    # Print startup banner passed from the app via base64-encoded env var.
    # Pipe directly to base64 -d (no command substitution) to preserve
    # trailing newline bytes in the ANSI escape sequence output.
    if [[ -n "$ATERM_BANNER_B64" ]]; then
        printf '%s' "$ATERM_BANNER_B64" | base64 -d
        unset ATERM_BANNER_B64
    fi

    add-zsh-hook -d precmd __aterm_first_precmd
    return $last_status
}
add-zsh-hook precmd __aterm_first_precmd
add-zsh-hook precmd __aterm_precmd
add-zsh-hook preexec __aterm_preexec
