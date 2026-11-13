// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help spawn`: a new session: a tab, a pane, a connected pair, a worker on a task, or a program
//! worker. `aterm-gui`'s `control_media::parse_spawn_args` reads the words, and its
//! `spawn_usage` names the worker words a spawn starts.

use super::{Page, Section, para, row};

/// The `spawn` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        row(
            "spawn [window=<id>] [raise=<t|f>] [cwd=<path>] [split=<v|h>] [theme=<name>] \
             [why=<one line>] [req=<id>]",
            "a new tab (or, with split=, a pane of the focused one), addressable at once",
        ),
        row(
            "spawn … agent=<claude|codex|agy|muse> [model=<m>] [autopilot=<land|cruise>] \
             [from=<sid>] len=<n>",
            "a worker on a task (owner): replies `GO <n>`, reads <n> task bytes, launches the \
             agent under Autopilot (`aterm ctl spawn agent=<word> … -` reads the task from stdin)",
        ),
        row(
            "spawn … program=<pct> [arg=<pct>]… [from=<sid>] len=<n>",
            "a program worker (owner): runs a program no harness reads on the task, concluded by \
             its exit status",
        ),
        row(
            "spawn connected=controlled|controller place=window|tab of=<sid> [cwd=<path>]",
            "a session connected to <sid> (owner)",
        ),
        row(
            "aterm ctl --host <h> spawn …",
            "the same on another Mac that lets this one drive it, with fewer words (`help spawn \
             fleet`)",
        ),
    ],
    options: &[
        row(
            "window=<id>",
            "the window (`inspect app/v1 tabs`; 0 on --headless); `@<sid> spawn` picks that \
             session's",
        ),
        row(
            "raise=<t|f>",
            "bring it to front (default true with no window named, else false)",
        ),
        row("cwd=<path>", "its folder"),
        row(
            "split=<v|h>",
            "split the focused pane instead of opening a tab",
        ),
        row("theme=<name>", "a session theme, as `theme session` sets"),
        row(
            "why=<one line>",
            "what it is for: visible text, at most 200 characters and 256 bytes encoded (a space or \
             a non-ASCII byte takes 3), no quote; its tab's open keeps it whole as `why_text=` \
             (`log kinds=tab`); a fleet drive's plain shell must give one",
        ),
        row(
            "req=<id>",
            "join the request an earlier spawn's reply named (`req=r-<yyyymmdd>-<hhmm>-<head>-<n>`): \
             one of the same requester's that has not concluded, so its tabs are one request \
             (`ERR spawn req=<id>: <why>` otherwise, `<why>` `this caller makes no request` from \
             a caller that makes none: a person's shell, a client in no tab); not on the \
             connected form",
        ),
        row(
            "agent=<word>",
            "the worker's agent; `grok` is refused for now (`help spawn worker`)",
        ),
        row("model=<m>", "the agent's model"),
        row(
            "autopilot=<land|cruise>",
            "the worker's Autopilot (default land); `meta set autopilot` switches later",
        ),
        row("from=<sid>", "who the task is from (`task from=`)"),
        row(
            "len=<n>",
            "the task's bytes after `GO <n>`: 1 to 65536, no NUL",
        ),
        row(
            "label=<word>",
            "`aterm ctl` only: the worker's label on this Mac's `board`, never sent",
        ),
    ],
    reply: &[
        row(
            "OK <sid> [req=<id>]",
            "the new session's sid (always the first word), and the request it is part of where \
             one was made: a spawn over \
             a fleet drive, or from a tab where an agent runs, opens a request in this Mac's \
             `requests/` journal (`req=` joins one), which records who asked, why (`why=`, else \
             a worker's brief's first line, else the asking tab's goal or asked line, recorded as \
             a fallback) and its tabs, and which that requester's later `turn`, `send`, `key` and \
             `close` on those tabs attach to",
        ),
        row(
            "GO <n>",
            "a worker spawn: send the <n> task bytes now; then `OK <sid> [req=<id>]`",
        ),
        row(
            "ERR …",
            "ERR no such window <id>, ERR no such folder <path>, ERR no access to folder <path>, \
             ERR theme \"<name>\": <why>, ERR unsupervised: <switch>, ERR spawn agent=<word>: \
             <why>, ERR spawn program=<p>: <why>, ERR spawn req=<id>: <why>, ERR request store: \
             <why>, ERR denied: … (fleet), ERR usage: spawn …",
        ),
        para(
            "A worker's progress is its task: `task`, `await task @<sid>`, then `result` \
             (`aterm help workers`).",
        ),
    ],
    examples: &[
        row(
            "aterm ctl spawn cwd=/Users//me/repo",
            "a new tab in that folder, brought to front",
        ),
        row(
            "echo 'fix the failing test' | aterm ctl spawn agent=claude cwd=/Users//me/repo -",
            "a Claude Code worker on that task; then `aterm ctl await task @<sid> since=0` in the \
             background",
        ),
        row(
            "aterm ctl --host mini spawn agent=codex 'cwd=~/repo' - < task.md",
            "a codex-alab worker on another Mac, in a folder of that Mac's home",
        ),
    ],
    sections: &[
        Section {
            name: "worker",
            about: "where a worker lands, how it launches and installs, and each stall",
            rows: &[
                para(
                    "Worker form (Owner-only): `spawn … agent=<claude|codex|agy|muse|grok>` \
                     (`grok` is refused, below) `\
                     [model=<m>] [autopilot=<land|cruise>] [from=<sid>] len=<n>` lands its tab \
                     under the window's Workers tab, behind what the window shows whatever \
                     `raise=` says, not on the strip (`raise <sid>` or the row opens it, and the \
                     row's Move to Tab puts it among the person's tabs; `inspect app/v1 tabs` says \
                     `parked=`; with `split=` it is a pane of the tab in front instead, and any \
                     `split=` while a worker is in front answers `ERR spawn split= does not land \
                     beside a worker under the Workers tab: …`).",
                ),
                para(
                    "It replies `GO <n>`, reads `<n>` task bytes (`1..=65536`, no NUL; `ERR spawn \
                     agent=<word>: the task is <n> bytes (max 65536)` beyond; `agent=gemini`, the \
                     retired Gemini CLI, is `ERR spawn agent=gemini: Gemini CLI is no longer \
                     supported: agy replaces it (agent=agy)`; `aterm ctl spawn agent=<word> \
                     [model=<m>] [cwd=<p>] -` reads stdin), writes `<state>/tasks/` \
                     (`ATERM_TASK_FILE`), and at the login shell's (`zsh`/`bash`/`fish`) first \
                     prompt launches the interactive agent (`--prompt-interactive=` for `agy`; \
                     `--trust-workspace`, `--contributor-mode off` and `--reasoning-effort xhigh` \
                     for `muse`, which runs `muse-wonder-full-1.0` unless `model=` names another; \
                     `codex` is codex-alab, which carries the session itself, \
                     `supervisor_proven=1`, and whose landing marks `task` `done why=landed` with \
                     its rollout's report).",
                ),
                para(
                    "It sets `task` to `working` (`from=<sid|ctl>`) and `autopilot` to `land` (or \
                     `autopilot=cruise`; `meta set autopilot land` switches later, on this Mac \
                     alone: a fleet drive names it at spawn and serves no `meta`).",
                ),
                para("A launch that does not come up marks the task `stalled` with a `why=`:"),
                row(
                    "why=shell",
                    "its shell ends or a person uses it before its first prompt",
                ),
                row(
                    "why=startup",
                    "no agent composer (for `codex`, no proven claim, and a `codex` program a \
                     fabric hold keeps from claiming is not judged until 120 s after the hold \
                     lifts) within 120 s of `since_ms=` or sooner if its launcher gives up, with \
                     what held it in `attention=` (each names the member too, where its label is \
                     known, among them: a launch refused for a cause no wait ends, what mends it \
                     first; a launch held because no account of its pool can take it now, and \
                     when the first opens, which never starts later; an install or sign-in check \
                     with no progress for 45 minutes; a program atpkg keeps off, and why; and `no \
                     shell prompt`, with what the shell showed: one that drew nothing and used no \
                     CPU is stuck at its start, as on a Mac that needs a restart)",
                ),
                row("why=held", "stuck 120 s at an unhandled screen"),
                row(
                    "why=cancelled",
                    "a shutdown or update stopped a launch that holds an account scope before it \
                     was typed",
                ),
                row(
                    "why=signin, why=install",
                    "a sign-in only a person can make, or a failed install (below)",
                ),
                para(
                    "It refuses with `ERR unsupervised: <switch>` when the window harness cannot \
                     supervise (`[harness] enabled = false`, `headless = false`, or `autopilot = \
                     \"off\"`), and with `ERR spawn agent=<word>: <why>` for an agent whose turn \
                     end no reader sees yet (`grok`, until aterm reads Grok Build's turn end), \
                     before any check of this Mac.",
                ),
                para(
                    "Not reached while that refusal stands: once aterm reads Grok Build's turn \
                     end, a Grok Build worker with no usable sign-in file (a pooled \
                     `GROK_AUTH_PATH`, else `<GROK_HOME or ~/.grok>/auth.json`, missing, empty or \
                     one grok refused) is `stalled why=signin` before anything is installed, and \
                     only a person signs in.",
                ),
                para(
                    "An agent whose program atpkg's store holds no build of yet (every agent is in \
                     atpkg's default set; a fresh Mac before its update pass lands it) is then \
                     installed by atpkg's verified install, and a failed install is `stalled \
                     why=install`, atpkg's last line of the failure in `attention=` \
                     (`why=signin` and `why=install` name the member and the cause there too); \
                     where `[packages] exclude`, a person's removal or a no to unattended installs \
                     (Keep ALab tools up to date or Install ALab tools off in Settings ▸ Packages, \
                     or the toolset removed by `aterm pkg uninstall --all`) keeps it off, nothing \
                     is installed, the launch note says so and the line runs any copy of the \
                     shell's own; if none starts, its `stalled why=startup` says why.",
                ),
                para(
                    "After any install, the launch tries to renew a signed-out Google login, which \
                     reads `why=signin` only if that leaves it signed out; a renewal that could \
                     not run and any other failure read `why=startup`. The startup deadline waits \
                     while it installs, until the install makes no progress for 45 minutes, and a \
                     launcher whose task is no longer `working` types nothing, so a launch that \
                     stalled while held or installing never starts later.",
                ),
            ],
        },
        Section {
            name: "program",
            about: "program= workers: what runs, how the exit status concludes it, refusals",
            rows: &[
                para(
                    "Program form (Owner-only): `spawn … program=<pct> [arg=<pct>]… [from=<sid>] \
                     len=<n>` (`aterm ctl spawn program=<p> [arg=<a>]… -` reads stdin and \
                     percent-encodes each word) runs as a worker a program no harness of aterm's \
                     reads: it lands and parks as an agent's worker does and writes the task file \
                     as given, with no fact line, and at the first prompt types the program and \
                     its words, quoted, with the task file as its standard input (`'<p>' '<a>'… < \
                     \"$ATERM_TASK_FILE\"`), setting no Autopilot.",
                ),
                para(
                    "The exit status of that line's block (from the shell block it was typed into \
                     on; a command a person runs in the tab is never it) concludes it once: `done \
                     why=exit-status exit=0`, else `failed why=exit-status exit=<n>` (a signal as \
                     the shell reports it, 128 plus its number), or `failed \
                     why=no-exit-status` where no status was read (the next prompt sealed its \
                     block first, job control hid how it ended, a restore found its shell gone); \
                     a job stopped (Ctrl-Z) has not ended. `result` answers `source=output` with \
                     its output tail (its last 200 rows still held, redacted as `survey` redacts a \
                     screen, at most 16 KiB). It is `stalled why=startup` if its line has not run \
                     within 120 s, and a stall before its line was typed stands.",
                ),
                para(
                    "No Autopilot word moves its task, a `turn` types into its tab without \
                     reopening it, and a harness's `task mark` on it is `ERR task mark: a program \
                     worker's task is concluded by its program's exit status, not by a harness` \
                     (a CLI the program runs is read as any tab's, but concludes nothing). \
                     Nothing reads its screen, so a program that asks on its terminal (a \
                     password, a `[y/N]`) waits there until someone answers in its tab: bound a \
                     wait with `await task … timeout=`.",
                ),
                para(
                    "A `program=` empty, starting `-` or `~`, or holding a control character, an \
                     `arg=` holding one, an `arg=` with no `program=`, and a `program=` beside \
                     `agent=`, `model=` or `autopilot=` are the usage line; a program whose file \
                     name is a CLI whose harness concludes its worker \
                     (`agent=claude|codex|agy|muse`) is `ERR spawn program=<p>: <CLI> has a \
                     harness here; spawn it as `agent=<word>``. It needs the window harness \
                     supervising (`ERR unsupervised: …`, `[harness] enabled = false` and \
                     `headless = false` included) and shell integration (`ERR spawn program=<p>: \
                     shell integration is off, …`), not the harness's agent switches or its \
                     Autopilot.",
                ),
            ],
        },
        Section {
            name: "connected",
            about: "connected= sessions: a controlled or controller pair",
            rows: &[para(
                "Connected form (Owner-only): `spawn connected=controlled|controller \
                 place=window|tab of=<sid> [cwd=<path>]` mints a `both` connection with \
                 `of=<sid>` (`controller` sets `ATERM_OBSERVE_SESSION_ID`; `place=window` is \
                 `ERR headless` without a GUI).",
            )],
        },
        Section {
            name: "fleet",
            about: "spawn on another Mac: the words a drive takes, plain shells, --host any",
            rows: &[
                para(
                    "Over an authenticated fleet drive, a worker's spawn takes `agent=`, \
                     `model=`, `autopilot=`, an absolute `cwd=` or `'cwd=~/<path>'` (a folder in \
                     the driven Mac's home, resolved there and refused where it leaves the home, \
                     lies in a folder macOS guards, or is not there; quoted, since bash turns an \
                     unquoted `cwd=~/…` into this Mac's own home before aterm sees it), `from=`, \
                     `why=<one line>`, `req=<id>` and `raise=false` alone, and each `ERR denied:` names the \
                     word it refused. `aterm ctl … spawn agent=… label=<word>` keeps the label on \
                     this Mac's `board` and never sends it.",
                ),
                para(
                    "There, `aterm ctl --host <h> spawn cwd=/absolute|'cwd=~/<path>' 'why=<one \
                     line>'` opens a background plain shell with no task or AI requirement. It \
                     requires the named folder and a `why=` of at most 200 characters saying what \
                     it is for (refused without one: the driven Mac's tab open keeps it as \
                     `why_text=` for whoever reviews what was asked of it), clears the host's \
                     global `-e` command, and accepts only `cwd=`, `why=` and optional `req=` and \
                     `raise=false`; no stdin frame, command, model or implicit folder. Either form \
                     opens parked under the driven Mac's Workers tab, a child of no tab there, and \
                     never raises its window; it and each line typed into it are logged \
                     `by=fleet:<node>:<sid>`, the session on the driving Mac that asked (`-` \
                     where none), and its open keeps that session's program, model and account. \
                     The creating node can close this shell while no person has adopted it and no \
                     foreground job runs. On its own untouched shell it may also read `blocks [n] \
                     [--json]`, `blocktext <id> [trim]`, and `await idle <0..22000>|block \
                     timeout=<0..22000>`. Zero timeout checks once without waiting. Ownership is \
                     checked again after each read or wait.",
                ),
                para(
                    "`--host any` still requires an AI worker, and takes \
                     `need=repo:<name>[,free:<n>m|g|t]`: the Mac it picks lists the checkout \
                     `~/<name>` with a HEAD in a read of its own within 30 min and has that much \
                     free on its home's volume, and one whose survey cannot say is never chosen \
                     (`aterm help workers`).",
                ),
            ],
        },
    ],
};
