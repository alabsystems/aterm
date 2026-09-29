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
# - The managed dirs, LIVE: an already-running session shell resolves `claude`/`codex`
#   to atpkg's <prefix>/agents twins (and cargo/rustc to <prefix>/reroute) the moment
#   atpkg lays them — no new tab, no `exec zsh` (owner ask 2026-09-16; see "LIVE" below)
# - A LOADER and a BODY (2026-09-26): the shell that is ALREADY RUNNING takes a newer
#   build's integration at its next prompt, when the host that owns it points it there
#   (see "THE LOADER" below)
# - The tty settings, KEPT (2026-09-26): a raw-mode program that dies without restoring
#   them no longer leaves Ctrl-C and Enter broken for every later command (`ttyctl -f`,
#   with `stty`/`reset`/`tset` thawing it — see "The tty settings the shell keeps"
#   below)
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

# THE IN-PLACE UPGRADE (2026-09-26). The guard below is exported, so a NON-empty
# value normally proves this shell did not come straight from aterm (a pane — see
# above). There is one other way to arrive here with it set: THIS VERY SHELL
# sourcing the file again — the live agent upgrade's relaunch line sources a newer
# build's loader into a shell spawned before loaders existed (`typed_rekey` in
# aterm-shell-integration), or a user's rc that sources this file is re-read. That
# shell, and only that shell, already holds `$__aterm_shell_nonce`: a plain global,
# never exported, so no pane or child shell can have inherited it. For it the guard
# is its own, and the load goes on as an upgrade in place: everything below is
# idempotent, and what must run once per shell (the package blocks, the hook and
# widget wiring) is skipped where it already ran.
typeset -gi __aterm_fresh_load=1
if [[ -n "${ATERM_SHELL_INTEGRATION_INSTALLED:-}" ]] && (( ${+__aterm_shell_nonce} )); then
    __aterm_fresh_load=0
    ATERM_SHELL_INTEGRATION_INSTALLED=
fi

# Skip if already loaded — marking the boundary on the way out when the
# inherited guard means we crossed one.
if [[ -n "${ATERM_SHELL_INTEGRATION_INSTALLED:-}" ]]; then
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
                    printf 'aterm: inside %s — no command blocks, exit codes or cwd tracking in these panes (`aterm ctl mux` explains; ATERM_MUX_NOTICE=0 silences)\n' "$__aterm_mux" >&2
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

# ─── THE LOADER (2026-09-26) ───
#
# Everything from here to the BODY marker, and the wiring after the body's end
# marker, is the LOADER: what a shell runs once and keeps for its whole life — the
# nonce and the two host channels captured and scrubbed, the one precmd trampoline,
# the hook registrations. Everything between the two markers is the BODY: every
# mark, every prompt behaviour, the managed-PATH machinery — and it is written a
# second time, alone, as `aterm_shell_integration_body.zsh` in the same
# content-addressed folder, so a shell that is already running can source a NEWER
# build's body at its next prompt.
#
# WHY (gap 20 of the 2026-09-24 audit): an integration script used to be fixed at
# spawn. Only the nonce (the re-key channel) and PATH order were live, so every fix
# to the marks or anything else reached only NEW tabs — and the long-lived agent
# tabs (2 and 14 days old on the owner's Mac) kept the old code, with no status
# word saying so.
#
# HOW A NEWER BODY ARRIVES: the host names a per-session POINTER file at spawn
# ($ATERM_INTEGRATION_POINTER, `<control dir>/integration/<sid>` in its 0700
# control dir), captured and scrubbed below like the re-key path. A successor that
# adopts this shell across an update writes its own script folder's 16-hex address
# there (0600, created exclusively, never through a symlink), and the trampoline
# takes it at the next prompt: one fork-free `[[ -f ]]` while nothing waits; when a
# pointer waits, the read, the removal and one `source`. The body is taken ONLY
# from `<root>/<address>/aterm_shell_integration_body.zsh`, where `<root>` is the
# folder THIS file was loaded from — the host's own content-addressed cache — and
# only a 16-hex address is accepted, so no pointer can steer a source anywhere
# else. A pointer naming the body already running, a missing body, or one not owned
# by this user changes nothing.
#
# WHAT THE HOST SEES: the body signs `633;P;AtermIntegration=<address>` before each
# prompt's 133;A, and `status integration_rev=` compares it with the address the
# host ships (`current`, `stale:<address>`, or `frozen` for a shell from before
# loaders, which no pointer can reach).

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
#
# In an upgrade in place (see above) the environment holds no nonce any more — it
# was scrubbed at this shell's first load — so the one the shell already signs
# with is kept.
typeset -g __aterm_shell_nonce="${ATERM_SHELL_NONCE:-${__aterm_shell_nonce:-}}"
unset ATERM_SHELL_NONCE

# Precomputed capability-nonce suffix for OSC 133/633 emissions.
# The nonce is captured exactly once (above) and the env var is unset on the
# very next line, so this string changes only through the re-key channel below,
# which rewrites it together with the nonce. Computing it here
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

# THE RE-KEY CHANNEL (2026-09-24) — the one way the nonce above changes after
# source time. A seamless update that could not carry a shell's nonce (a parent
# from before 0.92, a session adopted without its screen) left every mark it
# emits dropped for the rest of its life (`status integration=degraded`, both
# live tabs on the owner's Mac that day), and only closing the tab cured it.
# The host names a per-session file at spawn ($ATERM_REKEY_PATH, in its 0700
# control dir); to re-key an adopted shell it writes a fresh 64-hex nonce there
# (0600, created exclusively, never through a symlink) and authorizes it, and
# THIS shell takes it at its next prompt. While nothing is pending the cost is
# one fork-free `[[ -f ]]` per prompt; the read and the removal happen only
# when a key waits. Captured and scrubbed from the environment like the nonce,
# so no child process learns the path; the key itself never appears in typed
# text, scrollback or history.
typeset -g __aterm_rekey_path="${ATERM_REKEY_PATH:-${__aterm_rekey_path:-}}"
unset ATERM_REKEY_PATH

# THE BODY POINTER (see "THE LOADER" above), captured and scrubbed exactly like the
# re-key path: no child process learns it. An upgrade in place keeps the one the
# shell already has, or takes the one its relaunch line hands it.
typeset -g __aterm_body_pointer="${ATERM_INTEGRATION_POINTER:-${__aterm_body_pointer:-}}"
unset ATERM_INTEGRATION_POINTER

# Where this file was loaded from — `<root>/<address>/`, the content-addressed
# folder the host prepared — and so which body is running: `<address>` when the
# folder is named by one (16 lowercase hex digits), empty for a hand-installed
# copy, which signs no revision. `%x` is the file being sourced, never the
# caller's. The pointer's bodies are taken from `<root>` only.
typeset -g __aterm_si_dir="${${(%):-%x}:h}"
typeset -g __aterm_si_root="${__aterm_si_dir:h}"
typeset -g __aterm_body_rev="${__aterm_si_dir:t}"
if [[ ${#__aterm_body_rev} -ne 16 || "$__aterm_body_rev" == *[^0-9a-f]* ]]; then
    __aterm_body_rev=""
fi

# Package bin directory — once per shell: an upgrade in place already has it (and
# the shell.d hooks below already ran; the LIVE block re-sources the atpkg hook
# whenever it changes).
if (( __aterm_fresh_load )) && [ -d "$HOME/.aterm/bin" ]; then
    export PATH="$HOME/.aterm/bin:$PATH"
fi

# Source package shell hooks. The `(N)` NULL_GLOB qualifier is REQUIRED: without it
# zsh's default NOMATCH raises "no matches found" the instant a glob matches nothing
# (e.g. shell.d holds only `*.zsh` hooks and no `*.sh`) and ABORTS this whole sourced
# script — killing every OSC 7 (cwd) / OSC 133 (command-block) hook defined below, and
# printing an error as the first line of every session. Per-glob `(N)` expands an
# empty match to nothing instead. (bash's unmatched-glob-stays-literal + the `[ -f ]`
# guard makes the bash script safe without this.)
if (( __aterm_fresh_load )) && [ -d "$HOME/.aterm/shell.d" ]; then
    for f in "$HOME/.aterm/shell.d"/*.zsh(N) "$HOME/.aterm/shell.d"/*.sh(N); do
        [ -f "$f" ] && . "$f"
    done
fi

# THE BODY CHECK, first in every precmd (the trampoline below): a newer body waits
# when the host has written the pointer. Fork-free while nothing waits — one
# `[[ -f ]]` — and the read, the removal (the one fork) and the source happen only
# when a pointer waits. The body is sourced under `emulate -L zsh`, so a user's
# ksh_arrays / sh_word_split / warn_create_global cannot bend its top level, and
# the revision moves only when the source succeeded: a body that failed to load is
# still reported as the one before it.
__aterm_body_check() {
    [[ -n "$__aterm_body_pointer" && -f "$__aterm_body_pointer" ]] || return 0
    emulate -L zsh
    local __aterm_addr="" __aterm_file=""
    { IFS= read -r __aterm_addr < "$__aterm_body_pointer" } 2>/dev/null
    command rm -f -- "$__aterm_body_pointer"
    # Exactly 16 lowercase hex digits — a folder address — or nothing changes.
    [[ ${#__aterm_addr} -eq 16 && "$__aterm_addr" != *[^0-9a-f]* ]] || return 0
    [[ "$__aterm_addr" != "$__aterm_body_rev" ]] || return 0
    __aterm_file="$__aterm_si_root/$__aterm_addr/aterm_shell_integration_body.zsh"
    [[ -f "$__aterm_file" && -O "$__aterm_file" ]] || return 0
    . "$__aterm_file" && __aterm_body_rev="$__aterm_addr"
    return 0
}

# THE TRAMPOLINE: the one precmd hook, and the one piece of the prompt path that
# never changes. A body sourced by the check is the body whose precmd runs in the
# SAME prompt, so the prompt that takes a new body already emits its marks.
__aterm_precmd() {
    local __aterm_status=$?
    __aterm_body_check
    __aterm_body_precmd "$__aterm_status"
    return $__aterm_status
}

# Install hooks using zsh hook arrays (the registrations are at the end of the
# file, after the body has defined what they name). Autoloaded here, before any
# `add-zsh-hook` a function below spells.
autoload -Uz add-zsh-hook

# @@ATERM-INTEGRATION-BODY-BEGIN@@ — everything from here to the END marker is re-sourced live

# ─── The reroute directory, FIRST — and the agents directory beside it ───
#
# $ATERM_REROUTE_DIR is set by aterm's spawn seam: the session-scoped directory of
# stubs for the upstream Rust names (`aterm help reroute`), which the seam already
# put FIRST on the PATH it handed this shell. That position is not final. This file
# runs from the wrapper .zshenv — BEFORE /etc/zprofile (path_helper rebuilds PATH
# from /etc/paths) and BEFORE ~/.zshrc (`. ~/.cargo/env` prepends ~/.cargo/bin) —
# and the package blocks just above may prepend too. Measured 2026-09-07:
# ~/.cargo/bin at position 17, ahead of the managed store at 19, so a bare `cargo`
# ran upstream Rust silently. An ORDER failure — which is why this is move-to-front
# (every existing occurrence removed, then prepended), never skip-if-present.
# Asserted here, after the package blocks, and again from __aterm_first_precmd,
# which runs after every rc file has had its say. Inert outside a session: the
# variable is unset, or the directory (Windows lays none) does not exist.
#
# $ATPKG_AGENTS — exported by the atpkg shell.d hook sourced just above — names
# <prefix>/agents, which holds ONLY the claude and codex shims aterm keeps current
# (owner decision 2026-09-10). It fails the same way, for the same reasons: measured
# 2026-09-10 on m27 at PATH position 14, behind /opt/homebrew/bin (path_helper) and
# ~/.local/bin (~/.zshrc), so `codex` ran a brew cask that could run no command and
# `claude` an older native install. It is moved to the front first, then the reroute
# directory, so PATH reads reroute, agents, … — the spawn seam's own order; the two
# hold disjoint names, so what matters is that both precede everything else.
#
# `${(@)path:#…}`: `:#` matches the expanded value LITERALLY (no GLOB_SUBST), so a
# directory named with `[` or `*` is still removed by equality; `(@)` in quotes
# keeps an EMPTY entry ("here", to a POSIX shell) — the user's — from being dropped.
#
# It also records, in $__aterm_managed_want, the dirs it put in front (in order),
# which is what the per-prompt hot path below compares the head of $path against,
# and in $__aterm_managed_agents_on / $__aterm_managed_reroute_on whether each dir
# WAS there to front. A dir that was absent is re-probed by the hot path (one `-d`
# per prompt, only while it stays absent — review finding 2026-09-16: a hook that
# predates agents/ set $ATPKG_AGENTS, the `-d` here failed once, and the shell
# never looked again) and fronted the moment it appears. The `-d` stats of the
# steady state live HERE, on the change path, never on the per-prompt one.
#
# `emulate -L zsh` opens every function of this block: a user's rc may `setopt
# ksh_arrays` (subscripts from 0 — `path[1,n]` read the wrong elements, and
# `${(@)path:#…}` collapsed to element 0, so PATH was truncated to the two managed
# dirs at every prompt; measured 2026-09-16), `sh_word_split`, `glob_subst`, or
# `warn_create_global` (sourcing the hook from inside a function then printed four
# "created globally" lines at the prompt). `emulate -L` is a builtin, local to the
# function, and restores every option on return.
#
# $__aterm_managed_agents_listing leaves the names inside <prefix>/agents, joined
# by ":", in $__aterm_managed_agents_now: one readdir, in-process — a bare glob
# stats nothing, `(N)` makes an empty directory the empty string, and atpkg's
# dot-prefixed temp files are not matched. It is the TWIN WATCH of the hot path
# (step 4 below); the front records what it saw so the first prompt after a twin
# lands is the one that rehashes.
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

# ─── LIVE: the tab that is ALREADY OPEN picks the managed dirs up the moment atpkg lays them ───
#
# Owner, 2026-09-16, looking at a status row that read "✓ Claude Code 2.1.273 ·
# Codex 0.154.0 — aterm-managed, current   what `claude` and `codex` run in new
# tabs": "HEY! this is a bad experience. aterm atpkg DID install the latest but it
# didn't make them available for me. instead, it is telling me to open a new tab.
# NO! all the latest and best MUST WORK IN THE SAME TAB with live update! fix this
# and this message and audit that this is the actual behavior."
#
# What was measured in that tab: its zsh (pid 1784) was spawned at 10:44:24 by the
# PREVIOUS app build and ADOPTED across the seamless update — the running app
# (0.86.0, pid 1868) started at 10:44:32 — and <prefix>/agents plus the shell.d
# hooks were created at 10:46 by the new build's first pass. Nothing above runs
# again in a shell that is already up: the load-time assert and the first-precmd
# one both fire ONCE, gated on $ATPKG_AGENTS / $ATERM_REROUTE_DIR being set and the
# directories existing AT THAT INSTANT, and that shell had neither variable and no
# directory to find. So `which -a claude` read ~/.local/bin/claude first, `codex`
# resolved to a brew cask that hung two minutes on `--version`, and the only way to
# the build atpkg had just installed was a new tab. The same freeze hits EVERY fresh
# machine: the first tab opens before the seed pass creates agents/.
#
# The fix is a per-prompt AND per-command re-assert — preexec matters because a
# command typed at an idle prompt after the dirs appear runs BEFORE the next precmd
# — in four steps, all builtin-only (no `$(...)`, no backticks, no external
# stat/dirname/readlink; pinned by a grep test):
#
#  1. THE HOOK IS THE SOURCE OF TRUTH when the environment is missing or stale.
#     ~/.aterm/shell.d/00-atpkg.zsh is what atpkg generates (crates/atpkg/src/hooks.rs;
#     the spelling is pinned from that crate's side): it exports $ATPKG_AGENTS and
#     $ATPKG_BIN, moves agents/ to the front and appends bin/, and it is idempotent.
#     It is (re)sourced when the copy on disk is not the copy last sourced — it
#     appeared (a shell spawned before the file existed), or atpkg rewrote it
#     temp+rename on a later pass, so mtime OR inode moved. The stamp is read with
#     `zstat` (zsh/stat, loaded as the one builtin `b:zstat` so the module never
#     shadows /usr/bin/stat): ONE stat syscall, in-process, and its `2>/dev/null` is
#     a builtin redirection, not a fork. A hook that predates R1 (no `export
#     ATPKG_AGENTS`) is sourced ONCE per copy, not once per prompt (review finding
#     2026-09-16). Without the module (a minimal zsh) the fallback probes `-f` and
#     sources only while $ATPKG_AGENTS is unset (bash and fish compare the hook's
#     TEXT instead — their step 1; this rare fallback keeps the cheaper rule and
#     picks a REWRITTEN hook up in the next tab).
#  2. A DIR THAT WAS ABSENT when the front was last laid is probed again — one `-d`
#     per prompt, only in that degraded state — and fronted when it appears: a hook
#     that names an agents/ atpkg has not created yet, or a session whose seam
#     exported no $ATERM_REROUTE_DIR. Nothing is assigned while it stays absent.
#  3. THE ORDER. $__aterm_managed_want holds the dirs that must lead $path; the hot
#     path compares the head of $path against it by string equality and assigns
#     ONLY on a mismatch — assigning $path flushes zsh's command hash, which is
#     exactly what a change needs (`claude` re-resolves to the twin) and pure
#     waste otherwise.
#  4. THE TWIN WATCH. zsh hashes a command's path on first use, and a hashed name is
#     never searched again while the file exists — so once agents/ leads $path and
#     `claude` has run the foreign copy, a twin that lands LATER (a fresh machine:
#     agents/ is created at launch, the twin only once the managed program is
#     installed — the exact window in which the owner typed `claude`) would keep
#     losing to the hashed path for the life of the shell (measured 2026-09-16, zsh
#     5.9 and bash 3.2.57). The names inside agents/ are listed each call (one
#     readdir, no stat, no fork) and compared to the listing recorded when the dir
#     was fronted; on a change — a twin laid, or removed — `rehash` empties the
#     table and the next lookup walks $path again. A twin RE-laid under the same
#     name changes nothing here and needs nothing: the hashed path IS the twin.
#
# Per prompt and per command, steady state: one zstat, one readdir, no assignment.
# Measured 2026-09-16 (zsh 5.9, 10000 calls): ~30 µs per call before the twin
# watch; a fork of /usr/bin/true costs ~1400 µs.
#
# $ATERM_REROUTE_DIR is derived for a shell that predates it — the sibling
# `<dir of $ATPKG_AGENTS>/reroute`, when it is a directory and $__ATERM_REROUTE_PASSTHROUGH is
# not engaged (set, non-empty and not "0": atpkg::reroute::engaged) — so the final
# order is reroute, agents, everything else, bin/ last (the hook appends it).
#
# Gated on BEING INSIDE AN ATERM SESSION ($ATERM_CHILD=1, which the spawn seam sets
# for every child, or $ATERM_SESSION_ID) — NOT on $ATERM_REROUTE_DIR, which is
# precisely what the adopted shell lacks. Inert everywhere else.
typeset -g __aterm_atpkg_hook="$HOME/.aterm/shell.d/00-atpkg.zsh"
typeset -gi __aterm_managed_live=0
typeset -gi __aterm_have_zstat=0
if [[ -n "${ATERM_CHILD:-}" || -n "${ATERM_SESSION_ID:-}" ]]; then
    __aterm_managed_live=1
    zmodload -F zsh/stat b:zstat 2>/dev/null && __aterm_have_zstat=1
fi

# Leaves "<mtime>:<inode>" of the hook on disk in $__aterm_atpkg_hook_now, or the
# empty string when it is absent (or zsh/stat is unavailable). One stat syscall,
# no fork. Its own global, not $REPLY: precmd runs between a user's `read` and
# the line that consumes $REPLY, and must not clobber it.
typeset -g __aterm_atpkg_hook_now=""
__aterm_atpkg_hook_stamp() {
    emulate -L zsh
    __aterm_atpkg_hook_now=""
    (( __aterm_have_zstat )) || return 0
    local -A __aterm_st
    zstat -H __aterm_st -- "$__aterm_atpkg_hook" 2>/dev/null || return 0
    __aterm_atpkg_hook_now="$__aterm_st[mtime]:$__aterm_st[inode]"
}
# The copy the shell.d loop above sourced at load is the copy last sourced —
# whatever it exported (a pre-R1 hook exports no $ATPKG_AGENTS, and is still not
# sourced again until atpkg rewrites it). Recorded ONCE per shell: a body
# re-sourced live keeps the record, so a hook rewritten just before is still
# sourced at the next prompt.
if (( ! ${+__aterm_atpkg_hook_seen} )); then
    typeset -g __aterm_atpkg_hook_seen=""
    if (( __aterm_managed_live )); then
        __aterm_atpkg_hook_stamp
        __aterm_atpkg_hook_seen="$__aterm_atpkg_hook_now"
    fi
fi

# Exports $ATERM_REROUTE_DIR (status 0) or leaves it alone (status 1).
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
# A session shell whose seam exported no $ATERM_REROUTE_DIR but whose rc block
# sourced the hook derives it now, so the load-time order is final too.
if (( __aterm_managed_live )) && __aterm_managed_derive_reroute; then
    __aterm_reroute_path_front
fi

# The hot path: every precmd and every preexec. Builtin-only — see above.
__aterm_managed_path_live() {
    emulate -L zsh
    (( __aterm_managed_live )) || return 0
    # 1. The hook: sourced when the copy on disk is not the copy last sourced.
    if (( __aterm_have_zstat )); then
        __aterm_atpkg_hook_stamp
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
    # 2. A dir that was absent when the front was last laid: probe it again.
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
    # 3. The order: assign only on a mismatch.
    local -i __aterm_n=$#__aterm_managed_want
    if (( __aterm_n )) && [[ "${(j.:.)path[1,__aterm_n]}" != "${(j.:.)__aterm_managed_want}" ]]; then
        __aterm_reroute_path_front
        return 0
    fi
    # 4. The twin watch: a name appearing in (or leaving) agents/ empties the hash.
    if (( __aterm_managed_agents_on )); then
        __aterm_managed_agents_listing
        if [[ "$__aterm_managed_agents_now" != "$__aterm_managed_agents_seen" ]]; then
            __aterm_managed_agents_seen="$__aterm_managed_agents_now"
            rehash
        fi
    fi
    return 0
}

# State tracking. KEPT across a live re-source: the body is re-sourced from the
# precmd trampoline, and a command that was running must still get its 133;D.
typeset -g __aterm_in_command="${__aterm_in_command:-0}"
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

# The re-key channel's check (the path is captured by the loader above).
__aterm_rekey_check() {
    emulate -L zsh
    [[ -n "$__aterm_rekey_path" && -f "$__aterm_rekey_path" ]] || return 0
    local key=""
    { IFS= read -r key < "$__aterm_rekey_path" } 2>/dev/null
    command rm -f -- "$__aterm_rekey_path"
    # Exactly 64 lowercase hex digits, or nothing changes.
    [[ ${#key} -eq 64 && "$key" != *[^0-9a-f]* ]] || return 0
    __aterm_shell_nonce="$key"
    __aterm_id_suffix_str=";id=${key}"
}

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

# The body's revision, SIGNED (633;P, the VS Code property mark): the folder
# address this body was taken from, so the host can tell a shell running its own
# body from one running an older build's (`status integration_rev=`). Emitted
# before every 133;A — a builtin printf, nothing when the revision is unknown (a
# hand-installed copy) — so a successor that adopted this shell learns it at the
# first prompt after the update, whatever it carried.
__aterm_mark_integration_rev() {
    [[ -n "$__aterm_body_rev" ]] || return 0
    __aterm_osc "633;P;AtermIntegration=${__aterm_body_rev}${__aterm_id_suffix_str}"
}

# ─── The tty settings the shell keeps (2026-09-26) ───
#
# THE DEFECT. zsh ADOPTS whatever terminal settings the last foreground job left
# behind: when a job ends, an unfrozen zsh re-reads the tty and runs every later
# command under what it found. A raw-mode program that dies without restoring
# them — SIGKILLed, or exiting after `tty.setraw` without a `finally` — therefore
# leaves every command after it with `-isig -iexten -icrnl -ixon -opost`: Ctrl-C
# no longer interrupts anything (it arrives as a literal ^C byte), Enter sends a
# bare CR that ends no line (`read x`, a password prompt: "hello^M", and only
# Ctrl-J finishes it), and output staircases. The zle prompt itself looks fine —
# zle sets its own modes to edit — which is why it reads as "Ctrl-C is broken in
# this tab" and nothing else. Measured 2026-09-26 on zsh 5.9, this file sourced,
# in a pty: a python `tty.setraw` child that died by SIGKILL, `os._exit(1)` or
# `os._exit(0)`, and a node `setRawMode` child SIGKILLed (the Claude Code shape),
# all left it; Ctrl-C sent 0.4 s into `sleep 3` ended it at 3.04–3.09 s. And it
# was seen for real: a SIGKILLed Claude Code 2.1.283 in a headless aterm left the
# integrated login zsh exactly so ("sleep STILL RUNNING 1s after ctrl+c",
# `read x` returning `hello\r` only on Ctrl-J). The terminal-mode handback
# (f7757d04b) repairs what the dead program told the TERMINAL; this is what it
# told the TTY DRIVER, which only the shell that owns the tty can keep straight —
# a repair from the pty master does not stick, because zsh re-applies the state
# it adopted after the next job.
#
# THE FIX, in two parts, armed once per shell at the first prompt this body runs
# (`__aterm_tty_arm`, from `__aterm_body_precmd`): after every rc file has had its
# say, and — through the loader's body pointer — also in a shell that was already
# running, possibly already broken, when this body reached it.
#
#  1. REPAIR FIRST: when the tty is in a dead raw program's state (ISIG off —
#     every raw mode clears it, and nobody keeps it off at a prompt on purpose,
#     since it disables Ctrl-C and Ctrl-Z), turn back on what raw modes turn off:
#     `isig icanon iexten echo icrnl opost`. IXON is deliberately NOT touched: a
#     raw mode clears it too, but so does a user's own `stty -ixon`, and which of
#     the two cleared it cannot be told apart — so a deliberate `stty -ixon`
#     survives. Freezing a tab that is already broken would lock the broken state
#     in; that is why the repair comes first.
#  2. FREEZE (`ttyctl -f`, zsh's own mechanism for exactly this): from now on the
#     shell does not adopt a job's tty changes; it puts back what it had after
#     every job, exactly — IXON included. Fork-free at every prompt. What a user
#     changes ON PURPOSE must still stick, so `stty`, `reset` and `tset` become
#     thin functions that thaw the tty, run the real command, and freeze what it
#     left (`__aterm_tty_thaw_run`) — typed at the prompt, from an alias, or in a
#     sourced file alike. What a freeze does undo is a change made by a CHILD
#     process on its own (a script running `stty`, `command stty`, `/bin/stty`):
#     `ttyctl -u` thaws for the rest of the shell.
#
# Measured with exactly this code (2026-09-26, zsh 5.9, pty): all five dying
# children above leave `isig iexten icrnl ixon opost` and Ctrl-C ends `sleep 3`
# in 0.46–0.54 s; `read -r x` ends on Enter; `stty -ixon` typed before the kill
# survives it; a shell that was ALREADY broken when the body arrived is repaired
# at that prompt and stays repaired across the next kill; `reset` still works.
#
# RESPECTING THE USER. The freeze and the wrappers are skipped — the shell keeps
# zsh's adopting behaviour — when the user's rc defines ANY of `stty`, `reset`,
# `tset` as a function or an alias: their definition would bypass the thaw, and a
# frozen tty would then silently undo what it does. (That is also the way to keep
# zsh's own behaviour on purpose; there is deliberately no environment switch —
# the owner's rule of 2026-09-22 is one batteries-included default, not knobs.
# `ttyctl -u` at a prompt thaws the tty for the rest of that shell.) Such a shell
# still gets the repair: after every command that FAILED (status non-zero — a
# SIGKILL is 137), one `stty -a` fork, and the repair only when the raw signature
# is there. What that fallback cannot catch is a raw program that exits 0 without
# restoring; the freeze catches that too.
#
# Kept across a live re-source: a newer body arriving through the pointer finds
# the shell already armed and changes nothing about it.
typeset -gi __aterm_tty_armed="${__aterm_tty_armed:-0}"
typeset -gi __aterm_tty_frozen="${__aterm_tty_frozen:-0}"

# The repair (part 1 above). One fork to read the tty, a second only when it is
# in a dead raw program's state. Silent when stdin is not a terminal.
__aterm_tty_repair() {
    emulate -L zsh
    local __aterm_tty_state=""
    __aterm_tty_state="$(command stty -a 2>/dev/null)" || return 0
    local -a __aterm_tty_words
    __aterm_tty_words=(${=__aterm_tty_state})
    (( ${__aterm_tty_words[(Ie)-isig]} )) || return 0
    command stty isig icanon iexten echo icrnl opost 2>/dev/null
    return 0
}

# Run a tty-changing command with the tty THAWED, so the shell adopts what it
# leaves, then freeze that (part 2 above). `builtin`/`command` so no alias or
# function of the user's, and not these wrappers themselves, is reached.
#
# The re-freeze MUST run on every way out, because `__aterm_tty_frozen` stays 1
# and so keeps the fallback repair off: a wrapper that leaves the tty thawed
# brings back the whole defect, silently, for the rest of the shell. Two ways out
# skipped it until the review of 2026-09-26, both measured in a pty:
#  - a user's `setopt err_return`: this function ran under the USER's options,
#    so a failing command (`stty bogusflag`, a typo) returned before `ttyctl -f`.
#    `ttyctl` then said "tty is not frozen" with `__aterm_tty_frozen=1`, and a
#    raw child's `exit 1` left `-isig -iexten -icrnl -opost -ixon` (Ctrl-C into
#    `sleep 3` took 3.03-4.04 s). `emulate -L zsh` closes that.
#  - Ctrl-C while the command runs (`tset`'s "TERM = (unknown)?" question,
#    `reset`'s pause): zsh abandons the rest of a function whose foreground child
#    died by SIGINT, `emulate` or not. The `always` block runs on that path too.
# The command's own status is still what the wrapper returns (130 for the ^C).
__aterm_tty_thaw_run() {
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

# Once per shell, at the first prompt this body runs. The wrappers are spelled
# with the `function` keyword: a user's alias for the name would otherwise be
# expanded when a live body is parsed and define something else entirely.
__aterm_tty_arm() {
    emulate -L zsh
    __aterm_tty_armed=1
    __aterm_tty_repair
    local __aterm_tty_name
    for __aterm_tty_name in stty reset tset; do
        (( ${+aliases[$__aterm_tty_name]} )) && return 0
        if (( ${+functions[$__aterm_tty_name]} )) &&
           [[ "${functions[$__aterm_tty_name]}" != *__aterm_tty_thaw_run* ]]; then
            return 0
        fi
    done
    function stty { __aterm_tty_thaw_run stty "$@"; }
    function reset { __aterm_tty_thaw_run reset "$@"; }
    function tset { __aterm_tty_thaw_run tset "$@"; }
    builtin ttyctl -f
    __aterm_tty_frozen=1
    return 0
}

# precmd - runs before each prompt, from the loader's trampoline, which hands it
# the status the command before it left.
__aterm_body_precmd() {
    local last_status=$1

    # A waiting re-key first, so every mark this prompt emits carries it.
    __aterm_rekey_check

    # The tty settings (see "The tty settings the shell keeps" above): armed once;
    # after that, fork-free while frozen, and one probe after a failed command
    # where the user's own definitions ruled the freeze out.
    (( __aterm_tty_armed )) || __aterm_tty_arm
    (( __aterm_tty_frozen || last_status == 0 )) || __aterm_tty_repair

    # The managed dirs, live (see "LIVE" above): one stat, one readdir, an assign
    # (or a rehash) only on a change.
    __aterm_managed_path_live

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

    # The revision this prompt runs, then the prompt start.
    __aterm_mark_integration_rev
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

    # The managed dirs, live — BEFORE this command resolves: a `claude` typed at a
    # prompt that was drawn before atpkg laid agents/ must already run the twin.
    __aterm_managed_path_live

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
# Called ONCE per shell, by the loader (the end of this file): a body re-sourced
# live leaves the user's bindings alone.

# ─── OSC 133;B (end of prompt / start of user input) ───
# Emitted via zle-line-init so it fires after the prompt is fully drawn.
# Placing it in preexec is too late (user has already typed their command).
# The widget is wired once, by the loader (end of file).
__aterm_zle_line_init() {
    __aterm_mark_command_start
    (( ${+widgets[__aterm_orig_zle_line_init]} )) && zle __aterm_orig_zle_line_init
}

# ─── Deferred First-Precmd Setup ───
# Runs once on the very first precmd after the shell has fully initialized
# and processed SIGWINCH from the initial terminal resize. Handles the prompt
# override, then uninstalls itself.
__aterm_first_precmd() {
    local last_status=$?

    # Apply prompt override if requested
    # `${…:-}`: under a user's `setopt nounset` the bare form errored at every
    # prompt and this one-shot never uninstalled itself (review note 2026-09-16).
    if [[ -n "${ATERM_PROMPT_STYLE:-}" && "${ATERM_PROMPT_STYLE:-}" != "none" ]]; then
        __aterm_set_prompt
    fi

    # The reroute and agents directories, FIRST — unconditionally, once: /etc/zprofile
    # and ~/.zshrc have both run by now (see __aterm_reroute_path_front for why the
    # load-time assert above is not final), and a ~/.zshrc carrying atpkg's rc block
    # may have set $ATPKG_AGENTS itself, so $__aterm_managed_want is recomputed here.
    # From this prompt on, __aterm_precmd/__aterm_preexec keep it live (the "LIVE"
    # block above) without assigning $path unless the order is actually wrong.
    __aterm_reroute_path_front

    add-zsh-hook -d precmd __aterm_first_precmd
    return $last_status
}

# The body ends in success: the loader moves its revision only when the source
# returned 0.
true

# @@ATERM-INTEGRATION-BODY-END@@

# ─── THE LOADER, continued: the wiring, once per shell ───
#
# OSC 133;B rides zle-line-init. A user's own zle-line-init is kept and chained —
# on a FRESH load only (review finding 2026-09-26). By an upgrade in place the
# user's rc has run, and a plugin may have wrapped our widget (`add-zle-hook-widget`
# keeps it as the first of its hooks; powerlevel10k and older
# zsh-syntax-highlighting wrap it too): re-wiring then aliased the plugin's widget
# — which calls ours — as `__aterm_orig_zle_line_init` and installed ours in
# front of it, which calls that alias. A loop: measured on zsh 5.9, ~250 133;B
# marks and "recursion limit exceeded" at every prompt of the upgraded shell.
# Nothing needs re-wiring: a widget names its FUNCTION, so whatever chain the
# shell has reaches the new body's `__aterm_zle_line_init` as it stands.
if (( __aterm_fresh_load )); then
    if (( ${+widgets[zle-line-init]} )) && [[ "${widgets[zle-line-init]}" != "user:__aterm_zle_line_init" ]]; then
        zle -A zle-line-init __aterm_orig_zle_line_init
    fi
    zle -N zle-line-init __aterm_zle_line_init
fi

# Install hooks using zsh hook arrays (`add-zsh-hook` never adds a name twice).
# __aterm_first_precmd is registered first so the prompt override lands before
# __aterm_precmd emits OSC 133;A (prompt start marker) — on a fresh load only: in
# an upgrade in place it has run and removed itself long ago.
if (( __aterm_fresh_load )); then
    add-zsh-hook precmd __aterm_first_precmd
fi
add-zsh-hook precmd __aterm_precmd
add-zsh-hook preexec __aterm_preexec

# The key bindings, ONCE per shell: a fresh load only (review finding
# 2026-09-26). This file runs before the user's own rc (zsh reads it from the
# ZDOTDIR .zshenv), so a binding of theirs for one of these keys wins; a body
# re-sourced live — a newer build's through the pointer, or an upgrade in place
# — binding them again at a prompt took those keys back from the user in every
# live tab at every update.
if (( __aterm_fresh_load )); then
    __aterm_setup_keybindings
fi

# An upgrade in place SIGNS its revision now, not at the next prompt: the typed
# line that upgrades a shell from before loaders goes on to relaunch its agent,
# which holds the terminal for hours, and until the shell's next prompt the host
# would name it `integration_rev=frozen` — and an update in between would carry
# no revision to point it by. (Signed with the key the shell holds; a fresh load
# signs at its first prompt.)
if (( ! __aterm_fresh_load )); then
    __aterm_mark_integration_rev
fi
