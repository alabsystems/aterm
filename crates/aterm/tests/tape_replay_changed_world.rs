// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A HARNESS TAPE REPLAYS IN A CHANGED WORLD (the replay plan's T6): tapes
//! recorded here by real `aterm drive watch|supervise --tape` runs over a
//! headless instance, then each replayed by a child `aterm drive replay` in a
//! world that shares nothing with the recording's — `HOME=/nonexistent`,
//! another `$TMPDIR` and other XDG roots, the zone `Pacific/Kiritimati`
//! (UTC+14, as far from the recording's as a zone gets), another working
//! directory, and no control socket anywhere it could look — and each is
//! REPRODUCED.
//!
//! What it proves: a replay reads the tape and nothing else. Every clock, zone
//! offset, file and reply the recorded loop observed is the tape's; the files
//! the run named (its journal, its notes, its approval ledger) are moved
//! under a directory that does not exist, and the recording's own copies are
//! byte for byte what they were before the replay; and the replay makes
//! nothing — no file, no directory — in its temp dir.
//!
//! * THE WATCH (`support/box_worker.rs`'s worker, in a session of its own):
//!   its box pressed, its decision told to the window (`story`, the teller's
//!   own lane), journaled and noted; the test waits for the worker's finished
//!   turn (the press drew it), then closes the session, and the watch ends on
//!   its own — the session gone — as a watch does. Its `--max-s` is a
//!   backstop that ends a broken run before the client's hang detector,
//!   never the window the press must land in: that wait is the one-minute
//!   `await match` for the finished turn.
//! * THE SUPERVISE (a second worker, in the boot session): its box pressed,
//!   then the finished turn handed back (`--no-continue`), journaled and
//!   noted — a supervise ends at its review point by itself.

use std::path::Path;

use crate::box_worker::{self, drive, said};
use crate::headless_boot::{self, boot_session, ctl, ctl_ok};

/// Every file under `dir` with its bytes, and every directory (named with a
/// trailing `/`, no bytes), sorted by path. Links are not followed.
fn contents(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                out.push((format!("{}/", path.display()), Vec::new()));
                stack.push(path);
            } else {
                let bytes = std::fs::read(&path).unwrap_or_default();
                out.push((path.display().to_string(), bytes));
            }
        }
    }
    out.sort();
    out
}

/// The first directory named `name` under `root`, depth first.
fn dir_named(root: &Path, name: &str) -> Option<std::path::PathBuf> {
    let mut stack = vec![root.to_path_buf()];
    while let Some(d) = stack.pop() {
        for entry in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = entry.path();
            if entry.file_type().is_ok_and(|t| t.is_dir()) {
                if entry.file_name() == name {
                    return Some(path);
                }
                stack.push(path);
            }
        }
    }
    None
}

/// Replay `tape` from a world that shares nothing with the recording's
/// ([module docs](self)).
fn replay_elsewhere(tape: &Path) -> std::process::Output {
    let elsewhere = aterm_tempfile::Builder::new()
        .prefix("aterm-elsewhere-")
        .tempdir()
        .expect("another world");
    let root = elsewhere.path();
    std::fs::create_dir_all(root.join("tmp")).expect("its temp dir");
    let out = box_worker::drive_in(
        root,
        &["replay", tape.to_str().expect("a UTF-8 tape path")],
        &[("HOME", "/nonexistent"), ("TZ", "Pacific/Kiritimati")],
        Some(root),
    );
    let made = contents(&root.join("tmp"));
    assert!(
        made.is_empty(),
        "the replay made nothing in its temp dir: {:?}",
        made.iter().map(|(p, _)| p).collect::<Vec<_>>()
    );
    out
}

/// The control-check of [`contents`]: a directory with nothing in it is
/// seen, so "the replay made nothing" sees one too.
#[test]
fn contents_sees_an_empty_directory() {
    let dir = aterm_tempfile::Builder::new()
        .prefix("aterm-contents-")
        .tempdir()
        .expect("a dir");
    assert!(contents(dir.path()).is_empty());
    std::fs::create_dir_all(dir.path().join("a").join("b")).expect("made");
    let seen: Vec<String> = contents(dir.path()).into_iter().map(|(p, _)| p).collect();
    assert_eq!(seen.len(), 2, "{seen:?}");
    assert!(seen.iter().all(|p| p.ends_with('/')), "{seen:?}");
}

/// Tapes recorded here, of a watch and of a supervise, each replayed in a
/// changed world: REPRODUCED, and the recording's journal, notes and ledger
/// untouched by it.
#[test]
fn a_tape_replays_in_a_changed_world() {
    let inst = headless_boot::boot("atchw", &["--lines", "40", "--columns", "120"]);
    let (_, boot_sid) = boot_session(&inst);
    let logs = inst.tmp.join("logs");
    std::fs::create_dir_all(&logs).expect("the logs' dir");
    let path = |name: &str| logs.join(name).to_str().expect("UTF-8").to_string();

    // THE WATCH, over a worker in a session of its own.
    let spawned = ctl_ok(&inst, &["spawn"]);
    let watched_sid = spawned
        .trim()
        .strip_prefix("OK ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or_else(|| panic!("spawn: {spawned}"))
        .to_string();
    let first = box_worker::start(&inst, &watched_sid);
    let watched_target = format!("@{watched_sid}");
    let watch = inst.tmp.join("watch.atape");
    let watched = std::thread::scope(|scope| {
        let running = scope.spawn(|| {
            drive(
                &inst,
                &[
                    "watch",
                    &watched_target,
                    "--tape",
                    watch.to_str().expect("UTF-8"),
                    "--journal",
                    &path("watch.jsonl"),
                    "--notes",
                    &path("watch-notes.txt"),
                    "--max-s",
                    "100",
                ],
            )
        });
        // The worker draws its finished turn when the press reaches it.
        let done = ctl(
            &inst,
            &[
                &watched_target,
                "await",
                "match",
                "Created.x",
                "timeout=60000",
            ],
        );
        let reply = String::from_utf8_lossy(&done.stdout);
        assert!(
            done.status.success() && reply.starts_with("OK") && !reply.starts_with("OK timeout"),
            "the watch never pressed the box: {reply:?} {:?}",
            String::from_utf8_lossy(&done.stderr)
        );
        let closed = ctl_ok(&inst, &[&watched_target, "close"]);
        assert!(closed.starts_with("OK closed"), "close: {closed}");
        running.join().expect("the watch's thread")
    });
    assert_eq!(
        first.presses(),
        ["1"],
        "the watch answered the box: {}",
        said(&watched)
    );
    let stdout = String::from_utf8_lossy(&watched.stdout);
    assert!(
        stdout.lines().any(|l| l.starts_with("EXIT session gone")),
        "the watch ended on the closed session, not its backstop: {}",
        said(&watched)
    );

    // THE SUPERVISE, over a second worker in the boot session.
    let second = box_worker::start(&inst, &boot_sid);
    let supervise = inst.tmp.join("supervise.atape");
    let supervised = drive(
        &inst,
        &[
            "supervise",
            &format!("@{boot_sid}"),
            "--tape",
            supervise.to_str().expect("UTF-8"),
            "--journal",
            &path("supervise.jsonl"),
            "--notes",
            &path("supervise-notes.txt"),
            "--no-continue",
            "--max-s",
            "60",
        ],
    );
    assert_eq!(
        second.presses(),
        ["1"],
        "the supervise answered its box: {}",
        said(&supervised)
    );

    // The approval ledger's directory (`<state>/drive`, wherever this
    // platform puts the state root in the scratch world): the instance's own
    // state beside it goes on changing while it runs, and is not compared.
    let ledgers = dir_named(&inst.tmp, "drive")
        .unwrap_or_else(|| panic!("the approval ledger's dir under {}", inst.tmp.display()));
    let before = (contents(&logs), contents(&ledgers));
    assert!(!before.1.is_empty(), "the runs ledgered their decisions");
    for tape in [&watch, &supervise] {
        let out = replay_elsewhere(tape);
        assert_eq!(
            out.status.code(),
            Some(0),
            "{}: {}",
            tape.display(),
            said(&out)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        assert!(text.starts_with("REPRODUCED"), "{}: {text}", tape.display());
    }
    assert!(
        before == (contents(&logs), contents(&ledgers)),
        "the replays touched none of the recording's files"
    );
}
