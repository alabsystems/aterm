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
# - Live managed dirs: running shells resolve `claude`/`codex` to atpkg's
#   <prefix>/agents twins and cargo/rustc to <prefix>/reroute as soon as atpkg
#   lays them.
# - Loader + body split: a running shell sources a newer build's integration
#   body at its next prompt when the host updates the body pointer.
#
# Compatible with fish 3.7+: the floor a lane runs is apt's fish 3.7.0 in the
# Linux guest (`targo --unverified verify linux`); on macOS the tests run the
# pinned 4.9.3 that `targo --unverified verify provision` builds. (`return`
# outside a function needs 3.4; `string escape --style=url` needs 3.1.)

# Only run in interactive shells. Use `return` (not `exit`, which kills the
# fish process when loaded via XDG_DATA_DIRS conf.d).
if not status is-interactive
    return
end

# Skip if already loaded.
#
# Use `test -n` (emptiness), never `set -q` (definedness), for ATERM_* env vars:
# aterm's spawn seam defines `ATERM_SHELL_INTEGRATION_INSTALLED=""` on every
# integrated spawn so nested launches still load integration, matching bash/zsh
# `[[ -n ... ]]`. `set -q` is used only for internal script variables or where
# `set -e` must also scrub an empty exported variable (`ATERM_SHELL_NONCE`).
#
# Multiplexer boundary (screen / tmux):
# aterm hosts one session per PTY. Inside screen or tmux, `$ATERM_PARENT_SESSION_ID`
# and `$ATERM_SHELL_INTEGRATION_INSTALLED` are inherited from the outer shell.
# Because aterm injects fish via `XDG_DATA_DIRS` (exported), pane shells source
# this file and hit the guard below. When a multiplexer is detected at that
# boundary, export `$ATERM_MUX` and `$ATERM_MUX_OUTER_SESSION_ID` so `aterm ctl`
# refuses implicit self-targeting rather than driving the outer session, while
# leaving `$ATERM_PARENT_SESSION_ID` intact for nested-aterm capability edges.
set -l __aterm_mux ""
if test -n "$TMUX"
    set __aterm_mux tmux
else if test -n "$STY"
    set __aterm_mux screen
else
    # tmux's default TERM is screen-256color, so $TMUX/$STY are checked first.
    switch "$TERM"
        case tmux 'tmux-*' 'tmux.*'
            set __aterm_mux tmux
        case screen 'screen-*' 'screen.*'
            set __aterm_mux screen
    end
end

contains -- "$ATERM_MUX" 0 off no none false; and set __aterm_mux ""

# In-place upgrade: when a running shell re-sources this file (`typed_rekey`),
# it already holds `$__aterm_shell_nonce` and the uninherited function
# `__aterm_fish_preexec`. Record the origin script for `old_bin` cleanup and
# clear the guard so the load proceeds as an in-place upgrade.
if set -q __aterm_shell_nonce; and functions -q __aterm_fish_preexec; and test -n "$ATERM_SHELL_INTEGRATION_INSTALLED"
    if not set -q __aterm_old_bin_off; and not set -q __aterm_bin_from
        if not set -q __aterm_fresh_load; and not set -q __aterm_si_dir
            set -g __aterm_bin_from -
        else if test "$__aterm_fresh_load" = 1; and test -n "$__aterm_si_dir"
            set -g __aterm_bin_from "$__aterm_si_dir/aterm_shell_integration.fish"
        else
            set -g __aterm_bin_from ""
        end
    end
    set -g __aterm_fresh_load 0
    set -gx ATERM_SHELL_INTEGRATION_INSTALLED ""
else
    set -g __aterm_fresh_load 1
end

if test -n "$ATERM_SHELL_INTEGRATION_INSTALLED"
    if test -n "$__aterm_mux"
        set -gx ATERM_MUX $__aterm_mux
        if test -n "$ATERM_PARENT_SESSION_ID"
            set -gx ATERM_MUX_OUTER_SESSION_ID $ATERM_PARENT_SESSION_ID
            # Emit notice once per multiplexer session, keyed by $TMUX / $STY.
            if test "$ATERM_MUX_NOTICE" != 0
                set -l __aterm_mux_id "$TMUX"
                test -n "$__aterm_mux_id"; or set __aterm_mux_id "$STY"
                test -n "$__aterm_mux_id"; or set __aterm_mux_id "$TERM"
                set -l __aterm_mux_root "$XDG_RUNTIME_DIR"
                test -n "$__aterm_mux_root"; or set __aterm_mux_root "$TMPDIR"
                test -n "$__aterm_mux_root"; or set __aterm_mux_root /tmp
                set -l __aterm_mux_dir "$__aterm_mux_root/aterm/mux-notice"
                set -l __aterm_mux_key (string replace -ra '[^A-Za-z0-9._-]' _ -- "$__aterm_mux_id")
                set -l __aterm_mux_stamp "$__aterm_mux_dir/$__aterm_mux-$__aterm_mux_key"
                if not test -e "$__aterm_mux_stamp"
                    if mkdir -p "$__aterm_mux_dir" 2>/dev/null; and printf '' >"$__aterm_mux_stamp" 2>/dev/null
                        printf 'aterm: inside %s — no command blocks, exit codes or cwd tracking in these panes (`aterm ctl mux` explains; ATERM_MUX_NOTICE=0 silences)\n' "$__aterm_mux" >&2
                    end
                end
            end
        end
    end
    return
end
set -gx ATERM_SHELL_INTEGRATION_INSTALLED 1
# Record the multiplexer environment this session shell started in (fallback if
# not already stamped by the spawn seam) and clear any inherited pane markers.
if not test -n "$ATERM_MUX_BASE"
    set -gx ATERM_MUX_BASE "$TMUX|$STY"
end
if test -n "$ATERM_MUX"
    set -e ATERM_MUX
end
if test -n "$ATERM_MUX_OUTER_SESSION_ID"
    set -e ATERM_MUX_OUTER_SESSION_ID
end

# Loader: captures and scrubs the nonce and host channels once per shell and
# installs the `fish_prompt` trampoline. The section between the BODY markers
# is also written standalone as `aterm_shell_integration_body.fish` so running
# shells can reload it at the next prompt.

# Capture the capability nonce into a shell-global and unexport `ATERM_SHELL_NONCE`
# so child processes never inherit the 64-hex secret. `set -q` is deliberate so
# an empty-but-exported variable is still scrubbed by `set -e`. In-place upgrades
# preserve the existing `$__aterm_shell_nonce`.
if set -q ATERM_SHELL_NONCE
    set -g __aterm_shell_nonce "$ATERM_SHELL_NONCE"
    set -e ATERM_SHELL_NONCE
else if not set -q __aterm_shell_nonce
    set -g __aterm_shell_nonce ""
end

# Precomputed `;id=<64-hex>` suffix for OSC 133/633 emissions. Stored as a plain
# string global rather than a command substitution because in fish, gluing a
# string to a command substitution that prints nothing produces a zero-element
# cartesian product, which would pass zero arguments to `__aterm_osc`.
set -g __aterm_id_suffix_str ""
if test -n "$__aterm_shell_nonce"
    set -g __aterm_id_suffix_str ";id=$__aterm_shell_nonce"
end

# Per-session re-key file ($ATERM_REKEY_PATH in the 0700 control dir) for
# re-keying adopted shells across seamless updates. Captured and scrubbed from env.
if test -n "$ATERM_REKEY_PATH"; or not set -q __aterm_rekey_path
    set -g __aterm_rekey_path "$ATERM_REKEY_PATH"
end
set -e ATERM_REKEY_PATH 2>/dev/null

# Warm spare (docs/DESIGN-warm-spare-2026-10-16.md): fish runs vendor conf.d
# alongside user startup files, so a fish spare runs startup unseen and waits at
# its first prompt event when its hold FIFO exists. Also record the conf.d
# startup roots so the host stamps the files a spare ran unseen.
if test "$__aterm_fresh_load" = 1
    set -g __aterm_start_pending 1
    if set -q __fish_vendor_confdirs
        set -g __aterm_start_roots "$__fish_sysconf_dir" $__fish_vendor_confdirs
    else if set -q vendor_confdirs
        set -g __aterm_start_roots "$__fish_sysconf_dir" $vendor_confdirs
    end
    set -l __aterm_spare_dir (string replace -r '/rekey/[^/]*$' '/spare' -- "$__aterm_rekey_path")
    set -l __aterm_spare_sid (string replace -r '^.*/' '' -- "$__aterm_rekey_path")
    if test -n "$__aterm_rekey_path"; and test -p "$__aterm_spare_dir/$__aterm_spare_sid"
        set -g __aterm_spare_fifo "$__aterm_spare_dir/$__aterm_spare_sid"
    end
end

# Body pointer path, captured and scrubbed so child processes do not inherit it.
if test -n "$ATERM_INTEGRATION_POINTER"; or not set -q __aterm_body_pointer
    set -g __aterm_body_pointer "$ATERM_INTEGRATION_POINTER"
end
set -e ATERM_INTEGRATION_POINTER 2>/dev/null

# Resolve the content-addressed folder (`<root>/<16-hex-addr>/`) this script was
# loaded from; empty for a hand-installed copy.
set -l __aterm_here (status dirname)
set -g __aterm_si_dir (string replace -r '/fish-xdg/fish/vendor_conf\.d$' '' -- "$__aterm_here")
set -g __aterm_si_root (string replace -r '/[^/]*$' '' -- "$__aterm_si_dir")
set -g __aterm_body_rev (string replace -r '^.*/' '' -- "$__aterm_si_dir")
if not string match -qr '^[0-9a-f]{16}$' -- "$__aterm_body_rev"
    set -g __aterm_body_rev ""
end

# Fresh loads never add `~/.aterm/bin` to PATH, so no legacy entry cleanup is needed.
if test "$__aterm_fresh_load" = 1
    set -g __aterm_old_bin_off 1
end

# Source package shell hooks once on fresh load.
if test "$__aterm_fresh_load" = 1; and test -d "$HOME/.aterm/shell.d"
    for f in $HOME/.aterm/shell.d/*.fish $HOME/.aterm/shell.d/*.sh
        if test -f "$f"
            source "$f"
        end
    end
end

# Check for a newer body pointer at each prompt and source it if owned by the
# current user (`test -O`) and addressed by a 16-hex digest.
function __aterm_body_check
    test -n "$__aterm_body_pointer"; and test -f "$__aterm_body_pointer"; or return 0
    set -l addr ""
    read -l addr <"$__aterm_body_pointer" 2>/dev/null
    command rm -f -- "$__aterm_body_pointer"
    string match -qr '^[0-9a-f]{16}$' -- "$addr"; or return 0
    test "$addr" != "$__aterm_body_rev"; or return 0
    set -l file "$__aterm_si_root/$addr/aterm_shell_integration_body.fish"
    test -f "$file"; and test -O "$file"; or return 0
    if source "$file"
        set -g __aterm_body_rev "$addr"
    end
    return 0
end

# @@ATERM-INTEGRATION-BODY-BEGIN@@ — everything from here to the END marker is re-sourced live

# Enable agy's fast prompt editor by default so U+202F narrow no-break spaces in
# macOS screenshot filenames do not desynchronize Bubble Tea's textarea cursor.
if not set -q AGY_CLI_FAST_PROMPT_EDITOR
    set -gx AGY_CLI_FAST_PROMPT_EDITOR true
end

# One-shot cleanup of legacy `~/.aterm/bin` PATH entries for shells upgraded in
# place (`__aterm_old_bin_off` unset). Kept in a sidecar file so fresh shells do
# not pay to parse it.
if not set -q __aterm_old_bin_off
    set -g __aterm_old_bin_off 1
    set -l __aterm_here (status dirname)
    set -l __aterm_dir (string replace -r '/fish-xdg/fish/vendor_conf\.d$' '' -- "$__aterm_here")
    set -l __aterm_file "$__aterm_dir/aterm_shell_integration_old_bin.fish"
    if test -f "$__aterm_file"; and test -O "$__aterm_file"
        source "$__aterm_file"
    end
    set -e __aterm_bin_from
end

# Keep `$ATERM_REROUTE_DIR` first on PATH, followed by `$ATPKG_AGENTS`.
# Fish loads vendor conf.d before `~/.config/fish/config.fish`, where user
# `fish_add_path` calls prepend to PATH, so this moves both directories to the
# front (removing existing occurrences via exact equality rather than wildcard
# `string match -v`) at load and again on the first prompt event.
function __aterm_path_front --argument-names dir
    set -l rest
    for d in $PATH
        if test "$d" != "$dir"
            set -a rest "$d"
        end
    end
    set -gx PATH "$dir" $rest
end
# Track expected leading directories in `$__aterm_managed_want` and whether each
# existed when last fronted so the per-prompt hot path only stats when a
# directory was absent.
set -g __aterm_managed_want
set -g __aterm_managed_agents_on 0
set -g __aterm_managed_reroute_on 0
function __aterm_reroute_path_front
    set -g __aterm_managed_want
    set -g __aterm_managed_agents_on 0
    set -g __aterm_managed_reroute_on 0
    if test -n "$ATPKG_AGENTS"; and test -d "$ATPKG_AGENTS"
        __aterm_path_front "$ATPKG_AGENTS"
        set -g __aterm_managed_want "$ATPKG_AGENTS"
        set -g __aterm_managed_agents_on 1
    end
    if test -n "$ATERM_REROUTE_DIR"; and test -d "$ATERM_REROUTE_DIR"
        __aterm_path_front "$ATERM_REROUTE_DIR"
        set -g __aterm_managed_want "$ATERM_REROUTE_DIR" $__aterm_managed_want
        set -g __aterm_managed_reroute_on 1
    end
end
__aterm_reroute_path_front
# Keep atpkg's managed `bin/` at the end of PATH after all startup files have run
# so it never shadows user or system directories.
function __aterm_managed_bin_last
    test -n "$ATPKG_BIN"; and test "$PATH[-1]" != "$ATPKG_BIN"; and contains -- "$ATPKG_BIN" $PATH
    or return 0
    set -l rest
    for d in $PATH
        if test "$d" != "$ATPKG_BIN"
            set -a rest "$d"
        end
    end
    set -gx PATH $rest "$ATPKG_BIN"
end
function __aterm_reroute_first_prompt --on-event fish_prompt
    functions -e __aterm_reroute_first_prompt
    __aterm_reroute_path_front
    __aterm_managed_bin_last
end

# Live managed PATH re-assertion on `fish_prompt` and `fish_preexec` (builtin-only:
# `test`, `set`, `math`, `read`, `source` — no external forks):
#  1. Re-source `~/.aterm/shell.d/00-atpkg.fish` when its on-disk content changes
#     (compared via builtin `read -z`).
#  2. Re-probe `$ATPKG_AGENTS` / `$ATERM_REROUTE_DIR` if absent when last laid.
#  3. Re-front `$__aterm_managed_want` if the head of `$PATH` drifted.
# Gated on `$ATERM_CHILD` or `$ATERM_SESSION_ID`.
set -g __aterm_atpkg_hook "$HOME/.aterm/shell.d/00-atpkg.fish"
set -g __aterm_managed_live 0
if test -n "$ATERM_CHILD"; or test -n "$ATERM_SESSION_ID"
    set -g __aterm_managed_live 1
end

set -g __aterm_atpkg_hook_now ""
function __aterm_atpkg_hook_read
    set -g __aterm_atpkg_hook_now ""
    if test -f "$__aterm_atpkg_hook"
        read -z -g __aterm_atpkg_hook_now < "$__aterm_atpkg_hook"
    end
    return 0
end
if not set -q __aterm_atpkg_hook_seen
    set -g __aterm_atpkg_hook_seen ""
    if test "$__aterm_managed_live" = 1
        __aterm_atpkg_hook_read
        set -g __aterm_atpkg_hook_seen "$__aterm_atpkg_hook_now"
    end
end

# Derive `$ATERM_REROUTE_DIR` from `<parent of $ATPKG_AGENTS>/reroute` when the
# shell predates `$ATERM_REROUTE_DIR` and passthrough is not engaged.
function __aterm_managed_derive_reroute
    if test -n "$ATERM_REROUTE_DIR"; or test -z "$ATPKG_AGENTS"
        return 1
    end
    if test -n "$__ATERM_REROUTE_PASSTHROUGH"; and test "$__ATERM_REROUTE_PASSTHROUGH" != 0
        return 1
    end
    set -l parent (string replace -r '/[^/]*$' '' -- "$ATPKG_AGENTS")
    set -l dir "$parent/reroute"
    if not test -d "$dir"
        return 1
    end
    set -gx ATERM_REROUTE_DIR "$dir"
    return 0
end
if test "$__aterm_managed_live" = 1; and __aterm_managed_derive_reroute
    __aterm_reroute_path_front
end

function __aterm_managed_path_live
    if test "$__aterm_managed_live" != 1
        return 0
    end
    __aterm_atpkg_hook_read
    if test "$__aterm_atpkg_hook_now" != "$__aterm_atpkg_hook_seen"
        set -g __aterm_atpkg_hook_seen "$__aterm_atpkg_hook_now"
        if test -n "$__aterm_atpkg_hook_now"
            source "$__aterm_atpkg_hook"
            __aterm_managed_derive_reroute
            __aterm_reroute_path_front
            return 0
        end
    end
    set -l refront 0
    if test "$__aterm_managed_agents_on" != 1; and test -n "$ATPKG_AGENTS"; and test -d "$ATPKG_AGENTS"
        set refront 1
    end
    if test "$__aterm_managed_reroute_on" != 1
        if test -n "$ATERM_REROUTE_DIR"
            if test -d "$ATERM_REROUTE_DIR"
                set refront 1
            end
        else if __aterm_managed_derive_reroute
            set refront 1
        end
    end
    if test "$refront" = 1
        __aterm_reroute_path_front
        return 0
    end
    set -l i 1
    for d in $__aterm_managed_want
        if test "$PATH[$i]" != "$d"
            __aterm_reroute_path_front
            return 0
        end
        set i (math $i + 1)
    end
    return 0
end

# State tracking preserved across live body reloads. `$__aterm_last_status` is a
# single scalar `$status`; `$__aterm_last_pipestatus` holds `$pipestatus`.
if not set -q __aterm_last_status
    set -g __aterm_last_status 0
end
if not set -q __aterm_prompt_gen
    set -g __aterm_prompt_gen 0
end

# OSC escape sequences
function __aterm_osc
    printf '\e]%s\a' $argv[1]
end

# Resolve host identity once at load time into string globals (`$HOST`,
# `$HOSTNAME`, fish's `$hostname`, `command hostname` if installed, else
# `localhost`) so OSC 7 and prompts never glue a command substitution directly
# into an argument (which in fish collapses to zero arguments if empty and
# aborts if `hostname` is not installed).
set -g __aterm_report_host localhost
if test -n "$HOST"
    set -g __aterm_report_host "$HOST"
else if test -n "$HOSTNAME"
    set -g __aterm_report_host "$HOSTNAME"
else if test -n "$hostname"
    set -g __aterm_report_host "$hostname"
else if command -sq hostname
    set -l __aterm_host_probe (command hostname 2>/dev/null)
    if test -n "$__aterm_host_probe"
        set -g __aterm_report_host "$__aterm_host_probe"
    end
end

# Short (first DNS label) hostname for prompt styles.
set -g __aterm_report_host_short "$__aterm_report_host"
set -l __aterm_host_first_label (string replace -r '\..*' '' -- "$__aterm_report_host")
if test -n "$__aterm_host_first_label"
    set -g __aterm_report_host_short "$__aterm_host_first_label"
end

# Check the re-key channel. When called inside a pre-loader prompt wrapper
# copy (`__fish_prompt_orig` / `_old_fish_prompt`), also preserve `$status` and
# `$pipestatus` for the inner prompt draw.
function __aterm_rekey_check
    set -l st $status $pipestatus
    __aterm_prompt_inner; and set -g __aterm_prompt_status $st
    test -n "$__aterm_rekey_path"; and test -f "$__aterm_rekey_path"; or return 0
    set -l key ""
    read -l key <"$__aterm_rekey_path" 2>/dev/null
    command rm -f -- "$__aterm_rekey_path"
    if string match -qr '^[0-9a-f]{64}$' -- "$key"
        set -g __aterm_shell_nonce "$key"
        set -g __aterm_id_suffix_str ";id=$key"
    end
end

# Capability-nonce suffix helper for external callers; hot emitters interpolate
# `$__aterm_id_suffix_str` directly.
function __aterm_id_suffix
    if test -n "$__aterm_shell_nonce"
        printf ';id=%s' "$__aterm_shell_nonce"
    end
end

# Percent-encode a string for file:// URIs (RFC 3986), converting query-string
# `+` for spaces to `%20`.
function __aterm_urlencode
    string escape --style=url -- $argv[1] | string replace -a '+' '%20'
end

# Report current working directory (signed 633;P;AtermCwd + OSC 7).
function __aterm_report_cwd
    set -l cwd (__aterm_urlencode "$PWD")
    set -l uri "file://$__aterm_report_host$cwd"
    if test -n "$__aterm_id_suffix_str"
        printf '\e]633;P;AtermCwd=%s%s\a\e]7;%s\a' "$uri" "$__aterm_id_suffix_str" "$uri"
    else
        __aterm_osc "7;$uri"
    end
end

# Mark prompt start (OSC 133;A); suppressed during nested prompt draws.
function __aterm_mark_prompt_start
    __aterm_prompt_inner; and return 0
    __aterm_osc "133;A$__aterm_id_suffix_str"
end

# Mark command line start (OSC 133;B). Signed marks carry `aterm_input=start`
# so typing prediction knows the input buffer starts at the cursor.
function __aterm_mark_command_start
    __aterm_prompt_inner; and return 0
    if test -n "$__aterm_id_suffix_str"
        __aterm_osc "133;B$__aterm_id_suffix_str;aterm_input=start"
    else
        __aterm_osc "133;B$__aterm_id_suffix_str"
    end
end

# Mark command execution start (OSC 133;C)
function __aterm_mark_exec_start
    __aterm_osc "133;C$__aterm_id_suffix_str"
end

# Mark command completion (OSC 133;D;exitcode)
function __aterm_mark_exec_finish
    __aterm_osc "133;D;$argv[1]$__aterm_id_suffix_str"
end

# Report the active integration body revision (signed 633;P;AtermIntegration).
function __aterm_mark_integration_rev
    test -n "$__aterm_body_rev"; or return 0
    __aterm_osc "633;P;AtermIntegration=$__aterm_body_rev$__aterm_id_suffix_str"
end

# Format a 0-255 palette index as `\e[38;5;Nm` (fish's `set_color` only accepts
# color names and 6-digit hex triples, rejecting palette indices with stderr and
# empty output). Non-numeric values still delegate to `set_color`.
function __aterm_prompt_color
    if string match -qr '^[0-9]{1,3}$' -- "$argv[1]"
        printf '\e[38;5;%sm' "$argv[1]"
    else
        set_color "$argv[1]"
    end
end

# Emit `\e[0m` to match bash/zsh SGR reset bytes (`set_color normal` emits terminfo sgr0).
function __aterm_prompt_color_reset
    printf '\e[0m'
end

# When ATERM_PROMPT_STYLE is set, render the prompt using palette-indexed colors.
function __aterm_custom_prompt
    set -l style "$ATERM_PROMPT_STYLE"
    set -l hc (test -n "$ATERM_PROMPT_HOST_COLOR"; and echo $ATERM_PROMPT_HOST_COLOR; or echo 2)
    set -l pc (test -n "$ATERM_PROMPT_PATH_COLOR"; and echo $ATERM_PROMPT_PATH_COLOR; or echo 4)
    set -l gc (test -n "$ATERM_PROMPT_GIT_COLOR"; and echo $ATERM_PROMPT_GIT_COLOR; or echo 3)
    set -l ec (test -n "$ATERM_PROMPT_ERROR_COLOR"; and echo $ATERM_PROMPT_ERROR_COLOR; or echo 1)
    set -l sc (test -n "$ATERM_PROMPT_SEP_COLOR"; and echo $ATERM_PROMPT_SEP_COLOR; or echo 8)

    set -l prompt_color $sc
    if test "$__aterm_last_status[1]" -ne 0
        set prompt_color $ec
    end

    # Capture each segment into a local and interpolate quoted so an empty
    # output never drops an argument from `printf`.
    set -l h (__aterm_prompt_color $hc)
    set -l p (__aterm_prompt_color $pc)
    set -l g (__aterm_prompt_color $gc)
    set -l s (__aterm_prompt_color $sc)
    set -l e (__aterm_prompt_color $prompt_color)
    set -l r (__aterm_prompt_color_reset)
    set -l cwd (prompt_pwd)

    set -l user "$USER"
    if test -z "$user"; and command -sq whoami
        set user (whoami)
    end

    set -l git_text ""
    if command -sq git
        set -l branch (command git rev-parse --abbrev-ref HEAD 2>/dev/null)
        if test -n "$branch"
            set git_text " ($branch)"
        end
    end
    set -l git_seg "$g$git_text$r"

    switch "$style"
        case minimal
            printf '%s%s%s %s$%s ' "$p" "$cwd" "$r" "$e" "$r"
        case standard
            printf '%s%s@%s%s:%s%s%s%s %s$%s ' \
                "$h" "$user" "$__aterm_report_host_short" \
                "$s" \
                "$p" "$cwd" \
                "$r" "$git_seg" \
                "$e" "$r"
        case powerline
            set -l sep "$s$r"
            printf '%s%s@%s%s %s %s%s%s%s %s %s$%s ' \
                "$h" "$user" "$__aterm_report_host_short" \
                "$r" "$sep" \
                "$p" "$cwd" \
                "$r" "$git_seg" "$sep" \
                "$e" "$r"
    end
end

# User prompt wrapping and dispatch:
#  - Every trampoline captures `$status $pipestatus` on entry into
#    `$__aterm_prompt_status`, and `__aterm_prompt_dispatch` restores both via a
#    pipeline of `__aterm_return` calls before invoking the user's prompt with
#    `$argv` (preserving fish 4.1 `--final-rendering` for transient prompts).
#  - If `config.fish` or a command replaces `fish_prompt`, `__aterm_prompt_wrap`
#    copies it to `__aterm_fish_prompt_<n>` and installs a trampoline naming it.
#  - Before each command (`__aterm_prompt_give_back`), `fish_prompt` is restored
#    to the unwrapped function so `funcsave`, `funced`, and virtualenv wrappers
#    see the user's own prompt definition and source file.
#  - `__aterm_prompt_inner` checks an exported local (`$__aterm_prompt_drawing`)
#    on `__aterm_prompt_dispatch`'s frame to suppress duplicate 133;A/133;B marks
#    and bound recursion at depth 4, cleaning up automatically on Ctrl-C.

function __aterm_return
    return $argv[1]
end

function __aterm_prompt_fallback
    set -l user "$USER"
    if test -z "$user"; and command -sq whoami
        set user (whoami)
    end
    set -l cwd (prompt_pwd)
    printf '%s@%s %s $ ' "$user" "$__aterm_report_host" "$cwd"
end

function __aterm_prompt_inner
    set -ql __aterm_prompt_drawing
end

function __aterm_prompt_dispatch
    set -l fn __aterm_fish_prompt_0
    if string match -qr '^__aterm_fish_prompt_[0-9]+$' -- "$__aterm_prompt_fn"
        and functions -q $__aterm_prompt_fn
        set fn $__aterm_prompt_fn
    end
    set -e __aterm_prompt_fn
    set -l st $__aterm_prompt_status
    set -q st[1]; or set st $__aterm_last_status $__aterm_last_pipestatus
    set -l ps $st[2..-1]
    set -q ps[1]; or set ps $st[1]
    set -l run (string join ' | ' -- '__aterm_return '$ps)
    if test "$st[1]" != "$ps[-1]"
        set run "not $run"
    end
    string match -qr '^(not )?__aterm_return [0-9]+( \| __aterm_return [0-9]+)*$' -- "$run"
    or set run true
    set -l depth 0
    __aterm_prompt_inner; and set depth $__aterm_prompt_drawing
    if test "$depth" -ge 4; or not functions -q $fn
        __aterm_prompt_fallback
        return 0
    end
    # Write all captured prompt lines in a single piece so prompts that pipe
    # through `string collect` / `string unescape` do not split onto a new line.
    set -lx __aterm_prompt_drawing (math $depth + 1)
    set -l lines (eval "$run; $fn \$argv")
    set -q lines[1]; and printf '%s' $lines[1]
    set -q lines[2]; and printf '\n%s' $lines[2..-1]
    return 0
end

# Move the load-time prompt to `__aterm_fish_prompt_0` and install
# `__aterm_prompt_dispatch` as `__aterm_original_fish_prompt`. If the load-time
# prompt was itself a saved trampoline, restore fish's default `fish_prompt`.
function __aterm_prompt_shim
    functions --no-details __aterm_original_fish_prompt 2>/dev/null | string match -q -- '*__aterm_prompt_dispatch*'
    and return 0
    if functions -q __aterm_original_fish_prompt; and not functions -q __aterm_fish_prompt_0
        functions -c __aterm_original_fish_prompt __aterm_fish_prompt_0
    end
    functions -e __aterm_original_fish_prompt
    function __aterm_original_fish_prompt
        __aterm_prompt_dispatch $argv
    end
    if functions --no-details __aterm_fish_prompt_0 2>/dev/null | string match -q -- '*__aterm_body_prompt*'
        functions -e __aterm_fish_prompt_0
        status get-file functions/fish_prompt.fish 2>/dev/null \
            | string replace -r -- '^function fish_prompt\b' 'function __aterm_fish_prompt_0' | source
    end
    return 0
end

function __aterm_prompt_wrap
    functions -q fish_prompt; or return 0
    functions --no-details fish_prompt | string match -q -- '*__aterm_body_prompt*'
    and return 0
    __aterm_prompt_shim
    set -l fn __aterm_fish_prompt_$__aterm_prompt_gen
    set -l now (functions --no-details fish_prompt)[2..-1]
    set -l was (functions --no-details $fn 2>/dev/null)[2..-1]
    if test "$__aterm_prompt_gen" = 0; or test "$now" != "$was"
        set -g __aterm_prompt_gen (math $__aterm_prompt_gen + 1)
        set fn __aterm_fish_prompt_$__aterm_prompt_gen
        functions -e $fn
        functions -c fish_prompt $fn; or return 0
    end
    functions -e fish_prompt
    eval "function fish_prompt
    set -g __aterm_prompt_status \$status \$pipestatus
    set -g __aterm_prompt_fn $fn
    __aterm_body_check
    __aterm_body_prompt \$argv
end"
end
function __aterm_prompt_wrap_first --on-event fish_prompt
    functions -e __aterm_prompt_wrap_first
    __aterm_prompt_wrap
    __aterm_prompt_give_back_arm
end
function __aterm_prompt_wrapped
    set -l text (functions --no-details fish_prompt 2>/dev/null)
    string match -q -- '*__aterm_body_prompt*' $text; or return 1
    set -l fn (string replace -rf -- '^\s*set -g __aterm_prompt_fn (__aterm_fish_prompt_[0-9]+)\s*$' '$1' $text)
    set -q fn[1]; or set fn __aterm_fish_prompt_0
    functions -q $fn[1]; or return 1
    printf '%s\n' $fn[1]
end
# Define `__aterm_prompt_give_back` from a prompt or postexec context (outside
# any sourced file) so fish 4's `functions -c` preserves the user's original
# source file attribution for `funced`.
function __aterm_prompt_give_back_arm
    set -g __aterm_prompt_rewrap_rev "$__aterm_body_rev"
    functions -q __aterm_prompt_give_back; and return 0
    function __aterm_prompt_give_back --on-event fish_preexec
        test "$__aterm_prompt_rewrap_rev" = "$__aterm_body_rev"; or return 0
        functions -q __aterm_prompt_wrapped; or return 0
        set -l fn (__aterm_prompt_wrapped); or return 0
        functions -e fish_prompt
        functions -c $fn fish_prompt
    end
end
function __aterm_prompt_unwrap
    functions -q __aterm_prompt_give_back; and return 0
    set -l fn (__aterm_prompt_wrapped); or return 0
    functions -e fish_prompt
    functions -c $fn fish_prompt
end
if functions -q __aterm_fish_prompt_trampoline
    __aterm_prompt_wrap
end

function __aterm_body_prompt
    if __aterm_prompt_inner
        __aterm_original_fish_prompt $argv
        return 0
    end
    __aterm_prompt_give_back_arm
    __aterm_rekey_check
    __aterm_managed_path_live
    __aterm_report_cwd
    __aterm_mark_integration_rev
    __aterm_mark_prompt_start

    # Set tab title to abbreviated CWD (OSC 0), stripping control bytes to
    # prevent OSC break-out from directory names.
    if test -z "$ATERM_DISABLE_PROMPT_TITLES"
        set -l title "$PWD"
        if string match -q "$HOME/*" $PWD; or test "$PWD" = "$HOME"
            set -l rel (string sub -s (math (string length -- "$HOME") + 1) -- $PWD)
            set title "~$rel"
        end
        set -l safe_title (string replace -ra '[\x00-\x1f\x7f]' '' -- "$title")
        __aterm_osc "0;$safe_title"
    end

    __aterm_prompt_shim
    if test -n "$ATERM_PROMPT_STYLE"; and test "$ATERM_PROMPT_STYLE" != "none"
        __aterm_custom_prompt
    else
        __aterm_original_fish_prompt $argv
    end

    __aterm_mark_command_start
end

# Encode a string for OSC 633;E (VS Code convention, reversed by
# `unescape_vscode_string`): backslash-hex encodes `\`, `;`, space, and all
# control bytes (`0x00-0x1f`, `0x7f`) so raw ESC/BEL cannot break out of the OSC
# sequence. Operates on whole lines via `string replace -a` with a fast path
# when no reserved bytes are present.
function __aterm_encode_cmd
    if not string match -qr '[\\\\; \x00-\x1f\x7f]' -- "$argv"
        printf '%s' "$argv"
        return 0
    end
    set -l lines (string split -- \n "$argv")
    set lines (string replace -a -- '\\' '\\\\' $lines)
    set lines (string replace -a -- ';' '\x3b' $lines)
    set lines (string replace -a -- ' ' '\x20' $lines)
    set -l done
    for c in (string match -ar '[\x00-\x1f\x7f]' -- $lines)
        contains -- "$c" $done; and continue
        set -a done "$c"
        set -l hex (string escape --style=url -- "$c" | string sub -s 2 | string lower)
        set lines (string replace -a -- "$c" "\\x$hex" $lines)
    end
    string join -- '\x0a' $lines
end

# Emit the signed startup record (`AtermStartRoots`, `AtermStartRc`,
# `AtermStartFlags`, `AtermStart`) at the first prompt event
# (docs/DESIGN-warm-spare-2026-10-16.md). Startup roots are joined by `\x00`.
function __aterm_start_mark --argument-names kind pre
    set -l rc "$__fish_config_dir"
    set -l folders $__aterm_start_roots
    if string match -qr '[\\\\; \x00-\x1f\x7f]' -- "$rc" $folders
        string match -qr '[\\\\; \x00-\x1f\x7f]' -- "$rc"; and set rc (__aterm_encode_cmd "$rc")
        if string match -qr '[\x00-\x1f\x7f]' -- $folders
            set -l escaped
            for folder in $folders
                set -l one (__aterm_encode_cmd "$folder")
                set -a escaped "$one"
            end
            set folders $escaped
        else
            set folders (string replace -a -- '\\' '\\\\' $folders | string replace -a -- ';' '\x3b')
        end
    end
    set -l roots
    if set -q __aterm_start_roots
        set -l joined (string join '\x00' -- $folders)
        set roots \e']633;P;AtermStartRoots='"$joined$__aterm_id_suffix_str"\a
    end
    set -l jobs 0
    jobs -q; and set jobs (count (jobs -p))
    if test -z "$pre"
        set pre 0
        set -l ahead (__aterm_prompt_handlers_ahead); and set pre (count $ahead)
    end
    printf '%s\e]633;P;AtermStartRc=%s%s\a\e]633;P;AtermStartFlags=pre:%s,s:0,j:%s,r:0%s\a\e]633;P;AtermStart=%s%s\a' \
        "$roots" "$rc" "$__aterm_id_suffix_str" "$pre" "$jobs" "$__aterm_id_suffix_str" \
        "$kind" "$__aterm_id_suffix_str"
end

# List user `fish_prompt` event handlers defined before `__aterm_fish_prompt_event`.
function __aterm_prompt_handlers_ahead
    set -l handlers (functions --handlers-type generic)
    string match -qr -- '(^| )fish_prompt (?!__aterm_).* fish_prompt __aterm_fish_prompt_event( |$)' "$handlers"
    or return 1
    set -l mine (contains -i -- "fish_prompt __aterm_fish_prompt_event" $handlers); and set -e handlers[$mine..-1]
    string replace -rf -- '^fish_prompt (?!__aterm_)' '' $handlers
end

# Warm spare hold at the first prompt event: emit the `spare` start mark, wait
# for `go` on `$__aterm_spare_fifo`, then merge history and re-run prior
# `fish_prompt` handlers upon release.
function __aterm_spare_hold
    set -l ahead (__aterm_prompt_handlers_ahead)
    __aterm_start_mark spare (count $ahead)
    set -l n 0
    while true
        if read -l go <"$__aterm_spare_fifo" 2>/dev/null
            test "$go" = go; and break
        else
            set n (math $n + 1)
            if test $n -ge 50; or not test -p "$__aterm_spare_fifo"
                exit 0
            end
        end
    end
    set -e __aterm_spare_fifo
    history merge 2>/dev/null
    for handler in $ahead
        $handler >/dev/null 2>&1
    end
end

# fish_preexec - runs before command execution
function __aterm_fish_preexec --on-event fish_preexec
    __aterm_prompt_unwrap
    __aterm_managed_path_live

    set -l encoded (__aterm_encode_cmd "$argv")
    __aterm_osc "633;E;$encoded$__aterm_id_suffix_str"
    # Set tab title to running command (OSC 0), stripped of control bytes and
    # truncated to 256 characters.
    if test -z "$ATERM_DISABLE_PROMPT_TITLES"
        set -l cmd_title (string replace -ra '[\x00-\x1f\x7f]' '' -- "$argv")
        if test (string length -- "$cmd_title") -gt 256
            set cmd_title (string sub -l 255 -- "$cmd_title")"…"
        end
        __aterm_osc "0;$cmd_title"
    end
    __aterm_mark_exec_start
end

# fish_postexec - runs after command execution
function __aterm_fish_postexec --on-event fish_postexec
    set -l st $status $pipestatus
    set -g __aterm_last_status $st[1]
    set -g __aterm_last_pipestatus $st[2..-1]
    set -e __aterm_prompt_status
    __aterm_prompt_wrap
    __aterm_prompt_give_back_arm
    __aterm_rekey_check
    __aterm_mark_exec_finish $__aterm_last_status
end

# Update cwd on directory change and at startup
function __aterm_fish_pwd --on-variable PWD
    __aterm_report_cwd
end

# Bind xterm-style modifier+arrow and navigation sequences once on fresh load.
function __aterm_setup_keybindings
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
end

# Return 0 so `__aterm_body_check` advances `$__aterm_body_rev`.
true

# @@ATERM-INTEGRATION-BODY-END@@

# Loader wiring (runs once per shell):
functions -c fish_prompt __aterm_original_fish_prompt 2>/dev/null

# Install the prompt trampoline on fresh load, or on in-place upgrade when
# `fish_prompt` is an older unwrapped aterm prompt function.
function __aterm_fish_prompt_trampoline
    set -g __aterm_prompt_status $status $pipestatus
    __aterm_body_check
    __aterm_body_prompt $argv
end
set -l __aterm_prompt_text (functions --no-details fish_prompt 2>/dev/null)
if test "$__aterm_fresh_load" = 1; or begin
        string match -q -- '*__aterm_*' $__aterm_prompt_text
        and not string match -q -- '*__aterm_prompt_fn*' $__aterm_prompt_text
    end
    functions -e fish_prompt
    functions -c __aterm_fish_prompt_trampoline fish_prompt
end

# Also run the body check and startup mark on the `fish_prompt` event so shells
# whose `config.fish` replaces `fish_prompt` still reload bodies and report revisions.
function __aterm_fish_prompt_event --on-event fish_prompt
    if set -q __aterm_start_pending
        set -e __aterm_start_pending
        if set -q __aterm_spare_fifo
            __aterm_spare_hold
        else
            __aterm_start_mark cold
        end
    end
    __aterm_body_check
    functions --no-details fish_prompt 2>/dev/null | string match -q -- '*__aterm_body_prompt*'
    or __aterm_mark_integration_rev
end

if test "$__aterm_fresh_load" = 1
    __aterm_setup_keybindings
end

# On an in-place upgrade, immediately emit CWD and the new integration revision
# before the relaunch command continues.
if test "$__aterm_fresh_load" = 0
    __aterm_report_cwd
    __aterm_mark_integration_rev
end
