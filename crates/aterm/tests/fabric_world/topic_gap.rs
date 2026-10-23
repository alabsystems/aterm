// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Push-lane topic opt-in recovery across a session log eviction ([`Hold`], [`settle`], [`SLOG_RING`]).

use crate::harness;

use aterm_link::ctl::Ctl;
use harness::{Hold, World, bridge_holds, say_records, topic_rows, until, watchers};

/// The opt-in this test is about.
const TOPIC: &str = "r24.gap";

/// The opt-in that shows the bridge has settled on the session — see
/// [`settle`].
const WARM: &str = "r24.warm";

/// Each session log's bound in this world (`ATERM_SLOG_RING`): one [`churn`] of 64 overflows it.
const SLOG_RING: &str = "96,65536";

/// `2n` timeline records on `sid` — `meta set title` alternating two values.
fn churn(c: &mut Ctl, sid: &str, n: usize) {
    for _ in 0..n {
        for verb in ["meta set title r24a", "meta set title r24b"] {
            let reply = c.request(&format!("@{sid} {verb}")).expect("churn");
            assert!(reply.ok(), "{verb}: {}", reply.header());
        }
    }
}

/// A session's retained timeline, oldest first, as `(id, row)`.
fn timeline(c: &mut Ctl, sid: &str) -> Vec<(u64, String)> {
    let reply = c.request(&format!("@{sid} timeline")).expect("timeline");
    assert!(reply.ok(), "timeline: {}", reply.header());
    reply
        .rows()
        .iter()
        .filter_map(|row| {
            let mut f = row.split_whitespace();
            if f.next() != Some("event") {
                return None;
            }
            Some((f.next()?.parse().ok()?, row.clone()))
        })
        .collect()
}

/// The newest row id of a session's timeline.
fn head(c: &mut Ctl, sid: &str) -> u64 {
    match timeline(c, sid).last() {
        Some((high, _)) => *high,
        None => panic!("{sid}'s timeline is empty"),
    }
}

/// Churn `sid` until row `id` is issued and the timeline no longer holds it; answers the first row held above it.
fn evict_past(c: &mut Ctl, sid: &str, id: u64) -> u64 {
    until(&format!("the timeline to drop row #{id}"), || {
        let tl = timeline(c, sid);
        if let Some(low) = tl.iter().map(|(i, _)| *i).find(|&i| i > id)
            && tl.iter().all(|(i, _)| *i != id)
        {
            return Some(low);
        }
        churn(c, sid, 64);
        None
    })
}

/// Every `events-dropped=` count the bridge has published for the session at
/// `local` — the endpoint's `GAP <local> events-dropped=<n>`, as `ev gap …` —
/// in the order the bus holds them.
fn gaps_for(w: &World, local: u64) -> Vec<u64> {
    w.ev()
        .iter()
        .filter_map(|e| {
            let mut f = e.strip_prefix("gap ")?.split_whitespace();
            if f.next()?.parse::<u64>().ok()? != local {
                return None;
            }
            f.find_map(|t| t.strip_prefix("events-dropped="))?
                .parse()
                .ok()
        })
        .collect()
}

/// SETTLE THE BRIDGE ON `sid`: on return the bridge has handled its push
/// lane's `sub` ack for the session and owes no read of its opt-ins, so from
/// here only a `GAP` (or a broker re-attach) makes it read them again.
fn settle(w: &World, hold: &Hold, c: &mut Ctl, sid: &str) -> u64 {
    until("the push lane to watch the session", || {
        (watchers(w, sid)? >= 1).then_some(())
    });
    let local = until("the session's local id", || {
        w.sessions()
            .into_iter()
            .find(|(_, s, _)| s == sid)
            .map(|(local, _, _)| local)
    });
    hold.on("timeline");
    let high = head(c, sid);
    evict_past(c, sid, high + 1);
    hold.off("timeline");
    until("the bridge to publish the empty hole's GAP", || {
        (!gaps_for(w, local).is_empty()).then_some(())
    });
    let warm = w.verb(&format!("@{sid} topic add {WARM}"));
    assert!(warm.ok(), "topic add {WARM}: {}", warm.header());
    until("the bridge to learn the settling opt-in", || {
        bridge_holds(w, sid, WARM)
    });
    local
}

/// **AN ADD EVICTED WHILE THE PUSH LANE WAS NOT DRAINING IS STILL LEARNED.**
#[test]
fn an_add_evicted_from_the_timeline_is_learned_after_the_gap() {
    let hold = Hold::new("r24");
    let w = World::boot_with(
        "r24gap",
        &[],
        &[
            ("ATERM_TEST_PUSH_HOLD", &hold.env()),
            ("ATERM_SLOG_RING", SLOG_RING),
            harness::FAST_TIMERS,
        ],
    );
    w.wait_ready();
    let s_a = until("aterm's boot session", || {
        w.sessions().first().map(|(_, sid, _)| sid.clone())
    });

    let posted = w.verb(&format!(
        "@{s_a} post to=say:{TOPIC} kind=note r24gapbacklog"
    ));
    assert!(posted.ok(), "{}", posted.header());
    let (off, _, _) = until("the backlog record to land", || {
        say_records(&w)
            .into_iter()
            .find(|(_, _, b)| b.contains("r24gapbacklog"))
    });

    let spawned = w.verb("spawn");
    assert!(spawned.ok(), "spawn: {}", spawned.header());
    let s_b = spawned
        .header()
        .split_whitespace()
        .nth(1)
        .expect("spawn answers OK <sid>")
        .to_string();
    let mut c = w.ctl();
    let local = settle(&w, &hold, &mut c, &s_b);

    hold.on("timeline");
    let added = c
        .request(&format!("@{s_b} topic add {TOPIC} since=@{off}"))
        .expect("topic add");
    assert_eq!(added.header(), format!("OK {TOPIC} since=@{off} added=1"));
    let needle = format!("kind=topic add {TOPIC} ");
    let add = timeline(&mut c, &s_b)
        .into_iter()
        .rev()
        .find(|(_, row)| row.contains(&needle))
        .map(|(id, _)| id)
        .expect("the add is on the session's timeline");
    let low = evict_past(&mut c, &s_b, add);
    assert!(
        bridge_holds(&w, &s_b, TOPIC).is_none(),
        "the premise: the bridge learned {TOPIC} while the add was withheld and \
         no read of {s_b} was owed — a broker re-attach, or a read the endpoint \
         failed? log:\n{}",
        w.log_tail()
    );
    let earlier = gaps_for(&w, local).len();
    hold.off("timeline");

    until("the bridge to learn r24.gap from the GAP's re-read", || {
        bridge_holds(&w, &s_b, TOPIC)
    });
    let reported = until("the GAP that names the hole to be on the bus", || {
        let reported = gaps_for(&w, local).split_off(earlier);
        (!reported.is_empty()).then_some(reported)
    });
    assert!(
        reported.iter().any(|n| *n >= low - add),
        "the endpoint reported the hole that evicted #{add} (the first row held \
         above it was #{low} at the release) as at least {} records: {reported:?}",
        low - add
    );

    let backlog = until("the backlog record to be delivered", || {
        let rows = topic_rows(&w, &s_b, TOPIC);
        (!rows.is_empty()).then_some(rows)
    });
    assert!(
        backlog.iter().any(|r| r.contains(&format!("off={off} "))),
        "the record named by since=@{off} arrived: {backlog:?}"
    );

    let posted = w.verb(&format!("@{s_a} post to=say:{TOPIC} kind=note r24live"));
    assert!(posted.ok(), "{}", posted.header());
    until("a record published after the gap to be delivered", || {
        topic_rows(&w, &s_b, TOPIC)
            .into_iter()
            .find(|r| r.contains("text=r24live"))
    });
}
