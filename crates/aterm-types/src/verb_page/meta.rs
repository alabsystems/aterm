// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help meta`: a session's metadata, keyed attention, the supervisor claim and a self-claiming
//! harness's reports. `aterm-gui`'s `control_session` answers it; the byte caps are
//! `session_timeline`'s `META_*_MAX`.

use super::{Page, Section, para, row};

/// The `meta` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        row("meta", "read the session's metadata (one line)"),
        row(
            "meta set <title|description|icon|role|attention|questions|autopilot> <text…>",
            "set a field (questions and autopilot: owner)",
        ),
        row("meta unset <field>", "clear it"),
        row(
            "meta set attention owner=<k> [kind=ask|wait] <text…>",
            "keyed attention: one entry per writer (`meta unset attention owner=<k>`)",
        ),
        row(
            "meta set supervisor <holder> [ttl=<ms>] [contract=3 scope=<s,…> …]",
            "claim the session for a supervisor (owner; `meta unset supervisor [holder=<h>]`)",
        ),
        row(
            "meta set|prepare harness_act …, meta set|unset harness_held|harness_land|harness_needs …",
            "a proven self-claim reports its acts, its stop, its Land hold and its needs",
        ),
    ],
    options: &[
        row(
            "title",
            "120 bytes; sets `user_title=`, which overrides the program's OSC title",
        ),
        row("description", "1024 bytes"),
        row("icon, role", "64 bytes each"),
        row(
            "attention",
            "256 bytes; keyed: 200 bytes, at most 8 owners with the bare `-`",
        ),
        row(
            "questions",
            "ask|recommended|long-term|short-term: overrides `[harness] answer_questions` and \
             `question_priority` (owner)",
        ),
        row(
            "autopilot",
            "cruise [until=HH:MM|+DURATION]|land|off: the session's Autopilot (owner)",
        ),
    ],
    reply: &[
        row(
            "OK title= user_title= description= icon= role= attention= cwd= state= …",
            "then attention_owner= attention_owners= attention_kinds= supervisor= \
             supervisor_proven= questions= agent_cwd= trusted_cwd= worker= autopilot= land_at= \
             autopilot_by= autopilot_since=",
        ),
        para(
            "Values are percent-encoded; `-` is unset, but `title=` (the tab's title as the \
             terminal shows it) is empty when there is none. A set or unset answers `OK`, or `ERR \
             …` (`help meta <section>` names each; `help meta fields` says what each field is).",
        ),
    ],
    examples: &[
        row("aterm ctl @self meta", "this session's metadata"),
        row(
            "aterm ctl @self meta set attention owner=build kind=wait waiting on CI",
            "a record under the owner `build` that never counts as an ask",
        ),
        row(
            "aterm ctl @s-ab12 meta set autopilot land",
            "switch that session's Autopilot to Land",
        ),
    ],
    sections: &[
        Section {
            name: "fields",
            about: "every field the read prints, and the user fields' caps",
            rows: &[
                para(
                    "`meta` answers `OK title= user_title= description= icon= role= attention= \
                     cwd= state=` then `attention_owner= attention_owners= attention_kinds= \
                     supervisor= supervisor_proven= questions= agent_cwd= trusted_cwd= worker= \
                     autopilot= land_at= autopilot_by= autopilot_since=` (pct-encoded; `-` is \
                     unset, an unset `title=` is empty; `trusted_cwd=` and `worker=` come from \
                     `spawn agent=<word>`; the window is in `sessions` and `dims`).",
                ),
                para(
                    "`meta set <title|description|icon|role|attention|questions|autopilot> \
                     <text...>` / `meta unset <field>` set or clear user metadata (title 120 B, \
                     description 1024 B, icon 64 B, role 64 B, attention 256 B; `user_title=` \
                     overrides the OSC `title=`).",
                ),
                row(
                    "title=",
                    "the title the terminal holds (the program's OSC title), empty with none; \
                     `user_title=` the one `meta set title` gave, which the tab shows instead",
                ),
                row("cwd=", "the folder the shell last reported"),
                row(
                    "state=",
                    "the session's life: `spawning`, `alive` or `exited`",
                ),
                row(
                    "attention_owner=",
                    "the owner of the entry `attention=` shows (`%2D` the bare one); \
                     `attention_owners=` how many entries stand; `attention_kinds=` each as \
                     `<ask|wait>:<owner>` (`help meta attention`)",
                ),
                row(
                    "supervisor=",
                    "the live claim's holder; `supervisor_proven=1` where it is proven (`help \
                     meta supervisor`)",
                ),
                row(
                    "questions=",
                    "the session's own answer word, `-` where `[harness]` decides",
                ),
                row(
                    "agent_cwd=",
                    "the folder the agent's process group runs in, read from the process; \
                     `trusted_cwd=` and `worker=` a `spawn agent=<word>` worker's folder and agent",
                ),
                row(
                    "autopilot=",
                    "the session's own Autopilot word (`-`: none, `[harness]` decides); \
                     `land_at=` a timed Cruise's end, unix seconds",
                ),
                row(
                    "autopilot_by=",
                    "who set it: `human:chip`, `human:menu`, `human:settings`, `human:every`, \
                     `human:launch`, `human:border`, `ctl`, `harness`, `bridge`, `aterm` (a \
                     timer), a session's sid, `restored` (from before a restart), `-`; \
                     `autopilot_since=` when, unix seconds",
                ),
            ],
        },
        Section {
            name: "attention",
            about: "keyed attention: owners, kind=ask and kind=wait, what each surface shows",
            rows: &[para(
                "KEYED ATTENTION: `meta set attention owner=<k> [kind=ask|wait] <text...>` / `meta \
                 unset attention owner=<k>` (`<k>` in `[A-Za-z0-9._:@/-]{1,64}`, text <= 200 B, \
                 at most 8 owners with the bare `-`, else `ERR attention owners full`) lets \
                 writers track escalations apart. `kind=ask` (the default, unless the text starts \
                 `limited:` while the harness handles the wall) is a need (`status needs=ask`: \
                 the tab's hover, the Overview, Next That Needs You); `kind=wait` is a record that never counts and never wakes `await \
                 task`, and one written on the connection a proven claim (`supervisor_proven=1`) \
                 is bound to lapses when that claim ends or that connection closes. \
                 `attention_kinds=` lists `<ask|wait|->:<owner>`; `attention=` shows the newest \
                 entry (`attention_owner=`, `%2D` for the bare one; `sessions`, `task` and `await \
                 task` rows show the newest ask before any wait); each change is a timeline \
                 `meta-change` row.",
            )],
        },
        Section {
            name: "supervisor",
            about: "the supervisor claim, proven claims and contract 3's scoped claims",
            rows: &[
                para(
                    "SUPERVISOR: `meta set supervisor <holder> [ttl=<1..600000>]` / `meta unset \
                     supervisor [holder=<holder>]` (Owner-only; `ERR busy supervisor=<holder>` \
                     while another holds it) raises no need for a prompt while the supervisor \
                     handles prompts (`supervisor_proven=1`: a direct foreground \
                     `codex-alab@<pid>` connection, or a contract 3 claim).",
                ),
                para(
                    "CONTRACT 3: `meta set supervisor <id>@<pid> contract=3 \
                     scope=<relaunch|upgrade|turn-end|approvals|questions|walls>[,…] \
                     [version=<MAJOR.MINOR.0[+meta]>] [vendor=<version>]` claims a session for a \
                     rostered harness (`grok-alab`: scope `relaunch,upgrade`), bound to its \
                     connection and proven or refused whole (`ERR unproven`: the claimant is not \
                     the direct peer that leads the foreground group on a build of its program \
                     from atpkg's store or its dev link; `ERR usage`: a scope word unknown or \
                     outside the row); aterm's built-in engine stands down from exactly its scope; \
                     a rostered holder never claims without `contract=3`, and an unrostered one's \
                     is an ordinary claim, never proven.",
                ),
            ],
        },
        Section {
            name: "self-claim",
            about: "harness_act, harness_held, harness_land and harness_needs of a proven claim",
            rows: &[
                para(
                    "HARNESS_ACT: `meta set harness_act <act> [until_us=<n>]` and `meta prepare \
                     harness_act` (`OK until_us=<n> lease_ms=5000`; `ERR expired harness_act`, \
                     `ERR busy harness_act`) record a proven self-claim's act (`timeline` \
                     `harness-act act= holder=`, `status last_act=`) as its holder's: an \
                     approval, an answer, a continuation, a resume or a land prompt lights the \
                     Autopilot chip and joins its title's trail and the Settings ▸ Harness row's \
                     `last:` line named for the holder (`codex-alab: ✓ approved`); a landing \
                     shows as the chip's landed face. Every act is booked in the session's ledger \
                     (`aterm harness ledger @<sid>`: `self-claim <holder> <word> <act>`), and its \
                     approvals, answers, continuations, resumes, land prompts, landings and \
                     upgrades count in a row of its own on Settings ▸ Harness (`codex-alab did`), \
                     never in what aterm's harness did; the same standing face's act told again \
                     at the same turn is booked once.",
                ),
                para(
                    "HARNESS_HELD: `meta set harness_held same-reply|no-work` / `meta unset \
                     harness_held` (`OK`; `ERR denied` from anything but the session's standing \
                     proven self-claim) tell that claim's own stop, the arm of the hold that \
                     stopped its typing, or its lift: while the claim stands it is `status held=` \
                     and the stop the Overview, the tab's hover and the Settings ▸ Harness state \
                     line say in codex-alab's name, none of them while \
                     the tab reads landed or quiet, as for aterm's own stop, and it lapses with \
                     the claim (its release or replacement, its connection's close, its process \
                     gone; the claim after a `ttl=` lapse starts with none).",
                ),
                para(
                    "HARNESS_LAND: `meta set harness_land since=<ms> nudges=<n> [read=1 \
                     legs=<leg>[,<leg>…] [open=<word>[,<word>…]] [changed=<n>] [untracked=<n>] \
                     [stashes=<n>] [unpushed=<n>] [unlanded=<n>] [left=<n>]]` / `meta unset \
                     harness_land` (`OK`; `ERR denied` as for HARNESS_HELD; `ERR usage: …`, with \
                     or without a claim, for an unknown or repeated token, a missing `since=` or \
                     `nudges=`, a count or `since=` that is not plain digits or a `since=` past an \
                     i64, a `read=` other than `1`, a leg word other than `needs`, `repo`, \
                     `repo-unread`, `left`, `open`, `defense`, `cut`, `read=1` and `legs=` \
                     without each other, `open=`, a repository count or `left=` without \
                     `read=1`, or any token after `unset harness_land`) tell why that claim's own \
                     Land still holds: when its land act was typed (unix ms), the nudges since, \
                     and once a yield was judged its legs, up to four percent-encoded open words, \
                     the repository's counts and the processes its agent left running, neither \
                     ended nor kept (`left`). While the claim stands it is `status land_held= \
                     land_nudges= land_since=`, and the hold the Overview, the tab's hover and \
                     the Settings ▸ Harness state line say \
                     in codex-alab's name, only while the tab reads Land and neither landed nor \
                     quiet, as for aterm's own hold, and it lapses with the claim as \
                     HARNESS_HELD's stop does.",
                ),
                para(
                    "HARNESS_NEEDS: `meta set harness_needs entry=<pct text> need=<pct need> \
                     [need=<pct need> …]` / `meta unset harness_needs` (`OK`; `ERR denied` as for \
                     HARNESS_HELD; `ERR usage: …`, with or without a claim, for an unknown token, \
                     no `entry=`, `entry=` twice or empty, no `need=`, more than 32, a need empty \
                     or blank (only spaces, controls or format characters), not UTF-8 or over 640 \
                     bytes decoded, a bad percent escape, or any token after `unset \
                     harness_needs`) tell every need that claim's own `owner=needs` entry stands \
                     for, whole and in the worker's order, `entry=` being that entry's text as it \
                     set it, or that the entry went: told as aterm's own harness tells its set, so \
                     a set told before its entry stands waits for it and one told for another \
                     entry's text is never shown.",
                ),
                para(
                    "While the claim stands the tab's hover gives each need a `Needs you:` line, \
                     and `status needs_n=` counts them and any the entry's `(+N more)` counts past the \
                     32 told, as for aterm's own harness's set; it lapses with the claim as \
                     HARNESS_HELD's stop does, and a codex-alab that tells none is shown by its \
                     entry's first need and count.",
                ),
            ],
        },
        Section {
            name: "autopilot",
            about: "questions and autopilot: overriding the harness, cruise until=, Land's ends",
            rows: &[
                para(
                    "QUESTIONS: `meta set questions ask|recommended|long-term|short-term` / `meta \
                     unset questions` (Owner-only) override `[harness] answer_questions` and \
                     `question_priority`.",
                ),
                para(
                    "AUTOPILOT: `meta set autopilot cruise [until=HH:MM|+DURATION]|land|off` / \
                     `meta unset autopilot` (Owner-only; aliases `forever`/`infinity` and \
                     `wrap-up`/`wrapup`/`wrap`; `until=` goes with `cruise` only) sets the \
                     session's Autopilot (`OK`, plus `land_at=<unix s> \
                     local=<date>T<HH:MM><offset>` for `until=`, plus `journaled=0` when the disk \
                     sync took over 5 s); a finished Land sets `task=landed`, and a worker's own \
                     failed conclusion under Land (`Could not finish:`, `why=repeated`) \
                     `task=failed`.",
                ),
            ],
        },
    ],
};
