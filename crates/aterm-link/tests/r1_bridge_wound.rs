// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 1 — the bridge's own wound, and the holes beside it.**
//!
//! One post from any principal that could address a node used to be a PERMANENT
//! remote drive-halt of that node's whole fleet, with the message committed away
//! and lost. Three separate defects made one failure:
//!
//! 1. `deliver` built ONE control-request line carrying the whole body, and
//!    aterm's control server drops — with no reply — any connection whose
//!    request line reaches 64 KiB. The endpoint meanwhile takes a 256 KiB `post`
//!    body and the broker's record ceiling is 16 MiB: three components, three
//!    implicit limits for one value, and no place where they met.
//! 2. Nothing produced `Item::Closed(Source::Aterm)` for the VERB lane, so the
//!    bridge never noticed its own connection dying, never exited, and its
//!    supervisor never relaunched it.
//! 3. The inbox group cursor was committed whatever `deliver` answered, so the
//!    record that killed the lane was also lost.
//!
//! Meanwhile aterm's `BridgeLostGuard` had already held every session the bridge
//! governed under `reason=fabric-lost origin=fleet`. Every test here is over the
//! REAL binaries — a guarded broker, a headless `aterm-gui`, and the bridge child
//! aterm launches itself — because every one of the three is a property of the
//! seam rather than of a function.

mod harness;

use harness::{until, World, FLEET};

/// **A message far over the request-line bound is delivered, and the node lives.**
///
/// The body is 200 000 bytes — comfortably past aterm's 64 KiB request line, and
/// exactly the size the endpoint's own `post len=` frame accepts, which is how
/// the two sides used to disagree. It arrives TRUNCATED with `len=` naming its
/// true size (the field `deliver`'s grammar has for precisely this, and which
/// every `msg` row prints), the cut is recorded as an `ev`, the control
/// connection survives, and no session is halted.
///
/// Before the fix this line was written verbatim: aterm dropped the connection
/// at byte 65 536 without answering, the bridge stayed alive with a dead verb
/// lane, and every session on the instance answered `ERR halted
/// reason=fabric-lost` forever.
#[test]
fn a_body_over_the_request_line_bound_is_delivered_truncated_and_the_node_lives() {
    let w = World::boot("r1big", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // 200 000 bytes of ordinary prose. pct-encoding makes it worse than it
    // looks: a space is `%20`, so this is well over half a megabyte on the wire.
    let text: String = "the quick brown fox ".repeat(10_000);
    assert_eq!(text.len(), 200_000);
    let subject = format!("/f/{FLEET}/in/{}/{a}/h-sender/note", w.node);
    let body = format!("v=1 t=1 text={}", aterm_link::pct::encode(&text));
    let (off, _) = w
        .god()
        .publish(6_100, 1, &subject, body.as_bytes())
        .expect("publish the oversized note");

    let row = until("the oversized note to land", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("from=h-sender"))
    });
    assert!(
        row.contains(&format!(" len={} ", text.len())),
        "the row must name the body's TRUE length, so the reader can see the \
         cut: {row}"
    );

    // THE CONNECTION SURVIVED, which is the whole point. A dropped bridge lane
    // is aterm's fail-closed halt over every session it governs.
    assert!(
        w.verb("status").header().contains("fabric=connected"),
        "the bridge's control connection must survive an oversized body: {}",
        w.log_tail()
    );
    let send = w.verb(&format!("@{a} send hi"));
    assert!(
        send.ok(),
        "no session may be halted by one oversized message: {}",
        send.header()
    );

    // AND THE CUT IS ON THE LOG, not only in the row's arithmetic. Polled,
    // because the `ev` is published AFTER the endpoint answers the `deliver`:
    // the row is readable from the endpoint strictly before the record naming
    // the cut is readable from the bus, so a single read here is a race.
    until("the cut to be recorded on the log", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("truncated ") && e.contains(&format!("off={off}")))
    });
}

/// **A `deliver` the endpoint never answered does not move the cursor, and the
/// bridge exits so its supervisor can bring it back.**
///
/// `ATERM_LINK_FAULT=lose-aterm-before-deliver` closes the bridge's own VERB
/// descriptor immediately before its first `deliver` — the one failure that
/// cannot be raced from outside, because no other process can close one
/// inherited fd of another. It is one-shot across restarts (`fault-fired` in the
/// state dir), so the replacement bridge runs clean.
///
/// Three claims in one run, and A3 failed all three: the group cursor is NOT
/// committed past a record the endpoint never took; the process EXITS rather
/// than draining into a dead socket forever; and the record is redelivered by
/// the replacement, so nothing is lost.
#[test]
fn a_deliver_that_never_reached_aterm_is_not_committed_and_the_bridge_relaunches() {
    let w = World::boot_with(
        "r1lose",
        &[],
        &[("ATERM_LINK_FAULT", "lose-aterm-before-deliver")],
    );
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let first = w.bridge_pid();

    let subject = format!("/f/{FLEET}/in/{}/{a}/h-sender/task", w.node);
    w.god()
        .publish(6_200, 1, &subject, b"v=1 t=1 text=r1survives")
        .expect("publish the task");

    // THE FAULT FIRED — the verb lane is gone.
    until("the bridge to lose its verb lane", || {
        std::fs::read_to_string(w.state.join("fault-fired"))
            .ok()
            .map(|_| ())
    });
    // IT EXITED. Before the fix the child stayed alive with a dead verb lane,
    // `child.wait()` never returned, and no relaunch was ever attempted.
    until("the bridge child to exit", || {
        (!harness::alive(first)).then_some(())
    });
    // AND CAME BACK, and the record it never delivered is delivered now: the
    // cursor was left where it was, so the durable group re-offered it.
    let row = until("the replacement to redeliver the record", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("text=r1survives"))
    });
    assert!(row.contains("from=h-sender"), "{row}");
    // ONCE, not twice: `deliver` is idempotent on `off=`, which is the whole
    // reason leaving the cursor put is safe.
    assert_eq!(
        w.inbox(&a)
            .iter()
            .filter(|r| r.contains("text=r1survives"))
            .count(),
        1
    );
    until("the replacement to lift the fail-closed hold", || {
        w.verb(&format!("@{a} status"))
            .header()
            .contains("hold=0")
            .then_some(())
    });
}

/// **A transiently-refused `term/in` never leaves the journal without a verdict.**
///
/// The feed journal is ONE slot, and the retry path deliberately KEEPS an entry
/// across records: a non-final verdict — `ERR busy` while a local turn holds the
/// write-block lease, `ERR halted`, `ERR rate`, an aterm that did not answer —
/// publishes no `ev` so the record can be asked again. A3 then wrote the next
/// drive record's intent straight over it, and the first keystroke was never
/// fed, never retried and never reported: no `applied`, no `refused`, no
/// `in-doubt`, nothing anywhere.
///
/// `ATERM_LINK_FAULT=refuse-first-feed` answers the first feed `ERR busy turn=0`
/// — the exact string a live endpoint gives while a turn holds the lease —
/// without asking aterm. The state itself is REACHABLE, and the fault is here
/// for determinism rather than for reach: claim-first is exclusive (a `turn`
/// started under the bridge's live cooperative lease is refused `ERR busy
/// lease=`), but turn-first is not — the human's `control` claim still lands,
/// `acquire_lease` records the refused lease as an `ev lease-refused` instead
/// of failing the handoff, and every keystroke the bridge then feeds is refused
/// for as long as the turn runs. Everything after the reply is the shipped path.
#[test]
fn a_transiently_refused_keystroke_is_never_erased_without_a_verdict() {
    let w = World::boot_with("r1feed", &[], &[("ATERM_LINK_FAULT", "refuse-first-feed")]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();
    let epoch = until("the session's launch nonce", || {
        w.sessions()
            .into_iter()
            .find(|(_, sid, _)| *sid == a)
            .map(|(_, _, nonce)| nonce)
    });

    // The human takes the keyboard. With an EMPTY `--accept-from` — the
    // documented default — which is its own regression: see
    // `a_humans_claim_moves_the_keyboard_on_a_default_configuration`.
    let mut god = w.god();
    let control = format!("/f/{FLEET}/in/{}/{a}/h-andrew/control", w.node);
    god.publish(
        6_300,
        1,
        &control,
        format!("v=1 t=1 epoch={epoch} text=claim").as_bytes(),
    )
    .expect("publish the claim");
    let row = format!("/f/{FLEET}/pub/{}/{a}/control", w.node);
    until("the claim to stand on the bus", || {
        let (rows, _) = w.god().last(&row, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == row)
            .filter(|(_, _, b)| String::from_utf8_lossy(b).contains("holder=h-andrew"))
            .map(|_| ())
    });

    // KEYSTROKE ONE — refused transiently, so it stays in the journal with no
    // `ev`. The journal file is the observable: nothing is published yet, on
    // purpose.
    let drive = format!("/f/{FLEET}/term/{}/{a}/in/h-andrew", w.node);
    let one = format!("v=1 t=1 epoch={epoch} len=3\n").into_bytes();
    let mut first_body = one.clone();
    first_body.extend_from_slice(b"aa\r");
    let (first, _) = god
        .publish(6_301, 2, &drive, &first_body)
        .expect("publish the first keystroke");
    until("the first keystroke to be journalled and refused", || {
        std::fs::read_to_string(w.state.join("feeding"))
            .ok()
            .filter(|j| j.contains(&format!("off={first}")))
            .map(|_| ())
    });

    // KEYSTROKE TWO — the record that used to erase it.
    let mut second_body = one;
    second_body.extend_from_slice(b"bb\r");
    let (second, _) = god
        .publish(6_302, 3, &drive, &second_body)
        .expect("publish the second keystroke");

    // NEITHER OFFSET LEAVES WITHOUT A VERDICT. The first's is `in-doubt` —
    // honest, because the bridge does not know whether those bytes reached the
    // PTY — and the second's is whatever it earned.
    let ev = until("both keystrokes to be accounted for", || {
        let ev = w.ev();
        let has = |off: u64| {
            ev.iter()
                .any(|e| e.contains(&format!("re={off}")) && e.contains("face=term"))
        };
        (has(first) && has(second)).then_some(ev)
    });
    let verdict = ev
        .iter()
        .find(|e| e.contains(&format!("re={first}")))
        .expect("the first keystroke's verdict");
    assert!(
        verdict.starts_with("in-doubt ") || verdict.starts_with("applied "),
        "a transiently-refused keystroke is reported, never erased: {verdict}"
    );
}
