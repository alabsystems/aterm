// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **WHAT A SEAMLESS UPDATE'S BRIDGE HANDOFF COSTS THE STATE DIR, AND WHAT A
//! QUIT'S CLEAN STOP DOES** (sim item S6): the flushes the old bridge makes as
//! it goes, the ones its successor's start makes, and the ones the
//! successor's own clean stop makes, counted and timed on the real stack, so
//! the numbers in the commit messages and the CHANGELOG are this test's
//! output.
//!
//! One headless `aterm` hosting eight sessions, each used the way an agent
//! uses the fabric: it opts into a topic and is handed a broadcast it has not
//! yet handled (the bridge keeps its refill floor, `unhandled/<sid>`), and it
//! posts once under a `key=` (the bridge keeps the key's reservation,
//! `keys/<sid>`) — sixteen files of the kinds a power cut may tear, written
//! with no flush, as the bridge writes them. Then the handoff: the instance
//! says `bye handoff` and closes the bridge's lanes, which is all its bridge
//! sees of a seamless update's Commit (the QA seam `ATERM_DEBUG_QUIT_BYE`, a
//! development build's only, as `roster_bye.rs` uses it), and its successor
//! comes up on the same state dir, in the same boot. Then the successor
//! quits, a real quit (`bye quit`), and its bridge stops the run cleanly.
//!
//! Each bridge says on stderr, which aterm keeps in its log, what it flushed:
//! a handoff (`aterm-link: handed off: …`), a clean stop (`aterm-link: the
//! run stopped cleanly: …`) and a start that writes the run record
//! (`aterm-link: the run record says a bridge runs in this boot: …`). The
//! times are printed, never judged (a flush is the disk's cost, and this
//! machine's load is not the test's); the counts are asserted.
//!
//! Every wait is an `until` over an observable state, a hang detector.

use crate::harness;

use harness::{World, alive, until};

/// The sessions the instance hosts: its boot session and seven spawned.
const SESSIONS: usize = 8;

/// The topic every session opts into.
const TOPIC: &str = "s6.cost";

/// The lines of aterm's log past byte `from` that start with `prefix`.
fn said(w: &World, from: usize, prefix: &str) -> Vec<String> {
    let log = std::fs::read_to_string(&w.gui_log).unwrap_or_default();
    log.get(from..)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.starts_with(prefix))
        .map(str::to_string)
        .collect()
}

/// How long aterm's log is now: what a later [`said`] reads past.
fn log_len(w: &World) -> usize {
    std::fs::read_to_string(&w.gui_log).map_or(0, |log| log.len())
}

/// The number just before `before` on `line`.
fn number(line: &str, before: &str) -> u64 {
    line.split(before)
        .next()
        .and_then(|head| head.split_whitespace().last())
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("a number before `{before}` in: {line}"))
}

/// Quit the instance the way a person does (`invoke Quit`) and wait for it,
/// then for its bridge, to exit: what the bridge said is in the log by then.
fn quit(w: &mut World) {
    let bridge = w.bridge_pid();
    let _ = w.ctl().request("invoke Quit");
    until("aterm to exit", || {
        w.gui.as_mut()?.try_wait().ok().flatten()
    });
    w.gui = None;
    until("its bridge to exit after it", || {
        (!alive(bridge)).then_some(())
    });
    let _ = std::fs::remove_file(w.state.join("pid"));
}

/// **A HANDOFF FLUSHES NOTHING, AND ITS SUCCESSOR'S CLEAN STOP FLUSHES WHAT
/// THE RUN WROTE.** Eight sessions in use leave sixteen torn-kind files
/// written with no flush; the instance hands off: its bridge flushes nothing
/// and leaves the run open, and its successor, on the same dir in the same
/// boot, establishes its own durable OPEN fence by two full flushes. The
/// successor then quits,
/// and its clean stop flushes every one of the sixteen it inherited where it is (nothing
/// rewritten), each by the barrier, and ends with one full flush.
#[test]
fn a_handoff_flushes_nothing_and_the_quit_after_it_flushes_the_run() {
    let mut w = World::boot_with("s6-handoff", &[], &[("ATERM_DEBUG_QUIT_BYE", "handoff")]);
    w.wait_ready();
    let (first, _) = w.two_sessions();
    for _ in 2..SESSIONS {
        let reply = w.verb("spawn");
        assert!(reply.ok(), "spawn: {}", reply.header());
    }
    let sids: Vec<String> = until("the bridge to see every session", || {
        let rows = w.sessions();
        (rows.len() >= SESSIONS).then(|| rows.into_iter().map(|(_, sid, _)| sid).collect())
    });
    for sid in &sids {
        let added = w.verb(&format!("@{sid} topic add {TOPIC}"));
        assert!(added.ok(), "topic add: {}", added.header());
        until("the bridge to hold the topic", || {
            harness::bridge_holds(&w, sid, TOPIC)
        });
    }
    // ONE BROADCAST, handed to every session and handled by none: each one's
    // refill floor is on disk.
    let shout = w.verb(&format!("@{first} post to=say:{TOPIC} kind=note s6shout"));
    assert!(shout.ok(), "{}", shout.header());
    for sid in &sids {
        until("the broadcast's refill floor", || {
            std::fs::read_to_string(w.state.join(format!("unhandled/{sid}")))
                .ok()
                .filter(|floor| floor.starts_with(TOPIC))
                .map(|_| ())
        });
    }
    // ONE KEYED POST EACH, landed: each one's key reservation is on disk.
    for (i, sid) in sids.iter().enumerate() {
        let posted = w.verb(&format!(
            "@{sid} post to=@{first} kind=note key=s6-k{i} --wait=60000 keyed"
        ));
        assert!(
            posted.ok() && posted.header().contains("off="),
            "keyed post: {}",
            posted.header()
        );
        assert!(w.state.join(format!("keys/{sid}")).exists());
    }

    // THE HANDOFF.
    let before = log_len(&w);
    quit(&mut w);
    let stopped = said(&w, before, "aterm-link: the run stopped cleanly:");
    let handed = said(&w, before, "aterm-link: handed off:");
    eprintln!("MEASURED handoff, the old bridge: {stopped:?} {handed:?}");
    assert!(stopped.is_empty(), "a handoff is not the run's end");
    assert_eq!(handed.len(), 1, "it says so once");

    // THE SUCCESSOR, on the same state dir in the same boot.
    let successor = log_len(&w);
    let gui = w.gui.insert(World::relaunch_gui(&w));
    w.token = World::wait_for_owned_token_of(&w.ctl_sock, gui, &w.gui_log);
    w.wait_ready();
    let began = said(
        &w,
        successor,
        "aterm-link: the run record says a bridge runs",
    );
    eprintln!("MEASURED handoff, the successor's start: {began:?}");
    assert_eq!(
        began.len(),
        1,
        "the successor establishes its own OPEN fence"
    );
    assert_eq!(
        (
            number(&began[0], "barriers"),
            number(&began[0], "full flushes")
        ),
        (0, 2),
        "over the open run it inherits, in full: {}",
        began[0]
    );

    // ITS QUIT: a real one, and a clean stop of the run it inherited.
    let before = log_len(&w);
    quit(&mut w);
    let stopped = said(&w, before, "aterm-link: the run stopped cleanly:");
    eprintln!("MEASURED quit after the handoff, its clean stop: {stopped:?}");
    // The mirror's floor, durable at the successor's first raise in its
    // open (if its mirror sent a batch), is the other flush its run paid
    // for its start (raise_logfloor's tier is not this item's): printed,
    // never judged.
    let floor = said(
        &w,
        successor,
        "aterm-link: mirror: the floor of the node's total",
    );
    eprintln!("MEASURED handoff, the successor's first floor raise: {floor:?}");
    let [line] = stopped.as_slice() else {
        panic!("one stop line, not {stopped:?}");
    };
    let files = number(line, "state files");
    assert!(
        files >= 2 * SESSIONS as u64,
        "every session's floor and key, inherited: {line}"
    );
    assert_eq!(
        (number(line, "barriers"), number(line, "full flushes")),
        (files + number(line, "directories flushed") + 1, 1),
        "a barrier a file, a directory and the record's bytes, then one full flush: {line}"
    );
}
