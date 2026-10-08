// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **One oversized post cannot halt a node's fleet or be lost.**
//!
//! Three limits meet on one value: aterm's control server drops, with no
//! reply, a connection whose request line reaches 64 KiB; the endpoint takes a
//! 256 KiB `post` body; the broker's record ceiling is 16 MiB. So:
//!
//! 1. `deliver` cuts a body to fit the request line (`len=` names its size);
//! 2. a VERB lane that closes produces `Item::Closed(Source::Aterm)`, so the
//!    bridge exits and its supervisor relaunches it;
//! 3. the inbox group cursor is committed only on what `deliver` answered.
//!
//! Otherwise aterm's `BridgeLostGuard` holds every session the bridge governs
//! under `reason=fabric-lost origin=fleet`. Every test here is over the REAL
//! binaries — a guarded broker, a headless `aterm-gui`, and the bridge child
//! aterm launches itself — because each is a property of the seam rather than
//! of a function.

use crate::harness;

use harness::{until, World, FLEET};

/// **A message far over the request-line bound is delivered, and the node lives.**
///
/// The body is 200 000 bytes — comfortably past aterm's 64 KiB request line, and
/// exactly the size the endpoint's own `post len=` frame accepts. It arrives TRUNCATED with `len=` naming its
/// true size (the field `deliver`'s grammar has for precisely this, and which
/// every `msg` row prints), the cut is recorded as an `ev`, the control
/// connection survives, and no session is halted.
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
    // IT EXITED, so the supervisor can relaunch it.
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
