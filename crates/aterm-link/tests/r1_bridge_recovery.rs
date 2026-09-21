// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 3 — the recoveries that were written down and did not run.**
//!
//! Four of this round's findings are the same shape: a repair the design names,
//! whose code path exists, whose trigger cannot fire. §6.2's refill runs only
//! for sessions the bridge learns about AFTER it starts, and every session an
//! instance already hosts is exactly the case the refill was written for. The
//! feed retry's budget was a COUNT of scheduler rounds whose period moved under
//! it, so a keystroke refused cleanly for two seconds was published as
//! unknowable and lost. §6.6 row 5's mirror had no withdrawal, and the lease
//! renewal took the mirror the handoff table deliberately refuses to take. And a
//! `control` claim moved the keyboard before the row it travelled on could be
//! refused.
//!
//! Every assertion here is an `until` over something the fabric PUBLISHES, and
//! every induced condition is opened and closed from INSIDE the bridge under
//! `$ATERM_LINK_FAULT` — a marker file the test creates and removes — rather
//! than raced from outside.

mod harness;

use std::time::Duration;

use harness::{until, World, FLEET};

/// The `msg` row ids and texts a session's ring holds, `--peek` so nothing is
/// listed or marked by looking.
fn rows(w: &World, sid: &str) -> Vec<String> {
    w.inbox(sid)
}

/// Wait for the bridge to die and its supervisor to bring a replacement up.
fn restart_bridge(w: &World) {
    let pid = w.bridge_pid();
    harness::kill(pid, 9);
    until("the bridge to come back under a new pid", || {
        let back = std::fs::read_to_string(w.state.join("pid")).ok()?;
        let back: i32 = back.trim().parse().ok()?;
        (back != pid).then_some(())
    });
    until("the replacement bridge to attach", || {
        w.verb("status")
            .header()
            .contains("fabric=connected")
            .then_some(())
    });
}

/// **THE FLEET FACE RESUMES FROM ITS LAST RECORD, NOT FROM OFFSET ZERO.**
///
/// The fleet face is a last-value face: `on_fleet_record` keeps only
/// `fleet/h-*/halt`, and the standing verdict is the newest row per human. The
/// attach already reads exactly that with a `Last` walk and REBUILDS the halt
/// table from it (`read_fleet_halts` → `apply_fleet_halts`). Subscribing from
/// zero afterwards then re-delivered every halt record ever published, to
/// re-derive the state that read had just derived — and `state.rs` says what
/// it cost in as many words, on the field that exists only to paper over it:
///
/// > The fleet face resubscribes from offset 0 on every reconnect, so without
/// > this the node re-answers every halt in the fleet's history at every
/// > reconnect.
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
/// The second assertion is the cheaper symptom. `halt-acked` exists only to
/// stop the node re-ANSWERING those replayed barriers; delete it and every one
/// of them earns a fresh record on the node's retained `ack` subject. A bridge
/// that resumes at the mark its own `Last` read returned is never offered them
/// at all, so the ack face does not move.
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
        let mut c = w.god();
        let (rows, _) = c.fetch(0, &ack, 256).expect("fetch the ack face");
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

    // REMOVE THE SUPPRESSOR. Nothing else changes; this only stops the node
    // from recognising barriers it has already answered, which is precisely
    // what a face that does not re-offer them does not need.
    std::fs::remove_file(w.state.join("halt-acked")).expect("drop the halt-acked watermark");

    restart_bridge(&w);

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

    // AND NOTHING WAS RE-ANSWERED. Give a replay every chance to appear first.
    std::thread::sleep(Duration::from_millis(750));
    let after_bodies = ack_bodies(&w);
    let acks_after = after_bodies.len();
    assert_eq!(
        acks_after,
        acks_before,
        "the fleet face re-answered {} barrier(s) it had already answered — it is \
         replaying the fleet's history from offset 0\nbefore: {before_bodies:#?}\nafter: \
         {after_bodies:#?}\nbarrier offsets: {offsets:?}",
        acks_after - acks_before
    );

    // A BARRIER PUBLISHED AFTER THE SNAPSHOT STILL ARRIVES: the mark is a
    // splice, not a truncation.
    god.publish(5_000, seq + 1, &subject, b"v=1 t=1 state=on reason=after")
        .expect("publish after the restart");
    until("the fresh barrier to hold the fleet", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=1")
            .then_some(())
    });
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
#[test]
fn a_session_that_existed_when_the_bridge_started_is_still_refilled() {
    let w = World::boot("r3refill", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // SENDER_QUOTA is 64 UNLISTED rows per peer; the 65th is refused.
    let mut god = w.god();
    let lane = format!("/f/{FLEET}/in/{}/{a}/h-andrew/note", w.node);
    for seq in 1..=65u64 {
        god.publish(
            9_100,
            seq,
            &lane,
            format!("v=1 t=1 text=row{seq}").as_bytes(),
        )
        .expect("publish");
    }
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
    // rows) and HANDLE the last, which is what moves the watermark the refill
    // resumes from.
    assert!(w.verb(&format!("@{a} inbox")).ok(), "list the ring");
    let last_id = rows(&w, &a)
        .last()
        .and_then(|r| r.split_whitespace().nth(1).map(str::to_string))
        .expect("a row id");
    assert!(w.verb(&format!("@{a} inbox seen {last_id} handled")).ok());
    until("the bridge to persist the watermark", || {
        std::fs::read_to_string(w.state.join("seen").join(&a))
            .ok()
            .map(|_| ())
    });

    // The bridge restarts against the same state dir, and `s-a` is a session
    // that EXISTS at its first roster read — the case with no trigger.
    restart_bridge(&w);

    until("the refill to re-offer the row the quota refused", || {
        rows(&w, &a)
            .iter()
            .any(|r| r.ends_with("text=row65"))
            .then_some(())
    });
    // AND EXACTLY ONCE. `deliver` is idempotent on `off=`, which is what makes
    // running the refill unconditionally safe.
    let held = rows(&w, &a);
    for n in [1u64, 32, 64, 65] {
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
    std::fs::write(&marker, b"1\n").expect("arm the failing publish");
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
    // the bridge remembered — the idle arm's 250 ms, or sooner.
    //
    // `a_post_to_a_session_whose_exit_line_never_arrives_is_still_retired`
    // pins the same door without needing a loaded machine.
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
        let mut c = w.god();
        let (rows, _) = c.last(&presence, "", 8).ok()?;
        rows.iter()
            .find(|(_, s, _)| *s == presence)
            .filter(|(_, _, body)| String::from_utf8_lossy(body).contains("state=live"))
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
