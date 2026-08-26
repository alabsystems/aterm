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

# Skip if already loaded.
#
# `test -n`, NOT `set -q`: `set -q` asks whether the variable is DEFINED, and
# aterm's spawn seam deliberately defines this guard as an EMPTY string on
# every integrated spawn. That override is the NESTED-LAUNCH lifeline (the
# 0.19.0 gauntlet's F3): a shell running inside aterm exports the guard as `1`,
# that export rides the environment into the shell the NEXT aterm spawns, and
# without the override the inner shell's loader would bail. bash and zsh spell
# the guard `[[ -n "$ATERM_SHELL_INTEGRATION_INSTALLED" ]]`, which the empty
# override defuses exactly as designed. fish's `set -q` saw "defined" and
# returned before defining a single function — so the empty value meant to
# REVIVE the integration was what killed it.
#
# Measured on Linux with fish 4.2.1 under aterm 0.55.0 before this fix: ZERO
# OSC 133 marks on the wire, `aterm ctl blocks` -> "no results", and
# ATERM_SHELL_NONCE left un-scrubbed in the child environment (#8015's leak
# defense also lives past this guard) — in EVERY integrated fish session, not
# just nested ones. The OSC 133 marks that DID appear were fish 4.x's own
# built-in ones (`133;A;click_events=1`, `133;C;cmdline_url=…`), which aterm
# drops for carrying no `id=` nonce.
#
# `test -n "$var"` is false for BOTH unset and empty — the bash/zsh semantics
# this guard always intended. (The variable is quoted so an unset variable
# still expands to exactly one empty argument rather than zero arguments.)
#
# THE RULE, for every ATERM_* variable in this file: bash and zsh read these
# with `-n` / `-z` / `${VAR:-default}`, all of which treat UNSET and EMPTY
# alike, and aterm's spawn seam plus the user's own environment can and do
# deliver empty values. So test EMPTINESS (`test -n` / `test -z`), never
# definedness (`set -q`). `set -q` is correct only for a variable this script
# defines itself and always defines non-empty (`__aterm_pending_banner`), or
# where both arms are equivalent and the `set -q` arm additionally scrubs
# (`ATERM_SHELL_NONCE`, whose note explains why). Pinned by
# `test_fish_never_tests_definedness_of_an_aterm_variable`.
if test -n "$ATERM_SHELL_INTEGRATION_INSTALLED"
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

# Package suite version.
#
# `test -z`, NOT `not set -q` — see the loader-guard note above. bash and zsh
# spell this `export ATERM_SUITE_VERSION="${ATERM_SUITE_VERSION:-}"`, and `:-`
# substitutes for BOTH unset and empty, so an inherited empty value is always
# re-exported. `set -q` skipped that branch for an empty-but-defined variable,
# which left a non-exported (universal or global) empty definition unexported.
if test -z "$ATERM_SUITE_VERSION"
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
#
# This is the ONE `set -q` on an ATERM_* variable that is deliberate. Both arms
# leave $__aterm_shell_nonce empty when the env var is empty, so the definedness
# test cannot change the nonce; taking the arm for an empty-but-defined variable
# is strictly BETTER, because it also runs the `set -e` scrub on it. Do not
# "fix" this one to `test -n` — that would leave an empty ATERM_SHELL_NONCE
# exported into every child process.
if set -q ATERM_SHELL_NONCE
    set -g __aterm_shell_nonce "$ATERM_SHELL_NONCE"
    set -e ATERM_SHELL_NONCE
else
    set -g __aterm_shell_nonce ""
end

# Precomputed capability-nonce suffix for OSC 133/633 emissions — expands to
# ";id=<64-hex>" when the captured nonce is non-empty, or to the empty string
# otherwise. Mirrors the zsh script's `$__aterm_id_suffix_str`, and for the
# same two reasons — plus one that is fish-specific and load-bearing.
#
# THE FISH-SPECIFIC REASON (a silent, total mark loss). The emitters below used
# to spell the suffix as a command substitution glued to a string:
#
#     __aterm_osc "133;A"(__aterm_id_suffix)
#
# In fish, gluing a string to a command substitution is a CARTESIAN PRODUCT, and
# a substitution that prints nothing is a ZERO-ELEMENT list — so the product is
# ZERO arguments, not the string "133;A". Whenever the nonce was empty,
# `__aterm_osc` was therefore called with NO arguments at all and its
# `printf '\e]%s\a' $argv[1]` emitted a bare, EMPTY `ESC ] BEL` — every 133;A,
# 133;B, 133;C, 133;D and 633;E silently replaced by a malformed empty OSC.
# (bash and zsh interpolate a possibly-empty parameter, so neither shell has
# this failure mode; it is unique to fish's list semantics.)
#
# The empty-nonce path is NOT hypothetical: it is this file's own documented
# pre-nonce fallback, and it is exactly what the header's manual install
# (`source ~/.config/aterm/shell_integration.fish` from config.fish) produces,
# since nothing sets ATERM_SHELL_NONCE there. Measured on fish 4.2.1 before
# this fix: OSC 7 arrived, and 133;A/B/C/D + 633;E were all absent.
#
# A plain variable is immune — `"133;A$__aterm_id_suffix_str"` is ordinary
# string interpolation, one argument, empty suffix or not. Byte-identical
# output when the nonce IS set (same ";id=<hex>" spelling), and it drops four
# to five forkless-but-not-free command substitutions per command cycle.
# `set -g` (not `-gx`), exactly like $__aterm_shell_nonce itself, so #8015 (no
# nonce inheritance by subprocesses) is preserved.
set -g __aterm_id_suffix_str ""
if test -n "$__aterm_shell_nonce"
    set -g __aterm_id_suffix_str ";id=$__aterm_shell_nonce"
end

# Capability-nonce suffix for OSC 133/633 emissions (#7960, #7987, #8015).
# Prints ";id=<64-hex>" when the captured nonce is non-empty, or nothing
# otherwise. Reads from the captured global — never from the environment
# — so the nonce is not inherited by subprocesses. Kept as the documented
# helper / external entry point (the zsh script keeps its twin for the same
# reason); the hot emitters below interpolate $__aterm_id_suffix_str instead,
# because a command substitution that prints nothing would take the whole
# argument with it (see above).
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
    __aterm_osc "133;A$__aterm_id_suffix_str"
end

# Mark command line start (OSC 133;B)
function __aterm_mark_command_start
    __aterm_osc "133;B$__aterm_id_suffix_str"
end

# Mark command execution start (OSC 133;C)
function __aterm_mark_exec_start
    __aterm_osc "133;C$__aterm_id_suffix_str"
end

# Mark command completion (OSC 133;D;exitcode)
function __aterm_mark_exec_finish
    __aterm_osc "133;D;$argv[1]$__aterm_id_suffix_str"
end

# ─── Prompt Override ───
# When ATERM_PROMPT_STYLE is set, override fish_prompt using palette-indexed colors.
function __aterm_custom_prompt
    set -l style "$ATERM_PROMPT_STYLE"
    # `test -n`, NOT `set -q` — see the loader-guard note at the top. bash and
    # zsh spell every one of these `${ATERM_PROMPT_*_COLOR:-<default>}`, and `:-`
    # substitutes the default for BOTH unset and empty. `set -q` accepted an
    # empty-but-defined value, `echo $VAR` then printed a bare newline, and the
    # default was never reached — so `set_color ""` ran on EVERY prompt render
    # and fish answered `set_color: Unknown color ""` (exit 2) straight onto the
    # user's terminal. Measured on fish 4.2.1.
    set -l hc (test -n "$ATERM_PROMPT_HOST_COLOR"; and echo $ATERM_PROMPT_HOST_COLOR; or echo 2)
    set -l pc (test -n "$ATERM_PROMPT_PATH_COLOR"; and echo $ATERM_PROMPT_PATH_COLOR; or echo 4)
    set -l gc (test -n "$ATERM_PROMPT_GIT_COLOR"; and echo $ATERM_PROMPT_GIT_COLOR; or echo 3)
    set -l ec (test -n "$ATERM_PROMPT_ERROR_COLOR"; and echo $ATERM_PROMPT_ERROR_COLOR; or echo 1)
    set -l sc (test -n "$ATERM_PROMPT_SEP_COLOR"; and echo $ATERM_PROMPT_SEP_COLOR; or echo 8)

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
    #
    # `test -z`, NOT `not set -q` — see the loader-guard note at the top. bash
    # and zsh spell this `[[ -z "${ATERM_DISABLE_PROMPT_TITLES:-}" ]]`, so an
    # EMPTY value means "not disabled" and titles keep flowing. `set -q` read an
    # empty-but-defined value as "disabled" and silently killed tab titles in
    # fish only — the opposite of every other shell.
    if test -z "$ATERM_DISABLE_PROMPT_TITLES"
        if string match -q "$HOME/*" $PWD; or test "$PWD" = "$HOME"
            set -l rel (string sub -s (math (string length -- "$HOME") + 1) -- $PWD)
            __aterm_osc "0;"(string replace -ra '[\x00-\x1f\x7f]' '' -- "~$rel")
        else
            __aterm_osc "0;"(string replace -ra '[\x00-\x1f\x7f]' '' -- "$PWD")
        end
    end

    # Use custom prompt if ATERM_PROMPT_STYLE is set.
    #
    # `test -n`, NOT `set -q` — the SAME defect that killed the loader guard
    # above, in a second place. bash and zsh both spell this
    # `[[ -n "$ATERM_PROMPT_STYLE" && "$ATERM_PROMPT_STYLE" != "none" ]]`, so an
    # empty value falls through to the user's own prompt. `set -q` accepted the
    # empty value, handed it to __aterm_custom_prompt, and its `switch "$style"`
    # matched no case — so fish printed NO PROMPT AT ALL (an empty line between
    # the 133;A and 133;B marks), while the user's real prompt was skipped.
    if test -n "$ATERM_PROMPT_STYLE"; and test "$ATERM_PROMPT_STYLE" != "none"
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
    # Report command text for session memory (OSC 633;E).
    #
    # The encoded command line is captured into a QUOTED local before it is
    # interpolated, for the same fish list reason as $__aterm_id_suffix_str
    # above: glued command substitutions form a cartesian product, so a
    # substitution that prints nothing collapses the ENTIRE argument to zero
    # elements. Here that had two triggers — an empty nonce (always, on the
    # manual-install path) and an empty command line — and either one turned
    # this whole 633;E emission into a bare empty `ESC ] BEL`. `set -l` then
    # `"$encoded"` is safe: a quoted empty list expands to exactly one empty
    # string, so the payload degrades to `633;E;` instead of vanishing.
    set -l encoded (__aterm_encode_cmd "$argv")
    __aterm_osc "633;E;$encoded$__aterm_id_suffix_str"
    # Set tab title to running command (OSC 0).
    # Truncate to first 64 chars and strip control characters.
    # `test -z`, NOT `not set -q` — same emptiness contract as the prompt-title
    # path above.
    if test -z "$ATERM_DISABLE_PROMPT_TITLES"
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
# `test -n` alone (the zsh script's `[[ -n "$ATERM_BANNER_B64" ]]`): the
# `set -q` that used to lead this conjunction was already defused by the
# `test -n` beside it, but it modelled the wrong idiom for the next reader.
if test -n "$ATERM_BANNER_B64"
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
