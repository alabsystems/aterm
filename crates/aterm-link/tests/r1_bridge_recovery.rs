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

/// The last-value `control` row for one session, as a string.
fn control_row(w: &World, sid: &str) -> Option<String> {
    let subject = format!("/f/{FLEET}/pub/{}/{sid}/control", w.node);
    let mut c = w.god();
    let (rows, _) = c.last(&subject, "", 8).ok()?;
    rows.into_iter()
        .find(|(_, s, _)| *s == subject)
        .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
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

/// **A KEYSTROKE REFUSED CLEANLY FOR LONGER THAN A SCHEDULER ROUND IS STILL
/// THERE WHEN THE REFUSAL LIFTS.**
///
/// `FEED_TRIES_MAX` was 8, and 8 meant fourteen seconds while the retry ran on
/// the idle roster round. Round 2 moved the retry onto its own 250 ms deadline —
/// correctly — and the same 8 attempts silently became 1.75 s, with both
/// constants' docs still saying "long enough for a hold or a lease to clear".
/// Every transient verdict `Verdict::is_final` names outlives that by design:
/// `LEASE_TTL_MS` is 30 s, an `ERR busy` is a whole `turn`, an `ERR halted` is
/// lifted by a human. Exhausting the budget publishes `in-doubt` and CLEARS the
/// journal, and §6.5 makes in-doubt terminal — so the keystroke was gone, and
/// the fleet log said its fate was unknown when the bridge held eight
/// unambiguous refusals saying it had not been typed.
///
/// The window is closed on the bridge's OWN progress: `tries=` in the journal,
/// waited past the old count, so this test cannot pass by being slow.
#[test]
fn a_transient_refusal_outlives_the_old_count_and_the_keystroke_still_lands() {
    let w = World::boot_with(
        "r3feed",
        &[],
        &[("ATERM_LINK_FAULT", "refuse-feeds-while-marked")],
    );
    let marker = w.state.join("refuse-feeds");
    std::fs::write(&marker, b"1\n").expect("arm the refusal window");
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let epoch = until("the session's launch nonce", || {
        w.sessions()
            .into_iter()
            .find(|(_, sid, _)| *sid == a)
            .map(|(_, _, nonce)| nonce)
    });

    let mut god = w.god();
    let control = format!("/f/{FLEET}/in/{}/{a}/h-andrew/control", w.node);
    god.publish(
        9_200,
        1,
        &control,
        format!("v=1 t=1 epoch={epoch} text=claim").as_bytes(),
    )
    .expect("publish the claim");
    until("the claim to stand on the bus", || {
        control_row(&w, &a)
            .filter(|r| r.contains("holder=h-andrew"))
            .map(|_| ())
    });

    let drive = format!("/f/{FLEET}/term/{}/{a}/in/h-andrew", w.node);
    let mut body = format!("v=1 t=1 epoch={epoch} len=3\n").into_bytes();
    body.extend_from_slice(b"hi\r");
    let (off, _) = god.publish(9_201, 2, &drive, &body).expect("publish a key");

    // PAST THE OLD BUDGET, MEASURED THE WAY THE BRIDGE MEASURES IT. Eight was
    // the whole budget; twelve journalled attempts is unambiguously past it, and
    // every one of them was a clean pre-write refusal.
    until(
        "the bridge to retry past the old eight-attempt budget",
        || {
            let journal = std::fs::read_to_string(w.state.join("feeding")).ok()?;
            let tries: u32 = journal
                .split_whitespace()
                .find_map(|t| t.strip_prefix("tries="))?
                .parse()
                .ok()?;
            (journal.contains(&format!("off={off}")) && tries >= 12).then_some(())
        },
    );
    // Nothing has been published about it yet: a transient refusal is not a
    // verdict, and the journal is the only record of it.
    assert!(
        !w.ev()
            .iter()
            .any(|e| e.contains(&format!("re={off}")) && e.contains("face=term")),
        "a keystroke still being retried has no verdict yet"
    );

    std::fs::remove_file(&marker).expect("lift the refusal");
    let verdict = until(
        "the keystroke to reach the PTY once the refusal lifts",
        || {
            w.ev()
                .into_iter()
                .find(|e| e.contains(&format!("re={off}")) && e.contains("face=term"))
        },
    );
    assert!(
        verdict.starts_with("applied "),
        "a keystroke refused before any byte moved is not in doubt, and it is not \
         lost: {verdict}"
    );
}

/// **§6.6 ROW 5's MIRROR IS NEVER TAKEN OVER BY THE BRIDGE, AND IT IS WITHDRAWN
/// WHEN THE LEASE IT MIRRORS ENDS.**
///
/// `apply_handoff` refuses to take aterm's cooperative lease for a mirrored
/// `owner-cli:` holder and says why: re-acquiring it under a `fabric:` name is
/// refused every sample and turns one local `lease acquire` into a stream of
/// durable `ev lease-refused` records. That rule was written at ONE of four call
/// sites; `renew_leases` took the same lease every ten seconds, and the moment
/// the local driver's TTL lapsed it SUCCEEDED — leaving aterm reporting
/// `driving=lease:fabric:owner-cli:drv-7` forever, the real driver's reconnect
/// refused `ERR lease held`, and `lease release force` the only escape.
///
/// The other half of the same hole: nothing withdrew the mirror. Once
/// `holders[sid]` named a mirror it was never the no-holder arm again, so a
/// driver that exited left a bus row naming it as the holder for the life of
/// the node.
#[test]
fn a_mirrored_local_lease_is_neither_taken_over_nor_left_standing() {
    let w = World::boot("r3mirror", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // A LOCAL SOCKET DRIVER takes aterm's own cooperative lease, briefly.
    let took = w.verb(&format!("@{a} lease acquire holder=drv-7 ttl=2000"));
    assert!(took.ok(), "lease acquire: {}", took.header());
    until("the fabric to mirror the local driver", || {
        control_row(&w, &a)
            .filter(|r| r.contains("holder=owner-cli:drv-7"))
            .map(|_| ())
    });

    // THE MIRROR IS WITHDRAWN when the lease it mirrors lapses — through §6.6's
    // own table, as the holder's own release.
    until(
        "the mirror to be withdrawn once the local lease lapses",
        || {
            control_row(&w, &a)
                .filter(|r| r.contains("holder=-"))
                .map(|_| ())
        },
    );
    // AND THE BRIDGE NEVER HELD IT. `local_lease_holder` filters `fabric:`
    // holders out, so a bridge that had taken this lease could not even see
    // itself holding it; aterm can.
    let status = w.verb(&format!("@{a} lease status"));
    assert!(
        !status.header().contains("fabric:owner-cli:"),
        "the bridge must never hold a mirror of a lease it does not own: {}",
        status.header()
    );

    // AND NOT ONE `ev lease-refused` FOR A MIRRORED HOLDER, across a bridge
    // restart — `restore_control_rows` re-takes every restored row's lease on
    // every attach, and it read the same rule from the same wrong place.
    let long = w.verb(&format!("@{a} lease acquire holder=drv-8 ttl=60000"));
    assert!(long.ok(), "lease acquire: {}", long.header());
    until("the second driver to be mirrored", || {
        control_row(&w, &a)
            .filter(|r| r.contains("holder=owner-cli:drv-8"))
            .map(|_| ())
    });
    restart_bridge(&w);
    until("the restored row to still name the local driver", || {
        control_row(&w, &a)
            .filter(|r| r.contains("holder=owner-cli:drv-8"))
            .map(|_| ())
    });
    let refused: Vec<String> = w
        .ev()
        .into_iter()
        .filter(|e| e.starts_with("lease-refused") && e.contains("owner-cli:"))
        .collect();
    assert!(
        refused.is_empty(),
        "a mirrored lease is not a lease this bridge takes, so it can never be \
         refused one: {refused:#?}"
    );
}

/// **A `control` CLAIM WHOSE ROW WAS REFUSED DOES NOT MOVE THE KEYBOARD.**
///
/// `deliver_record` ran `on_control_message` before the `deliver` line was even
/// built, and every refusal below it returns `Accounted` — so the group cursor
/// commits past the record and no bridge offers it again. The handover then
/// happened INSTEAD of the delivery, permanently: the human was told
/// `state=refused` on their own lane for a claim that had just taken the
/// keyboard, and the agent whose wheel moved saw no row at all.
#[test]
fn a_control_claim_whose_row_is_refused_leaves_the_keyboard_alone() {
    let w = World::boot("r3ctlq", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let epoch = until("the session's launch nonce", || {
        w.sessions()
            .into_iter()
            .find(|(_, sid, _)| *sid == a)
            .map(|(_, _, nonce)| nonce)
    });

    // FILL h-andrew's PER-PEER QUOTA, so their next row — whatever kind it is —
    // is refused at `deliver`.
    let mut god = w.god();
    let lane = format!("/f/{FLEET}/in/{}/{a}/h-andrew/note", w.node);
    for seq in 1..=64u64 {
        god.publish(
            9_300,
            seq,
            &lane,
            format!("v=1 t=1 text=fill{seq}").as_bytes(),
        )
        .expect("publish");
    }
    until("the ring to fill to the sender's quota", || {
        (rows(&w, &a).len() >= 64).then_some(())
    });

    let control = format!("/f/{FLEET}/in/{}/{a}/h-andrew/control", w.node);
    let (claim, _) = god
        .publish(
            9_300,
            65,
            &control,
            format!("v=1 t=1 epoch={epoch} text=claim").as_bytes(),
        )
        .expect("publish the claim");
    until("the claim to be refused for quota", || {
        w.ev()
            .into_iter()
            .find(|e| {
                e.starts_with("undeliverable ")
                    && e.contains(&format!("off={claim}"))
                    && e.contains("reason=quota")
            })
            .map(|_| ())
    });

    // THE ROW WAS NOT DELIVERED, SO THE KEYBOARD DID NOT MOVE. The verdict the
    // sender was given and the state of the session now agree — and the check
    // is not a race: the `undeliverable` above is published AFTER the endpoint
    // refused, while the handoff this guards against ran BEFORE the line was
    // even built.
    let row = control_row(&w, &a);
    assert!(
        !row.as_deref().unwrap_or("").contains("holder=h-andrew"),
        "a refused claim must not take the keyboard: {row:?}"
    );
    // NOR ANY OF THE HANDOFF'S OTHER EFFECTS. `on_control_message` is a
    // mutation, not a classification: it also takes aterm's own cooperative
    // lease for the claimant, which a local driver and a person at the glass
    // both see in `who`.
    let lease = w.verb(&format!("@{a} lease status"));
    assert!(
        !lease.header().contains("fabric:h-andrew"),
        "a refused claim must not take the mirror lease either: {}",
        lease.header()
    );
}

/// **AN UNREADABLE `status` IS NOT A REVISION OF ZERO.**
///
/// `status_sample` answers `None` when the verb cannot be read, and both callers
/// folded that into the value `0` — which lives in the same range as a real
/// `revision=`. One failed read therefore installed a baseline of
/// `revision = 0, seen = true`, the next SUCCESSFUL read was an advance from
/// zero, and one settled second later §6.6 row 4 parked the session at `human?`,
/// told the holder they had lost the wheel and refused their every `term/in`
/// `reason=holder` — on a session nobody had touched.
///
/// `baseline_local` is called on every `Decision::Hold`, so the window is the
/// exact instant a human claims a session.
#[test]
fn an_unreadable_status_does_not_park_a_session_nobody_touched() {
    let w = World::boot_with(
        "r3status",
        &[],
        &[("ATERM_LINK_FAULT", "fail-status-while-marked")],
    );
    w.wait_ready();
    // ARMED BEFORE THE SESSION EXISTS, so BOTH callers meet the `None`: the
    // sampler's first-ever sighting of this session (`observe_local_control`)
    // and the claim's re-baseline (`baseline_local`). Each one folded the failed
    // read into `revision = 0` on its own.
    let marker = w.state.join("fail-status");
    std::fs::write(&marker, b"1\n").expect("arm the failing status");
    let (_first, a) = w.two_sessions();
    let epoch = until("the session's launch nonce", || {
        w.sessions()
            .into_iter()
            .find(|(_, sid, _)| *sid == a)
            .map(|(_, _, nonce)| nonce)
    });
    // THE TEST IS ONLY MEANINGFUL IF THE SESSION HAS A REVISION TO LOSE: the
    // damage is that a real reading is replaced by zero.
    let revision: u64 = w
        .verb(&format!("@{a} status"))
        .header()
        .split_whitespace()
        .find_map(|t| t.strip_prefix("revision="))
        .and_then(|v| v.parse().ok())
        .expect("status carries a revision");
    assert!(revision > 0, "a booted session has already been classified");
    let mut god = w.god();
    let control = format!("/f/{FLEET}/in/{}/{a}/h-andrew/control", w.node);
    god.publish(
        9_400,
        1,
        &control,
        format!("v=1 t=1 epoch={epoch} text=claim").as_bytes(),
    )
    .expect("publish the claim");
    until("the claim to stand on the bus", || {
        control_row(&w, &a)
            .filter(|r| r.contains("holder=h-andrew"))
            .map(|_| ())
    });
    // THE WINDOW IS HELD OPEN FOR ONE `SETTLE_QUIET`, because that is what the
    // rule the zero corrupts is made of: an advance counts only once the
    // session has been seen QUIET for a second. This is the duration of an
    // induced fault, not a wait for a race — nothing here is being synchronised
    // with, and the assertions below are all `until`s over published state.
    std::thread::sleep(Duration::from_millis(2_000));
    std::fs::remove_file(&marker).expect("let `status` be read again");

    // ROW 4'S WHOLE INPUT HERE IS THE BASELINE, and its verdict lands within
    // one `SETTLE_QUIET` (1 s) of the first successful sample at `LOCAL_OBSERVE`
    // (250 ms). This window is those two periods with an order of magnitude of
    // margin — the pre-fix park is published inside 1.5 s of the marker coming
    // off, reproducibly, because the damage is a stored zero and not a race.
    let settle = std::time::Instant::now() + Duration::from_secs(6);
    while std::time::Instant::now() < settle {
        let row = control_row(&w, &a).unwrap_or_default();
        assert!(
            !row.contains("holder=human?"),
            "a status that could not be read is not a change, and must not park a \
             session nobody touched: {row}"
        );
        std::thread::sleep(Duration::from_millis(100));
    }

    // AND THE WHEEL IS STILL THEIRS, proved the only way that matters. A parked
    // row refuses this `reason=holder`; a held one applies it.
    let before = w
        .ev()
        .into_iter()
        .filter(|e| e.contains("face=term"))
        .count();
    let drive = format!("/f/{FLEET}/term/{}/{a}/in/h-andrew", w.node);
    let mut body = format!("v=1 t=1 epoch={epoch} len=1\n").into_bytes();
    body.extend_from_slice(b"\r");
    god.publish(9_401, 2, &drive, &body).expect("publish a key");
    let verdict = until("the keystroke's verdict", || {
        w.ev()
            .into_iter()
            .filter(|e| e.contains("face=term"))
            .nth(before)
    });
    assert!(
        verdict.starts_with("applied "),
        "the human still holds the keyboard: {verdict}"
    );
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

    let verdict = until("the post to be retired at the door", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("undeliverable ") && e.contains(&format!("to=@{b}")))
    });
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
