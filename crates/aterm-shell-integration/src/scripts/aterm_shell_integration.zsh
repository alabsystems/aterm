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

# Precomputed capability-nonce suffix for OSC 133/633 emissions.
# The nonce is captured exactly once (above) and the env var is unset on the
# very next line, so this string is CONSTANT for the life of the shell — there
# is no in-shell rotation path that could make it stale. Computing it here
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

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Expands to ";id=<64-hex>" when the captured nonce is non-empty, or to
# the empty string otherwise. Reads from the captured local — never from
# the environment — so the nonce is not inherited by subprocesses.
# Kept as the documented helper / external entry point; the hot emitters
# below use $__aterm_id_suffix_str instead to avoid a fork per marker.
__aterm_id_suffix() {
    if [[ -n "$__aterm_shell_nonce" ]]; then
        print -rn -- ";id=${__aterm_shell_nonce}"
    fi
}

# Percent-encode a string for use in file:// URIs (RFC 3986).
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; all others
# are encoded byte-by-byte as %XX. LC_ALL=C ensures multi-byte UTF-8
# characters are split into individual bytes for correct encoding.
#
# Runs once per prompt (via __aterm_report_cwd), so it is fork-free by
# construction: `printf -v` writes into a variable instead of spawning a
# `$(printf ...)` subshell per encoded byte. A path with a single space used
# to cost a fork; a 4-byte emoji cost four. No `& 0xFF` mask is needed (or
# present, historically): unlike bash, zsh's `printf '%d' "'<byte>"` returns
# the UNSIGNED byte value (195 for 0xC3), so `%02X` is already correct.
__aterm_urlencode() {
    local LC_ALL=C
    # Fast path: no byte needs encoding, so the loop would copy the string
    # verbatim. Skip it. (The class is exactly the loop's pass-through class,
    # so this is the same decision the loop would make for every byte.)
    if [[ "$1" != *[^a-zA-Z0-9_.~/-]* ]]; then
        print -rn -- "$1"
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
    __aterm_osc "133;A${__aterm_id_suffix_str}"
}

# Mark command line start (OSC 133;B)
__aterm_mark_command_start() {
    __aterm_osc "133;B${__aterm_id_suffix_str}"
}

# Mark command execution start (OSC 133;C)
__aterm_mark_exec_start() {
    __aterm_osc "133;C${__aterm_id_suffix_str}"
}

# Mark command completion (OSC 133;D;exitcode)
__aterm_mark_exec_finish() {
    __aterm_osc "133;D;$1${__aterm_id_suffix_str}"
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
#
# Runs once per user command, between Enter and the command actually
# starting, so it is fork-free. Space is split out of the old
# `[[:cntrl:]]|' '` arm because it is unconditionally 0x20 — a literal
# beats a subshell, and spaces are the only member of that arm a real
# command line ever contains. Control bytes keep the computed form but
# use `printf -v` instead of a `$(printf ...)` subshell.
__aterm_encode_cmd() {
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
    print -rn -- "$encoded"
}

# preexec - runs before command execution
__aterm_preexec() {
    __aterm_in_command=1

    # Report command text for session memory (OSC 633;E)
    __aterm_osc "633;E;$(__aterm_encode_cmd "$1")${__aterm_id_suffix_str}"

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
