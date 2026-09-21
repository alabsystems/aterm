// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! **ROUND 2 — what a BUSY bridge stopped doing.**
//!
//! `Bridge::run`'s idle arm is entered only when `mailbox.take(IDLE_TICK)`
//! answers `None`, i.e. only after a full 250 ms with every queue empty, and the
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

use harness::{until, World, FLEET};

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
