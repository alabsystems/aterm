// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Live-headless check of the HARNESS TAPE (the replay plan's T6): a real
//! `aterm drive supervise --tape` over a real headless instance and its real
//! control socket, then the tape replayed OFFLINE — `aterm drive replay`, no
//! socket — and REPRODUCED.
//!
//! * THE WORKER (`support/box_worker.rs`) draws Claude Code 2.1.280's
//!   measured Bash box for `touch x`, logs every byte it reads and every digit
//!   that answers its box, and then sits at a finished turn.
//! * THE RECORDING: `supervise` at the owner's default (full power) presses
//!   the box's `1` and stops at the finished turn (`--no-continue`: nothing is
//!   typed at it). The tape is written `0600`, and its ACTS are the worker's
//!   key log: every key the tape says the loop pressed reached the worker, and
//!   nothing else did.
//! * THE REPLAY: `aterm drive replay FILE`, in the same scratch world but
//!   with no socket named, says REPRODUCED and exits 0; the same tape replayed
//!   in this process gives the structured report — every effect consumed, the
//!   requests the loop asked, the line it printed. How long the replay took is
//!   printed, never judged.
//!
//! ISOLATION: scratch HOME/XDG roots, a private control socket, a config with
//! every automatic lane off, `SHELL=/bin/sh` (`support/launch_isolation.rs`).
//! A headless instance opens no window.

use std::time::Instant;

use aterm_agent::supervise::tape::replay::{Options, replay};
use aterm_agent::supervise::tape::report::{Report, Verdict};

use crate::box_worker::{self, drive, said};
use crate::headless_boot::{self, boot_session};

/// The keys a REPRODUCED replay's loop pressed — each request on its lane
/// the tape holds, in order, checked against it one by one — the last word of
/// each `key` act.
pub(crate) fn pressed(report: &Report) -> Vec<String> {
    assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
    report
        .requests
        .iter()
        .filter(|words| {
            words
                .iter()
                .find(|w| !w.starts_with('@'))
                .is_some_and(|v| v == "key")
        })
        .filter_map(|words| words.last().cloned())
        .collect()
}

/// A SUPERVISED BOX, TAPED AND REPLAYED OFFLINE: the box's `1` pressed and
/// the run taped; the tape's keys are the worker's key log; the tape replays
/// from the file alone, REPRODUCED, and how long that took is printed.
#[test]
fn a_supervised_box_is_taped_and_replays_offline() {
    let inst = headless_boot::boot("attape", &["--lines", "40", "--columns", "120"]);
    let (_, sid) = boot_session(&inst);
    let worker = box_worker::start(&inst, &sid);
    let tape = inst.tmp.join("run.atape");
    let tape_s = tape.to_str().expect("a UTF-8 scratch path");
    let ran = drive(
        &inst,
        &[
            "supervise",
            &format!("@{sid}"),
            "--tape",
            tape_s,
            "--no-continue",
            "--max-s",
            "60",
        ],
    );
    assert_eq!(worker.presses(), ["1"], "the box answered: {}", said(&ran));
    assert_eq!(
        std::fs::read_to_string(&worker.keys).expect("the key log"),
        "1",
        "the worker read the press and nothing else: {}",
        said(&ran)
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&tape)
            .expect("the tape")
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600, "the tape is private: {mode:o}");
    }
    let bytes = std::fs::read(&tape).expect("the tape");

    // Offline: the same scratch world, no socket named.
    let started = Instant::now();
    let replayed = box_worker::drive_in(&inst.tmp, &["replay", tape_s], &[], None);
    let child_took = started.elapsed();
    assert_eq!(replayed.status.code(), Some(0), "{}", said(&replayed));
    let text = String::from_utf8_lossy(&replayed.stdout);
    assert!(text.starts_with("REPRODUCED"), "{text}");

    let started = Instant::now();
    let report = replay(&bytes, &Options::default()).expect("the tape replays");
    let took = started.elapsed();
    assert_eq!(report.verdict, Verdict::Reproduced, "{}", report.render());
    assert_eq!(report.consumed, report.effects);
    assert_eq!(report.entry, "supervise");
    assert_eq!(
        pressed(&report),
        worker.presses(),
        "the tape's acts are what reached the worker"
    );
    eprintln!(
        "replay speed: {} effects in {took:?} in-process ({:.0} effects/s); the `aterm drive \
         replay` child, process start included, took {child_took:?}",
        report.effects,
        report.effects as f64 / took.as_secs_f64().max(1e-9),
    );
}
