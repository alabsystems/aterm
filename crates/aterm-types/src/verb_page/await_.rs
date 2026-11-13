// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help await`: block until a condition holds. `aterm-gui`'s `control_session::cmd_await`
//! parses the screen, agent, task and log kinds; inbox, sys, consent, momentum, wake, edit and
//! elsewhere have their own readers. `aterm help sys wait` is the one reference for `await sys`.

use super::{Page, Section, para, row};

/// The `await` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        row("await idle <ms>", "no output for <ms>"),
        row(
            "await seq [<n>]",
            "the screen's content seq passes <n> (default: the current one)",
        ),
        row(
            "await match <re> [rows <a> <b>]",
            "a visible row matches <re>",
        ),
        row(
            "await gone <re> [rows <a> <b>]",
            "every row in range is clear of <re>",
        ),
        row("await frame", "an open DEC 2026 frame closes"),
        row(
            "await block",
            "a command finishes after the wait is armed (shell integration)",
        ),
        row(
            "await agent <word>[,<word>…] [since=<rev>]",
            "`status agent=` is one of the words at a rev past since= (a codex: `help await agent`)",
        ),
        row(
            "await task [@<sel>[:<rev>],…] [any|all] [since=<rev>] [facts=1]",
            "a worker's task ends, or asks for someone (`aterm ctl await task board …` too)",
        ),
        row(
            "await log need|open=<lvl>|close=<lvl>|kind=<k> [since=<seq> ep=<e> [base=<b>]]",
            "a session log record (a codex's own reports: kind=program)",
        ),
        row(
            "await inbox since=<id>|re=<off> [topic=<t>] [kinds=<k|hold,…>]",
            "mail, a reply, a broadcast or a hold change",
        ),
        row("await wake", "the session's next ready wake (`wake`)"),
        row(
            "await sys <condition> [for=<dur>] [scope=tab|aterm]",
            "a machine state, a job's verdict or a volume's free space (`aterm help sys wait`)",
        ),
        row(
            "await momentum <floor>",
            "`trail status momentum=` falls to <floor> (0 to 1)",
        ),
        row(
            "await consent [kind=debugger]",
            "the session's privacy consent changes",
        ),
        row(
            "await edit ticket=<t>|uri=<file-uri>",
            "an Editor tab's edit ends (owner)",
        ),
        row(
            "await elsewhere",
            "until the caller hangs up: `aterm ctl` holds it while another aterm serves its wait",
        ),
    ],
    options: &[
        row(
            "timeout=<ms>",
            "the server's wait: default 30000 (await consent 300000), a longer one held at \
             600000. `aterm ctl await task|agent|inbox|edit` asks again in legs up to its \
             timeout= (any length), or with none for an hour (`help await client`)",
        ),
        row(
            "autopilot=<cruise|land|off|->",
            "idle, seq, gone, agent and log: also answer `OK autopilot <word>` once `meta \
             autopilot=` is another word than the one given",
        ),
    ],
    reply: &[
        row(
            "OK <kind> <seq>",
            "idle, seq, match, gone, frame and block: the condition held at content seq <seq>",
        ),
        row(
            "OK agent <word> rev=<n>",
            "await agent; each other kind's reply is in its section",
        ),
        row(
            "OK timeout",
            "the time ran out (`aterm ctl` exits 124); the condition never held",
        ),
        row(
            "@<sid> rev= state= …",
            "await task, in `aterm ctl`: one row per worker on stdout, then `OK timeout` on \
             stderr if none had news in time (`help await task`)",
        ),
        row(
            "ERR exited",
            "the session ended. `ERR control server busy; retry`: the instance's 64 wait lanes \
             and its spare request lanes are taken; `ERR too many waits open on this session; \
             retry when one ends`: 256 screen waits on that session",
        ),
    ],
    examples: &[
        row(
            "aterm ctl @s-ab12 await match 'Finished' timeout=600000",
            "wait up to 10 minutes for a row that shows Finished",
        ),
        row(
            "aterm ctl @s-ab12 await agent idle,prompt,question,wall since=41",
            "after `turn … settle=submitted` gave agent_rev=41: wait for Claude Code to stop",
        ),
        row(
            "aterm ctl await task @s-ab12,@s-cd34 any since=0",
            "wake when either worker's task ends or asks (an hour at most); run it in the \
             background, and wait again from each row's rev= (`@s-ab12:<rev>,…`)",
        ),
    ],
    sections: &[
        Section {
            name: "screen",
            about: "idle, seq, match, gone, frame and block in full",
            rows: &[
                para(
                    "A wait blocks on a condition until it holds or its `timeout=` runs out \
                     (OPTIONS, `help await client`); the instance's 64 wait lanes, then its spare \
                     request lanes, serve waits, and past them it answers `ERR control server \
                     busy; retry`.",
                ),
                para(
                    "`await idle <ms>`: no output for `<ms>`. `await seq [<n>]`: content `seq` > \
                     `<n>`, by default the current one, and an `<n>` past it answers at once (`OK \
                     seq <current>`). `await match <re> [rows <a> <b>]` / `await gone <re> [rows \
                     <a> <b>]`: a visible row matches, or every row in range is clear of `<re>` on \
                     a closed DEC 2026 frame or for 1 s (`ERR bad rows` on an empty span).",
                ),
                para(
                    "`await frame`: an open DEC 2026 frame closes (60 s hang cap; `text --json` \
                     `\"frame\":\"open\"`). `await block`: a command finishes after the wait is \
                     armed, by an OSC 133/633 `D` that follows its `C`, which follows its \
                     prompt's `A` and `B`, with no other `A` and no reset (RIS) between them (a \
                     `D` out of that order answers nothing; `wait` also answers a command that \
                     already finished).",
                ),
                para(
                    "`await idle`, `seq`, `gone`, `agent` and `log` take \
                     `autopilot=<cruise|land|off|->`, the word `meta autopilot=` last printed: \
                     they also answer `OK autopilot <word>` when the stored word is another, at \
                     once or on the write that moves it (`ERR usage` for any other spelling).",
                ),
            ],
        },
        Section {
            name: "agent",
            about: "await agent's words, and waiting on a codex instead",
            rows: &[
                para(
                    "`await agent <word>[,<word>…] [since=<rev>]`: `status agent=` is one of \
                     `busy|prompt|question|idle|survey|unknown|-|wall|wall:<kind>` at a `rev` \
                     past `since=`, such as a `settle=submitted` turn's `agent_rev=` (`OK agent \
                     <word> rev=<n>`).",
                ),
                para(
                    "A codex's `agent=` stays `unknown` (aterm reads no Codex screen), so `await \
                     agent` never answers on its state; `await log kind=program since=<log_seq> \
                     ep=<log_ep>`, both read from `status` before typing to it, answers on the \
                     program's own report (`program state=<w> [ask=<k>] [coalesced=<n>] \
                     src=engine conf=claim`). On a codex tab an approval or question step is the \
                     program's own claim (`src=engine conf=claim`): one per box a status sweep's \
                     program mark shows (stacked boxes of one kind share one; a box shown and \
                     answered between two sweeps, or one whose `blocked` coalesced past the \
                     bucket, has none), closed when the box goes, `out=gone` when its program or \
                     its turn went; `await log need` answers on its open.",
                ),
            ],
        },
        Section {
            name: "task",
            about: "await task: selectors, any|all, since=, facts=, the rows it answers",
            rows: &[
                para(
                    "`await task [@<sel>[:<rev>[:<fact_rev>]][,...]] [any|all] [since=<rev>] \
                     [facts=1 [fact_since=<fact_rev>]]`: a `task` `rev` past `since=` whose state \
                     is not `working` or whose ask stands (never a `kind=wait` entry), with \
                     `facts=1` a fact word noted past `fact_since=` (by default the `fact_rev` at \
                     the wait; a clear wakes nothing), or the session closing.",
                ),
                para(
                    "`any` (the default) answers at the first target with news, `all` once every \
                     target has had some. The server answers `OK <n>`, or `OK <n> \
                     status=timeout` when its leg ran out, then one row per target; `aterm ctl` \
                     prints the rows alone on stdout, and `OK timeout` on stderr (exit 124) when \
                     the whole wait ran out. A row is `@<sid> rev=<n> \
                     state=<working|done|exited|stalled|failed|-|closed> agent=<word> \
                     program_state=<word|-> attention=<pct|-> result_bytes=<n> \
                     [task_state=<state>] [conclusion=<finished|failed>] [why=<word>] \
                     [checked=<word>] [left=<word>] [exit=<n>] [fact=<word|-> fact_rev=<n>] \
                     task_news=<0|1>` rows, `conclusion=`, `why=`, `checked=`, `left=` and `exit=` \
                     as `task` says (a tab closed on a task with no conclusion reads \
                     `conclusion=failed`; a closed row's `task_state=` is its record's own state, \
                     so `task_state=working conclusion=failed` is that closed tab's reading, not \
                     an end its task came to); `program_state=` is `status program_state=`, a \
                     claim that never wakes it; `task_news=1` marks a target that had news for \
                     this wait (it woke it, or under `all` had its share), `0` one whose row is \
                     only how it stands now. Wait again from each row's `rev=` \
                     (`@<sid>:<rev>,…`).",
                ),
                para(
                    "`aterm ctl [--host <h>] await task board …` waits on a `board`'s open rows \
                     instead of named sids.",
                ),
            ],
        },
        Section {
            name: "client",
            about: "how `aterm ctl` times a wait: legs, an hour, --timeout",
            rows: &[para(
                "The server holds one wait at most 600000 ms. `aterm ctl` asks an `await task`, \
                 `await agent`, `await inbox` or `await edit` again in legs of at most 600 s \
                 until its own `timeout=` runs out, which may be any length; with no `timeout=` \
                 an explicit `--timeout` is the whole wait (`--timeout 0`: no end), and with \
                 neither it waits an hour. A `timeout=` it names wins over `--timeout`, except \
                 over `--host` and on `await task board`, where `--timeout` ends the wait \
                 whatever its `timeout=` (`aterm ctl --help`). Any other kind is one request: \
                 its `timeout=` is the server's, held at 600000. Run a long wait as a background \
                 command: a tool that ends commands sooner (Claude Code's Bash, at 2 minutes) \
                 cuts it.",
            )],
        },
        Section {
            name: "log",
            about: "await log: need, open=, close=, kind=, the cursor and gaps",
            rows: &[para(
                "`await log need|open=<lvl>|close=<lvl>|kind=<k> [since=<seq> ep=<e> [base=<b>]]` \
                 (`<lvl>` in `session|run|turn|step`; `need` also answers a run's `out=fail` \
                 close, the session's close and a `mark` need (`conf=claim`); `status log_seq= \
                 log_ep= log_base=`, `base=` naming the log so another log's cursor is answered \
                 `OK gap why=reset`): `OK seq=<n> ep=<e> [base=<b>] [as_of=<head> latched=1] \
                 [gap=<why>:<lost>] <record>` or `OK gap why=<w> lost=<k> resync_to=<seq> ep=<e> \
                 [base=<b>]` (`base=` this log's, when the wait named one: re-arm with it).",
            )],
        },
        Section {
            name: "inbox",
            about: "await inbox: since=, re=, topic=, kinds= and holds",
            rows: &[para(
                "`await inbox since=<id>|re=<off> [topic=<t>] [kinds=<k|hold,...>]`: an inbox row \
                 past `since=` of those kinds, a reply to `re=<off>` (an `ack … verdict=` wakes \
                 it), a `post to=say:<t>` broadcast, or, with `hold` among `kinds=`, a `hold` \
                 change (`OK inbox <id>`, `OK inbox hold=0|1`; `ERR bad since high=<n>`). \
                 `aterm help fabric` has the message fields.",
            )],
        },
        Section {
            name: "wake",
            about: "await wake: the session's declared wakes",
            rows: &[para(
                "`await wake [timeout=<ms>]`: the session's next ready wake (`wake`), its `need=` \
                 holding, a build slot reserved where that names `slots` (`OK wake #<n> ready \
                 since=<ms>[ slots=<usable>/<n>]`), or its last open wake's end (`EXPIRED wake \
                 #<n> why=<due|unless|agent|path|off|admit|rm|taken|told>`); one parked at a time \
                 a session (`ERR busy await wake parked`), `ERR usage await wake: no wake \
                 pending`, and `ERR denied wake: <why>` from outside the session's tree as `wake` \
                 says. A reply that cannot be written leaves the wake ready.",
            )],
        },
        Section {
            name: "other",
            about: "await sys, momentum, consent and edit",
            rows: &[
                para(
                    "`await sys <condition> [for=<dur>] [scope=tab|aterm]`: a machine state, a \
                     job's verdict, or a volume's free space (`OK sys <metric>=<value> … held_ms= \
                     as_of=`); every term, value, unit and reply is in `aterm help sys wait`.",
                ),
                para(
                    "`await momentum <floor 0..=1>`: `trail status momentum=` falls to `<floor>` \
                     (`OK momentum <v>`).",
                ),
                para(
                    "`await consent [kind=debugger] [timeout=<ms>]`: this session's consent \
                     (`privacy`'s `full_disk_access=` and its `session` row's `fs_consent=` and \
                     `attribution=`; with `kind=debugger`, `debugger mode=`) differs from the \
                     first completed reading (`OK consent fs_consent= fda= attribution= \
                     elapsed_ms=`, or `OK consent debugger_mode= elapsed_ms=`).",
                ),
                para(
                    "`await edit ticket=<t>|uri=<file-uri> [timeout=<ms>]` (Owner-only): `OK edit \
                     done result=<finished|closed|aborted> saved=<0|1> seq=<n>`, `ERR unknown \
                     ticket` or `ERR hangup`.",
                ),
            ],
        },
    ],
};
