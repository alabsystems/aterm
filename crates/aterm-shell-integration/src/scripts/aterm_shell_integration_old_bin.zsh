# aterm_shell_integration_old_bin.zsh - AN OLDER SCRIPT'S `~/.aterm/bin`, taken off
# (2026-10-19; its own file since 2026-10-20).
#
# Copyright 2026 Andrew Yates
# Author: Andrew Yates
# Licensed under the Apache License, Version 2.0
#
# What the BODY of aterm_shell_integration.zsh in this folder sources once per
# shell where it reaches a shell an older script loaded (its stub there says
# when). It runs on the loader's and the body's state, so it is never sourced by
# hand.
#
# Every script before 613305828c (2026-10-16) put `$HOME/.aterm/bin` FIRST on
# PATH when it loaded a shell fresh and that folder existed; none since does
# (the loader's package block says why). Such a shell kept the entry through an
# upgrade in place — this body sourced at a prompt by its older loader, or this
# loader sourced over it — until it exited. So the body takes it off, once per
# shell, where the shell PROVES that such a script put it there, and nowhere
# else: v0.106.0 and every build from 613305828c up to this one put none there
# and set no mark, so in their shells a `~/.aterm/bin` on PATH is the user's
# own. The proof has two halves:
#
#  1. The script that loaded this shell fresh carried the prepend. Where the
#     shell names that script's loader and the loader is still there, the
#     loader alone says: the one in `__aterm_si_dir` when its own load was the
#     fresh one (`__aterm_fresh_load` 1), or, when this build's loader came in
#     place, the one it noted there (`__aterm_bin_from`, the loader's block
#     says how; `-` for a shell from before loaders, whose every script
#     prepended). The folder is content-addressed, so its loader still holds
#     the bytes that loaded the shell, and the prepend line is looked for in
#     it, spelled here so that this build's loader never holds it. Where the
#     loader is gone (collected) or unnamed (a build from 613305828c on sourced
#     its own in place), either of two records the shell keeps for life says so. Its
#     start: before 2026-10-16 00:00 UTC every script prepended (613305828c,
#     the first that did not, was written that evening, UTC-7); the clock less
#     `ps -o etime=`, and the day's margin makes the seconds between the two
#     readings never matter. Or the build that spawned it, which aterm names
#     in `TERM_PROGRAM_VERSION` (with `TERM_PROGRAM` aterm) and no script
#     touches: every one up to v0.105.0 prepended but the dev builds of one
#     branch in the hour after 613305828c, before the 0.106.0 bump reached it,
#     whose shells their loader answers for while it is there.
#  2. The folder was there at that load: it is older than the shell process —
#     its positive birth, or its last change only when birth is unavailable,
#     by more than the whole seconds `stat`, `date` and `ps -o etime=` count in.
#     A usable birth takes precedence even if mtime was preserved or backdated.
#     The clock is read before the shell's age; mtime without a usable birth
#     remains a fallback reading, not an independent creation-time witness.
#
# One `sh` asks both (stat, date, ps and, where the loader is there, grep: once
# per shell, and only where PATH holds the entry and the folder exists). Then
# one copy goes — the first equal to `$HOME/.aterm/bin`, where the old script
# put it — and nothing else moves: an EMPTY entry ("here") keeps its place, and
# a copy the user's own startup added keeps the folder on PATH. `(ie)` finds it
# by string equality, never as a pattern, and under `emulate -L zsh`, so a
# user's ksh_arrays cannot move the index.
#
# A body from dc187bcff8 through v0.107.0 asked the loader alone, and its
# once-per-shell flag stands in every shell it reached: nothing records whether
# it took the entry off there, and asking again would take the user's own copy
# where it had.
() {
    emulate -L zsh
    local from= bin=${HOME-}/.aterm/bin
    if (( ${+__aterm_bin_from} )); then
        from=$__aterm_bin_from
    elif [[ ${__aterm_fresh_load-} == 1 && -n ${__aterm_si_dir-} ]]; then
        from=$__aterm_si_dir/aterm_shell_integration.zsh
    fi
    local -i at=${path[(ie)$bin]}
    if [[ -d $bin ]] && (( at <= $#path )) &&
        command sh -c '
            t=$(stat -f "%B %m" -- "$3") || t=$(stat -c "%W %Y" -- "$3") || exit 1
            b=${t%% *}; m=${t##* }
            case $m in ""|*[!0-9]*) exit 1 ;; esac
            case $b in ""|*[!0-9]*|0) b=$m ;; esac
            n=$(date +%s) || exit 1
            e=$(ps -o etime= -p "$4") || exit 1
            e=${e##* }; d=0
            case $e in *-*) d=${e%%-*}; d=${d#0}; e=${e#*-} ;; esac
            case $d$e in ""|*[!0-9:]*) exit 1 ;; esac
            s=0; IFS=:
            for x in $e; do x=${x#0}; s=$((s * 60 + ${x:-0})); done
            s=$((n - ${d:-0} * 86400 - s))
            [ $((b + 1)) -lt "$s" ] || exit 1
            [ "$1" = - ] && exit 0
            if [ -f "$1" ]; then
                grep -qF -e "$2" -- "$1"; exit
            fi
            [ "$s" -lt 1792108800 ] && exit 0
            case ${TERM_PROGRAM-}:${TERM_PROGRAM_VERSION-} in
                aterm:0.[0-9]*) v=${TERM_PROGRAM_VERSION#0.} ;;
                *) exit 1 ;;
            esac
            [ "${v%%[!0-9]*}" -le 105 ]
        ' sh "$from" "export PATH=\"\$HOME/.aterm/bin:\$PATH\"" "$bin" "$$" 2>/dev/null
    then
        path[at]=()
    fi
}
