// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help log`: the session's hierarchical log. `aterm-gui`'s `session_log::LogQuery::parse`
//! reads the request; the kinds, keys and outs are `aterm_slog`'s (a test in `aterm-primer`
//! holds every code span here to them).

use super::{Page, Section, para, row};

/// The `log` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        row(
            "log [<node>] [<option>…] [--json]",
            "the session's log: runs, agent turns and steps under it, and point marks",
        ),
        row(
            "log n [since=<seq> ep=<e>] [n=<count>] [kinds=<k>,…] [--json]",
            "the instance's node log (owner): the word `n` alone, apart from `n=<count>`",
        ),
        row(
            "@<sid> log",
            "a closed session's log too, while the journal kept it",
        ),
    ],
    options: &[
        row(
            "<node>",
            "a subtree: `-` the session, `r<n>` a run, `t<n>` an agent turn, `p<n>` a step",
        ),
        row(
            "lvl=<l>",
            "the deepest level shown: session, run, turn or step",
        ),
        row(
            "since=<seq> ep=<e>",
            "only records past the cursor of an earlier reply (its as_of= and ep=)",
        ),
        row(
            "base=<b>",
            "the log the cursor is of (the header's base=, `status log_base=`)",
        ),
        row("n=<count>", "the newest <count> matching records"),
        row("kinds=<k>,…", "only these kinds (`help log records`)"),
        row(
            "class=turn",
            "only the turn class, what the bus mirror would carry",
        ),
        row(
            "tier=content",
            "add the in-memory-only content fields (default tier=host)",
        ),
        row("--json", "one JSON object per record"),
    ],
    reply: &[
        row(
            "OK <n> v=2 as_of=<seq> ep=<e> base=<b> oldest=<seq>[ lost=<k>][ more=<k>]",
            "the header (on stderr in `aterm ctl`), then <n> lines: a `gap why=<w> lost=<k> \
             resync_to=<seq>` line first where the read crossed records it cannot show, then the \
             records (`n=` counts records, so a gap makes one line more)",
        ),
        row(
            "<seq> <open|close|mark|amend|gap> <node> <kind> [par=<node>] <k=v …> …",
            "one record: … [by=<actor>] src=<where> conf=<exact|strong|heuristic|claim> t=<wall \
             ms> (`help log records`)",
        ),
        para(
            "Page with `since=<as_of> ep=<ep> base=<b>` from the header; a gap says what was lost \
             (`help log paging`).",
        ),
    ],
    examples: &[
        row(
            "aterm ctl @s-ab12 log lvl=turn",
            "that session's runs and agent turns, no steps",
        ),
        row(
            "aterm ctl @s-ab12 log since=812 ep=3 base=7",
            "what was logged after an earlier reply whose header said as_of=812 ep=3 base=7",
        ),
        row(
            "aterm ctl @s-ab12 log kinds=task,program",
            "its task marks and its program's reports",
        ),
    ],
    sections: &[
        Section {
            name: "records",
            about: "the nodes, the point marks and their fields, a codex's claimed steps",
            rows: &[
                para(
                    "A hierarchical session log: session (`-`), runs (`r<n>`), agent turns \
                     (`t<n>`) and steps (`p<n>`), plus point marks, among them `human` \
                     (`why=mail`, `reached=0`), `input` (`verb= [act=] [re=] [dropped= \
                     [pool=shared]] [cause= cause_k=]`), `task` (`rev= state= attention= [why=] \
                     [code=<exit>]`), `agent`, `program` (OSC 7501 `status program_state=`: \
                     `state=<idle|working|blocked|done|error|none> \
                     [ask=<permission|question|auth>] [coalesced=<n>] src=engine conf=claim`, at \
                     most 64 a run, the oldest taken first and counted in `lost=`; past 20 at once \
                     one a second, a flap coalesced to its newest state, which is never dropped), \
                     `shown` (on `-`, what the person is shown: `[need=<word> sev=<word>] \
                     [held=<word>] [coalesced=<n>] by=aterm src=aterm conf=strong`, a flap past 20 \
                     at once, then one a second, coalesced the same way; local, never mirrored), \
                     `row` (`row=<kind>`: a timeline row held as a row alone, such as \
                     `job-change`) and `usage` (`h5=` and `d7=`, the plan's 5-hour and 7-day \
                     windows used in percent, `by=vendor`); `kinds=` takes any kind the log \
                     writes. `lvl=` is `session|run|turn|step`.",
                ),
                para(
                    "On a codex tab an approval or question step is the program's own claim \
                     (`src=engine conf=claim`): one per box a status sweep's program mark shows \
                     (stacked boxes of one kind share one; a box shown and answered between two \
                     sweeps, or one whose `blocked` coalesced past the bucket, has none), closed \
                     when the box goes, `out=gone` when its program or its turn went.",
                ),
                para(
                    "Header (stderr on `aterm ctl`): `OK <n> v=2 as_of=<seq> ep=<e> base=<b> \
                     oldest=<seq>[ lost=<k>][ more=<k>]`, then `<seq> \
                     <open|close|mark|amend|gap> <node> <kind> [par=<node>] <k=v ...> \
                     [by=<actor>] src=<where> conf=<exact|strong|heuristic|claim> t=<wall ms>` \
                     (`close` adds `out=`, `dur_ms=`, `dur=wall`, `steps= appr= qs= walls= \
                     stalls= errs= lost=`; `detail=` holds a sanitized command or wall reset).",
                ),
                para(
                    "`tier=content` adds the in-memory-only `title=` (or `title_fp=` alone for a \
                     title that is the command line the shell announced), `meta field=attention \
                     attn_text=` (`fp=`), `reason=`, `topic_name=`, `since=`, `closer=`, \
                     `reply=`, `row_text=`.",
                ),
            ],
        },
        Section {
            name: "commands",
            about: "what a run keeps of a command: its program name, and when it has none",
            rows: &[para(
                "It keeps a command's program name, and only once that program has started (the \
                 kernel saw its image run; a shell builtin's word typed bare needs none, and an \
                 `amend` carries a name the `open` could not) and, for a listed tool, one known \
                 subcommand word, never its other arguments or typed bytes; where the typed word \
                 is no program file that ran (an alias, a function), the first program the \
                 command's own job ran stands in for it until an `amend` names the word itself; a \
                 line that runs no simple command at its top (a function's definition, a line \
                 that starts with a subshell) makes no run under a bash before 5.3, which has no \
                 hook that hears it (5.3's `PS0` does).",
            )],
        },
        Section {
            name: "paging",
            about: "since=, ep= and base=, handoffs, gaps and resets",
            rows: &[
                para(
                    "Page with `since=<as_of> ep=<ep>`, and add `base=<b>` (the header's, `status \
                     log_base=`) to name the log the cursor is of: another log's cursor, even one \
                     an update's epochs place in this log's range, is then reset. A `since=` or \
                     `base=` with no `ep=` is the usage line.",
                ),
                para(
                    "A reply whose read crossed records it cannot show opens with one line, no \
                     `<seq>` before it: `gap why=<handoff|crash|pruned|evicted|reset|unread> \
                     lost=<k> resync_to=<seq>` (`lost=` what it missed, a lower bound across a \
                     handoff; `resync_to=` the seq the records after it begin past). The log \
                     itself records an update as a `<seq> mark - handoff carried=<n> shed=<m> \
                     epoch=<ep>` record, or an update that carried nothing as a `<seq> gap - gap \
                     why=handoff epoch=<ep>` record.",
                ),
            ],
        },
        Section {
            name: "closed",
            about: "a closed session's log, the journal that keeps it, and the fabric mirror",
            rows: &[para(
                "A closed session stays readable as `@<sid> log` (its last 512, journaled while \
                 `status log_journal=` is `on`, or `armed` or `paused` until it starts or \
                 resumes, and by an update's successor once `handed`; `stopped` and `off` journal \
                 nothing more, and `log_journal_why=` says why: `[session_log] journal = false`, \
                 `restore_session = false`, an instance that keeps no desktop, as a headless one \
                 does, a platform other than macOS and Linux, no logs folder (`no-folder`), or \
                 what stopped the writer), mirrored under `[fabric] log = \"auto\"|\"turns\"`.",
            )],
        },
        Section {
            name: "node",
            about: "log n: the instance's node log and its marks",
            rows: &[para(
                "`log n` (Owner-only, no `base=`) reads the instance node log (`open n start \
                 prev_end=<clean|handoff|killed|signal|panic|running|unknown|none|->`, `mark n \
                 link state=<stalled|connected|disconnected> gen=<n> by=bridge|aterm`, `mark n \
                 config key=<key>` / `config n=<k>`).",
            )],
        },
    ],
};
