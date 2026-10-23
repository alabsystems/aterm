// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **The recoveries the design names actually run.**

use crate::harness;

use harness::{FLEET, World, until};

/// A session's `msg` rows, `--peek` so looking marks nothing.
fn rows(w: &World, sid: &str) -> Vec<String> {
    w.inbox(sid)
}

/// The fleet face resumes from its last record, not from offset zero.
#[test]
fn the_fleet_face_resumes_from_its_last_record_and_does_not_re_answer_history() {
    let w = World::boot("r21fleetresume", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/fleet/h-x/halt");
    let ack = format!("/f/{FLEET}/pub/{}/node/ack", w.node);

    let mut seq = 0;
    let mut offsets: Vec<u64> = Vec::new();
    for (state, want_hold) in [("on", true), ("off", false), ("on", true), ("off", false)] {
        seq += 1;
        let (o, _) = god
            .publish(
                5_000,
                seq,
                &subject,
                format!("v=1 t=1 state={state} reason=stop").as_bytes(),
            )
            .expect("publish the barrier");
        offsets.push(o);
        let want = if want_hold { "hold=1" } else { "hold=0" };
        until(&format!("the fleet to reach {want}"), || {
            w.verb(&format!("@{a} status"))
                .header()
                .contains(want)
                .then_some(())
        });
    }

    let ack_bodies = |w: &World| -> Vec<String> {
        let (rows, _) = w
            .read(|c| c.fetch(0, &ack, 256))
            .expect("fetch the ack face");
        rows.iter()
            .map(|(o, _, b)| format!("@{o} {}", String::from_utf8_lossy(b)))
            .collect()
    };
    let last_barrier = *offsets.last().expect("four barriers were published");
    until("the node to answer the last barrier", || {
        ack_bodies(&w)
            .iter()
            .any(|b| b.contains(&format!("re={last_barrier}")))
            .then_some(())
    });
    let before_bodies = ack_bodies(&w);
    let acks_before = before_bodies.len();
    assert!(
        acks_before >= 2,
        "the node answered the barriers while it was up: {before_bodies:#?}"
    );

    until("the node to record the last barrier as answered", || {
        let have = std::fs::read_to_string(w.state.join("halt-acked")).ok()?;
        (have.trim().parse::<u64>().ok()? >= last_barrier).then_some(())
    });

    std::fs::remove_file(w.state.join("halt-acked")).expect("drop the halt-acked watermark");

    w.restart_bridge();

    assert!(
        w.verb(&format!("@{a} status")).header().contains("hold=0"),
        "a lifted halt must not be re-applied by a reconnect replaying the \
         fleet's history — log:\n{}",
        w.log_tail()
    );

    let (fresh, _) = god
        .publish(5_000, seq + 1, &subject, b"v=1 t=1 state=on reason=after")
        .expect("publish after the restart");
    until("the fresh barrier to hold the fleet", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=1")
            .then_some(())
    });

    until("the node to answer the fresh barrier", || {
        ack_bodies(&w)
            .iter()
            .any(|b| b.contains(&format!("re={fresh} state=held")))
            .then_some(())
    });
    let after_bodies = ack_bodies(&w);
    let answered: Vec<&str> = after_bodies[acks_before..]
        .iter()
        .map(|b| b.split_once(' ').map_or("", |(_, body)| body))
        .collect();
    assert!(
        answered.len() == 2
            && answered[0].contains(&format!("re={last_barrier} state=ready"))
            && answered[1].contains(&format!("re={fresh} state=held")),
        "the attach answers the standing row once and the tail the fresh barrier; \
         more is a replay from offset 0\nbefore: {before_bodies:#?}\nafter: \
         {after_bodies:#?}\nbarrier offsets: {offsets:?}"
    );
}

/// §6.2's refill runs for a session that already existed when the bridge started.
#[test]
fn a_session_that_existed_when_the_bridge_started_is_still_refilled() {
    let w = World::boot("r3refill", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let lane = format!("/f/{FLEET}/in/{}/{a}/h-andrew/note", w.node);
    let rows_65: Vec<harness::Record> = (1..=65u64)
        .map(|seq| {
            let body = format!("v=1 t=1 text=row{seq}").into_bytes();
            (9_100, seq, lane.clone(), body)
        })
        .collect();
    harness::publish_pipelined(&mut w.god(), &rows_65);
    until("the ring to fill to the sender's quota", || {
        (rows(&w, &a).len() >= 64).then_some(())
    });
    let refused = until("the 65th to be refused for quota", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("undeliverable ") && e.contains("reason=quota"))
    });
    assert!(
        !rows(&w, &a).iter().any(|r| r.ends_with("text=row65")),
        "the quota refusal means the endpoint never took the row: {refused}"
    );

    assert!(w.verb(&format!("@{a} inbox")).ok(), "list the ring");
    let mid_id = rows(&w, &a)
        .iter()
        .find(|r| r.ends_with("text=row32"))
        .and_then(|r| r.split_whitespace().nth(1).map(str::to_string))
        .expect("the 32nd row's id");
    assert!(w.verb(&format!("@{a} inbox seen {mid_id} handled")).ok());
    until("the bridge to persist the watermark", || {
        std::fs::read_to_string(w.state.join("seen").join(&a))
            .ok()
            .map(|_| ())
    });

    w.restart_bridge();

    until("the refill to re-offer the row the quota refused", || {
        rows(&w, &a)
            .iter()
            .any(|r| r.ends_with("text=row65"))
            .then_some(())
    });
    let held = rows(&w, &a);
    for n in [1u64, 32, 33, 48, 64, 65] {
        assert_eq!(
            held.iter()
                .filter(|r| r.ends_with(&format!("text=row{n}")))
                .count(),
            1,
            "row{n} must appear exactly once: {held:#?}"
        );
    }
}

/// A post that becomes unroutable takes its durable reservation with it, and an
/// exited session is not a route.
#[test]
fn a_post_that_becomes_unroutable_leaves_no_reservation_and_no_pin() {
    let w = World::boot_with(
        "r3outbox",
        &[],
        &[("ATERM_LINK_FAULT", "fail-post-publish-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();

    let marker = w.state.join("fail-post-publish");
    std::fs::write(&marker, b"*\n").expect("arm the failing publish");
    let posted = w.verb(&format!("@{a} post to=@{b} kind=note hello"));
    assert!(posted.ok(), "post: {}", posted.header());
    let id = posted
        .header()
        .split_whitespace()
        .nth(1)
        .expect("post answers OK <id>")
        .to_string();
    let reservation = w.state.join("sent").join(format!("{a}.{id}"));
    until("the outbound sequence to be reserved durably", || {
        reservation.exists().then_some(())
    });

    assert!(w.verb(&format!("@{b} close")).ok(), "close the peer");
    until("the peer to leave the roster", || {
        (!w.sessions().iter().any(|(_, sid, _)| *sid == b)).then_some(())
    });
    std::fs::remove_file(&marker).expect("let the publish through");

    w.drained(&a);
    let verdict = until(
        "the post to be retired at the door (the first drain after the close)",
        || {
            w.ev()
                .into_iter()
                .find(|e| e.starts_with("undeliverable ") && e.contains(&format!("to=@{b}")))
        },
    );
    assert!(
        verdict.contains("reason=unroutable"),
        "a session whose only roster row says `state=exited` is not a route: {verdict}"
    );
    assert!(
        !reservation.exists(),
        "a retired post's sequence reservation is forgotten on BOTH arms: {}",
        reservation.display()
    );
    let pins = std::fs::read_to_string(w.state.join("pins")).unwrap_or_default();
    assert!(
        !pins.contains(&b),
        "no TOFU pin is written for a session that no longer exists: {pins:?}"
    );
}

/// A post to a session whose `session-exited` line never arrives is still retired at
/// the door.
#[test]
fn a_post_to_a_session_whose_exit_line_never_arrives_is_still_retired() {
    let w = World::boot_with(
        "r3noexit",
        &[],
        &[("ATERM_LINK_FAULT", "drop-session-exited-while-marked")],
    );
    w.wait_ready();
    std::fs::write(w.state.join("drop-session-exited"), b"1\n").expect("arm the dropped line");
    let (a, b) = w.two_sessions();

    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the peer's presence row to say live", || {
        w.row(&presence)
            .filter(|body| body.contains("state=live"))
            .map(|_| ())
    });

    assert!(w.verb(&format!("@{b} close")).ok(), "close the peer");
    until("the peer to leave the roster", || {
        (!w.sessions().iter().any(|(_, sid, _)| *sid == b)).then_some(())
    });

    let posted = w.verb(&format!("@{a} post to=@{b} kind=note hello"));
    assert!(posted.ok(), "post: {}", posted.header());

    let verdict = until(
        "the post to be retired at the door with no exit line to go on",
        || {
            w.ev()
                .into_iter()
                .find(|e| e.starts_with("undeliverable ") && e.contains(&format!("to=@{b}")))
        },
    );
    assert!(
        verdict.contains("reason=unroutable"),
        "a session aterm's own roster no longer lists is not a route: {verdict}"
    );
    let inbound = format!("/f/{FLEET}/in/{}/{b}/{}/note", w.node, w.node);
    let mut c = w.god();
    let landed = c.last(&inbound, "", 8).map_or(0, |(rows, _)| rows.len());
    assert_eq!(
        landed, 0,
        "a post acked as landed on the `in` face of a session that no longer exists: {inbound}"
    );
    let pins = std::fs::read_to_string(w.state.join("pins")).unwrap_or_default();
    assert!(
        !pins.contains(&b),
        "no TOFU pin is written for a session that no longer exists: {pins:?}"
    );
}

/// **A POST OR RECEIPT THE BROKER REFUSES FOR GOOD IS RETIRED, AND THE QUEUE
/// BEHIND IT LANDS.**
#[test]
fn a_record_the_broker_refuses_for_good_is_retired_and_the_queue_behind_it_lands() {
    let w = World::boot_with_grants("r3forgood", &|node| {
        let any_kind = format!("rw,p={node}:/f/{FLEET}/in/*/*/{node}/*");
        harness::node_grants(node)
            .into_iter()
            .map(|g| {
                if g == any_kind {
                    format!("rw,p={node}:/f/{FLEET}/in/*/*/{node}/ask")
                } else {
                    g
                }
            })
            .collect()
    });
    w.wait_ready();
    let (a, b) = w.two_sessions();

    let note = w.verb(&format!("@{a} post to=@{b} kind=note refused-for-good"));
    assert!(note.ok(), "post the note: {}", note.header());
    let ask = w.verb(&format!(
        "@{a} post to=@{b} kind=ask --wait=60000 behind-it"
    ));
    assert!(
        ask.ok(),
        "the ask behind a refused note lands: {} — log:\n{}",
        ask.header(),
        w.log_tail()
    );
    let ask_off = harness::off_of(ask.header());
    let verdict = until("the refused note to be recorded on the node's ev", || {
        w.ev().into_iter().find(|e| {
            e.starts_with("undeliverable ")
                && e.contains(&format!("to=@{b}"))
                && e.contains("reason=refused")
        })
    });
    assert!(verdict.contains("reason=refused"), "{verdict}");

    let again = w.verb(&format!(
        "@{a} post to=@{b} kind=note --wait=60000 refused-again"
    ));
    assert!(
        again.header().starts_with("ERR refused id="),
        "a post refused for good wakes its wait with that verdict: {}",
        again.header()
    );

    let id = until("the ask to reach B", || harness::id_at_off(&w, &b, ask_off));
    let seen = w.verb(&format!("@{b} inbox seen {id} handled"));
    assert!(seen.ok(), "inbox seen {id} handled: {}", seen.header());
    let unacked = until("the refused receipt to be retired on B's ev", || {
        w.ev().into_iter().find(|e| {
            e.starts_with(&format!("unacked re={ask_off} "))
                && e.contains("verdict=handled")
                && e.contains("reason=refused")
        })
    });
    assert!(unacked.contains("reason=refused"), "{unacked}");

    let later = w.verb(&format!(
        "@{a} post to=@{b} kind=ask --wait=60000 still-moving"
    ));
    assert!(
        later.ok(),
        "a post after a refused one and a refused receipt lands: {}",
        later.header()
    );
}

/// **A RECORD THE BROKER REFUSES FOR NOW HOLDS BACK ONLY ITS OWN SESSION.**
#[test]
fn a_record_refused_for_now_holds_back_only_its_own_session() {
    let w = World::boot_with(
        "r3fornow",
        &[],
        &[("ATERM_LINK_FAULT", "fail-post-publish-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();

    let (_, first_off) = w.post_landed(&a, &b, "ask", "", "before");

    let marker = w.state.join("fail-post-publish");
    std::fs::write(&marker, format!("{a}\n")).expect("refuse A's publishes");
    let held = w.verb(&format!("@{a} post to=@{b} kind=note held-back"));
    assert!(held.ok(), "A's note is queued: {}", held.header());
    let held_id = held
        .header()
        .split_whitespace()
        .nth(1)
        .expect("post answers OK <id>")
        .to_string();

    let (_, ask_off) = w.post_landed(&b, &a, "ask", "", "not-held");
    let in_flight = w.verb(&format!("@{a} inbox --peek"));
    assert!(
        in_flight
            .rows()
            .iter()
            .any(|r| r.starts_with(&format!("post {held_id} ")) && r.contains(" off=- ")),
        "A's refused note is still queued: {:?}",
        in_flight.rows()
    );

    let id = until("B's ask to reach A", || harness::id_at_off(&w, &a, ask_off));
    let seen = w.verb(&format!("@{a} inbox seen {id} handled"));
    assert!(seen.ok(), "A marks B's ask: {}", seen.header());
    let id = until("A's ask to reach B", || {
        harness::id_at_off(&w, &b, first_off)
    });
    let seen = w.verb(&format!("@{b} inbox seen {id} handled"));
    assert!(seen.ok(), "B marks A's ask: {}", seen.header());
    until("B's receipt to land while A's is refused", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with(&format!("ack re={first_off} ")))
    });
    assert!(
        !w.ev()
            .iter()
            .any(|e| e.starts_with(&format!("ack re={ask_off} "))),
        "A's receipt is refused while the marker names A"
    );
    w.post_landed(&b, &a, "note", "", "behind-a-refused-receipt");

    std::fs::remove_file(&marker).expect("let A's publishes through");
    let after = w.verb(&format!("@{a} post to=@{b} kind=note --wait=60000 after"));
    assert!(
        after.ok(),
        "A's queue moves once the refusal lifts: {}",
        after.header()
    );
    until("A's receipt to land once the refusal lifts", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with(&format!("ack re={ask_off} ")))
    });
}
