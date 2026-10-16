// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **The bridge's replay budget: late subscribers share one allowance, and
//! fresh mail and a halt never wait behind their backlog.**
//!
//! A module of its own (helpers in `broadcast.rs`) because of when it runs:
//! its sixteen two-page gaps are walked at the bridge's own pace —
//! `SAY_REPLAY_PAGES` (four) per two-second `ROSTER_REFRESH` round, and each
//! page delivers its 256 fillers — so it takes 23-27 s alone (measured
//! 2026-10-15) and about 29 s beside the suite. libtest starts tests in
//! name order, and this name sorts first, so the long pole overlaps the rest
//! instead of trailing them (measured 2026-10-05: 30.3 s at load ~20 against
//! 57.5 s when it sorted 100th of 109).

use crate::broadcast::{persisted_topic_cursor, subscribe};
use crate::harness;

use harness::{FLEET, PERIODIC_DEADLINE, World, topic_rows, until, until_within};

/// A roster of late subscribers must not multiply one replay slice into four
/// synchronous Fetches PER SID. One bridge-wide allowance leaves backlog owed
/// while fresh addressed mail and a fleet halt are handled, then rotates until
/// every sid reaches the record.
#[test]
fn many_late_subscribers_leave_room_for_fresh_mail_and_a_halt() {
    let w = World::boot("r23budget", &[]);
    w.wait_ready();
    let (sender, _) = w.two_sessions();
    let mut god = w.god();
    // TWO PAGES PER GAP ON THE TOPIC ITSELF: a catch-up page walks one topic
    // over that topic's own filter, so another topic's records would be
    // scanned past in the same page. 300 fillers on `r23.replay` are a
    // 256-match page and a second one that ends at the target. The filler is
    // another node's, so the per-sender quota it fills (64) leaves the
    // target's own allowance alone.
    let filler = format!("/f/{FLEET}/pub/n-history/s-peer/say/r23.replay");
    let target = format!("/f/{FLEET}/pub/n-target/s-peer/say/r23.replay");
    // The history, pipelined (`harness::publish_pipelined`): 300 fillers, then
    // the target, in that order — without 301 fsync'd round trips before the
    // frontier below even starts.
    let mut history: Vec<harness::Record> = (1..=300)
        .map(|seq| {
            let body = b"v=1 t=1 kind=note text=filler".to_vec();
            (8_123, seq, filler.clone(), body)
        })
        .collect();
    let catchup = b"v=1 t=1 kind=note text=r23catchup".to_vec();
    history.push((8_126, 1, target.clone(), catchup));
    let acks = harness::publish_pipelined(&mut god, &history);
    let (first, target_off) = (acks[0].0, acks[300].0);

    // A live marker proves the bridge's say subscription has passed that whole
    // history before any late opt-in is made. Otherwise these records could
    // arrive live and no backlog Fetch would be needed at all.
    subscribe(&w, &sender, "r23.frontier since=head");
    let frontier = format!("/f/{FLEET}/pub/n-history/s-peer/say/r23.frontier");
    god.publish(8_123, 402, &frontier, b"v=1 t=1 kind=note text=frontier")
        .expect("frontier marker");
    until("the bridge's say reader to pass the history", || {
        topic_rows(&w, &sender, "r23.frontier")
            .iter()
            .any(|row| row.contains("text=frontier"))
            .then_some(())
    });

    let mut late = Vec::new();
    for _ in 0..16 {
        let spawned = w.verb("spawn");
        assert!(spawned.ok(), "{}", spawned.header());
        let sid = spawned
            .header()
            .split_whitespace()
            .nth(1)
            .expect("spawn answers a sid")
            .to_string();
        subscribe(&w, &sid, &format!("r23.replay since=@{first}"));
        late.push(sid);
    }

    // Both arrive while the sixteen two-page gaps are still outstanding.
    // The old per-session four-page loop finished every one during the adds,
    // so the `still_owed` assertion below is its negative control.
    let receiver = late.last().expect("a late subscriber");
    god.publish(
        8_124,
        1,
        &format!("/f/{FLEET}/in/{}/{receiver}/h-x/note", w.node),
        b"v=1 t=1 kind=note text=fresh-during-replay",
    )
    .expect("fresh addressed mail");
    god.publish(
        8_125,
        1,
        &format!("/f/{FLEET}/fleet/h-x/halt"),
        b"v=1 t=1 state=on reason=stop",
    )
    .expect("fleet halt");
    until("fresh addressed mail and the halt", || {
        (w.inbox(receiver)
            .iter()
            .any(|row| row.contains("fresh-during-replay"))
            && w.verb(&format!("@{receiver} status"))
                .header()
                .contains("hold=1"))
        .then_some(())
    });
    let still_owed = late
        .iter()
        .filter(|sid| {
            persisted_topic_cursor(&w, sid, "r23.replay").is_none_or(|next| next <= target_off)
        })
        .count();
    assert!(
        still_owed > 0,
        "fresh mail and halt arrived only after all sixteen gaps were replayed"
    );

    // PACED BY THE BRIDGE'S ROUNDS, so the periodic budget: thirty-two pages at
    // four per two-second round when nothing local is queued, and ONE per round
    // while events are (`may_take_replay_page`) — a loaded machine's events
    // can stretch the walk past a minute of healthy rounds. Still a hang
    // detector: a bridge that stops rotating fails here.
    until_within(
        PERIODIC_DEADLINE,
        "each late subscriber to catch up fairly",
        || {
            late.iter()
                .all(|sid| {
                    persisted_topic_cursor(&w, sid, "r23.replay")
                        .is_some_and(|next| next > target_off)
                })
                .then_some(())
        },
    );
    for sid in &late {
        assert!(
            topic_rows(&w, sid, "r23.replay")
                .iter()
                .any(|row| row.contains("r23catchup")),
            "{sid} advanced its durable cursor without delivering the target"
        );
    }
}
