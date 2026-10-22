// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **Exactly once, and a deadline that says so, end to end.**
//!
//! Two properties, each proven on the whole path — a guarded broker, a headless
//! `aterm`, and the real `aterm link serve` child it launches:
//!
//! * `post key=<token>` is EXACTLY ONCE. A re-post under the same key is one
//!   record on the bus (counted by `Fetch` on the peer's own lane, which the
//!   endpoint ring's `off=` dedup could otherwise hide) and answers the ORIGINAL
//!   offset with `dup=1` — across a bridge restart and a broker restart alike
//!   (R7: the broker's `(producer_id, producer_seq)` dedup is rebuilt from its
//!   log on restart), and the 4097th key evicts the oldest.
//! * A `dl=` deadline that passes with no answer produces EXACTLY ONE `expired`
//!   row on the asker's lane, and NONE when an answer arrived first — the
//!   asker's own bridge does it at the armed deadline, because the broker holds no
//!   timers (R8).
//!
//! Every wait is `until <observable state>`, bounded by the harness deadline —
//! a hang detector, never a synchronisation sleep.

#![cfg(unix)]

use crate::harness;

use harness::{FLEET, PERIODIC_DEADLINE, World, until, until_within};

/// How many records sit on one session's `in` lane, by `Fetch` from zero: a duplicate
/// publish cannot hide here (the ring dedups on `off=`, the log does not).
fn lane_len(w: &World, to_sid: &str, src: &str, kind: &str) -> usize {
    let lane = format!("/f/{FLEET}/in/{}/{to_sid}/{src}/{kind}", w.node);
    let (rows, _) = w.read(|c| c.fetch(0, &lane, 256)).expect("fetch the lane");
    rows.len()
}

/// The rows of `sid`'s inbox containing `needle` (`World::inbox` peeks, marking nothing).
fn matching(w: &World, sid: &str, needle: &str) -> Vec<String> {
    w.inbox(sid)
        .into_iter()
        .filter(|r| r.contains(needle))
        .collect()
}

/// One post under a key, then three identical re-posts under it (as after an `ERR timeout`
/// indistinguishable from a loss), with a bridge kill and a broker restart between: each
/// answers the first offset with `dup=1`, and B's lane holds one record throughout.
#[test]
fn post_key_is_exactly_once_across_a_bridge_and_a_broker_restart() {
    let mut w = World::boot("r15key", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // The first post lands; `--wait` answers the offset the key reserved.
    let first = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-abc --wait=60000 only once"
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

    // A re-post under the same key, with a live bridge: deduped, same offset.
    let again = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-abc --wait=60000 only once"
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

    // Across a bridge restart: the reservation is in the state dir the relaunched bridge
    // reads, and the key rides the outbox peek.
    w.restart_bridge();
    let after_bridge = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-abc --wait=60000 only once"
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

    // Across a broker restart: the broker rebuilds its `(producer_id, producer_seq)` dedup
    // from the log (R7).
    w.stop_broker();
    w.start_broker();
    let after_broker = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-abc --wait=60000 only once"
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

    // Negative control: the guarantee is per key, so the same body under a fresh key is a
    // fresh record.
    let other = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-other --wait=60000 only once"
    ));
    assert!(
        other.ok() && !other.header().contains("dup=1"),
        "a fresh key is a fresh record: {}",
        other.header()
    );
    until("the fresh key's message to reach B", || {
        (matching(&w, &b, "text=only%20once").len() == 2).then_some(())
    });
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        2,
        "two keys, two records"
    );
}

/// A deadline that passes unanswered yields exactly one `expired` row on the asker's lane
/// (R8), and an ask answered first yields none. The broker holds no timers: the asker's own
/// bridge publishes `expired re=<off> dl=<ms>` into the asker's inbox.
#[test]
fn a_deadline_that_passes_records_one_expired_and_an_answered_one_records_none() {
    let w = World::boot("r15dl", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // A short deadline and no reply; `--wait` returns the ask's offset.
    let asked = w.verb(&format!(
        "@{a} post to=@{b} kind=ask dl=200 --wait=60000 unanswered?"
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

    // A second ask, answered before its longer deadline passes.
    let answered = w.verb(&format!(
        "@{a} post to=@{b} kind=ask dl=60000 --wait=60000 answered?"
    ));
    assert!(answered.ok(), "second ask: {}", answered.header());
    let answered_off: u64 = answered
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("off="))
        .and_then(|n| n.parse().ok())
        .expect("the answered ask offset");
    let ans = w.verb(&format!(
        "@{b} post to=@{a} kind=answer re={answered_off} --wait=60000 yes"
    ));
    assert!(ans.ok(), "answer: {}", ans.header());

    // The asker's bridge checks the bus before recording `expired re=<ask_off>`, so the
    // bound is `PERIODIC_DEADLINE`, not the 200 ms deadline.
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

    // Exactly one, counted on the lane (the ring would dedup a second delivery on `off=`).
    // The sweep drops the entry on the tick it publishes, and a relaunched bridge reading a
    // stale durable table checks the bus first and finds this record.
    assert_eq!(
        lane_len(&w, &a, &w.node, "expired"),
        1,
        "exactly one expired record on the asker's lane, never a repeat"
    );

    // The answered ask never earns a verdict: no expired row names its offset.
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
/// With the `session-exited` line held back
/// (`ATERM_LINK_FAULT=drop-session-exited-while-marked`, the state a `GAP`, a late attach or
/// a lost push fd leaves):
///
/// * the re-post to the departed session is retired `unroutable` and puts nothing on the
///   bus;
/// * the retirement neither consumes nor re-points the key: a later re-post that routes
///   answers the first record's offset `dup=1` (the broker's dedup, not the address, makes
///   a key one record) and appends nothing to the new lane either.
#[test]
fn a_keyed_re_post_to_a_session_whose_exit_line_never_arrives_is_unroutable_and_the_key_holds() {
    let w = World::boot_with(
        "r15keygone",
        &[],
        &[("ATERM_LINK_FAULT", "drop-session-exited-while-marked")],
    );
    w.wait_ready();
    // Arm the fault before the session that will fall into it exists.
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

    // The fabric has advertised the peer, so a stale `live` row exists to lie.
    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the peer's presence row to say live", || {
        w.row(&presence)
            .filter(|body| body.contains("state=live"))
            .map(|_| ())
    });

    // The first post under the key lands: one record, its offset the key's.
    let (first, off) = w.post_landed(&a, &b, "note", "key=k-gone", "once only");
    assert!(!first.contains("dup=1"), "first post: {first}");
    assert_eq!(
        lane_len(&w, &b, &w.node, "note"),
        1,
        "one record after the first post"
    );

    // The peer goes, and the bridge is never told.
    assert!(w.verb(&format!("@{b} close")).ok(), "close the peer");
    until("the peer to leave the roster", || {
        (!w.sessions().iter().any(|(_, sid, _)| *sid == b)).then_some(())
    });

    // The re-post to the session that went: the door asks the endpoint, finds no such
    // session and retires it, with no `dup=1` off a stale route.
    let gone = w.verb(&format!(
        "@{a} post to=@{b} kind=note key=k-gone --wait=60000 once only"
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

    // The key still names its record: a re-post under it that routes answers the first
    // record, and the new address's lane gets nothing.
    let routed = w.verb(&format!(
        "@{a} post to=@{c} kind=note key=k-gone --wait=60000 once only"
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

/// A deduped answer to a sequence the bridge never sent is a collision, not a retry
/// (DF §6.2): nothing is remembered as the node's own, the connection drops so the
/// re-attach rolls the incarnation past the claimed range, and `ev cap-compromised …
/// reason=deduped` names the sequence and the other writer's offset.
///
/// Ordered, not raced: with the broker away the bridge reserves nothing, and the test,
/// holding the fleet root, claims the next incarnation's first sequence under the node's
/// producer id on a scratch socket. The broker then returns on its own path, and the
/// bridge's `live` row is that sequence's first send.
#[test]
fn a_deduped_first_send_is_a_collision_that_rolls_the_incarnation() {
    let mut w = World::boot("collide", &[]);
    w.wait_ready();
    let (a, b) = w.two_sessions();

    w.stop_broker();
    // The incarnation the bridge took (the `inc` file holds the top of the
    // incarnation LEASE, `aterm-link`'s `state.rs`): the reattach takes the next.
    let inc = aterm_link::state::StateDir::peek(&w.state).incarnation();
    assert!(inc >= 1, "the bridge attached under an incarnation");
    let claimed = astream_broker::incarnation_base(inc + 1).expect("within the layout") + 1;
    let scratch = w.tmp.join("claim.sock");
    let (claimed_off, deduped) = {
        let broker = astream_broker::Broker::open_guarded(&w.broker_log, harness::SECRET.to_vec())
            .expect("reopen the guarded broker");
        let mut handle = broker.serve(&scratch).expect("serve on a scratch socket");
        let mut c = harness::broker_client(&scratch);
        let god = astream_cap::mint(harness::SECRET, &format!("/f/{FLEET}/>")).expect("god cap");
        c.attach(&god.filter, &god.tag).expect("attach the god cap");
        let answer = c
            .publish(
                astream_cap::producer_id_of(&w.node),
                claimed,
                &format!("/f/{FLEET}/x/decoy"),
                b"v=1 t=1 text=decoy",
            )
            .expect("claim the sequence");
        drop(c);
        handle.shutdown();
        answer
    };
    assert!(!deduped, "the claimed sequence was already used");
    let _ = std::fs::remove_file(&scratch);
    w.start_broker();

    let alarm = format!("cap-compromised seq={claimed} off={claimed_off} reason=deduped");
    until("the collision's alarm", || {
        w.ev().contains(&alarm).then_some(())
    });
    until(
        "the bridge to reattach past the claimed incarnation",
        || {
            let body = w.row(&format!("/f/{FLEET}/pub/{}/node/presence", w.node))?;
            let now: u64 = harness::kv(&body, "inc")?.parse().ok()?;
            (body.contains("state=live") && now > inc + 1).then_some(())
        },
    );
    let acked = std::fs::read_to_string(w.state.join("acked")).unwrap_or_default();
    assert!(
        !acked.split(',').any(|o| o == claimed_off.to_string()),
        "the other writer's offset was remembered as the node's own: {acked}"
    );
    // The node still works under the new incarnation (`start_broker` returned on the
    // attach that took it).
    let posted = w.verb(&format!("@{a} post to=@{b} kind=note --wait=60000 rolled"));
    assert!(
        posted.ok(),
        "post: {} — log:\n{}",
        posted.header(),
        w.log_tail()
    );
    until("the post after the roll to land", || {
        (!matching(&w, &b, "text=rolled").is_empty()).then_some(())
    });
}
