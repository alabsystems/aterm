// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 2 — the round-1 fix's own leak, and the loop the same fix opened.**
//!
//! Round 1 closed "the bridge's wound": one oversized message used to kill the
//! verb lane and, through aterm's fail-closed guard, halt every session the
//! bridge governed. Its fix put the request-line bound in one place, on the
//! writer, and sized `deliver`'s BODY budget from it.
//!
//! It sized the body, and only the body. `via=` sits on the same line, is taken
//! verbatim off a stranger's record with no length, count or shape check, and a
//! record body runs to the broker's 16 MiB ceiling. So the prefix alone went
//! past the bound, the budget saturated to zero, and the finished line was still
//! unsendable — at which point the writer refused it (correctly; the lane
//! survived, which is the half round 1 really did fix) and the caller filed a
//! permanent LOCAL refusal as a transport failure: no `ev`, no verdict on the
//! sender's lane, no inbox row, and the next record's absolute group commit
//! stepping the durable cursor straight over it. A silent message loss, from any
//! principal that can address the node.
//!
//! The same fix also gave the `undeliverable` verdict a lane the fabric actually
//! drains — which made the verdict a record like any other: addressed,
//! delivered, and refusable. With one saturated quota, answering a refused
//! verdict with another verdict is an unbounded publish loop.
//!
//! Every test here runs over the REAL binaries — a guarded broker, a headless
//! `aterm-gui`, and the bridge child aterm launches itself.

mod harness;

use harness::{until, World, FLEET};

/// **An oversized `via=` earns a VERDICT, and the record is never silently
/// lost.**
///
/// 2 000 comma-joined 34-byte principals: every element passes the endpoint's
/// own `valid_principal` check, so nothing about the chain is malformed — it is
/// simply longer than a control request line, which is a fact only the WRITER
/// can know. Before the fix this record produced no `ev` of any kind, no
/// `undeliverable` on the sender's lane and no inbox row, while the ordinary
/// note published behind it committed the group cursor past it forever.
///
/// Three claims: the poison record earns an `ev undeliverable … reason=via`, the
/// sender is told on the lane a `p/` principal actually drains, and the ordinary
/// note behind it lands — because a poison record must not stop the drain
/// either.
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

    // THE ORDINARY NOTE LANDS: one poison record does not stop the drain.
    until("the ordinary note behind the poison one to land", || {
        w.inbox(&a).into_iter().find(|r| r.contains("text=second"))
    });

    // THE POISON RECORD HAS A VERDICT ON THE LOG. This is the whole finding:
    // before the fix `w.ev()` held nothing at all for this offset.
    let verdict = until("the verdict for the refused record", || {
        w.ev()
            .into_iter()
            .find(|e| e.starts_with("undeliverable ") && e.contains(&format!("off={poison_off}")))
    });
    assert!(
        verdict.contains("reason=via"),
        "the reason must name the field that could not be sent: {verdict}"
    );

    // AND THE SENDER IS TOLD, on the lane §6.3 gives a `h-*` principal.
    let lane = format!("/f/{FLEET}/in/p/h-sender/{}/undeliverable", w.node);
    until("the sender's own verdict row", || {
        let mut c = w.god();
        let (rows, _) = c.fetch(0, &lane, 64).ok()?;
        rows.into_iter().find(|(_, _, body)| {
            let body = String::from_utf8_lossy(body);
            body.contains(&format!("re={poison_off}")) && body.contains("reason=via")
        })
    });

    // AND THE POISON RECORD WAS NEVER DELIVERED as a row.
    assert!(
        !w.inbox(&a).iter().any(|r| r.contains("text=hi")),
        "a record the bridge refused must not also be delivered"
    );

    // THE LANE SURVIVED, which is round 1's half of the property.
    assert!(
        w.verb("status").header().contains("fabric=connected"),
        "the bridge's control connection must survive an oversized field: {}",
        w.log_tail()
    );
}

/// **A `deliver` line over the bound is a VERDICT, not a transport failure.**
///
/// Every variable-length field the line carries is bounded now, so no record a
/// peer can publish reaches the total-line check — that guard exists for the
/// field a LATER rung adds, which is exactly how this defect arrived the first
/// time. `ATERM_LINK_FAULT=oversize-deliver-line` pads one line past the bound
/// from inside the process (one shot, across restarts), and everything after the
/// padding is the shipped path.
///
/// Before the fix, `Ctl`'s refusal — `InvalidInput`, nothing written, lane alive
/// — was mapped to `Delivery::Unaccounted`, the outcome reserved for a lane that
/// is GONE: no `ev`, no sender notice, and the cursor stepped past the record on
/// the next one that delivered.
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

    // THE LANE IS ALIVE AND THE DRAIN CONTINUES: the fault is one-shot, so the
    // next record goes through the ordinary path.
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

/// **A verdict never earns a verdict.**
///
/// The per-sender quota refuses the 65th unlisted row from one `from=`, and the
/// bridge answers the sender on their own lane. That notice is a record: it is
/// addressed to the same session, rendered under the same `from=` — which is
/// still at quota — and refused in turn. Nothing exempted it, so each refusal
/// published another notice plus an `ev`, each with a durable sequence `fsync`,
/// on an append-forever log, for the life of the incarnation. A `post` is an
/// ordinary `Scoped` verb every in-session client holds, so 65 of them armed it
/// with no capability at all.
///
/// The marker asserted here is the one the loop cannot produce: the bridge
/// records that the verdict was NOT answered, and stops. Before the fix this
/// record does not exist and the run never settles.
#[test]
fn a_refused_verdict_is_recorded_and_answered_with_nothing() {
    let w = World::boot("r2loop", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // 65 posts from this session to ITSELF: the 65th is the one the ring's
    // per-sender quota refuses, and its verdict comes back under the same
    // `from=` key that is already full.
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

    // AND THE LOOP IS BOUNDED. Every `undeliverable` on this node's own inbox
    // lanes is a notice the bridge published; unbounded, this grows without
    // limit for the life of the process.
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
