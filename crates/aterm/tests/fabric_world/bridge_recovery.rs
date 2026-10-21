// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **The recoveries the design names actually run.**
//!
//! §6.2's refill covers sessions an instance already hosts when the bridge
//! starts, not only ones it learns of later. The feed retry's budget is time,
//! not a count of scheduler rounds, so a keystroke refused cleanly for two
//! seconds is retried, not published as unknowable. §6.6 row 5's mirror is
//! withdrawn, and the lease renewal does not take the mirror the handoff table
//! refuses. A `control` claim moves the keyboard only once the row it travels
//! on can no longer be refused.
//!
//! Every assertion here is an `until` over something the fabric PUBLISHES, and
//! every induced condition is opened and closed from INSIDE the bridge under
//! `$ATERM_LINK_FAULT` — a marker file the test creates and removes — rather
//! than raced from outside.

use crate::harness;

use harness::{FLEET, World, until};

/// The `msg` row ids and texts a session's ring holds, `--peek` so nothing is
/// listed or marked by looking.
fn rows(w: &World, sid: &str) -> Vec<String> {
    w.inbox(sid)
}

/// **THE FLEET FACE RESUMES FROM ITS LAST RECORD, NOT FROM OFFSET ZERO.**
///
/// The fleet face is a last-value face: `on_fleet_record` keeps only
/// `fleet/h-*/halt`, and the standing verdict is the newest row per human. The
/// attach already reads exactly that with a `Last` walk and REBUILDS the halt
/// table from it (`read_fleet_halts` → `apply_fleet_halts`). Subscribing from
/// zero afterwards re-delivered every halt record ever published, to re-derive
/// the state that read had just derived.
///
/// HOW THIS TEST SEES IT — and the answer is worse than the wasted work.
///
/// The replay is not merely redundant, it is briefly WRONG. `on_fleet_record`
/// applies each halt as it arrives, so a from-zero resume walks the fleet's
/// history re-applying every barrier it ever carried: the first `state=on` in
/// the log HOLDS every session on this instance, and they stay held until the
/// replay reaches the matching `off`. `apply_fleet_halts` had already computed
/// the correct standing verdict from the `Last` read moments earlier, so the
/// hold this produces is one nothing on the bus justifies. Measured here: with
/// the from-zero subscribe restored, the assertion that fails FIRST is not the
/// ack count below but `hold=0` immediately after the restart — the session
/// was held, by a barrier that had been lifted three records ago.
///
/// The second assertion is the ack count. Deleting `halt-acked` leaves the
/// node where a halt published while its bridge was away leaves it: the newest
/// standing row is above the watermark. The attach answers THAT row, once —
/// the face resumes from the read's mark, so the tail never will — and a
/// from-zero resume would re-answer every barrier in the history besides. So
/// the ack face grows by exactly two: the standing row, then the fresh barrier.
///
/// It also pins the half that must NOT change: the standing verdict after the
/// restart is still the newest row's, and a halt published while the bridge
/// was away still arrives.
#[test]
fn the_fleet_face_resumes_from_its_last_record_and_does_not_re_answer_history() {
    let w = World::boot("r21fleetresume", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let mut god = w.god();
    let subject = format!("/f/{FLEET}/fleet/h-x/halt");
    let ack = format!("/f/{FLEET}/pub/{}/node/ack", w.node);

    // A HISTORY WORTH REPLAYING: four barriers, ending lifted.
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
    // WAIT FOR THE NODE TO ANSWER THE LAST BARRIER BEFORE TAKING THE BASELINE.
    // `on_fleet_record` applies the halt and THEN publishes the ack, so
    // `hold=0` becomes visible one publish before the ack does; a baseline read
    // in that gap undercounts by one and the comparison below then blames the
    // resume for an ack the bridge was always going to write. Measured as a
    // flake before this wait was added.
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

    // AND FOR ITS WATERMARK. `ack_halt` writes `halt-acked` only once the
    // ack's publish has returned, so the ack is on the bus a moment before the
    // file says so; a removal in that gap is undone by the old bridge, and the
    // restart then answers nothing (measured as a flake under a loaded suite).
    until("the node to record the last barrier as answered", || {
        let have = std::fs::read_to_string(w.state.join("halt-acked")).ok()?;
        (have.trim().parse::<u64>().ok()? >= last_barrier).then_some(())
    });

    // REMOVE THE SUPPRESSOR. Nothing else changes; the node now stands where a
    // halt published while its bridge was away leaves it, its newest standing
    // row above what it has answered, so the attach answers that row once.
    std::fs::remove_file(w.state.join("halt-acked")).expect("drop the halt-acked watermark");

    w.restart_bridge();

    // NO SESSION IS HELD BY A BARRIER THAT WAS LIFTED. This is the assertion
    // the from-zero resume actually fails: the replay re-applies the history's
    // first `state=on` and the session is held until the walk reaches the `off`
    // that lifted it. Read immediately after the attach, with no settling
    // wait, because the window is the defect.
    assert!(
        w.verb(&format!("@{a} status")).header().contains("hold=0"),
        "a lifted halt must not be re-applied by a reconnect replaying the \
         fleet's history — log:\n{}",
        w.log_tail()
    );

    // A BARRIER PUBLISHED AFTER THE SNAPSHOT STILL ARRIVES: the mark is a
    // splice, not a truncation.
    let (fresh, _) = god
        .publish(5_000, seq + 1, &subject, b"v=1 t=1 state=on reason=after")
        .expect("publish after the restart");
    until("the fresh barrier to hold the fleet", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=1")
            .then_some(())
    });

    // AND THE ACK FACE GREW BY EXACTLY TWO, in this order: the attach answered
    // the standing row before its fleet subscription opened, and the tail
    // answered the fresh barrier. Both are publishes on the one publisher
    // connection, so the order is the log's.
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

/// **§6.2's REFILL RUNS FOR A SESSION THAT ALREADY EXISTED.**
///
/// `Bridge::refill` had exactly two triggers and both iterate the sids
/// `refresh_sessions` NEWLY inserted — and `Bridge::run` opened by calling
/// `refresh_sessions` into `let _`, so by the time either trigger could fire,
/// every session the instance hosts had already been inserted and neither one
/// ever named it again. aterm's `sessions` push stream reports a
/// `session-created` only for a session that appears after the subscription, so
/// no event arrived for them either. The refill's own doc (`cmd_deliver`, in
/// `fabric.rs`) promises re-delivery "on every `session-created` and on the
/// roster tick"; the roster half was unreachable for precisely the sessions it
/// was written for — a seamless update preserves the sid, the endpoint's ring
/// starts empty, and the durable group cursor is already committed past every
/// row.
///
/// THE ROW THIS RECOVERS IS ONE THE ENDPOINT NEVER TOOK. A 65th message from
/// one peer is refused `ERR quota` at `deliver` — accounted for, so the group
/// cursor commits straight past it — while the session's HANDLED watermark
/// stays at the 64th. That is the state the refill exists to repair: a row on
/// the log, above `seen`, that no live subscription will ever offer again.
///
/// AND THE REFILL RE-OFFERS ROWS THE RING ALREADY HOLDS, safely. The handled
/// watermark is put at the 32nd row, so the restart's refill walks rows 33 to
/// 65: thirty-two the ring holds, then the one it never took. A refill
/// delivers in offset order, so the 65th's arrival is the barrier — every held
/// row has been re-offered before it — and each still appears once, because
/// `deliver` is idempotent on the bus `off=`.
#[test]
fn a_session_that_existed_when_the_bridge_started_is_still_refilled() {
    let w = World::boot("r3refill", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // SENDER_QUOTA is 64 UNLISTED rows per peer; the 65th is refused.
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

    // LIST them (which is what frees the per-peer quota: it counts UNLISTED
    // rows) and HANDLE the 32nd, which is what moves the watermark the refill
    // resumes from.
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

    // The bridge restarts against the same state dir, and `s-a` is a session
    // that EXISTS at its first roster read — the case with no trigger.
    w.restart_bridge();

    until("the refill to re-offer the row the quota refused", || {
        rows(&w, &a)
            .iter()
            .any(|r| r.ends_with("text=row65"))
            .then_some(())
    });
    // AND EXACTLY ONCE, the re-offered rows 33..64 among them. `deliver` is
    // idempotent on `off=`, which is what makes running the refill
    // unconditionally safe.
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

/// **A POST THAT DIES AT THE DOOR TAKES ITS DURABLE RESERVATION WITH IT — AND
/// AN EXITED SESSION IS NOT A ROUTE.**
///
/// Two findings meet on one path. `drain_outbox` reserves a durable producer
/// sequence in the `Route::To` arm and clears it only when the endpoint accepts
/// the retirement; the `route =>` arm retires the post and never cleared
/// anything, so a post that was resolvable on one drain and unroutable on the
/// next left `sent/<sid>.<id>` behind for the life of the node — no reader, no
/// sweep, and no operator undo but deleting files by hand.
///
/// Getting there needs the address to STOP resolving between two drains, and
/// that is the other finding: `advertisers` took the roster map's keys and threw
/// the `state=` away, while `holder_is_live` — reading the same rows — answered
/// on `state == "live"`. A session whose only row is the `state=exited`
/// withdrawal its own node published was therefore not live and WAS the single
/// routing candidate, so the post was published to a lane whose node answers
/// `NotHosted`, and a permanent TOFU pin was written for a session that no
/// longer exists.
#[test]
fn a_post_that_becomes_unroutable_leaves_no_reservation_and_no_pin() {
    let w = World::boot_with(
        "r3outbox",
        &[],
        &[("ATERM_LINK_FAULT", "fail-post-publish-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();

    // THE PUBLISH FAILS, WHICH IS WHAT LEAVES THE RESERVATION ON DISK. The
    // drain returns and keeps the file, correctly — the retry needs it.
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

    // THE ADDRESS STOPS RESOLVING: the peer session exits, and its node's own
    // roster row says so.
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

/// **A POST TO A SESSION WHOSE `session-exited` LINE NEVER ARRIVES IS STILL
/// RETIRED AT THE DOOR.**
///
/// The sibling above stages the same defect through a RACE — the window between
/// aterm dropping a session and the bridge taking `session-exited` off its
/// mailbox — and a race needs a loaded machine to lose. This one holds that
/// window open for as long as the assertion takes, with
/// `ATERM_LINK_FAULT=drop-session-exited-while-marked`, so the property is
/// asserted rather than sampled.
///
/// THE LINE GENUINELY GOES MISSING. §4.2's `GAP` is aterm saying frames were
/// coalesced away and a line "may simply not exist any more"; a bridge that
/// attaches after the exit never had it; either half of the verb/push pair can
/// be lost while the process lives. What every one of those leaves behind is a
/// bridge whose picture of which sessions exist is older than the endpoint's,
/// and the door is the one consumer of that picture that can LOSE A MESSAGE: it
/// routed the post to the departed session's own `in` face and answered its
/// sender `off=<n>` — landed — for a face nothing will ever fetch from.
///
/// Both halves of the repair are asserted here, because either alone still
/// delivers the post: `epochs` must be re-derived before the address is
/// resolved (or the door routes it as hosted), and the departure must withdraw
/// the presence row in the same breath (or `advertisers` still names this node
/// as the single claimant, writes a TOFU pin for a sid that no longer exists,
/// and routes it anyway).
#[test]
fn a_post_to_a_session_whose_exit_line_never_arrives_is_still_retired() {
    let w = World::boot_with(
        "r3noexit",
        &[],
        &[("ATERM_LINK_FAULT", "drop-session-exited-while-marked")],
    );
    w.wait_ready();
    // OPEN THE WINDOW BEFORE THE SESSION THAT WILL FALL INTO IT EXISTS.
    std::fs::write(w.state.join("drop-session-exited"), b"1\n").expect("arm the dropped line");
    let (a, b) = w.two_sessions();

    // THE FABRIC HAS ADVERTISED THE PEER. Without this the test could not fail:
    // a sid with no `live` row anywhere is unroutable for the ordinary reason,
    // and the defect is precisely that a STALE `live` row keeps answering.
    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the peer's presence row to say live", || {
        w.row(&presence)
            .filter(|body| body.contains("state=live"))
            .map(|_| ())
    });

    // THE PEER GOES, and the bridge is never told.
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
    // AND NOTHING WAS PUT ON THE DEAD SESSION'S FACE. The `undeliverable` row is
    // the verdict; this is the loss it exists to prevent.
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
