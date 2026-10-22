// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **The recoveries the design names actually run.**
//!
//! Every assertion is an `until` over something the fabric publishes, and every
//! induced condition is opened and closed from inside the bridge under
//! `$ATERM_LINK_FAULT` (a marker file the test creates and removes).

use crate::harness;

use harness::{FLEET, World, until};

/// A session's `msg` rows, `--peek` so looking marks nothing.
fn rows(w: &World, sid: &str) -> Vec<String> {
    w.inbox(sid)
}

/// The fleet face resumes from its last record, not from offset zero.
///
/// The attach rebuilds the halt table from a `Last` walk (`read_fleet_halts` →
/// `apply_fleet_halts`); a from-zero subscription afterwards would re-apply every
/// halt in the history, holding every session from the first `state=on` until the
/// replay reached its `off`. With `halt-acked` deleted, the node stands where a halt
/// published while its bridge was away leaves it: the attach answers the standing
/// row once, so the ack face grows by exactly two (that row, then a fresh barrier).
#[test]
fn the_fleet_face_resumes_from_its_last_record_and_does_not_re_answer_history() {
    let w = World::boot("r21fleetresume", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/fleet/h-x/halt");
    let ack = format!("/f/{FLEET}/pub/{}/node/ack", w.node);

    // A history worth replaying: four barriers, ending lifted.
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
    // Wait for the node to answer the last barrier before the baseline:
    // `on_fleet_record` applies the halt, then publishes the ack, so `hold=0` shows
    // one publish early.
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

    // And for its watermark: `ack_halt` writes `halt-acked` only once the ack's
    // publish returned, and a removal in that gap would be undone by the old bridge.
    until("the node to record the last barrier as answered", || {
        let have = std::fs::read_to_string(w.state.join("halt-acked")).ok()?;
        (have.trim().parse::<u64>().ok()? >= last_barrier).then_some(())
    });

    // Remove the suppressor: the newest standing row is now above what the node has
    // answered, so the attach answers it once.
    std::fs::remove_file(w.state.join("halt-acked")).expect("drop the halt-acked watermark");

    w.restart_bridge();

    // No session is held by a lifted barrier. Read right after the attach, with no
    // settling wait: the window is the defect.
    assert!(
        w.verb(&format!("@{a} status")).header().contains("hold=0"),
        "a lifted halt must not be re-applied by a reconnect replaying the \
         fleet's history — log:\n{}",
        w.log_tail()
    );

    // A barrier published after the snapshot still arrives: the mark is a splice,
    // not a truncation.
    let (fresh, _) = god
        .publish(5_000, seq + 1, &subject, b"v=1 t=1 state=on reason=after")
        .expect("publish after the restart");
    until("the fresh barrier to hold the fleet", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=1")
            .then_some(())
    });

    // And the ack face grew by exactly two, in log order on the one publisher
    // connection: the attach answered the standing row before its fleet subscription
    // opened, then the tail answered the fresh barrier.
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
///
/// The refill must cover the sessions the first roster read inserts: aterm's
/// `sessions` push reports `session-created` only for sessions that appear later,
/// and after a seamless update the sid is kept, the endpoint's ring starts empty, and
/// the group cursor is already past every row. The row recovered here is one the
/// endpoint never took: a 65th message from one peer, refused `ERR quota` (accounted
/// for, so the cursor commits past it) while the handled watermark stays below. With
/// the watermark at the 32nd row the refill walks rows 33 to 65 in offset order, so
/// the 65th's arrival is the barrier, and each row still appears once (`deliver` is
/// idempotent on `off=`).
#[test]
fn a_session_that_existed_when_the_bridge_started_is_still_refilled() {
    let w = World::boot("r3refill", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // `SENDER_QUOTA` is 64 unlisted rows per peer; the 65th is refused.
    let lane = format!("/f/{FLEET}/in/{}/{a}/h-andrew/note", w.node);
    // Pipelined (`harness::publish_pipelined`): the same 65 in order; nothing
    // lists the ring while they land, so the quota refuses the same 65th.
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

    // List them (which frees the per-peer quota, counted over unlisted rows) and
    // handle the 32nd, moving the watermark the refill resumes from.
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

    // The bridge restarts against the same state dir, and the session exists at its
    // first roster read.
    w.restart_bridge();

    until("the refill to re-offer the row the quota refused", || {
        rows(&w, &a)
            .iter()
            .any(|r| r.ends_with("text=row65"))
            .then_some(())
    });
    // Exactly once, the re-offered rows 33..64 among them: `deliver` is idempotent
    // on `off=`.
    let held = rows(&w, &a);
    for n in [1u64, 32, 33, 48, 64, 65] {
        // `ends_with`, not `contains`: `text=row1` is a prefix of `text=row10`.
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
///
/// `drain_outbox` reserves a durable producer sequence when a post resolves; a post
/// retired as unroutable on a later drain must clear `sent/<sid>.<id>` too, or it
/// stays for the life of the node. A session whose only roster row is its node's
/// `state=exited` withdrawal is not a routing candidate, so nothing is published to a
/// lane that answers `NotHosted` and no TOFU pin is written for it.
#[test]
fn a_post_that_becomes_unroutable_leaves_no_reservation_and_no_pin() {
    let w = World::boot_with(
        "r3outbox",
        &[],
        &[("ATERM_LINK_FAULT", "fail-post-publish-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // The publish fails, leaving the reservation on disk for the retry.
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

    // The address stops resolving: the peer exits, and its node's roster row says so.
    assert!(w.verb(&format!("@{b} close")).ok(), "close the peer");
    until("the peer to leave the roster", || {
        (!w.sessions().iter().any(|(_, sid, _)| *sid == b)).then_some(())
    });
    std::fs::remove_file(&marker).expect("let the publish through");

    // AND THE NEXT DRAIN RETIRES IT — [`DEADLINE`]'s ordinary budget, because
    // this is an ordinary wait again.
    //
    // IT WAS NOT, AND THE HISTORY IS THE POINT. This wait was moved to
    // `until_within(PERIODIC_DEADLINE, …)` on 2026-09-13, on the argument that
    // the retirement is period-driven and 60 s was simply not enough of a slow
    // machine's clock. It timed out at 180 s on 2026-09-14, and a bound that has
    // to grow twice is a bound standing in for a defect: the failure was never
    // slowness. The drain resolved `@<sid>` from the bridge's CACHED roster, so
    // in the window between aterm dropping the session and the bridge taking
    // `session-exited` off its own mailbox the post was PUBLISHED to a dead
    // session's `in` face and acked to its sender as landed — after which no
    // verdict exists to wait for and no budget is long enough (measured: 2 of 16
    // concurrent runs under load, the losing drain 67 ms ahead of the line).
    // `Bridge::drain_outbox` now re-reads the roster before it resolves
    // anything, so the first drain after the close answers `unroutable` whatever
    // the bridge remembered.
    //
    // THAT DRAIN IS PROMPTED, not waited for: a post of A's own wakes it, and
    // A's queue drains in order, so the barrier landing says the refused post
    // ahead of it was resolved again first. Left to the roster round it was a
    // two-second wait for the same drain.
    //
    // `a_post_to_a_session_whose_exit_line_never_arrives_is_still_retired`
    // pins the same door without needing a loaded machine.
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
///
/// `ATERM_LINK_FAULT=drop-session-exited-while-marked` holds open the window between
/// aterm dropping a session and the bridge hearing of it (a §4.2 `GAP`, an attach
/// after the exit, a lost lane half). Both halves of the repair are needed: `epochs`
/// is re-derived before the address is resolved (or the door routes it as hosted),
/// and the departure withdraws the presence row at once (or `advertisers` still names
/// this node, pins the sid and routes the post to a face nothing will fetch from).
#[test]
fn a_post_to_a_session_whose_exit_line_never_arrives_is_still_retired() {
    let w = World::boot_with(
        "r3noexit",
        &[],
        &[("ATERM_LINK_FAULT", "drop-session-exited-while-marked")],
    );
    w.wait_ready();
    // Open the window before the session that will fall into it exists.
    std::fs::write(w.state.join("drop-session-exited"), b"1\n").expect("arm the dropped line");
    let (a, b) = w.two_sessions();

    // The fabric has advertised the peer: a sid with no `live` row is unroutable for
    // the ordinary reason, and the defect is a stale `live` row answering.
    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the peer's presence row to say live", || {
        w.row(&presence)
            .filter(|body| body.contains("state=live"))
            .map(|_| ())
    });

    // The peer goes, and the bridge is never told.
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
    // And nothing was put on the dead session's face: the loss the verdict prevents.
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
///
/// The node's ring grants its `in` lanes for `ask` alone, so a guarded broker
/// refuses a `note` — and every `ack` receipt — as `ERR_UNAUTHORIZED`, a
/// refusal astream calls permanent: sent again unchanged, it is refused again.
/// The drain used to `return` at the first post that failed and leave it at
/// the head of the queue, where every drain retried it while `fabric=` read
/// connected, and every later post of that session — and of every session after
/// it in drain order — never left. Now the note is retired `reason=refused` (a
/// `post --wait` on one answers `ERR refused`, the node's `ev` says
/// `undeliverable … reason=refused`), the ask queued behind it lands, and a
/// refused receipt is retired `unacked … reason=refused` on the deciding
/// session's `ev` instead of staying owed for ever.
///
/// The refusal is the broker's own; no fault is injected, so it is classified
/// by the code a real guarded broker sends.
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

    // A NOTE THE BROKER WILL NEVER TAKE, queued ahead of an ask from the same
    // sender. The note does not wait; the ask waits for its landing, which is
    // the whole claim — before, it sat behind the note until its wait ran out.
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

    // A WAITING POST THE BROKER REFUSES answers its own verdict, not a timeout.
    let again = w.verb(&format!(
        "@{a} post to=@{b} kind=note --wait=60000 refused-again"
    ));
    assert!(
        again.header().starts_with("ERR refused id="),
        "a post refused for good wakes its wait with that verdict: {}",
        again.header()
    );

    // AND A RECEIPT: B decides the ask, and its `ack` lane is not granted.
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

    // THE QUEUE STILL MOVES: a second ask from A lands after all of it.
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
///
/// A refusal astream does not call permanent — `ERR_REFUSED`: a new subject
/// past the node producer's distinct-subject budget (another producer may
/// introduce that subject, and the identical publish then lands), a failed
/// append, every refusal of an older broker — leaves the post or receipt
/// queued under its pinned sequence, to be retried. It used to leave the drain
/// with it: the posts `return`ed at the first one that failed and the receipts
/// `break`ed, so the boot session, first in drain order, held every other
/// session's mail for as long as its own record was refused.
///
/// The staged refusal (`fail-post-publish` naming A) is the broker's own
/// `ERR_REFUSED`, in place of A's publishes only. B's ask, B's receipt for A's
/// earlier ask, and a note B posts once A's receipt is refused land while A's
/// note and A's receipt are refused; once the refusal lifts, A's queue moves,
/// in order. Negative control: a drain that ends at A's refused receipt
/// (`drain_outbox`'s `stalled.insert` -> `return`) never reaches B's note,
/// since the receipts drain before every post. A drain that ends at A's
/// refused NOTE is not seen here: the endpoint lists the least recently listed
/// session first (`cmd_outbox`), so B leads the next drain and lands anyway.
#[test]
fn a_record_refused_for_now_holds_back_only_its_own_session() {
    let w = World::boot_with(
        "r3fornow",
        &[],
        &[("ATERM_LINK_FAULT", "fail-post-publish-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // A's ASK LANDS FIRST, so B owes A a receipt once the refusal is armed.
    let (_, first_off) = w.post_landed(&a, &b, "ask", "", "before");

    // THE REFUSAL, for A's records alone.
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

    // B IS NOT HELD BEHIND A: its ask lands while A's note is refused (waited
    // for by the hang detector, never a latency bound).
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

    // AND THE RECEIPTS: A decides B's ask (refused, so it stays owed) and B
    // decides A's (which lands).
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
    // AND B'S MAIL STILL MOVES BEHIND A's REFUSED RECEIPT: every drain now
    // lists that receipt, before any post.
    w.post_landed(&b, &a, "note", "", "behind-a-refused-receipt");

    // THE REFUSAL LIFTS, and A's queue moves: its receipt lands, and a waiting
    // post lands behind the note that was held.
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
