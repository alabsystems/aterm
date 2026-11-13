// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help turn`: one human turn. The options are [`crate::control_verbs::TURN_OPTIONS`] (a test
//! holds every one to a row here), parsed by `aterm-gui`'s `control_session::turn_options`.

use super::{Page, Section, para, row};
use crate::turn_submit_busy_reply;

/// The `turn` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        row(
            "turn [<option>=<value>…] [--] <text>",
            "type <text>, press Enter and make sure it went, wait for the screen to settle, and \
             reply with the settled screen",
        ),
        row(
            "turn submit=none <text>",
            "type <text> and wait for the screen to settle, pressing nothing",
        ),
        row(
            "turn typed=1 settle=submitted <text>",
            "into Claude Code: press the keys one by one and reply once the submit is seen; then \
             `await agent idle,prompt,question,wall since=<agent_rev>` (a codex: `help turn \
             submit`)",
        ),
        para(
            "The text goes to the program in front: an agent (Claude Code, codex-alab) reads it \
             as its prompt, a shell as a command; read `status program= agent=` first.",
        ),
        para(
            "Leading, before the options: `id=<epoch>:<producer>:<seq>` (exactly once), \
             `if-supervisor=<holder>`, a scoped harness's `if-supervisor-revision=<n> \
             harness-duty=<scope>` and the window harness's `harness-lane=<lane>` (`help key`). \
             The options end at the first word that is none of them, or at `--` (`help turn \
             words`).",
        ),
    ],
    options: &[
        row(
            "idle=<ms>",
            "the quiet that settles the screen (default 1500; 1 to 600000)",
        ),
        row(
            "timeout=<ms>",
            "the whole turn's bound (default 240000, at most 600000)",
        ),
        row(
            "submit=<key|none|guarded:<re>>",
            "the submit key, any `key` name (default enter); none presses nothing; guarded:<re> \
             presses Enter only while the cursor row matches <re>",
        ),
        row(
            "submit_window=<ms>",
            "how long a submit has to show (default 2000; 1 to 600000)",
        ),
        row(
            "presses=<n>",
            "Enters pressed within submit_window= (default 3, at most 10)",
        ),
        row(
            "submit_verify=<auto|seq|block>",
            "what proves the submit: auto (the default) by the program in front, seq by the \
             screen moving, block by the shell's command block",
        ),
        row(
            "settle=<how>",
            "match:<re> a visible row matches; gone:<re> <re> shows within submit_window= and \
             leaves; submitted: reply once the submit is seen (status=submitted)",
        ),
        row(
            "trim=<0|1>",
            "drop the reply's trailing blank rows (hash= covers them all)",
        ),
        row(
            "typed=<0|1>",
            "1: press the text key by key instead of one paste (`help turn typed`)",
        ),
        row(
            "cadence=<ms>",
            "typed=1's pace per key (default 70; 1 to 2000)",
        ),
        row(
            "cap=<n>",
            "typed=1's keys pressed one by one (default 240, at most 4096); the rest goes as \
             typed runs",
        ),
        row(
            "yield=<floor>",
            "wait for `trail status momentum=` to fall to <floor> (0 to 1) before typing",
        ),
        row(
            "if-gen=<epoch>.<seq>",
            "type only if the screen generation is still `status gen=`",
        ),
        row(
            "if-rows=<first>-<last>:<hex16>",
            "type only if those rows still hash (FNV-1a-64) to <hex16>",
        ),
        row(
            "if-human=<n>",
            "type only if the person gave no input since `status human_seq=` <n>",
        ),
        row(
            "cause=@<off>|<seq>",
            "link the turn in `log` to held mail (@<off>) or a log record (<seq>)",
        ),
    ],
    reply: &[
        row(
            "OK <n> turn submitted=<0|1> status=<settled|timeout|hangup|submitted> seq=<n> \
             id=<n> dur_ms=<n> hash=<hex16>",
            "then the settled screen's <n> rows, as `text` prints them. Optional fields follow \
             hash= in this order: submit_by=<claim|composer|block|screen> block=<id> \
             typed=<keys> pasted=<graphemes> yielded_ms=<ms> pressed=<n> task_rev=<n> \
             agent_rev=<n> frame=open trimmed=<k>",
        ),
        row(
            "OK 0 turn skipped reason=<why> …",
            "it stopped before the submit, what it typed left standing: \
             reason=guard|deadline|person|changed|halted|preempted (`help turn stops`)",
        ),
        row(
            "ERR …",
            "nothing more was written: ERR halted, ERR busy turn=<id> or lease=<holder> \
             (another driver's turn or lease), ERR busy input-unread, ERR busy input-queue-full, \
             ERR busy supervisor=<holder|->, ERR key delivery failed, ERR paste delivery failed, \
             ERR yield timeout, ERR cause, ERR usage: turn …",
        ),
        para(
            "`id=` is the turn ledger's id (`history`); `block=` the shell block `blocktext` \
             reads; `agent_rev=` feeds `await agent … since=`. `aterm ctl` prints the rows on \
             stdout and the verdict, `turn submitted=… status=…` without `OK <n>`, on stderr.",
        ),
    ],
    examples: &[
        row(
            "aterm ctl @s-ab12 turn 'make test'",
            "type a shell command, press Enter, reply with the screen once it is quiet 1.5 s",
        ),
        row(
            "aterm ctl @s-ab12 turn typed=1 settle=submitted 'fix the failing test'",
            "type a prompt into Claude Code and reply once the submit shows; read its agent_rev=",
        ),
        row(
            "aterm ctl @s-ab12 turn timeout=600000 settle=match:Finished 'targo --unverified \
             build'",
            "reply once a row shows `Finished`, or after 10 minutes with status=timeout",
        ),
    ],
    sections: &[
        Section {
            name: "words",
            about: "where the options end, the leading options, `--`, and what the text keeps",
            rows: &[
                para(
                    "The text is typed as given: a trailing `\\n` stays the two characters it \
                     is, where `paste` and `send` read a line break (Enter); a control character \
                     besides a tab or line break is dropped. The turn's own Enter submits it.",
                ),
                para(
                    "Two runs of words come before the text: the leading ones (`id=`, \
                     `if-supervisor=`, `cause=`), then the options. Each run ends at its first \
                     word that is not one of its own, or at a `--`, which it takes:",
                ),
                row(
                    "turn idle=500 make test",
                    "the text is `make test`: the first word that is no option starts it",
                ),
                row(
                    "turn idle=500 -- idle=5",
                    "one `--` after an option ends the options: the text is `idle=5`",
                ),
                row(
                    "turn -- -- idle=5",
                    "with no option, two: the first `--` ends only the leading run",
                ),
                para(
                    "`turn` takes no `unread=ok`. A text that is empty or blank is the usage line \
                     unless `submit=none`, and `settle=submitted` with `submit=none` is the usage \
                     line.",
                ),
            ],
        },
        Section {
            name: "submit",
            about: "how a submit is verified for each program, presses, guarded:, settle=",
            rows: &[
                para(
                    "`submit_verify=auto` (the default) verifies a Claude Code submit by the typed \
                     text leaving the composer, a shell's by its command block, a proven \
                     codex-alab's between turns by its own OSC 7501 report beginning a turn \
                     within `submit_window=` and by nothing else (a `working` re-sent mid-turn \
                     begins none; a press its screen showed with no such report by the window's \
                     close, such as a popup taking the Enter, is `submitted=0 … pressed=<n>`, \
                     pressed no more and reopening no task), and any other program's (one with no \
                     shell integration among them, and a codex-alab already in a turn, which takes \
                     the Enter into it or its queue) by the screen, as `seq` does: content that \
                     moves, or the cursor leaving the row the text was typed on.",
                ),
                para(
                    "A press that shows neither is pressed again, up to `presses=<n>` (default 3, \
                     max 10) within `submit_window=<ms>` (default 2000), and a turn none of whose \
                     presses showed is `submitted=0 … pressed=<n>`, the Enters it pressed. Any \
                     turn that pressed Enter and verified no submit names its Enters so, a \
                     composer that still holds the text when the turn's time runs out included, \
                     since its program may yet take a late Enter.",
                ),
                para(
                    "A verified submit says what showed it: \
                     `submit_by=claim|composer|block|screen`. A block-verified submit also says \
                     `block=<id>` when the exact input block was retained and exactly one command \
                     started since Enter was armed; it is absent when that association is \
                     unavailable or ambiguous. This shell block ID feeds `blocktext`; the reply's \
                     `id=` is instead the turn ledger ID (`history`), unrelated to shell block IDs \
                     or a leading exactly-once request `id=epoch:producer:seq`.",
                ),
                para(concat!(
                    "`submit=guarded:<re>` presses Enter only while the cursor row matches `<re>`, \
                     under the same terminal lock (as `key if=`; a first-press miss is `OK 0 turn \
                     skipped reason=guard submitted=0 seq=<n> id=<n>`, plus `pressed=<n>` when \
                     re-presses stop; a first Enter the sink takes none of before \
                     `submit_window=` (or `timeout=`, if sooner) ends is `",
                    turn_submit_busy_reply!(),
                    "`, the text typed and not submitted, and a re-press it takes none of stops \
                     the re-presses as a miss does).",
                )),
                para(
                    "`settle=match:<re>` settles when a visible row matches; `settle=gone:<re>` \
                     waits for `<re>` to appear within `submit_window` and then leave (an idle \
                     settle when it never shows); `settle=submitted` returns once the submit is \
                     verified (`status=submitted`). The settled screen is read on a whole frame \
                     too (` frame=open` when not).",
                ),
                para(
                    "After a Claude Code turn wait with `await agent <words> since=<agent_rev>`. A \
                     codex's `agent=` stays `unknown`: read `status log_seq= log_ep= log_base=` \
                     before the `turn` and wait with `await log kind=program since=<log_seq> \
                     ep=<log_ep> base=<log_base>`.",
                ),
            ],
        },
        Section {
            name: "typed",
            about: "typed=1 key by key: cadence=, cap=, the runs, and how the timeout is kept",
            rows: &[
                para(
                    "`typed=1` presses key by key at `cadence=<ms>` (default 70) up to `cap=<n>` \
                     graphemes (default 240, max 4096) and writes the rest as typing it would go \
                     (`typed=<keys> pasted=<graphemes>`, `pasted=` counting the rest's \
                     graphemes): each run of its text in one write of the bytes its keys send \
                     under the program's keyboard mode, never a bracketed paste, which a program \
                     may take otherwise than the keys (fish escapes the quotes of a paste in a \
                     quoted word), and each Tab or line break pressed as its key between the \
                     runs.",
                ),
                para(
                    "The timeout never leaves part of the text typed (a fenced turn's waits for \
                     its program aside, below): a key goes only while it, the rest's writes and \
                     the submit's window (at most half of `timeout=`, with the echo's 150 ms \
                     settle) still fit before `timeout=` at the pace the keys have gone, with \
                     room for the longest wait a fenced key has needed so far, and once they \
                     would not the rest goes so (each run its keys' bytes in one write), so the \
                     submit takes the whole text; the echo's settle then waits only while that \
                     share of the submit still fits (a Claude Code composer's echo is waited for \
                     as ever, since an Enter before it shows proves nothing; an echo that spends \
                     the submit's share sends the Enter late, `pressed=<n>` when unverified).",
                ),
                para(
                    "A turn whose first key does not fit writes nothing (`OK 0 turn skipped \
                     reason=deadline typed=0 pasted=0 submitted=0 seq=<n> id=<n>`, the composer \
                     as it was).",
                ),
                para(
                    "Fenced on a composer box, a key waits for an open DEC 2026 frame to close, a \
                     digit also for the program to read and draw the key before it (as does a run \
                     of the rest, where it holds a digit), and a guarded submit for the program \
                     to draw the last write, and for input queued before it to go; a wait gives \
                     its time back to `timeout=` once it ends, so the time the turn has left \
                     bounds each wait, not their sum: one that outlasts it, as one the program \
                     never ends does or one slower than the room the pace keeps (the submit's \
                     share, and the longest wait so far), is cut there, the part typed left \
                     standing (`reason=deadline typed=<k>`).",
                ),
            ],
        },
        Section {
            name: "fences",
            about: "if-gen=, if-rows=, if-human=, if-supervisor=, yield= and cause=",
            rows: &[
                para(
                    "`if-gen=<epoch>.<seq>` (`status gen=`) and `if-rows=<first>-<last>:<hex16>` \
                     (FNV-1a-64 of those rows) fence on the screen before typing \
                     (`reason=changed` on a mismatch). `if-rows=` never vouches for half a frame: \
                     a `typed=1` turn whose first look finds the fence spanning a composer box \
                     (rules at `<first>` and `<last>`) in an unfinished DEC 2026 frame waits \
                     within `timeout=` for it to close and judges the fence on it; any other \
                     turn waits as `text` does, and a frame still open has moved.",
                ),
                para(
                    "`if-human=<n>` (`status human_seq=`, the `text --json` read's `human_seq`) \
                     fences on the person: any gesture since, one that moves no fenced row among \
                     them, is `OK 0 turn skipped reason=person submitted=0 seq=<n> id=<n>`, \
                     judged under the fence's lock, and again before a plain turn's paste and \
                     before the first submit press (`reason=person typed=<k> pasted=<p>`, the \
                     text left unsubmitted).",
                ),
                para(
                    "`if-supervisor=<holder>` fences on the live supervisor claim (`meta set \
                     supervisor`), judged at each write (`help turn stops`). `cause=` links the \
                     turn in `log` to held mail (`@<off>`) or a log record (`<seq>`; `ERR cause` \
                     when it names neither).",
                ),
                para(
                    "`yield=<floor 0..=1>` waits for `trail status` `momentum=` to fall to \
                     `<floor>` before typing (`yielded_ms=<n>`, or `ERR yield timeout \
                     momentum=<v>`).",
                ),
            ],
        },
        Section {
            name: "stops",
            about: "what stops a turn once it has typed, holds, leases, refusals, exactly-once",
            rows: &[
                para(
                    "Once its first write has gone, a turn types and submits whatever its caller \
                     does, its Enter after the text (into a session not in front, once a plain \
                     turn's paste has reached that session's input, up to 30 s: one not all there \
                     by then is left queued and unsubmitted, `ERR paste delivery failed`), except \
                     that an `if-supervisor=` fence is judged at each write, so a claim that ends \
                     meanwhile (its holder's connection gone) refuses the rest and ends the turn \
                     in doubt (`ERR key delivery failed`); a caller already gone when it would \
                     first write gets nothing written (`ERR paste delivery failed`, or `ERR key \
                     delivery failed` for a first key or Enter).",
                ),
                para(
                    "A person's key or a screen or input-frame change stops it with `OK 0 turn \
                     skipped reason=<person|changed> typed=<k> pasted=<p> submitted=0 seq=<n> \
                     id=<n>`, the part typed left standing.",
                ),
                para(
                    "A `hold` before the bytes land is `ERR halted …`; one after but before the \
                     first submit press stops it (`OK 0 turn skipped reason=halted[ typed=<k> \
                     pasted=<p>] submitted=0 seq=<n> id=<n>`, the fields where text stands: a \
                     `typed=1` turn's keys and runs, a plain turn's paste as `typed=0 \
                     pasted=<p>`, the graphemes it kept); a later one only ends re-presses.",
                ),
                para(
                    "`lease release force` preempts a turn the same way: it writes nothing more, \
                     what it wrote standing (`OK 0 turn skipped reason=preempted[ typed=<k> \
                     pasted=<p>] submitted=0 seq=<n> id=<n>`), and once an Enter has gone it only \
                     ends re-presses.",
                ),
                para(
                    "Into a `stalled` or stopped program it is `ERR busy input-unread …`; input \
                     over 1 s old appends `warn=input-unread input_bytes=<n> \
                     input_wait_ms=<ms>`. Its first write refused whole, by a full input queue or \
                     the `if-supervisor=` fence, is `ERR busy input-queue-full …` or `ERR busy \
                     supervisor=<holder|->` as for `send`, nothing typed; a later one is `ERR \
                     paste delivery failed` or `ERR key delivery failed`.",
                ),
                para(
                    "A leading `id=<epoch>:<producer>:<seq>` makes it exactly-once (`OK 0 dup=1` \
                     on a repeat; a turn that never reached for the PTY, or whose first write a \
                     full input queue or the `if-supervisor=` fence refused, gives the id back, \
                     whatever it answered, and any other refused write that ends the turn leaves \
                     it in doubt; see `send`).",
                ),
            ],
        },
    ],
};
