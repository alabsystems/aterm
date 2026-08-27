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

# Skip if already loaded — marking the boundary on the way out when the
# inherited guard means we crossed one.
if [[ -n "$ATERM_SHELL_INTEGRATION_INSTALLED" ]]; then
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
                    printf 'aterm: inside %s — command blocks, exit codes and cwd tracking do not cross the multiplexer,\n       so aterm records none of them for these panes. `aterm ctl mux` explains; ATERM_MUX_NOTICE=0 silences this.\n' "$__aterm_mux" >&2
                fi
                unset __aterm_mux_id __aterm_mux_dir __aterm_mux_stamp
            fi
        fi
    fi
    unset __aterm_mux
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

# Precomputed capability-nonce suffix for OSC 133/633 emissions.
# The nonce is captured exactly once (above) and the env var is unset on the
# very next line, so this string is CONSTANT for the life of the shell — there
# is no in-shell rotation path that could make it stale. Computing it here
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

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Expands to ";id=<64-hex>" when the captured nonce is non-empty, or to
# the empty string otherwise. Reads from the captured local — never from
# the environment — so the nonce is not inherited by subprocesses.
# Kept as the documented helper / external entry point; the hot emitters
# below use $__aterm_id_suffix_str instead to avoid a fork per marker.
__aterm_id_suffix() {
    if [[ -n "$__aterm_shell_nonce" ]]; then
        printf ';id=%s' "$__aterm_shell_nonce"
    fi
}

# Percent-encode a string for use in file:// URIs (RFC 3986).
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; all others
# are encoded byte-by-byte as %XX. Setting LC_ALL=C ensures multi-byte
# UTF-8 characters are split into individual bytes for correct encoding.
#
# Runs once per prompt (via __aterm_report_cwd), so it is fork-free by
# construction: `printf -v` (bash 3.1+, i.e. inside the documented 3.2+
# floor) writes into a variable instead of spawning a `$(printf ...)`
# subshell per encoded byte. A path with a single space used to cost a
# fork; a 4-byte emoji cost four.
__aterm_urlencode() {
    local LC_ALL=C
    # Fast path: no byte needs encoding, so the loop would copy the string
    # verbatim. Skip it. (The class is exactly the loop's pass-through class,
    # so this is the same decision the loop would make for every byte.)
    if [[ "$1" != *[^a-zA-Z0-9_.~/-]* ]]; then
        printf '%s' "$1"
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
    # Emitted once rather than streamed per character; the sole caller already
    # captures the whole result via `cwd=$(__aterm_urlencode "$PWD")`.
    printf '%s' "$out"
}

# Report current working directory (OSC 7)
__aterm_report_cwd() {
    local cwd
    cwd=$(__aterm_urlencode "$PWD")
    __aterm_osc "7;file://${HOSTNAME:-$(hostname)}${cwd}"
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

# Encode a string for OSC 633;E (VS Code convention).
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
    printf '%s' "$result"
}

# Capture command before execution
# Uses DEBUG trap which fires before each command
__aterm_preexec() {
    # Always chain the previous DEBUG trap handler first, before any early
    # returns, so pre-existing handlers (starship, pyenv, etc.) always run.
    [[ -n "$__aterm_prev_debug_handler" ]] && eval "$__aterm_prev_debug_handler"

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

    # Only capture the first command (not subshells)
    if [[ -z "$__aterm_last_command" ]]; then
        __aterm_last_command="$BASH_COMMAND"
        # Report command text for session memory (OSC 633;E)
        __aterm_osc "633;E;$(__aterm_encode_cmd "$BASH_COMMAND")${__aterm_id_suffix_str}"
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

# Install the integration. The trap goes LAST: the DEBUG trap is live from the
# very next top-level command, and inside a sourced script that next command is
# the remainder of THIS file — with the trap first, the PROMPT_COMMAND wiring
# below was captured as a "user command" at load (a bogus 633;E + 133;C + tab
# title, and __aterm_last_command left occupied before the first real prompt).
if (( __aterm_prompt_cmd_is_array )); then
    PROMPT_COMMAND=("__aterm_prompt_command" "${PROMPT_COMMAND[@]}")
else
    PROMPT_COMMAND="__aterm_prompt_command"
fi
trap '__aterm_preexec' DEBUG
