// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **THE PUSHED OPT-IN SURVIVES A HOLE IN THE PUSH LANE.**
//!
//! A `topic add` is one record on the session's timeline, and the timeline is
//! a drop-oldest ring of [`TIMELINE_CAP`] records. A push-lane watch that
//! stopped draining — its bridge stopped, its socket full on a busy node —
//! keeps its watermark while the ring moves on, and once the ring has evicted
//! the add the only honest answer the endpoint can give is a `GAP`: records
//! this watcher never saw are gone. The bridge treats that GAP as what it is
//! and re-reads every session's `topic ls`, so the opt-in is learned and its
//! backlog delivered. Over the real binaries, because the two halves live in
//! two processes.
//!
//! THE HOLE IS MADE, NOT RACED. The endpoint's debug-only
//! `$ATERM_TEST_PUSH_HOLD` `timeline` marker withholds every watch's timeline
//! drain, and it is read under the lock each record is appended under, so a
//! record made while it stands is in the hole by construction ([`Hold`]). A
//! stopped bridge left that to whether the push loop had filled its socket
//! before the add, to whether its watch of a fresh session was seeded before
//! the stop, and to whether the bridge still owed a read of the session that
//! would teach it the add ahead of the GAP's — and under a loaded machine each
//! of the three went the wrong way. The last is closed by observation
//! ([`settle`]). Nothing here sleeps: every step waits for an observation.
//!
//! [`TIMELINE_CAP`]: the endpoint's `session_timeline::TIMELINE_CAP`

use crate::harness;

use aterm_link::ctl::Ctl;
use harness::{Hold, World, bridge_holds, say_records, topic_rows, until, watchers};

/// The opt-in this test is about.
const TOPIC: &str = "r24.gap";

/// The opt-in that shows the bridge has settled on the session — see
/// [`settle`].
const WARM: &str = "r24.warm";

/// `2n` timeline records on `sid` — `meta set title` alternating two values.
/// Each is pushed as `EVENT <local> meta`, which the bridge answers with one
/// `meta` read, so a release costs it at most a ring of those.
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

/// The `(low, high)` ids of a session's retained timeline.
fn bounds(c: &mut Ctl, sid: &str) -> (u64, u64) {
    let tl = timeline(c, sid);
    match (tl.first(), tl.last()) {
        (Some((low, _)), Some((high, _))) => (*low, *high),
        _ => panic!("{sid}'s timeline is empty after it was churned"),
    }
}

/// Churn `sid` until the ring's low is past record `id`: the endpoint no
/// longer holds it. Answers that low. The ring drops its oldest at its cap, so
/// this takes at most a ring of records; one that never drops them meets the
/// hang detector.
fn evict_past(c: &mut Ctl, sid: &str, id: u64) -> u64 {
    until(
        &format!("the timeline ring to drop the oldest past #{id}"),
        || {
            let (low, _) = bounds(c, sid);
            if low > id {
                return Some(low);
            }
            churn(c, sid, 64);
            None
        },
    )
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
///
/// The bridge reads a session's opt-ins on that ack, on a `session-created`,
/// on every roster round while it holds no ack for the session or its last
/// read failed, and on any `GAP`. A read still owed when the add is recorded
/// would teach the bridge the add ahead of the GAP's, and `who` cannot say
/// whether one is: `watchers=` is the endpoint's half, true before the ack
/// reaches the bridge. A `GAP` can say it, because the bridge publishes each
/// one as an `ev` record and nothing else writes that. So, in order:
///
/// 1. `watchers >= 1`: the watch is seeded, and its `sub` ack and the
///    session's `session-created` are queued ahead of every frame the watch
///    drains ([`harness::watchers`]).
/// 2. A hole with nothing in it, and its GAP published: the GAP is one of
///    those frames, so the bridge has handled every line the lane carried
///    before it, the ack and the `session-created` among them, and has begun
///    the GAP's re-read.
/// 3. [`WARM`] added, and held: what taught it ran after the add, so after
///    that re-read began — the lane's `EVENT` behind the GAP, or the re-read
///    itself — and with it the session's last owed read is done.
///
/// A read the endpoint fails stays owed and is made again later. That is a
/// broken verb lane, not a race, and the test's premise check names it.
///
/// Answers the session's `local` id.
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
    // The watermark is at most `high`, and a GAP needs the low past `high + 1`.
    hold.on("timeline");
    let (_, high) = bounds(c, sid);
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
///
/// A session the bridge is settled on ([`settle`]) has its timeline drain held
/// ([`Hold`]), adds the topic it wants (with a backlog to replay), and records
/// more until the add is evicted from the endpoint's own timeline; the drain
/// is released. The endpoint must report the hole, with a count that covers
/// every record from the add to the ring's low; the bridge must publish it,
/// learn the topic from the re-read it triggers — nothing else is left to
/// teach it — deliver the backlog record, and deliver a record published after
/// it came back. A hole nobody reports, or one the bridge does not act on,
/// never teaches the bridge the topic, and the wait for it is the hang
/// detector.
#[test]
fn an_add_evicted_from_the_timeline_is_learned_after_the_gap() {
    let hold = Hold::new("r24");
    let w = World::boot_with("r24gap", &[], &[("ATERM_TEST_PUSH_HOLD", &hold.env())]);
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

    // THE HOLE, WITH THE ADD IN IT.
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
    // THE GAP IS WAITED FOR, NOT READ ONCE. The bridge publishes it before the
    // re-read it causes, but PIPELINED (`publish_ev_for` does not wait for the
    // broker's ack), and what the wait above observed is the bridge's own
    // `topics/` file, a different channel: it orders the re-read, not the
    // GAP's commit. A broker read answers from the log as it stands, and while
    // the GAP's batch is being written and flushed that is the log before it —
    // astream's writer does that I/O with the log lock released
    // (`broker.unlocked-batch-write`), so a read no longer waits out the batch
    // and sees it after. A hole nobody reports still fails here, at the hang
    // detector. Its count is `low' - w - 1`, `w` the watermark (below the add,
    // by the hold) and `low'` the ring's low at the drain that found it, at or
    // above `low`: at least `low - add`. The first GAP after the release is
    // the drain that found the hole, and the bridge's `ev`s commit in the
    // order it sent them, so the first one visible is that one.
    let reported = until("the GAP that names the hole to be on the bus", || {
        let reported = gaps_for(&w, local).split_off(earlier);
        (!reported.is_empty()).then_some(reported)
    });
    assert!(
        reported.iter().any(|n| *n >= low - add),
        "the endpoint reported the hole that evicted #{add} (the ring's low was \
         #{low} at the release) as at least {} records: {reported:?}",
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
