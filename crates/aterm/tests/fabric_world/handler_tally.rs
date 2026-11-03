// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! aterm's handler tally ([`aterm_types::wait_tally::HANDLER_SEAM`]) read from a real aterm: the
//! fabric ledger's handler rows come from a bench `verify` does not run, so this holds each verb
//! to the row the ledger names it by.

use std::path::Path;

use aterm_types::wait_tally::{HANDLER_FILE, HANDLER_SEAM, HandlerRow, HandlerTally};

use crate::harness::{DEADLINE, World, until};

/// The tally now: a wait with no time has aterm write the line before it answers, and counts in no
/// row.
fn tally(w: &World, path: &Path, sid: &str) -> HandlerTally {
    let line = format!("@{sid} await inbox since=0 kinds=note timeout=0");
    assert!(w.verb(&line).ok(), "{line}");
    let text = std::fs::read_to_string(path)
        .unwrap_or_else(|e| panic!("aterm wrote no handler tally at {}: {e}", path.display()));
    HandlerTally::parse(&text).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn each_verb_of_a_round_counts_in_the_row_the_ledger_names_it_by() {
    let w = World::boot_with("handlers", &[], &[(HANDLER_SEAM, "1")]);
    w.wait_ready();
    let (a, b) = w.two_sessions();
    let path = w.instance_state().join(HANDLER_FILE);
    let before = tally(&w, &path, &b);
    assert!(
        before.row(HandlerRow::OtherBridge).count > 0,
        "the bridge's hello and links, by the time it serves"
    );

    let mut parked = w.ctl();
    let wait = format!(
        "@{b} await inbox since=0 kinds=note timeout={}",
        DEADLINE.as_millis()
    );
    let waiter = std::thread::spawn(move || parked.request(&wait));
    until("the wait's first park", || {
        (tally(&w, &path, &b).row(HandlerRow::AwaitArm).count
            == before.row(HandlerRow::AwaitArm).count + 1)
            .then_some(())
    });
    let spent = tally(&w, &path, &b);
    assert_eq!(
        spent
            .since(&before)
            .expect("one aterm")
            .row(HandlerRow::AwaitWake)
            .count,
        0,
        "nothing has arrived"
    );

    let post = format!("@{a} post to=@{b} kind=note -- the round's one message");
    assert!(w.verb(&post).ok(), "{post}");
    let answer = waiter.join().expect("the waiter").expect("its reply");
    assert!(
        answer.header().starts_with("OK inbox "),
        "{}",
        answer.header()
    );
    let listing = format!("@{b} inbox since=0");
    assert!(w.verb(&listing).ok(), "{listing}");

    let d = until("the bridge's verbs of the round", || {
        let d = tally(&w, &path, &b).since(&before).expect("one aterm");
        [
            HandlerRow::Deliver,
            HandlerRow::WakeWatchers,
            HandlerRow::Outbox,
            HandlerRow::Sent,
            HandlerRow::Sessions,
        ]
        .into_iter()
        .all(|row| d.row(row).count > 0)
        .then_some(d)
    });
    for row in [
        HandlerRow::Post,
        HandlerRow::Inbox,
        HandlerRow::AwaitArm,
        HandlerRow::AwaitWake,
    ] {
        assert_eq!(d.row(row).count, 1, "{row:?}: {d:?}");
    }
    assert_eq!(
        d.row(HandlerRow::FleetMail).count,
        0,
        "a world that sent no fleet mail reads no mailbox: {d:?}"
    );
}
