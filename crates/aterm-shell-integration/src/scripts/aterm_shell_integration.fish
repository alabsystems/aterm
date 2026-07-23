#!/usr/bin/env fish
# aterm_shell_integration.fish - Shell integration for aTerm
#
# Copyright 2026 Andrew Yates
# Author: Andrew Yates
# Licensed under the Apache License, Version 2.0
#
# Source this file in your ~/.config/fish/config.fish:
#   test -e ~/.config/aterm/shell_integration.fish; and source ~/.config/aterm/shell_integration.fish
#
# Features enabled:
# - Directory tracking (OSC 7): tab title updates, "Open Terminal Here" support
# - Command tracking (OSC 133): command history indexing, timing, notifications
#
# Compatible with: fish 3.1+ (string escape --style=url requires 3.1)

# Only run in interactive shells.
# Use `return` (not `exit`) — `exit` in a sourced file kills the entire
# fish process, which breaks non-interactive invocations when loaded
# via XDG_DATA_DIRS conf.d auto-loading.
if not status is-interactive
    return
end

# Skip if already loaded
if set -q ATERM_SHELL_INTEGRATION_INSTALLED
    return
end
set -gx ATERM_SHELL_INTEGRATION_INSTALLED 1

# Package bin directory
if test -d "$HOME/.aterm/bin"
    set -gx PATH "$HOME/.aterm/bin" $PATH
end

# Source package shell hooks
if test -d "$HOME/.aterm/shell.d"
    for f in $HOME/.aterm/shell.d/*.fish $HOME/.aterm/shell.d/*.sh
        if test -f "$f"
            source "$f"
        end
    end
end

# Package suite version
if not set -q ATERM_SUITE_VERSION
    set -gx ATERM_SUITE_VERSION ""
end

# State tracking
set -g __aterm_last_status 0

# OSC escape sequences
function __aterm_osc
    printf '\e]%s\a' $argv[1]
end

# Capture the capability nonce into a shell-global so we can immediately
# drop it from the environment (#8015). Leaving ATERM_SHELL_NONCE in the
# exported env lets every child process (env, ssh SendEnv, docker, cron,
# tmux children, ...) read the 64-hex secret that would be used to bypass
# the #7960 nonce-enforcement defense. Capture first, then `set -e`
# (unexport) BEFORE any prompt hook fires so subprocesses never inherit
# it.
#
# If the env var is missing or empty at source-time, __aterm_shell_nonce
# stays empty and __aterm_id_suffix falls through to the unnonced form
# (pre-nonce compatibility for hosts that have not yet authorized a
# nonce). This matches the documented fallback: the host's OSC 133/633
# handler drops sequences missing/with a wrong id= only when
# `TerminalModes::require_shell_integration_nonce` is enabled.
if set -q ATERM_SHELL_NONCE
    set -g __aterm_shell_nonce "$ATERM_SHELL_NONCE"
    set -e ATERM_SHELL_NONCE
else
    set -g __aterm_shell_nonce ""
end

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Prints ";id=<64-hex>" when the captured nonce is non-empty, or nothing
# otherwise. Reads from the captured global — never from the environment
# — so the nonce is not inherited by subprocesses.
function __aterm_id_suffix
    if test -n "$__aterm_shell_nonce"
        printf ';id=%s' "$__aterm_shell_nonce"
    end
end

# Percent-encode a string for use in file:// URIs (RFC 3986).
# Unreserved chars (A-Z a-z 0-9 - _ . ~ /) pass through; all others
# are encoded byte-by-byte as %XX. fish's `string escape --style=url`
# uses query-string encoding (+ for spaces); we fix that to %20.
function __aterm_urlencode
    string escape --style=url -- $argv[1] | string replace -a '+' '%20'
end

# Report current working directory (OSC 7)
function __aterm_report_cwd
    set -l cwd (__aterm_urlencode (pwd))
    # file:// URL format
    __aterm_osc "7;file://"(hostname)"$cwd"
end

# Mark prompt start (OSC 133;A)
function __aterm_mark_prompt_start
    __aterm_osc "133;A"(__aterm_id_suffix)
end

# Mark command line start (OSC 133;B)
function __aterm_mark_command_start
    __aterm_osc "133;B"(__aterm_id_suffix)
end

# Mark command execution start (OSC 133;C)
function __aterm_mark_exec_start
    __aterm_osc "133;C"(__aterm_id_suffix)
end

# Mark command completion (OSC 133;D;exitcode)
function __aterm_mark_exec_finish
    __aterm_osc "133;D;$argv[1]"(__aterm_id_suffix)
end

# ─── Prompt Override ───
# When ATERM_PROMPT_STYLE is set, override fish_prompt using palette-indexed colors.
function __aterm_custom_prompt
    set -l style "$ATERM_PROMPT_STYLE"
    set -l hc (set -q ATERM_PROMPT_HOST_COLOR; and echo $ATERM_PROMPT_HOST_COLOR; or echo 2)
    set -l pc (set -q ATERM_PROMPT_PATH_COLOR; and echo $ATERM_PROMPT_PATH_COLOR; or echo 4)
    set -l gc (set -q ATERM_PROMPT_GIT_COLOR; and echo $ATERM_PROMPT_GIT_COLOR; or echo 3)
    set -l ec (set -q ATERM_PROMPT_ERROR_COLOR; and echo $ATERM_PROMPT_ERROR_COLOR; or echo 1)
    set -l sc (set -q ATERM_PROMPT_SEP_COLOR; and echo $ATERM_PROMPT_SEP_COLOR; or echo 8)

    # Error-aware prompt char: separator color on success, error color on failure
    set -l prompt_color $sc
    if test $__aterm_last_status -ne 0
        set prompt_color $ec
    end

    set -l git_info ""
    if command -sq git
        set -l branch (git rev-parse --abbrev-ref HEAD 2>/dev/null)
        if test -n "$branch"
            set git_info " "(set_color $gc)"($branch)"(set_color normal)
        end
    end

    switch "$style"
        case minimal
            printf '%s%s%s %s$%s ' (set_color $pc) (prompt_pwd) (set_color normal) (set_color $prompt_color) (set_color normal)
        case standard
            printf '%s%s@%s%s:%s%s%s%s %s$%s ' \
                (set_color $hc) (whoami) (hostname -s) \
                (set_color $sc) \
                (set_color $pc) (prompt_pwd) \
                (set_color normal) $git_info \
                (set_color $prompt_color) (set_color normal)
        case powerline
            set -l sep (set_color $sc)""(set_color normal)
            printf '%s%s@%s%s %s %s%s%s%s %s %s$%s ' \
                (set_color $hc) (whoami) (hostname -s) \
                (set_color normal) $sep \
                (set_color $pc) (prompt_pwd) \
                (set_color normal) $git_info $sep \
                (set_color $prompt_color) (set_color normal)
    end
end

# fish_prompt hook - wrap existing prompt
# We need to emit OSC 133;A before the prompt and OSC 133;B after
functions -c fish_prompt __aterm_original_fish_prompt 2>/dev/null

function fish_prompt
    # Print startup banner on first prompt (one-shot). Deferred from
    # source time so it survives config.fish clearing the screen.
    if set -q __aterm_pending_banner
        printf '%s' "$__aterm_pending_banner" | base64 -d
        set -e __aterm_pending_banner
    end

    # Mark prompt start
    __aterm_mark_prompt_start

    # Set tab title to abbreviated CWD (OSC 0).
    # Use prefix match (not substring) to avoid replacing $HOME in the middle of a path.
    # Strip control characters: a crafted directory name (Unix dir names may
    # contain any byte except '/' and NUL) could otherwise inject BEL/ESC and
    # smuggle a nested OSC (e.g. clipboard write) out of the title. Mirrors the
    # command-title path's string replace -ra '[\x00-\x1f\x7f]' '' guard.
    if not set -q ATERM_DISABLE_PROMPT_TITLES
        if string match -q "$HOME/*" $PWD; or test "$PWD" = "$HOME"
            set -l rel (string sub -s (math (string length -- "$HOME") + 1) -- $PWD)
            __aterm_osc "0;"(string replace -ra '[\x00-\x1f\x7f]' '' -- "~$rel")
        else
            __aterm_osc "0;"(string replace -ra '[\x00-\x1f\x7f]' '' -- "$PWD")
        end
    end

    # Use custom prompt if ATERM_PROMPT_STYLE is set
    if set -q ATERM_PROMPT_STYLE; and test "$ATERM_PROMPT_STYLE" != "none"
        __aterm_custom_prompt
    else if functions -q __aterm_original_fish_prompt
        __aterm_original_fish_prompt
    else
        # Fallback minimal prompt
        echo -n (whoami)'@'(hostname)' '(prompt_pwd)' $ '
    end

    # Mark command line start (user will type here)
    __aterm_mark_command_start
end

# Encode a string for OSC 633;E (VS Code convention).
# Backslash-hex encodes semicolons, backslashes, space, and every control
# byte (0x00-0x1f and 0x7f). Escaping ALL controls — not just the six
# whitespace bytes handled below — stops a raw ESC/BEL (reachable via Ctrl-V
# verbatim insert, paste, or tab-completing a filename that contains control
# bytes) from prematurely terminating the OSC 633;E string and letting the
# following bytes be parsed as fresh control sequences (a classic OSC
# break-out, e.g. smuggling an OSC 52 clipboard write). Mirrors the zsh/bash
# [[:cntrl:]] encoders and the control-byte stripping on the tab-title path,
# and emits the \xNN form the aterm decoder (unescape_vscode_string) reverses.
function __aterm_encode_cmd
    set -l input "$argv"
    set -l result ""
    for i in (string split '' -- "$input")
        switch "$i"
            case "\\"
                set result "$result\\\\"
            case ';'
                set result "$result\\x3b"
            case ' '
                set result "$result\\x20"
            case \t
                set result "$result\\x09"
            case \n
                set result "$result\\x0a"
            case \r
                set result "$result\\x0d"
            case '*'
                # Any remaining C0/DEL control byte (0x00-0x1f, 0x7f) — e.g.
                # ESC (0x1b) or BEL (0x07) — must be hex-escaped, never passed
                # through verbatim: raw ESC/BEL terminate an OSC string in the
                # parser and would break out of the 633;E sequence. Reuse the
                # same [\x00-\x1f\x7f] class the tab-title path strips, and the
                # url-escape helper already trusted by __aterm_urlencode to turn
                # the byte into %NN, then render it as the decoder's \xNN form
                # (lower-cased to match the \x3b/\x09 style used above). Space
                # (0x20) and \\t\\n\\r keep their explicit cases; non-control
                # bytes (including multi-byte UTF-8) still pass through as-is.
                if string match -qr '[\x00-\x1f\x7f]' -- "$i"
                    set result "$result\\x"(string escape --style=url -- "$i" | string sub -s 2 | string lower)
                else
                    set result "$result$i"
                end
        end
    end
    printf '%s' "$result"
end

# fish_preexec - runs before command execution
function __aterm_fish_preexec --on-event fish_preexec
    # Report command text for session memory (OSC 633;E)
    __aterm_osc "633;E;"(__aterm_encode_cmd "$argv")(__aterm_id_suffix)
    # Set tab title to running command (OSC 0).
    # Truncate to first 64 chars and strip control characters.
    if not set -q ATERM_DISABLE_PROMPT_TITLES
        __aterm_osc "0;"(string sub -l 64 -- "$argv" | string replace -ra '[\x00-\x1f\x7f]' '')
    end
    __aterm_mark_exec_start
end

# fish_postexec - runs after command execution
function __aterm_fish_postexec --on-event fish_postexec
    set __aterm_last_status $status
    __aterm_mark_exec_finish $__aterm_last_status
end

# Update cwd on directory change and at startup
function __aterm_fish_pwd --on-variable PWD
    __aterm_report_cwd
end

# Stash startup banner for deferred printing on first fish_prompt.
# Printing now would be erased if the user's config.fish or a framework
# clears the screen — vendor_conf.d loads before config.fish.
if set -q ATERM_BANNER_B64; and test -n "$ATERM_BANNER_B64"
    set -g __aterm_pending_banner "$ATERM_BANNER_B64"
    set -e ATERM_BANNER_B64
end

# ─── Key Bindings ───
# Bind xterm-style modifier+arrow sequences so they work at the prompt.
# Without these, sequences like \e[1;3C (Alt+Right) leak as literal text.
# Alt+Arrow: word navigation
bind \e\[1\;3C forward-word       # Alt+Right
bind \e\[1\;3D backward-word      # Alt+Left
# Ctrl+Arrow: word navigation
bind \e\[1\;5C forward-word       # Ctrl+Right
bind \e\[1\;5D backward-word      # Ctrl+Left
# Home/End
bind \e\[H beginning-of-line      # Home
bind \e\[F end-of-line             # End
bind \e\[1~ beginning-of-line     # Home (alternate)
bind \e\[4~ end-of-line           # End (alternate)
# Delete
bind \e\[3~ delete-char           # Delete/Fn+Backspace
# Shift+Arrow: history navigation
bind \e\[1\;2A up-or-search      # Shift+Up
bind \e\[1\;2B down-or-search    # Shift+Down

# Initial cwd report
__aterm_report_cwd
