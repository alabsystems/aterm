// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! One oversized post cannot halt a node's fleet or be lost.
//!
//! Three limits meet on one value: aterm's control server drops, with no reply, a
//! connection whose request line reaches 64 KiB; the endpoint takes a 256 KiB `post`
//! body; the broker's record ceiling is 16 MiB. So:
//!
//! 1. `deliver` cuts a body to fit the request line (`len=` names its size);
//! 2. a verb lane that closes produces `Item::Closed(Source::Aterm)`, so the bridge
//!    exits and its supervisor relaunches it;
//! 3. the inbox group cursor is committed only on what `deliver` answered.
//!
//! Otherwise aterm's `BridgeLostGuard` holds every session the bridge governs
//! under `reason=fabric-lost origin=fleet`. Every test here is over the REAL
//! binaries — a guarded broker, a headless `aterm`, and the bridge child
//! aterm launches itself — because each is a property of the seam rather than
//! of a function.

use crate::harness;

use harness::{FLEET, World, until};

/// A message far over the request-line bound is delivered, and the node lives.
///
/// 200 000 bytes: past aterm's 64 KiB request line, within the endpoint's `post len=`
/// frame. It arrives truncated with `len=` naming its true size (the field `deliver`
/// has for this, printed on every `msg` row), the cut is recorded as an `ev`, the
/// control connection survives, and no session is halted.
#[test]
fn a_body_over_the_request_line_bound_is_delivered_truncated_and_the_node_lives() {
    let w = World::boot("r1big", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // 200 000 bytes of prose; pct-encoding (a space is `%20`) puts it well over half
    // a megabyte on the wire.
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

    // The connection survived, and no session is halted.
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

    // And the cut is on the log. Polled: the `ev` is published after the endpoint
    // answers the `deliver`.
    until("the cut to be recorded on the log", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("truncated ") && e.contains(&format!("off={off}")))
    });
}

/// A `deliver` the endpoint never answered does not move the cursor, and the bridge
/// exits so its supervisor can bring it back.
///
/// `ATERM_LINK_FAULT=lose-aterm-before-deliver` closes the bridge's own verb
/// descriptor right before its first `deliver` (no other process can close one
/// inherited fd of another), one-shot across restarts (`fault-fired`). The group
/// cursor is not committed past the record, the process exits rather than drain into
/// a dead socket, and the replacement redelivers the record.
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

    // The fault fired: the verb lane is gone.
    until("the bridge to lose its verb lane", || {
        std::fs::read_to_string(w.state.join("fault-fired"))
            .ok()
            .map(|_| ())
    });
    // It exited, so the supervisor can relaunch it.
    until("the bridge child to exit", || {
        (!harness::alive(first)).then_some(())
    });
    // And came back, redelivering the record: the cursor stayed put, so the durable
    // group re-offered it.
    let row = until("the replacement to redeliver the record", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("text=r1survives"))
    });
    assert!(row.contains("from=h-sender"), "{row}");
    // Once, not twice: `deliver` is idempotent on `off=`, which is why leaving the
    // cursor put is safe.
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
