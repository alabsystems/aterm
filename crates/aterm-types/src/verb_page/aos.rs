// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `help aos`: the front window's typing-momentum glow hosted as an Aos task. `aterm-gui`'s
//! `aos_verb` answers it.

use super::{Page, Section, para, row};

/// The `aos` entry.
pub(crate) const PAGE: Page = Page {
    usage: &[
        para(
            "Aos is aterm's hosted execution: one task inside aterm, here the front window's \
             typing-momentum glow (the cursor's light while you type), is checkpointed, its \
             inputs recorded, replayed in shadow, repaired with candidate code and resumed.",
        ),
        row(
            "aos [status]",
            "the hosted glow's state: OK window=<id> attached=<bool> code=<name> …",
        ),
        row(
            "aos ls",
            "every window's task, with its timeline rows (segments, detaches, unseen)",
        ),
        row(
            "aos attach [code=<name>]",
            "checkpoint the glow and start recording its inputs",
        ),
        row("aos pause", "hold at the step boundary"),
        row(
            "aos repair [code=<name>]",
            "replay live and candidate code over the trace, and swap the candidate in if it holds",
        ),
        row("aos resume", "drop a pending update"),
        row("aos detach", "stop hosting"),
        row(
            "aos trace [hex]",
            "the log, trace and checkpoint digests (hex: the trace file's bytes)",
        ),
        row(
            "aos export [kept=<n>]",
            "write the live trace, or kept fault <n>, as a 0600 file",
        ),
        row(
            "aos check <file>",
            "replay a trace file under this build, with no live effect",
        ),
    ],
    options: &[
        row(
            "code=<name>",
            "the code to attach or repair to: shipped, safe (`help aos repair`)",
        ),
        row("kept=<n>", "a kept fault's index, as `aos ls` lists it"),
    ],
    reply: &[
        row(
            "OK window=<id> …",
            "status, attach, pause, repair, resume, detach, trace, export: one line",
        ),
        row(
            "OK <n>",
            "aos ls: <n> rows, `tasks=<n>` first (`aterm ctl` prints the rows alone)",
        ),
        row("OK checked …", "aos check: the replay's counts and verdict"),
        row(
            "ERR …",
            "a refused attach (Secure Keyboard Entry, Sparkle's paper gold), a refused check \
             (`ERR aos check refused=identity: …`), a debug-only form in a release build",
        ),
    ],
    examples: &[
        row("aterm ctl aos", "is the glow hosted, and with which code?"),
        row("aterm ctl aos attach", "start recording the glow's inputs"),
        row(
            "aterm ctl aos export",
            "write the live trace to the state folder's aos/ and print its path",
        ),
    ],
    sections: &[
        Section {
            name: "status",
            about: "aos status and aos ls: the fields and the timeline rows",
            rows: &[
                para(
                    "`aos` hosts the front window's typing-momentum glow as an Aos task, in every \
                     build. `aos [status]` answers `OK window=<id> attached=<bool> code=<name> …` \
                     (hot/active/settling flags, then while attached `unseen=on|off`, revision, \
                     pause queue, trace cursor, `cold_at=`, last wake and boundary counters).",
                ),
                para(
                    "`aos ls` answers `OK <n>` and n rows: `tasks=<n>`, then per window a \
                     `slot=<id> kind=effect task=<name> code= attached= paused= rev= log_epoch= \
                     log= waiting= kept= rows= dropped=` row and its timeline rows, `slot=<id> \
                     row=<k> segment cause=full|broken|restored|committed code= log_epoch= \
                     range=<a>..<b> judged=pass|fail:<law>@<cursor>|unjudged:<why> kept=<id>|no` \
                     or `slot=<id> row=<k> detached why=<why>` or `slot=<id> row=<k> \
                     unseen=on|off why=person|program_left`: an online watcher judges each \
                     retired log epoch on a host worker and keeps a failing one as a fault (at \
                     most 4, timeline rows only, no alert).",
                ),
            ],
        },
        Section {
            name: "attach",
            about: "what a recording logs, holds or detaches on, and keys not echoed",
            rows: &[
                para(
                    "`aos attach [code=<name>]` checkpoints the glow and records keys, resets, \
                     frame ticks and scheduler queries (refused while Secure Keyboard Entry is on \
                     or the glow wears Sparkle's paper gold on a light theme; a typed key the tty \
                     does not echo in canonical mode, one under Secure Keyboard Entry, or the gold \
                     going on detaches the recording before it is logged; a key a program reads \
                     raw with no prompt of aterm's own integrated shell open for it is held, \
                     logging nothing, until a frame shows its own echo visibly where it was typed, \
                     and is logged at that frame, while one shown masked, hidden or not at all \
                     within 1 s detaches the recording, as at an ssh, sudo or Node password \
                     prompt; an attach after a detached key starts the glow from rest).",
                ),
                para(
                    "On macOS the person alone, at View ▸ Record Keys Not Echoed… and aterm's \
                     alert, can let one recording also keep keys aterm cannot see echoed until it \
                     ends or the program leaves the front of its tab (no verb, invoke or setting \
                     grants it): a key no frame proves is then logged unseen, each burst's keys at \
                     the frame that settles them, while a mask glyph where it was typed, a no-echo \
                     key and Secure Keyboard Entry still detach.",
                ),
            ],
        },
        Section {
            name: "repair",
            about: "pause, repair, resume and detach: the code swap and what it checks",
            rows: &[para(
                "`aos pause` holds at the step boundary; `aos repair [code=<name>]` shadow-replays \
                 live and candidate code over the trace from the attach checkpoint on a host \
                 worker and, back on the main thread, swaps the candidate in only when live \
                 reproduces the paused head and the candidate keeps \
                 `momentum-glow.no-stranded-light`, then delivers the queue (`code=shipped` \
                 verifies and resumes, `code=safe` names a frame-interval wake whenever the glow \
                 is hot; the answer ends `main_us=<n> off_main=<bool>`; on a refusal a glow the \
                 repair paused resumes on the old code, and one `aos pause` held stays paused); \
                 `aos resume` drops a pending update; `aos detach` stops hosting.",
            )],
        },
        Section {
            name: "trace",
            about: "trace, export and check: the files and the replay's verdict",
            rows: &[
                para(
                    "`aos trace` reports the log length, the trace and checkpoint digests, the \
                     broken flag and the event counts with `unseen=<n>`, the keys logged unseen, \
                     and `aos trace hex` adds the exported trace file's bytes.",
                ),
                para(
                    "`aos export [kept=<n>]` writes the live trace, or kept fault `<n>`, as a new \
                     0600 file in the state directory's `aos/` (`OK window=<id> exported kept= \
                     log_epoch= identity= bytes= unseen= mode=0600 file=<name> path=<path>`, the \
                     path last).",
                ),
                para(
                    "`aos check <file>` (a bare export name, or an absolute path) replays a trace \
                     file under this build with no live effect, refusing one recorded under an \
                     identity this build does not run (`ERR aos check refused=identity: …`), and \
                     answers `OK checked code= log_epoch= first= keys= resets= ticks= queries= \
                     unseen= checkpoint= state= chain= verdict= bytes= file=`; a pane trace \
                     (`rewind check export`) answers `OK checked task=aterm.pane.screen \
                     code=this-build slices= resizes= witnesses= matched= replies= start= state= \
                     chain= verdict=pass|fail:<law>@<cursor> bytes= file=`.",
                ),
            ],
        },
        Section {
            name: "debug",
            about: "the forms only a debug build serves",
            rows: &[para(
                "Debug builds only, each answering `ERR aos <form> needs a debug build` in a \
                 release build: `code=demo-stranded-settle`; `aos publish plan=<24 hex>`, which \
                 judges a 12-byte schedule plan under the pinned acceptance policy and publishes \
                 it at the step boundary (pausing first) only if it is admitted and, over the \
                 shipped code, useful (`useful=`, `gain_ppm=`, `bundle=`; a plan over `code=safe` \
                 is refused, it is no policy base); and `aos bundle`, which exports the live \
                 plan's candidate bundle (`bundle=<digest> rev= bytes= hex=`).",
            )],
        },
    ],
};
