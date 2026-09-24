// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 24 — THE PUSHED OPT-IN SURVIVES A HOLE IN THE PUSH LANE.**
//!
//! A `topic add` is one record on the session's timeline, and the timeline is
//! a drop-oldest ring of [`TIMELINE_CAP`] records. A bridge whose reader has
//! stalled — a stopped process here, a full socket on a busy node — comes back
//! to a timeline that has evicted the record, and the only honest answer the
//! endpoint can give is a `GAP`: records this watcher never saw are gone. The
//! bridge treats that GAP as what it is and re-reads every session's `topic
//! ls`, so the opt-in is learned and its backlog delivered. Over the real
//! binaries, because the two halves live in two processes.
//!
//! [`TIMELINE_CAP`]: the endpoint's `session_timeline::TIMELINE_CAP` (512)

mod harness;

use aterm_link::ctl::Ctl;
use harness::{until, World, FLEET};

#[cfg(target_os = "macos")]
const SIGSTOP: i32 = 17;
#[cfg(target_os = "macos")]
const SIGCONT: i32 = 19;
#[cfg(target_os = "linux")]
const SIGSTOP: i32 = 19;
#[cfg(target_os = "linux")]
const SIGCONT: i32 = 18;

/// Every record currently on the fleet's broadcast subtree.
fn say_records(w: &World) -> Vec<(u64, String, String)> {
    let filter = format!("/f/{FLEET}/pub/*/*/say/>");
    let mut c = w.god();
    let mut out = Vec::new();
    let mut from = 0u64;
    loop {
        let (rows, (next, head)) = c.fetch(from, &filter, 256).expect("fetch the say subtree");
        for (off, subject, body) in rows {
            out.push((off, subject, String::from_utf8_lossy(&body).into_owned()));
        }
        if next >= head {
            return out;
        }
        from = next.max(from + 1);
    }
}

/// The rows of a session's ring that arrived on `topic`.
fn topic_rows(w: &World, sid: &str, topic: &str) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| r.contains(&format!(" topic={topic} ")))
        .collect()
}

/// `2n` timeline records on `sid` — `meta set title` alternating two values,
/// a record the bridge's event arms ignore, so the backlog it reads after
/// SIGCONT costs it nothing per line (a `topic` record costs a state write and
/// a broker fetch each, and a thousand of them under load outlast the hang
/// detector before the GAP frame is even reached).
fn churn(c: &mut Ctl, sid: &str, n: usize) {
    for _ in 0..n {
        for verb in ["meta set title r24a", "meta set title r24b"] {
            let reply = c.request(&format!("@{sid} {verb}")).expect("churn");
            assert!(reply.ok(), "{verb}: {}", reply.header());
        }
    }
}

/// The opt-in this test is about.
const TOPIC: &str = "r24.gap";

/// The opt-in a round's session takes BEFORE the stop, and which the bridge
/// must be seen to hold — see [`watched_session`].
const WARM: &str = "r24.warm";

/// Records a round writes before the add, in the first round. The add is
/// dropped only if the stopped bridge's push lane is FULL before it is
/// recorded, and a macOS `socketpair(AF_UNIX)` holds 8 KiB (measured: 8,160
/// bytes of 40-byte writes) — about 220 `EVENT <local> meta …` lines, plus the
/// one frame in flight when the write blocks. That is a property of the
/// kernel, not of this test, so the number is NOT trusted: each round's own
/// `GAP` report says whether the add fell in the hole ([`Round::held`]), and a
/// round that shows it did not is run again, on a fresh session, with twice
/// the fill.
const FIRST_FILL: usize = 1200;

/// How many rounds before the test gives up: 1,200 records doubling to 19,200,
/// about 675 KiB of these ~36-byte lines — more than eighty times the 8 KiB
/// measured here, and past a 208 KiB send buffer (a common Linux default, not
/// measured by this test).
const ROUNDS: usize = 5;

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

/// `Some` once the BRIDGE holds `topic` for `sid`: its persisted cursors, one
/// topic per line.
fn bridge_holds(w: &World, sid: &str, topic: &str) -> Option<()> {
    std::fs::read_to_string(w.state.join(format!("topics/{sid}")))
        .ok()?
        .lines()
        .any(|l| l.split(' ').next() == Some(topic))
        .then_some(())
}

/// Every `events-dropped=` count the bridge has published for the session at
/// `local` — the endpoint's `GAP <local> events-dropped=<n>`, as `ev gap …`.
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

/// A FRESH session the bridge's push lane is watching, whose opt-ins the bridge
/// has already read.
///
/// THIS IS THE RACE THE TEST USED TO LOSE. The push lane is `subscribe @*`, and
/// an `@*` subscription ADOPTS a session created after it on its next wake — a
/// notify on a session it already watches, or its 250 ms liveness tick — and
/// seeds the new watch at the timeline's high AT THAT MOMENT. Nothing the test
/// used to wait for implied that wake: it stopped the bridge once the backlog
/// record landed, and that record can land through the bridge's own outbox
/// drain, while a churned session nobody watches notifies nobody. Stopped
/// before the adoption, the bridge's watch is seeded after the churn, past the
/// add; it never falls behind, the endpoint correctly reports no hole, and the
/// bridge learns the add from its first-sighting sample (`session-created`,
/// then `topic ls`) — while the test waited 60 s for a GAP that could not
/// exist. Measured: a stop 9–25 ms after the spawn gave no GAP in 5 runs of 9
/// and a short one (735, 752 dropped) in 2 more, the topic learned every time;
/// the world a failing run left behind shows the bridge publishing the
/// session's presence only AFTER the churn (`title=r24b`) and after the
/// backlog record, where a passing run publishes it (`title=-`) before.
///
/// So the barrier is an observation: the session opts into [`WARM`] and the
/// bridge must hold it. It learns that off the push lane — the
/// `session-created` frame's sample or the `EVENT <local> topic` line (a
/// broker re-attach re-samples too, and is not in play here). In the usual
/// order the endpoint writes either only after the adoption has seeded the
/// watch (one wake adopts, then drains the roster, then the watches). That is
/// not a guarantee: adoption and the roster drain are two store reads, so a
/// spawn landing between them is announced one wake before it is adopted, and
/// a GAP on any other session makes the bridge re-read every session's topics,
/// adopted or not. Either way the barrier can pass before the cursor exists.
/// What follows is a possible 60 s hang, never a false pass, and the window is
/// microseconds against a 250 ms tick. Holding [`WARM`] also means the bridge
/// has SAMPLED this session, so once it is stopped the first-sighting sample
/// cannot teach it the add; only a GAP's re-read can. [`Round::held`] does not
/// lean on this barrier to be sound; the barrier is what makes the first
/// round's premise hold.
fn watched_session(w: &World) -> (String, u64) {
    let reply = w.verb("spawn");
    assert!(reply.ok(), "spawn: {}", reply.header());
    let sid = reply
        .header()
        .split_whitespace()
        .nth(1)
        .expect("spawn answers OK <sid>")
        .to_string();
    let warm = w.verb(&format!("@{sid} topic add {WARM}"));
    assert!(warm.ok(), "topic add {WARM}: {}", warm.header());
    until(
        "the bridge to read the new session's opt-ins off its push lane",
        || bridge_holds(w, &sid, WARM),
    );
    let local = until("the new session's local id", || {
        w.sessions()
            .into_iter()
            .find(|(_, s, _)| *s == sid)
            .map(|(local, _, _)| local)
    });
    (sid, local)
}

/// What one round measured.
struct Round {
    sid: String,
    local: u64,
    /// Records written after the stop and before the add.
    fill: usize,
    /// The add's timeline id.
    add: u64,
    /// The ring's low when the bridge was resumed: the churn after the add
    /// runs until this is at least `2 * add`.
    low: u64,
    /// Records the session gained after the churn, counted after the GAPs were
    /// read — an upper bound on how far the ring moved before the endpoint's
    /// first drain after the resume.
    later: u64,
    /// Every `events-dropped=` published for this session.
    dropped: Vec<u64>,
}

impl Round {
    /// The smallest `events-dropped=` that PROVES the hole held the add.
    ///
    /// A hole is `[w + 1, low' - 1]`, `w` the watch's watermark and `low'` the
    /// ring's low at the drain that found it, with `low <= low' <= low + later`.
    /// One that held the add (`w < add`) reports at least `low - add`; this asks
    /// for `later` more, so a record the resumed bridge wrote before that drain
    /// cannot pass for the hole. A hole AFTER the add (`w >= add`) reports at
    /// most `low + later - 1 - add`: below this. A hole BEFORE it holds at most
    /// the `add - 1` records the session had: also below, because `low >= 2 *
    /// add`. So a count at or above this is the endpoint saying, in numbers,
    /// that the add was never pushed.
    fn threshold(&self) -> u64 {
        self.low + self.later - self.add
    }

    /// Whether the endpoint reported a hole that held the add.
    fn held(&self) -> bool {
        let floor = self.threshold();
        self.dropped.iter().any(|n| *n >= floor)
    }

    fn summary(&self) -> String {
        format!(
            "{} (local {}): fill={} add=#{} low=#{} later={} needs events-dropped>={} got {:?}",
            self.sid,
            self.local,
            self.fill,
            self.add,
            self.low,
            self.later,
            self.threshold(),
            self.dropped
        )
    }
}

/// One round on a fresh watched session: stop the bridge, write `fill`
/// records, add [`TOPIC`] with a backlog at `off`, churn until the endpoint
/// has evicted the add, resume, and wait until the bridge holds the topic —
/// which it must, by the GAP's re-read or, if the lane absorbed the add, off
/// the lane itself. [`Round::held`] says which.
fn round(w: &World, off: u64, fill: usize) -> Round {
    let (sid, local) = watched_session(w);
    harness::kill(w.bridge_pid(), SIGSTOP);
    let mut c = w.ctl();
    churn(&mut c, &sid, fill / 2);
    let added = c
        .request(&format!("@{sid} topic add {TOPIC} since=@{off}"))
        .expect("topic add");
    assert_eq!(added.header(), format!("OK {TOPIC} since=@{off} added=1"));
    let needle = format!("kind=topic add {TOPIC} ");
    let add = timeline(&mut c, &sid)
        .into_iter()
        .rev()
        .find(|(_, row)| row.contains(&needle))
        .map(|(id, _)| id)
        .expect("the add is on the session's timeline");
    // CHURN UNTIL THE RING IS PAST THE ADD by more than the session's whole
    // history before it — measured on the ring, not assumed from a count —
    // which is what lets [`Round::threshold`] tell a hole that held the add
    // from one before it.
    let budget = 2 * usize::try_from(add).expect("an id fits") + 65_536;
    let mut spent = 0;
    let (low, high) = loop {
        let (low, high) = bounds(&mut c, &sid);
        if low >= 2 * add {
            break (low, high);
        }
        assert!(
            spent < budget,
            "{spent} records after the add and the ring's low is only #{low}: \
             it does not drop the oldest past a bound this test can reach"
        );
        churn(&mut c, &sid, 64);
        spent += 128;
    };
    // The endpoint itself no longer holds the record: the ring evicted it.
    assert!(
        !timeline(&mut c, &sid)
            .iter()
            .any(|(_, row)| row.contains(&needle)),
        "the add must be evicted for this test to mean anything"
    );
    harness::kill(w.bridge_pid(), SIGCONT);

    until(
        "the bridge to learn r24.gap — from the GAP's re-read, or off the live \
         lane if the lane absorbed the add",
        || bridge_holds(w, &sid, TOPIC),
    );
    // In THIS order: the GAP is published before the re-read it causes, so it
    // is on the bus by now; and `later` is read after it, so it counts every
    // record written before the drain that reported it.
    let dropped = gaps_for(w, local);
    let (_, now) = bounds(&mut c, &sid);
    Round {
        sid,
        local,
        fill,
        add,
        low,
        later: now - high,
        dropped,
    }
}

/// **AN ADD EVICTED WHILE THE BRIDGE WAS NOT READING IS STILL LEARNED.**
///
/// A session the bridge's push lane is watching ([`watched_session`]) has its
/// bridge stopped, records enough events to fill the push socket, adds the
/// topic it wants (with a backlog to replay), and records more until the add is
/// evicted from the endpoint's own timeline; the bridge is resumed. The
/// endpoint must report the hole, with a count that proves the add was in it
/// ([`Round::held`]); the bridge must publish it and learn the topic from the
/// re-read it triggers, deliver the backlog record, and deliver a record
/// published after it came back.
///
/// THE PREMISE IS MEASURED, NOT ASSUMED. Whether the add is dropped depends on
/// how much the stopped lane buffers, which is the kernel's business, so a
/// round whose report shows the lane absorbed the add is run again on a fresh
/// session with twice the fill ([`FIRST_FILL`], [`ROUNDS`]). That is not a
/// retry of a failure: such a round learned the topic too, off the lane, and
/// proved nothing about the GAP either way. A lost event still fails — a hole
/// nobody reports, or one the bridge does not act on, never teaches the bridge
/// the topic, and the wait for it is the hang detector.
#[test]
fn an_add_evicted_from_the_timeline_is_learned_after_the_gap() {
    let w = World::boot("r24gap", &[]);
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

    let mut absorbed = Vec::new();
    let mut fill = FIRST_FILL;
    let held = loop {
        let r = round(&w, off, fill);
        if r.held() {
            break r;
        }
        absorbed.push(r.summary());
        assert!(
            absorbed.len() < ROUNDS,
            "in {ROUNDS} rounds no published gap held the add: either the push lane absorbed \
             it every time (the rounds below show gaps too short to hold it), or the endpoint \
             or bridge never published a gap at all (the rounds show none) — either way \
             nothing here exercised the GAP: {absorbed:#?}"
        );
        fill *= 2;
    };
    let s_b = held.sid;

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
