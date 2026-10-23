// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Oversized fabric post truncation and bridge recovery integration tests.

use crate::harness;

use harness::{FLEET, World, until};

#[test]
fn a_body_over_the_request_line_bound_is_delivered_truncated_and_the_node_lives() {
    let w = World::boot("r1big", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

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

    until("the cut to be recorded on the log", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("truncated ") && e.contains(&format!("off={off}")))
    });
}

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

    until("the bridge to lose its verb lane", || {
        std::fs::read_to_string(w.state.join("fault-fired"))
            .ok()
            .map(|_| ())
    });
    until("the bridge child to exit", || {
        (!harness::alive(first)).then_some(())
    });
    let row = until("the replacement to redeliver the record", || {
        w.inbox(&a)
            .into_iter()
            .find(|r| r.contains("text=r1survives"))
    });
    assert!(row.contains("from=h-sender"), "{row}");
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
