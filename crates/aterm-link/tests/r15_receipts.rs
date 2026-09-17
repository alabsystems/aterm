// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 15 — exactly once, and a deadline that says so, end to end.**
//!
//! Two properties, each proven on the whole path — a guarded broker, a headless
//! `aterm-gui`, and the real `aterm-link serve` child gui launches:
//!
//! * `post key=<token>` is EXACTLY ONCE. A re-post under the same key is one
//!   record on the bus (counted by `Fetch` on the peer's own lane, which the
//!   endpoint ring's `off=` dedup could otherwise hide) and answers the ORIGINAL
//!   offset with `dup=1` — across a bridge restart and a broker restart alike
//!   (R7: the broker's `(producer_id, producer_seq)` dedup is rebuilt from its
//!   log on restart), and the 4097th key evicts the oldest.
//! * A `dl=` deadline that passes with no answer produces EXACTLY ONE `expired`
//!   row on the asker's lane, and NONE when an answer arrived first — the
//!   asker's own bridge does it, on its tick, because the broker holds no
//!   timers (R8).
//!
//! Every wait is `until <observable state>`, bounded by the harness deadline —
//! a hang detector, never a synchronisation sleep.

#![cfg(unix)]

mod harness;

use harness::{until, until_within, World, FLEET, PERIODIC_DEADLINE};

/// How many records sit on one session's `in` lane, by `Fetch` from zero — the
/// authority a duplicate publish cannot hide behind (the endpoint ring dedups
/// on `off=`, the log does not).
fn lane_len(w: &World, to_sid: &str, src: &str, kind: &str) -> usize {
    let lane = format!("/f/{FLEET}/in/{}/{to_sid}/{src}/{kind}", w.node);
    let mut c = w.god();
    let (rows, _) = c.fetch(0, &lane, 256).expect("fetch the lane");
    rows.len()
}

/// The `msg` rows in B's inbox whose text matches, `--peek` so nothing is marked.
fn matching(w: &World, sid: &str, needle: &str) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| r.contains(needle))
        .collect()
}

/// Wait for the bridge to die under a signal and its supervisor to bring a
/// replacement up, attached.
fn restart_bridge(w: &World) {
    let pid = w.bridge_pid();
    harness::kill(pid, 9);
    until("the bridge to come back under a new pid", || {
        let back: i32 = std::fs::read_to_string(w.state.join("pid"))
            .ok()?
            .trim()
            .parse()
            .ok()?;
        (back != pid).then_some(())
    });
    until("the replacement bridge to attach", || {
        w.verb("status")
            .header()
            .contains("fabric=connected")
            .then_some(())
    });
}

/// Restart the broker on the SAME log and path — the case R7 is about: the
/// broker's dedup map is rebuilt from the log it kept.
fn restart_broker(w: &mut World) {
    if let Some(mut h) = w.handle.take() {
        h.shutdown();
    }
    w.broker.take();
    let _ = std::fs::remove_file(&w.broker_sock);
    let broker = astream_broker::Broker::open_guarded(&w.broker_log, harness::SECRET.to_vec())
        .expect("reopen the guarded broker");
    let handle = broker.serve(&w.broker_sock).expect("re-serve");
    w.broker = Some(broker);
    w.handle = Some(handle);
    until("the bridge to reattach to the restarted broker", || {
        w.verb("status")
            .header()
            .contains("fabric=connected")
            .then_some(())
    });
}

/// **`post key=` IS EXACTLY ONCE ACROSS A BRIDGE RESTART AND A BROKER RESTART.**
///
/// The sender posts once under a key, then re-posts the identical body under the
/// same key three times — as an agent would after an `ERR timeout` it could not
/// tell from a loss — with a bridge kill and a broker restart in between. Every
/// re-post answers the first record's offset with `dup=1`, and B's lane holds
/// exactly one record throughout.
#[test]
fn post_key_is_exactly_once_across_a_bridge_and_a_broker_restart() {
    let mut w = World::boot("r15key", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // The first post lands. `--wait` gives us the offset the key reserved.
    let first = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-abc --wait=30000 only once"
    ));
    assert!(
        first.ok(),
        "first post: {} — log:\n{}",
        first.header(),
        w.log_tail()
    );
    let off = first
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .and_then(|n| n.parse::<u64>().ok())
        .unwrap_or_else(|| panic!("first post must answer an offset: {}", first.header()));

    until("the message to reach B", || {
        (!matching(&w, &b, "text=only%20once").is_empty()).then_some(())
    });
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        1,
        "one record after the first post"
    );

    // A RE-POST under the same key, with a live bridge: deduped, same offset.
    let again = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-abc --wait=30000 only once"
    ));
    assert!(again.ok(), "re-post: {}", again.header());
    assert!(
        again.header().contains(&format!("off={off}")) && again.header().contains("dup=1"),
        "a re-post under the same key answers the original offset with dup=1: {}",
        again.header()
    );
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        1,
        "still one record after the re-post"
    );

    // A RE-POST across a BRIDGE restart: the reservation is in the state dir the
    // relaunched bridge reads, and the key rides the outbox peek.
    restart_bridge(&w);
    let after_bridge = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-abc --wait=30000 only once"
    ));
    assert!(
        after_bridge.ok(),
        "re-post after bridge restart: {}",
        after_bridge.header()
    );
    assert!(
        after_bridge.header().contains(&format!("off={off}"))
            && after_bridge.header().contains("dup=1"),
        "still the original offset, still dup=1, after a bridge restart: {}",
        after_bridge.header()
    );
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        1,
        "still one record after a bridge restart"
    );

    // A RE-POST across a BROKER restart: R7 — the dedup map is rebuilt from the
    // log, so the identical `(producer_id, producer_seq)` is still deduped.
    restart_broker(&mut w);
    let after_broker = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-abc --wait=30000 only once"
    ));
    assert!(
        after_broker.ok(),
        "re-post after broker restart: {}",
        after_broker.header()
    );
    assert!(
        after_broker.header().contains(&format!("off={off}"))
            && after_broker.header().contains("dup=1"),
        "R7: still deduped to the original offset after a broker restart: {}",
        after_broker.header()
    );
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        1,
        "R7: still one record after a broker restart"
    );
    assert_eq!(
        matching(&w, &b, "text=only%20once").len(),
        1,
        "and one row in B's inbox"
    );
}

/// **A DIFFERENT KEY IS A DIFFERENT RECORD.** The exactly-once guarantee is
/// PER KEY: a re-post under the same key dedups, a post under a fresh key does
/// not. (The 4097th-key EVICTION bound is pinned exactly and cheaply in the
/// state unit test `a_key_pins_its_sequence_first_wins_and_the_4097th_evicts_the_oldest`
/// — end to end it would mean 4096 real publishes to overflow the table, which
/// the per-session `OUTBOX_CAP` of 128 refuses long before, and proves nothing
/// the state test does not.)
#[test]
fn a_different_key_is_a_different_record() {
    let w = World::boot("r15keys", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // Two distinct keys are two distinct records.
    let one = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-1 --wait=30000 body one"
    ));
    let two = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-2 --wait=30000 body two"
    ));
    assert!(
        one.ok() && two.ok(),
        "posts: {} / {}",
        one.header(),
        two.header()
    );
    assert!(
        !one.header().contains("dup=1") && !two.header().contains("dup=1"),
        "two fresh keys are two fresh records: {} / {}",
        one.header(),
        two.header()
    );
    until("both distinct-key messages to reach B", || {
        (matching(&w, &b, "text=body%20one").len() == 1
            && matching(&w, &b, "text=body%20two").len() == 1)
            .then_some(())
    });
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        2,
        "two keys, two records"
    );

    // The first key dedups: same offset, dup=1, no new record.
    let off1: u64 = one
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .and_then(|n| n.parse().ok())
        .expect("off of the first post");
    let dup = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-1 --wait=30000 body one"
    ));
    assert!(
        dup.header().contains(&format!("off={off1}")) && dup.header().contains("dup=1"),
        "k-1 still held, same offset, dup=1: {}",
        dup.header()
    );
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        2,
        "no new record for a held key"
    );
}

/// **A DEADLINE THAT PASSES WITH NO ANSWER YIELDS EXACTLY ONE `expired` ROW ON
/// THE ASKER'S LANE (R8), AND NONE WHEN AN ANSWER ARRIVED FIRST.**
///
/// The broker holds no timers; the asker's OWN bridge notices on its sweep and
/// publishes `expired re=<off> dl=<ms>` into the asker's own inbox. A second ask
/// is answered before its deadline and never earns a verdict.
#[test]
fn a_deadline_that_passes_records_one_expired_and_an_answered_one_records_none() {
    let w = World::boot("r15dl", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // A short deadline, no reply. `--wait=0` is not passed — we want the offset,
    // so a normal wait for the landing.
    let asked = w.verb(&format!(
        "@{a} post to=@{b} kind=ask dl=200 --wait=30000 unanswered?"
    ));
    assert!(
        asked.ok(),
        "ask: {} — log:\n{}",
        asked.header(),
        w.log_tail()
    );
    let ask_off: u64 = asked
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .and_then(|n| n.parse().ok())
        .expect("the ask offset");

    // A second ask, ANSWERED before its (longer) deadline passes.
    let answered = w.verb(&format!(
        "@{a} post to=@{b} kind=ask dl=30000 --wait=30000 answered?"
    ));
    assert!(answered.ok(), "second ask: {}", answered.header());
    let answered_off: u64 = answered
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .and_then(|n| n.parse().ok())
        .expect("the answered ask offset");
    let ans = w.verb(&format!(
        "@{b} post to=@{a} kind=answer re={answered_off} --wait=30000 yes"
    ));
    assert!(ans.ok(), "answer: {}", ans.header());

    // The asker's bridge records `expired re=<ask_off>` on the asker's own lane.
    // A bridge tick (DEADLINE_TICK 250 ms) plus the machine, so the periodic
    // budget rather than the event-driven one.
    let expired = until_within(
        PERIODIC_DEADLINE,
        "the asker's bridge to record expired",
        || {
            matching(&w, &a, &format!("re={ask_off}"))
                .into_iter()
                .find(|r| r.contains("kind=expired"))
        },
    );
    assert!(
        expired.contains("dl=200"),
        "the verdict echoes the deadline: {expired}"
    );

    // EXACTLY ONE, on the lane itself (the ring would dedup a second delivery on
    // off=, so the lane is the sharper count). The sweep removes the entry on the
    // tick it publishes, so there is nothing left to repeat — and even a
    // relaunched bridge reading a stale durable table checks the bus first and
    // finds this very record. The `aterm fabric` WARNINGS wiring that lists it is
    // pinned by the unit test `an_ask_past_its_deadline_with_no_reply_is_a_warning`.
    assert_eq!(
        lane_len(&w, &a, &w.node, "expired"),
        1,
        "exactly one expired record on the asker's lane, never a repeat"
    );

    // The ANSWERED ask never earns a verdict: no expired row names its offset.
    assert!(
        matching(&w, &a, &format!("re={answered_off}"))
            .iter()
            .all(|r| !r.contains("kind=expired")),
        "an ask answered before its deadline is never recorded expired"
    );
}

/// **A KEYED RE-POST IS RESOLVED AGAINST THE LIVE ROSTER BEFORE ITS KEY CHOOSES A
/// SEQUENCE — and the key still names its one record.**
///
/// Where round 15 met 69979209f. That commit made `drain_outbox` re-read the
/// endpoint's roster before it resolves a single address, because a door that
/// answered `@s-<sid>` from the bridge's REMEMBERED roster published posts onto
/// the `in` face of a session that had gone and told the sender `off=<n>`. Round
/// 15 put the idempotency key INSIDE that door: the address is resolved, and only
/// a post that routes has its key looked up and its sequence reused. So the two
/// meet on a re-post under a key whose recipient has left — the one case where
/// the key alone would have "worked": a stale roster routes it, the reserved
/// sequence dedups, and the sender hears `OK off=<first> dup=1` for a door that
/// never checked the session was still there.
///
/// The merged rule, asserted here with the `session-exited` line held back
/// (`ATERM_LINK_FAULT=drop-session-exited-while-marked`, the state a `GAP`, a late
/// attach or a lost push fd leaves) so the stale picture is guaranteed rather than
/// raced:
///
/// * the re-post to the departed session is retired `unroutable`, like any post
///   the door cannot route, and puts NOTHING on the bus (its lane still holds the
///   first record alone);
/// * that retirement neither consumes nor re-points the key: a later re-post under
///   it that DOES route answers the first record's offset `dup=1` — the broker's
///   `(producer_id, producer_seq)` dedup, not the address, is what makes a key one
///   record — and appends nothing to the new address's lane either.
#[test]
fn a_keyed_re_post_to_a_session_whose_exit_line_never_arrives_is_unroutable_and_the_key_holds() {
    let w = World::boot_with(
        "r15keygone",
        &[],
        &[("ATERM_LINK_FAULT", "drop-session-exited-while-marked")],
    );
    w.wait_ready();
    // OPEN THE WINDOW BEFORE THE SESSION THAT WILL FALL INTO IT EXISTS.
    std::fs::write(w.state.join("drop-session-exited"), b"1\n").expect("arm the dropped line");
    let (a, b) = w.two_sessions();
    let spawned = w.verb("spawn");
    assert!(spawned.ok(), "spawn: {}", spawned.header());
    let c = spawned
        .header()
        .split_whitespace()
        .nth(1)
        .expect("spawn answers OK <sid>")
        .to_string();
    until("the bridge to see all three sessions", || {
        (w.sessions().len() >= 3).then_some(())
    });

    // THE FABRIC HAS ADVERTISED THE PEER, so a stale `live` row exists to lie.
    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the peer's presence row to say live", || {
        let mut g = w.god();
        let (rows, _) = g.last(&presence, "", 8).ok()?;
        rows.iter()
            .find(|(_, s, _)| *s == presence)
            .filter(|(_, _, body)| String::from_utf8_lossy(body).contains("state=live"))
            .map(|_| ())
    });

    // The first post under the key lands: one record, its offset the key's.
    let first = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-gone --wait=30000 once only"
    ));
    assert!(
        first.ok() && !first.header().contains("dup=1"),
        "first post: {} — log:\n{}",
        first.header(),
        w.log_tail()
    );
    let off: u64 = first
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .and_then(|n| n.parse().ok())
        .unwrap_or_else(|| panic!("the first post names its offset: {}", first.header()));
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        1,
        "one record after the first post"
    );

    // THE PEER GOES, and the bridge is never told.
    assert!(w.verb(&format!("@{b} close")).ok(), "close the peer");
    until("the peer to leave the roster", || {
        (!w.sessions().iter().any(|(_, sid, _)| *sid == b)).then_some(())
    });

    // THE RE-POST UNDER THE SAME KEY, TO THE SESSION THAT WENT: the door asks the
    // endpoint, finds no such session, and retires it — no `dup=1` off a stale
    // route.
    let gone = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-gone --wait=30000 once only"
    ));
    assert!(
        gone.header().starts_with("ERR unroutable"),
        "a keyed re-post to a session aterm's roster no longer lists is not a route: {}",
        gone.header()
    );
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        1,
        "nothing appended to the departed session's face"
    );

    // THE KEY STILL NAMES ITS RECORD. A re-post under it that routes answers the
    // first record, and the new address's lane gets nothing.
    let routed = w.verb(&format!(
        "@{a} post to=@{c} kind=note key=k-gone --wait=30000 once only"
    ));
    assert!(
        routed.ok()
            && routed.header().contains(&format!("off={off}"))
            && routed.header().contains("dup=1"),
        "the unroutable retirement neither consumed nor re-pointed the key: {}",
        routed.header()
    );
    assert_eq!(
        lane_len(&w, &c, &w.node, "note"),
        0,
        "a deduped re-post appends nothing anywhere"
    );
    let pins = std::fs::read_to_string(w.state.join("pins")).unwrap_or_default();
    assert!(
        !pins.contains(&b),
        "no TOFU pin is written for a session that no longer exists: {pins:?}"
    );
}
