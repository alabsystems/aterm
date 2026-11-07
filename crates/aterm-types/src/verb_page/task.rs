// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help task`: a worker's task record. `aterm-gui`'s `task_record` writes it
//! (`TaskRecord::wire_fields`, `task_reply`) and parses `task mark`.

use super::{Page, Section, para, row};
use crate::{checked_by_hand, no_checked_record};

/// The `task` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        row("task", "the session's task record, one line"),
        row(
            "task mark <working|done|exited|stalled|failed> [from=] [why=] [exit=] …",
            "set it by hand (owner; `help task mark`)",
        ),
        para(
            "Wait on it with `await task @<sid> since=<rev>`; read the worker's report with \
             `result`.",
        ),
    ],
    options: &[],
    reply: &[
        row(
            "OK task rev=<n> state=<s> from=<sid|ctl> since_ms=<ms> result_bytes=<n>",
            "then, each only when set: conclusion= why= checked= left= exit= result_path= \
             fact= fact_rev=, then agent= program_state= attention=",
        ),
        row("OK task -", "the session has no task"),
        row(
            "state=",
            "working, done, exited, stalled or failed (`help task states`)",
        ),
        row(
            "conclusion=",
            "finished or failed, once the task has ended; stalled ones read failed until they \
             resume",
        ),
        row(
            "OK task … changed=<0|1>",
            "task mark: the record as `task` prints it, and whether it moved",
        ),
    ],
    examples: &[
        row(
            "aterm ctl @s-ab12 task",
            "is that worker still working, and how did it end?",
        ),
        row(
            "aterm ctl await task @s-ab12 since=0",
            "wait for its task to end or ask (run it in the background)",
        ),
        row(
            "aterm ctl @s-ab12 result",
            "the report it ended its task with",
        ),
    ],
    sections: &[
        Section {
            name: "states",
            about: "each state and how a task moves between them; conclusion=",
            rows: &[
                para(
                    "`task` answers `rev= state= from= since_ms= result_bytes= [conclusion=] \
                     [why=] [checked=] [left=] [exit=] [result_path=] [fact= fact_rev=] agent= \
                     program_state= attention=` (or `OK task -` when the session has no task; \
                     `program_state=` is `status program_state=`, a codex's or Grok Build's own \
                     report, `-` otherwise).",
                ),
                para(
                    "`state=`: `working`, `done` (under Land, a clean `Nothing left:` defense and \
                     a `git` check that found nothing left to land where it could read; `aterm \
                     help harness` names where it cannot), `exited` (`exit=<code>`), or `stalled` \
                     (`why=startup|shell|held|signin|install|cancelled`; a `why=startup` one \
                     resumes plain `working`, with no `why=`, when its agent starts, a `why=held` \
                     one `working why=moved` when it moves, and the others stay `stalled` until a \
                     `task` mark; a worker a restart brings back is `working why=restored`, or \
                     `exited` where its agent does not come back), or `failed`: the work ended \
                     unfinished with its agent still there (`why=could-not-finish`, a Land \
                     worker's reply whose first line starts `Could not finish: <why>`; `why=off`, \
                     a person turned its Autopilot Off and it stayed Off 3 s, `why=off-ctl`, a \
                     manager or a script did, and turning it on again reopens either; \
                     `why=expired`, the disk worker's time ran out; `why=repeated`, Land stopped \
                     typing to a reply that came back the same with no work), reopened `working \
                     why=turn` by a turn into it or by Cruise set after it failed.",
                ),
                para(
                    "`conclusion=` says how any ended task ended, once it has: `finished` for \
                     `done` and for `exited` after any `done` (which keeps that `why=` and \
                     `checked=`), `failed` for `failed`, `stalled` (until it resumes) and any \
                     other `exited`.",
                ),
                para(
                    "`why=` is \
                     `landed|marker|done-check|exit|exit-status|no-exit-status|shell|startup|\
                     held|turn|moved|signin|install|cancelled|restored|could-not-finish|\
                     usage-limit|off|off-ctl|expired|repeated`.",
                ),
            ],
        },
        Section {
            name: "landing",
            about: "checked= and left=: what aterm checked at the landing",
            rows: &[
                para(concat!(
                    "`checked=` is what aterm checked at the landing that marked it `done` (also \
                     on `await task` rows): `<head>@<ref>` where `HEAD`'s 12 digits were on the \
                     remote main git named (`3f9a1c2e4b7d@origin/main`), else `pushed`, \
                     `no-remote`, `no-repo` or `gone`; none where ",
                    no_checked_record!(),
                    ".",
                )),
                para(
                    "`left=` is what that landing read of the processes its agent started that \
                     still ran (also on `await task` rows): `0`, `kept:<n>` (none but those a \
                     `keep <token>` line under its `Nothing left:` kept running), `<n>` (left \
                     running once Land had held three times on them) or `unchecked` (no whole \
                     reading); none where nothing was read.",
                ),
            ],
        },
        Section {
            name: "revs",
            about: "rev= and the attention flag, the event, and the fact word",
            rows: &[
                para(
                    "`rev=` increments on record changes and when the record's attention flag \
                     flips: an ask under `owner=supervisor|needs|codex-alab` raises it and a \
                     `kind=wait` entry there never does (`status` and `sessions` carry \
                     `task_state=<state|-> task_rev=<n>`, `subscribe … events` emits `EVENT \
                     <local> task rev=<n> state=<s> attention=<0|1> [why=<w>] [exit=<n>]`, and \
                     `turn` verdicts carry `task_rev=`).",
                ),
                para(
                    "`fact=<rule>@v<n>#<ordinal>|-` is the occupancy door's word for a managed \
                     worker's line the harness held for its manager (written under the owner's \
                     `[harness] occupancy = \"prompt\"` alone), shown once first edited on the \
                     reply, `await task` rows and the event (`fact= fact_rev=` at their end); \
                     `fact_rev=` moves on its own, never `rev=`, and no verb takes `fact=` as \
                     input.",
                ),
            ],
        },
        Section {
            name: "mark",
            about: "task mark: its words and what a hand mark may not set",
            rows: &[para(concat!(
                "`task mark <working|done|exited|stalled|failed> [from=<s-…|ctl>] [why=<word>] \
                 [checked=<word>] [left=<word>] [exit=<n>] [result_bytes=<n>] \
                 [result_path=<pct>]` (Owner-only; `checked=` and `left=` on `done` alone and \
                 from aterm's own loop alone: a hand mark naming either is refused, `ERR task \
                 mark: ",
                checked_by_hand!(),
                "`; a hand's `failed` is refused while a loop carries the session, whose \
                 Autopilot Off stops the typing and fails the task; `ERR denied` for edges; `ERR \
                 busy update-committing` if a seamless handoff blocks > 10 s) updates the record \
                 and replies with `changed=<0|1>`.",
            ))],
        },
    ],
};
