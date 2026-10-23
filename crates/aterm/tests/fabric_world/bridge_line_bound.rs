// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests that oversized delivery lines earn verdicts without losing records or looping.

use crate::harness;

use harness::{FLEET, World, until};

#[test]
fn an_oversized_via_earns_a_verdict_and_the_ordinary_note_behind_it_lands() {
    let w = World::boot("r2via", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let hop = format!("n-{}", "a".repeat(32));
    let chain = vec![hop; 2_000].join(",");
    assert!(
        chain.len() > 64 * 1024,
        "the chain must be past the request-line bound: {}",
        chain.len()
    );
    let subject = format!("/f/{FLEET}/in/{}/{a}/h-sender/note", w.node);
    let poison = format!("v=1 t=1 via={chain} text=hi");
    let (poison_off, _) = w
        .god()
        .publish(6_200, 1, &subject, poison.as_bytes())
        .expect("publish the poison record");
    let (_, _) = w
        .god()
        .publish(6_200, 2, &subject, b"v=1 t=1 text=second")
        .expect("publish the ordinary note behind it");

    until("the ordinary note behind the poison one to land", || {
        w.inbox(&a).into_iter().find(|r| r.contains("text=second"))
    });

    let verdict = until("the verdict for the refused record", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("undeliverable ") && e.contains(&format!("off={poison_off}")))
    });
    assert!(
        verdict.contains("reason=via"),
        "the reason must name the field that could not be sent: {verdict}"
    );

    let lane = format!("/f/{FLEET}/in/p/h-sender/{}/undeliverable", w.node);
    until("the sender's own verdict row", || {
        let (rows, _) = w.read(|c| c.fetch(0, &lane, 64)).ok()?;
        rows.into_iter().find(|(_, _, body)| {
            let body = String::from_utf8_lossy(body);
            body.contains(&format!("re={poison_off}")) && body.contains("not%20delivered:%20via")
        })
    });

    assert!(
        !w.inbox(&a).iter().any(|r| r.contains("text=hi")),
        "a record the bridge refused must not also be delivered"
    );

    assert!(
        w.verb("status").header().contains("fabric=connected"),
        "the bridge's control connection must survive an oversized field: {}",
        w.log_tail()
    );
}

#[test]
fn a_deliver_line_over_the_bound_is_a_verdict_not_a_transport_failure() {
    let w = World::boot_with(
        "r2oversize",
        &[],
        &[("ATERM_LINK_FAULT", "oversize-deliver-line")],
    );
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    let subject = format!("/f/{FLEET}/in/{}/{a}/h-sender/note", w.node);
    let (off, _) = w
        .god()
        .publish(6_300, 1, &subject, b"v=1 t=1 text=padded")
        .expect("publish the record whose line is padded");

    let verdict = until("the verdict for the unsendable line", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("undeliverable ") && e.contains(&format!("off={off}")))
    });
    assert!(
        verdict.contains("reason=oversize"),
        "an unsendable line is refused as oversize, not filed as a lost lane: {verdict}"
    );

    w.god()
        .publish(6_300, 2, &subject, b"v=1 t=1 text=after")
        .expect("publish the record behind it");
    until("the record behind the padded one to land", || {
        w.inbox(&a).into_iter().find(|r| r.contains("text=after"))
    });
    assert!(
        w.verb("status").header().contains("fabric=connected"),
        "a refused line writes nothing and leaves the lane framed: {}",
        w.log_tail()
    );
}

#[test]
fn a_refused_verdict_is_recorded_and_answered_with_nothing() {
    let w = World::boot("r2loop", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    for i in 0..65 {
        let reply = w.verb(&format!("@{a} post to=@{a} kind=note m{i}"));
        assert!(reply.ok(), "post {i}: {}", reply.header());
    }

    let marker = until("the bridge to record a verdict it did not answer", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("unnotified ") && e.contains("reason=verdict"))
    });
    assert!(
        marker.contains("off="),
        "the record that was not answered is named: {marker}"
    );

    let mut c = w.god();
    let (rows, _) = c
        .fetch(0, &format!("/f/{FLEET}/in/{}/>", w.node), 4_096)
        .expect("read the node's own inbox lanes");
    let notices = rows
        .iter()
        .filter(|(_, s, _)| s.ends_with("/undeliverable"))
        .count();
    assert!(
        notices <= 4,
        "a verdict must not generate a verdict: {notices} notice records"
    );
}
