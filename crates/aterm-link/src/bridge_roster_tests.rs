// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

// The roster's count (`roster=<seq>` on `outbox` and `sessions bridge`) against a real broker: a
// drain reads the roster only when the count says it moved, and every read it skips is one it
// would have found nothing in.

use super::*;
use std::sync::atomic::Ordering;

const A: &str = "s-a";
const B: &str = "s-b";

/// A bridge attached to a lane that names `seq`, its roster read at that count.
fn at_count(
    seq: Option<u64>,
) -> (
    FakeVerbLane,
    Bridge,
    FakePushLane,
    Conn,
    astream_broker::BrokerHandle,
) {
    let (lane, mut bridge, push, reader, server) = advertising(&[(1, A), (2, B)]);
    *lane.roster_seq.lock().unwrap() = seq;
    bridge.refresh_sessions().unwrap();
    (lane, bridge, push, reader, server)
}

/// How many times `drain` read the roster.
fn roster_reads_of(lane: &FakeVerbLane, drain: impl FnOnce()) -> u64 {
    let before = lane.roster_reads.load(Ordering::SeqCst);
    drain();
    lane.roster_reads.load(Ordering::SeqCst) - before
}

/// One post queued, its frame the outbox's whole listing.
fn queue(lane: &FakeVerbLane, id: u64, to: &str) {
    *lane.outbox.lock().unwrap() =
        Some(format!("post sid={A} id={id} to={to} kind=note len=2\nhi"));
}

/// The post's retirement as the bridge sent it (`outbox sent <sid> <id> …`).
fn retired(lane: &FakeVerbLane, id: u64) -> Option<String> {
    let prefix = format!("outbox sent {A} {id} ");
    lane.sent
        .lock()
        .unwrap()
        .iter()
        .find(|line| line.starts_with(&prefix))
        .cloned()
}

/// A drain at the count its roster was read at reads no roster; a drain whose count moved reads
/// it once, and a post to a session that left with that move is unroutable, not landed on its
/// lane (69979209f1's guarantee, through the count).
#[test]
fn a_drain_reads_the_roster_only_when_its_count_moved() {
    let (lane, mut bridge, _push, mut reader, _server) = at_count(Some(5));
    queue(&lane, 1, &format!("@{B}"));
    assert_eq!(
        roster_reads_of(&lane, || bridge.drain_outbox()),
        0,
        "the count is the one this roster was read at"
    );
    bridge.settle_acks();
    let lane_of_b = format!("/f/f/in/{0}/{B}/{0}/note", bridge.node);
    assert_eq!(reader.last(&lane_of_b, "", 1).unwrap().0.len(), 1);

    lane.sessions.lock().unwrap().retain(|(_, sid)| sid != B);
    *lane.roster_seq.lock().unwrap() = Some(6);
    queue(&lane, 2, &format!("@{B}"));
    assert_eq!(
        roster_reads_of(&lane, || bridge.drain_outbox()),
        1,
        "the count moved: the door looks"
    );
    bridge.settle_acks();
    let verdict = retired(&lane, 2).expect("the post is retired");
    assert!(
        verdict.contains("off=- ") && verdict.contains("reason=unroutable"),
        "a session the roster no longer lists is no route: {verdict}"
    );

    queue(&lane, 3, &format!("@{A}"));
    assert_eq!(
        roster_reads_of(&lane, || bridge.drain_outbox()),
        0,
        "the read above is the count's look"
    );
}

/// A drain whose listing carries no post and no receipt resolves nothing, so it reads no roster
/// even when its count moved, as before the count; the next drain that carries a post looks.
#[test]
fn a_drain_with_nothing_to_deliver_reads_no_roster_at_any_count() {
    let (lane, mut bridge, _push, _reader, _server) = at_count(Some(5));
    *lane.roster_seq.lock().unwrap() = Some(6);
    *lane.outbox.lock().unwrap() = Some(String::new());
    assert_eq!(
        roster_reads_of(&lane, || bridge.drain_outbox()),
        0,
        "an empty listing has nothing to resolve"
    );
    assert_eq!(
        bridge.roster_seen,
        Some(5),
        "and leaves the last read's count standing"
    );
    queue(&lane, 1, &format!("@{B}"));
    assert_eq!(
        roster_reads_of(&lane, || bridge.drain_outbox()),
        1,
        "the first drain that carries a post looks at the moved count"
    );
    assert_eq!(bridge.roster_seen, Some(6));
}

/// An older aterm names no count: every drain reads, as before the count.
#[test]
fn an_aterm_that_names_no_count_is_read_at_every_drain() {
    let (lane, mut bridge, _push, _reader, _server) = at_count(None);
    assert_eq!(bridge.roster_seen, None);
    for id in 1..=2 {
        queue(&lane, id, &format!("@{B}"));
        assert_eq!(roster_reads_of(&lane, || bridge.drain_outbox()), 1);
        bridge.settle_acks();
    }
}

/// An outbox from an older aterm is read at every drain even by a bridge that holds a count from
/// a `sessions bridge` reply: no token on the peek is no look.
#[test]
fn a_peek_that_names_no_count_is_no_look() {
    let (lane, mut bridge, _push, _reader, _server) = at_count(Some(5));
    assert_eq!(bridge.roster_seen, Some(5));
    *lane.roster_seq.lock().unwrap() = None;
    queue(&lane, 1, &format!("@{B}"));
    assert_eq!(roster_reads_of(&lane, || bridge.drain_outbox()), 1);
    assert_eq!(bridge.roster_seen, None, "and that read named none");
}

/// A burst to a sid this node does not host reads the roster at most once a drain: its count's
/// look, not one per post.
#[test]
fn a_burst_to_a_sid_this_node_does_not_host_reads_the_roster_at_most_once() {
    let burst = |ids: std::ops::RangeInclusive<u64>| -> String {
        ids.map(|id| format!("post sid={A} id={id} to=@s-x kind=note len=2\nhi"))
            .collect()
    };
    let (lane, mut bridge, _push, _reader, _server) = at_count(Some(5));
    *lane.outbox.lock().unwrap() = Some(burst(1..=4));
    assert_eq!(
        roster_reads_of(&lane, || bridge.drain_outbox()),
        0,
        "the count is unmoved"
    );
    bridge.settle_acks();
    *lane.roster_seq.lock().unwrap() = Some(6);
    *lane.outbox.lock().unwrap() = Some(burst(5..=8));
    assert_eq!(
        roster_reads_of(&lane, || bridge.drain_outbox()),
        1,
        "the count moved: one look for the drain"
    );
    bridge.settle_acks();

    assert!(
        !bridge.roster_looked,
        "the drain's look ends with the drain"
    );
    assert_eq!(
        roster_reads_of(&lane, || {
            let _ = bridge.resolve_to("@s-x", Some(A));
        }),
        1,
        "outside a drain an unknown sid still asks"
    );
}

/// A drain's look stands for that drain alone: without the skip `resolve_to` would read once per
/// post (the negative control of the test above's `0`).
#[test]
fn a_resolve_outside_a_drain_reads_for_every_unknown_sid() {
    let (lane, mut bridge, _push, _reader, _server) = at_count(Some(5));
    assert_eq!(
        roster_reads_of(&lane, || {
            for _ in 0..3 {
                let _ = bridge.resolve_to("@s-x", Some(A));
            }
        }),
        3
    );
}

/// Past a bye no count stands for a roster: the count carries into a successor whose local ids
/// can differ (`continue_roster_seq`), so every drain after it looks, at any count.
#[test]
fn past_a_bye_every_drain_reads_the_roster() {
    for bye in [Bye::Handoff, Bye::Quit] {
        let (lane, mut bridge, _push, _reader, _server) = at_count(Some(5));
        assert_eq!(bridge.roster_seen, Some(5));
        bridge.take_bye(bye);
        assert_eq!(bridge.roster_seen, None, "{bye:?}");
        for id in 1..=2 {
            queue(&lane, id, &format!("@{B}"));
            assert_eq!(
                roster_reads_of(&lane, || bridge.drain_outbox()),
                1,
                "{bye:?} post {id}"
            );
            assert_eq!(bridge.roster_seen, None, "{bye:?}: and it stays forgotten");
            bridge.settle_acks();
        }
    }
}

/// An exit line forgets the count, so a read it could not finish leaves the next drain looking,
/// though the count reads the same: the line took the sid out of what the bridge holds.
#[test]
fn an_exit_line_whose_read_failed_leaves_the_next_drain_looking() {
    let (lane, mut bridge, mut push, _reader, _server) = at_count(Some(5));
    lane.refuse_roster(true);
    push.deliver(&mut bridge, &[&format!("EVENT 2 session-exited {B}")]);
    assert_eq!(bridge.roster_seen, None);
    lane.refuse_roster(false);
    queue(&lane, 1, &format!("@{A}"));
    assert_eq!(roster_reads_of(&lane, || bridge.drain_outbox()), 1);
    assert_eq!(
        bridge.roster_seen,
        Some(5),
        "the read names the count again"
    );
}

#[test]
fn the_roster_token_is_read_only_as_a_plain_decimal_token() {
    for (header, want) in [
        ("OK 12 roster=7", Some(7)),
        ("OK 12 hold=0 roster=7", Some(7)),
        ("OK 12", None),
        ("OK 12 roster=", None),
        ("OK 12 roster=+7", None),
        ("OK 12 roster=x", None),
        ("OK 12 xroster=7", None),
    ] {
        assert_eq!(roster_token(header), want, "{header}");
    }
}
