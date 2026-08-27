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
#
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
# fish is the ONE shell where this block really does run in a pane, and the
# reason is worth writing down: aterm injects fish through a vendor conf.d on
# $XDG_DATA_DIRS, an ordinary EXPORTED variable, so it rides into a pane shell
# and fish sources this file there. bash and zsh are injected through argv
# (`--rcfile`) and $ZDOTDIR — one-shot mechanisms only the shell ATERM ITSELF
# starts receives — so their pane shells never source their script at all
# (measured in a real GNU screen 4.09.01 window under a headless aterm: STY, a
# screen TERM and the inherited guard all set, $ATERM_MUX still EMPTY, no hook
# defined), and `aterm ctl` carries the boundary for them.
#
# What runs in every session aterm starts, in all three shells, is the tail of
# this file past the guard, and that is where the detection now originates:
# $ATERM_MUX_BASE records the multiplexer environment THIS session shell was born
# into. See the export below.
set -l __aterm_mux ""
if test -n "$TMUX"
    set __aterm_mux tmux
else if test -n "$STY"
    set __aterm_mux screen
else
    # tmux's default TERM is screen-256color, so TERM alone names the family,
    # not the program; the markers above are consulted first for that reason.
    switch "$TERM"
        case tmux 'tmux-*' 'tmux.*'
            set __aterm_mux tmux
        case screen 'screen-*' 'screen.*'
            set __aterm_mux screen
    end
end

if test -n "$ATERM_SHELL_INTEGRATION_INSTALLED"
    # Skipping as before — but mark the boundary on the way out when the
    # inherited guard means we crossed one.
    if test -n "$__aterm_mux"
        set -gx ATERM_MUX $__aterm_mux
        if test -n "$ATERM_PARENT_SESSION_ID"
            set -gx ATERM_MUX_OUTER_SESSION_ID $ATERM_PARENT_SESSION_ID
            # Say it ONCE per multiplexer session — not once per pane, which is
            # the same true sentence six times before lunch. The stamp is keyed
            # by the multiplexer's own id ($TMUX / $STY), so every pane of one
            # screen or tmux shares it.
            if test "$ATERM_MUX_NOTICE" != 0
                set -l __aterm_mux_id "$TMUX"
                test -n "$__aterm_mux_id"; or set __aterm_mux_id "$STY"
                test -n "$__aterm_mux_id"; or set __aterm_mux_id "$TERM"
                set -l __aterm_mux_root "$XDG_RUNTIME_DIR"
                test -n "$__aterm_mux_root"; or set __aterm_mux_root "$TMPDIR"
                test -n "$__aterm_mux_root"; or set __aterm_mux_root /tmp
                # Captured into locals and interpolated QUOTED — never glued to
                # a command substitution, this script's standing rule (a
                # substitution printing nothing would take the whole argument
                # with it; pinned by test_fish_never_glues_a_command_substitution).
                set -l __aterm_mux_dir "$__aterm_mux_root/aterm/mux-notice"
                set -l __aterm_mux_key (string replace -ra '[^A-Za-z0-9._-]' _ -- "$__aterm_mux_id")
                set -l __aterm_mux_stamp "$__aterm_mux_dir/$__aterm_mux-$__aterm_mux_key"
                if not test -e "$__aterm_mux_stamp"
                    if mkdir -p "$__aterm_mux_dir" 2>/dev/null; and printf '' >"$__aterm_mux_stamp" 2>/dev/null
                        printf 'aterm: inside %s — command blocks, exit codes and cwd tracking do not cross the multiplexer,\n       so aterm records none of them for these panes. `aterm ctl mux` explains; ATERM_MUX_NOTICE=0 silences this.\n' "$__aterm_mux" >&2
                    end
                end
            end
        end
    end
    return
end
set -gx ATERM_SHELL_INTEGRATION_INSTALLED 1
# Past the guard, so aterm started this shell ITSELF. Record the multiplexer
# environment this session shell was born into. This is the one detection input
# that comes from a place which ACTUALLY RUNS for every session, and it reaches a
# pane the only way anything can: ordinary environment inheritance. A pane's own
# $TMUX/$STY are the multiplexer's and no longer match this base — that mismatch
# IS the crossing — while an aterm window merely launched FROM a pane re-runs
# this file and re-stamps the base as its own, so it matches and is not refused.
# It also closes what TERM cannot: a tmux set to default-terminal
# "xterm-256color" is indistinguishable from an aterm window to TERM, and plainly
# a pane to this. `aterm ctl` reads it as $ATERM_MUX_BASE.
#
# Interpolated inside ONE double-quoted word, never glued to a substitution:
# unset expands to the empty string here (fish's rule for an undefined variable
# inside quotes), which is exactly the "<$TMUX>|<$STY>" spelling bash and zsh
# write with ${TMUX-}, so the three shells stamp byte-identical values.
# Fallback only — the spawn seam stamps this for every session, including one
# whose shell never sources this file; overwriting it after a pane was entered
# is exactly how a fresh window came to be refused as a pane. (`test -n --` is
# the emptiness read this file now uses everywhere: `set -q` is definedness,
# and aterm deliberately exports empty strings.)
if not test -n "$ATERM_MUX_BASE"
    set -gx ATERM_MUX_BASE "$TMUX|$STY"
end
# Any ATERM_MUX inherited from the pane we were launched out of describes a
# multiplexer this session is not inside. Clear it, or every window opened from
# a tmux pane would inherit a refusal it does not deserve.
# Guarded with `test -n`, this file's rule for every ATERM_* variable (an
# unguarded `set -e` on an absent name is a silent status 4, and the guard also
# documents that EMPTY counts as "not marked" here, exactly as bash/zsh read it).
if test -n "$ATERM_MUX"
    set -e ATERM_MUX
end
if test -n "$ATERM_MUX_OUTER_SESSION_ID"
    set -e ATERM_MUX_OUTER_SESSION_ID
end

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

# ─── Host identity (OSC 7 authority + prompt) ───
#
# Resolved ONCE at load time into plain string globals, exactly like zsh's
# `typeset -g __aterm_report_host="${HOST:-${HOSTNAME:-localhost}}"`.
#
# THE GLUE RULE (this file's second cartesian-product casualty; see the
# `$__aterm_id_suffix_str` note below for the first). `__aterm_report_cwd` used
# to spell its payload as a string glued to a command substitution:
#
#     __aterm_osc "7;file://"(hostname)"$cwd"
#
# In fish a string glued to a command substitution is a CARTESIAN PRODUCT, and a
# substitution that prints nothing is a ZERO-ELEMENT list — so the product is
# ZERO arguments, `__aterm_osc` is called with no argv, and its
# `printf '\e]%s\a' $argv[1]` emits a bare, empty `ESC ] BEL`. Measured on fish
# 4.2.1 with a `hostname` that prints nothing: OSC 7 came out as exactly
# `\e]\a` — no scheme, no path, so the host learns NOTHING about the cwd and
# "Open Terminal Here" / tab titles / new-tab-inherits-cwd all go dead.
#
# And a host with NO `hostname` command at all (a container image, a slim
# base, a Nix profile without inetutils — `hostname` is not in POSIX and not in
# coreutils) is worse than silent: the substitution raises `Unknown command`, so
# fish ABORTS the whole statement and additionally prints a five-line error
# block — at source time AND on every single directory change.
#
# bash interpolates `${HOSTNAME:-$(hostname)}` and zsh a plain parameter, so
# neither shell has this failure mode; it is unique to fish's list semantics.
#
# Resolution order mirrors the other two shells and ends somewhere non-empty:
# `$HOST` then `$HOSTNAME` (zsh's and bash's spellings, and what the test
# harness pins), then fish's own `$hostname` global (fish 3.5+, no fork), then
# the `hostname` command — invoked ONLY behind `command -sq`, so a host without
# it gets the `localhost` default instead of an error storm — then `localhost`,
# which is also the canonical RFC 8089 `file://` authority.
set -g __aterm_report_host localhost
if test -n "$HOST"
    set -g __aterm_report_host "$HOST"
else if test -n "$HOSTNAME"
    set -g __aterm_report_host "$HOSTNAME"
else if test -n "$hostname"
    set -g __aterm_report_host "$hostname"
else if command -sq hostname
    # `set -l` first, then a QUOTED emptiness test: a `hostname` that exists but
    # prints nothing yields a zero-element list, and promoting that would put us
    # right back at an empty OSC 7 authority.
    set -l __aterm_host_probe (command hostname 2>/dev/null)
    if test -n "$__aterm_host_probe"
        set -g __aterm_report_host "$__aterm_host_probe"
    end
end

# Short (first DNS label) form — the equivalent of bash's `\h` and the
# `hostname -s` the prompt styles used to fork for. `string replace` always
# prints its input, so this is a one-element substitution by construction; the
# emptiness guard is belt-and-braces so the prompt can never inherit a
# zero-element list.
set -g __aterm_report_host_short "$__aterm_report_host"
set -l __aterm_host_first_label (string replace -r '\..*' '' -- "$__aterm_report_host")
if test -n "$__aterm_host_first_label"
    set -g __aterm_report_host_short "$__aterm_host_first_label"
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
    # `"$PWD"`, not `(pwd)` — bash and zsh both read the parameter, and a
    # quoted parameter is one argument no matter what, where a substitution
    # that printed nothing would hand `__aterm_urlencode` zero arguments (its
    # `string escape -- $argv[1]` would then have no operand and read STDIN).
    set -l cwd (__aterm_urlencode "$PWD")
    # file:// URL format. ONE double-quoted argument, interpolating the two
    # precomputed globals — never a string glued to a command substitution;
    # see the glue rule beside $__aterm_report_host above.
    __aterm_osc "7;file://$__aterm_report_host$cwd"
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

# ─── Prompt Colors ───
#
# Render a palette INDEX as an SGR 256-color escape — byte-for-byte the same
# bytes bash puts in PS1 (`\[\033[38;5;${hc}m\]`) and zsh in PROMPT.
#
# THE DEFECT THIS REPLACES. Every `ATERM_PROMPT_*_COLOR` is a palette index —
# that is what the knob means in bash and zsh, and it is what all five of this
# script's own defaults are (host=2, path=4, git=3, error=1, sep=8). fish's
# `set_color`, though, accepts colour NAMES (`red`, `brblack`) and hex triples
# (`a0a0a0`) ONLY. Handed an index it answers
#
#     set_color: Unknown color “4”
#
# on stderr, exits 2, and — the part that does the damage — emits NOTHING.
#
# Emitting nothing is not merely "the prompt is uncoloured". A `set_color` that
# prints nothing is a ZERO-ELEMENT command substitution, so every place the old
# prompt spliced one in lost an ARGUMENT, and `printf`'s remaining arguments all
# slid one position left. Measured on fish 4.2.1, style=standard, before this
# fix — four `Unknown color` lines dumped onto the user's terminal on EVERY
# prompt render, and then:
#
#     userdev-host@/t/c/-/s/w/shell-render:  $
#
# instead of `user@dev-host:~/…`. The `@` and the `:` had migrated to the
# wrong side of their operands because the colour arguments they were supposed
# to follow no longer existed. powerline lost both separators outright.
#
# So: format the escape ourselves with `printf`, which always emits its literal
# bytes and therefore can never be a zero-element substitution. A non-numeric
# value still routes to `set_color`, so fish users who set a NAME or a hex
# triple keep working — bash would emit `\033[38;5;redm` for those, so there is
# no cross-shell output to match there, and fish may as well do the right
# thing. `[0-9]{1,3}` is the index form: 1–3 digits is what a 0-255 palette
# index looks like, and it leaves 6-digit hex to `set_color`.
function __aterm_prompt_color
    if string match -qr '^[0-9]{1,3}$' -- "$argv[1]"
        printf '\e[38;5;%sm' "$argv[1]"
    else
        set_color "$argv[1]"
    end
end

# Reset — bash's and zsh's `\033[0m`, not fish's `set_color normal` (which
# emits terminfo `sgr0`, i.e. `\e[m` on this terminal). Same meaning, but the
# point of this section is that the three shells put the SAME bytes on the wire.
function __aterm_prompt_color_reset
    printf '\e[0m'
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

    # EVERY segment is captured into a local and interpolated QUOTED below.
    #
    # This is the glue rule again (see $__aterm_report_host), in its
    # argument-position form. A bare `(cmd)` sitting in a `printf` argument list
    # is not one argument — it is however many lines `cmd` printed, which for a
    # failing `set_color`, a missing `hostname`, or a `whoami` that is not
    # installed is ZERO. Every later argument then shifts left and `printf`
    # pairs them with the wrong `%s`. `"$var"` is always exactly one argument,
    # empty value or not, so the layout survives even when a segment does not.
    set -l h (__aterm_prompt_color $hc)
    set -l p (__aterm_prompt_color $pc)
    set -l g (__aterm_prompt_color $gc)
    set -l s (__aterm_prompt_color $sc)
    set -l e (__aterm_prompt_color $prompt_color)
    set -l r (__aterm_prompt_color_reset)
    set -l cwd (prompt_pwd)

    # bash's `\u`: fish keeps the login name in $USER, so the common path costs
    # no fork; `whoami` is the fallback, guarded so a host without it degrades
    # to an empty user segment rather than an `Unknown command` error.
    set -l user "$USER"
    if test -z "$user"; and command -sq whoami
        set user (whoami)
    end

    # bash's git segment is `${g}$(__aterm_git_segment)${r}` — the colour and
    # the reset are emitted whether or not there is a branch, and the leading
    # space lives INSIDE them. Match that exactly: it makes fish's SGR sequence
    # identical to bash's in a non-repo directory as well as in a repo, and the
    # no-branch case is two invisible escapes, the same two bash already writes.
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
    #
    # The stripped title is captured into a QUOTED local rather than glued onto
    # `"0;"`. `string` is a builtin that always prints exactly one line, so this
    # pair could not collapse the way `(hostname)` did — but the glued spelling
    # is the shape that has now cost this file three separate outages, and the
    # next editor who swaps in an external command should not be able to
    # reintroduce it. Pinned by `test_fish_never_glues_a_command_substitution`.
    if test -z "$ATERM_DISABLE_PROMPT_TITLES"
        set -l title "$PWD"
        if string match -q "$HOME/*" $PWD; or test "$PWD" = "$HOME"
            set -l rel (string sub -s (math (string length -- "$HOME") + 1) -- $PWD)
            set title "~$rel"
        end
        set -l safe_title (string replace -ra '[\x00-\x1f\x7f]' '' -- "$title")
        __aterm_osc "0;$safe_title"
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
        # Fallback minimal prompt.
        #
        # Was `echo -n (whoami)'@'(hostname)' '(prompt_pwd)' $ '` — four glued
        # command substitutions, so a single one of them printing nothing (a
        # host with no `hostname`, the commonest case) collapsed the cartesian
        # product to zero arguments and `echo -n` printed NOTHING AT ALL: no
        # user, no host, no path, no `$`. Measured on fish 4.2.1 — the entire
        # fallback prompt vanished, leaving a bare line between the 133;A and
        # 133;B marks. Quoted interpolation of captured locals instead.
        set -l user "$USER"
        if test -z "$user"; and command -sq whoami
            set user (whoami)
        end
        set -l cwd (prompt_pwd)
        printf '%s@%s %s $ ' "$user" "$__aterm_report_host" "$cwd"
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
                #
                # Captured, then interpolated quoted — never glued. A glued
                # substitution here is the worst of the family: `set result`
                # with zero arguments does not append nothing, it WIPES the
                # whole accumulated string built so far.
                if string match -qr '[\x00-\x1f\x7f]' -- "$i"
                    set -l hex (string escape --style=url -- "$i" | string sub -s 2 | string lower)
                    set result "$result\\x$hex"
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
        set -l cmd_title (string sub -l 64 -- "$argv" | string replace -ra '[\x00-\x1f\x7f]' '')
        __aterm_osc "0;$cmd_title"
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
