// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 2 — what a BUSY bridge stopped doing.**
//!
//! `Bridge::run` used to enter its idle arm only when a fixed 250 ms mailbox
//! wait answered `None`, i.e. only after a full interval with every queue empty, and the
//! tick counter that gated the roster round advanced only there. Round 1
//! identified that hazard and moved the other periodic duties out onto their own
//! deadlines for exactly this reason — and left `sample_local_control` behind,
//! which is the only place `attention=` is re-sampled and republished. (It was
//! also the only producer of §6.6's rows 4 and 5, the conservative pause and the
//! local-lease mirror; round 21 cut both with the drive face they decided for,
//! and `renew_leases` and `resolve_pending_feed` went with them.)
//!
//! So a node receiving one record per <250 ms — a peer posting, a redelivery
//! backlog after an outage — silently stopped implementing the escalation path
//! `notify --on attention` reads, for as long as it stayed busy.
//!
//! The load here is LOAD, not synchronisation: a publisher thread keeps the
//! bridge's inbox queue non-empty, and every assertion is still an `until` over
//! an observable the bridge publishes.

mod harness;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use aterm_spec::derive::fabric_outbox_wake_model;
use harness::{until, until_within, World, FLEET};

/// A newly parked bus fetch wakes the bridge through its `fetch` timeline
/// event. The test fault disables only the periodic outbox drain, so the reply
/// proves that the real prompt event path carried the request.
#[test]
fn a_queued_fetch_has_a_prompt_event_without_the_roster_drain() {
    let w = World::boot_with(
        "r2fetch",
        &[],
        &[("ATERM_LINK_FAULT", "skip-roster-outbox-while-marked")],
    );
    w.wait_ready();
    let (_a, b) = w.two_sessions();
    let presence = format!("/f/{FLEET}/pub/{}/{b}/presence", w.node);
    until("the bridge to adopt the fetcher's session", || {
        let mut broker = w.god();
        let (rows, _) = broker.last(&presence, "", 8).ok()?;
        rows.iter()
            .any(|(_, subject, body)| {
                *subject == presence && String::from_utf8_lossy(body).contains("state=live")
            })
            .then_some(())
    });
    std::fs::write(w.state.join("skip-roster-outbox"), b"1\n").unwrap();

    let off = 1_000_000;
    let reply = w.verb(&format!("@{b} inbox get @{off}"));
    let timeline = w.verb(&format!("@{b} timeline 20"));
    let events = timeline.rows().join("\n");
    assert!(
        reply
            .header()
            .starts_with(&format!("ERR no such record off={off}")),
        "the pushed fetch was answered through the broker: {} — event_fired={} timeline:\n{events}\nlog:\n{}",
        reply.header(),
        w.state.join("fault-fired").exists(),
        w.log_tail()
    );
    assert!(
        events.contains(&format!("kind=fetch off={off}")),
        "the endpoint recorded the push event: {events}"
    );
    assert!(
        w.state.join("fault-fired").exists(),
        "the bridge received the fetch event, rather than a roster fallback"
    );

    let model = fabric_outbox_wake_model();
    let mut state = model.init_state();
    assert!(model.fire("Queue", &mut state), "the real fetch was queued");
    assert!(
        model.action_enabled("PromptDrain", &state),
        "the real fetch event enables the prompt drain"
    );
    assert!(model.fire("PromptDrain", &mut state));
    assert_eq!(state["delivered"], 1);
}

/// A post whose push event was lost still reaches its recipient on the roster
/// backstop. The marker holds the fault open until the bridge acknowledges the
/// dropped event, so a normal post event cannot make this test pass by accident.
#[test]
fn a_lost_post_event_is_delivered_by_the_roster_backstop() {
    let w = World::boot_with(
        "r2postdrop",
        &[],
        &[("ATERM_LINK_FAULT", "drop-post-event-while-marked")],
    );
    w.wait_ready();
    let (a, b) = w.two_sessions();
    std::fs::write(w.state.join("drop-post-event"), b"1\n").unwrap();

    let posted = w.verb(&format!("@{a} post to=@{b} kind=note backstop-only"));
    assert!(posted.ok(), "{}", posted.header());
    let model = fabric_outbox_wake_model();
    let mut state = model.init_state();
    assert!(model.fire("Queue", &mut state), "the real post was queued");
    until("the bridge to discard the prompt post event", || {
        w.state.join("fault-fired").exists().then_some(())
    });
    assert!(model.fire("LoseEvent", &mut state));
    let _ = std::fs::remove_file(w.state.join("drop-post-event"));
    let row = until_within(
        Duration::from_secs(5),
        "the roster backstop to deliver a post without its event",
        || {
            w.inbox(&b)
                .into_iter()
                .find(|r| r.contains("backstop-only"))
        },
    );
    assert!(row.contains("kind=note"), "{row}");
    for _ in 0..2 {
        assert!(model.fire("BackstopTick", &mut state));
    }
    assert_eq!(state["delivered"], 1, "the real inbox received the post");
}

/// **A busy bridge still republishes `attention=`.**
///
/// `attention=` is set with a purely LOCAL verb (`meta set attention …`) and
/// nothing on the bus or the push lane announces it: the bridge discovers it by
/// looking, on its periodic round. Under sustained inbound traffic that round
/// never came, so the escalation never reached `notify --on attention` or the
/// roster `ls` prints — the one path a session has to say "I need a human".
#[test]
fn a_busy_bridge_still_publishes_an_escalation_nothing_announces() {
    let w = World::boot("r2busy", &[]);
    w.wait_ready();
    let (a, _b) = w.two_sessions();

    // KEEP THE MAILBOX NON-EMPTY. One record every 40 ms against a 250 ms idle
    // tick: the loop takes an item on essentially every round, which is exactly
    // the state a chatty peer or a redelivery backlog puts a node in.
    let stop = Arc::new(AtomicBool::new(false));
    let load = {
        let (stop, subject) = (
            stop.clone(),
            format!("/f/{FLEET}/in/{}/{a}/h-noise/note", w.node),
        );
        let mut god = w.god();
        std::thread::spawn(move || {
            let mut seq = 0u64;
            while !stop.load(Ordering::SeqCst) {
                seq += 1;
                if god
                    .publish(6_400, seq, &subject, b"v=1 t=1 text=noise")
                    .is_err()
                {
                    return;
                }
                std::thread::sleep(Duration::from_millis(40));
            }
        })
    };

    let set = w.verb(&format!("@{a} meta set attention need-a-key"));
    assert!(set.ok(), "meta set attention: {}", set.header());

    let presence = format!("/f/{FLEET}/pub/{}/{a}/presence", w.node);
    let seen = until("the escalation to reach the session's presence row", || {
        let mut c = w.god();
        let (rows, _) = c.last(&presence, "", 8).ok()?;
        rows.into_iter()
            .find(|(_, s, _)| *s == presence)
            .map(|(_, _, b)| String::from_utf8_lossy(&b).into_owned())
            .filter(|b| b.contains("attention=need-a-key"))
    });
    stop.store(true, Ordering::SeqCst);
    load.join().expect("the load thread");
    assert!(
        seen.contains("state=live"),
        "the republished row is the ordinary presence row: {seen}"
    );
}
