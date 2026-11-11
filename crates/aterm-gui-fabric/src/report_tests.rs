// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::test_support::test_handle;

#[test]
fn a_report_to_a_session_here_is_queued_on_the_bus_only_where_a_bridge_runs() {
    crate::with_link_reset(|| {
        let worker = test_handle(1);
        let manager = test_handle(2);
        let to = To::Here(manager.sid.as_str());
        let line = "finished | sid=s-1 state=done";
        let absent = post(&*worker.ctx, to, "s-1.end.1", line);
        assert_eq!(
            absent,
            Err(Unsent::NoBridge(crate::fabric_state().to_owned())),
            "no bridge here, so nothing carries it"
        );
        assert!(
            worker.ctx.fabric.lock().posts.is_empty(),
            "a report no bridge carries is not left queued"
        );

        crate::note_bridge_supervised();
        assert_eq!(
            post(&*worker.ctx, to, "s-1.end.1", line),
            Ok("OK 1".to_owned())
        );
        let inbox = worker.ctx.fabric.lock();
        let queued: Vec<_> = inbox
            .posts
            .iter()
            .map(|p| {
                (
                    p.to.clone(),
                    p.kind.as_str(),
                    p.key.as_deref(),
                    p.body.as_str(),
                )
            })
            .collect();
        assert_eq!(
            queued,
            [(
                crate::pct_encode(&format!("@{}", manager.sid.as_str())),
                "report",
                Some("s-1.end.1"),
                line
            )],
            "one report on the bus, to the manager, under its key"
        );
    });
}

#[test]
fn a_malformed_report_is_refused_before_any_transport() {
    crate::with_link_reset(|| {
        crate::note_bridge_supervised();
        let worker = test_handle(1);
        let node = format!("n-{}", "b".repeat(16));
        for (to, key, text) in [
            (To::Here("s-2"), "k", "two\nlines"),
            (To::Here("s-2"), "k", ""),
            (To::Here("s-2"), "k", &"x".repeat(REPORT_MAX + 1)),
            (To::Here("s-2"), "a key", "text"),
            (To::Here("not a sid"), "k", "text"),
            (
                To::There {
                    sid: "s-2",
                    node: "beta",
                },
                "k",
                "text",
            ),
        ] {
            assert!(
                post(&*worker.ctx, to, key, text).is_err(),
                "{to:?} {key:?} {text:?}"
            );
        }
        assert!(worker.ctx.fabric.lock().posts.is_empty());
        // Control: the same report, well formed, is queued; one for another Mac is never put
        // on this Mac's bus, and with no fleet drive here nothing takes it.
        assert!(post(&*worker.ctx, To::Here("s-2"), "k", "text").is_ok());
        let there = post(
            &*worker.ctx,
            To::There {
                sid: "s-2",
                node: &node,
            },
            "k",
            "text",
        );
        assert!(
            matches!(&there, Err(Unsent::NoDrive { node: to, .. }) if *to == node),
            "{there:?}"
        );
        assert_eq!(worker.ctx.fabric.lock().posts.len(), 1);
    });
}

/// A REPORT FROM A SESSION LEAVING THE ROSTER IS NEVER QUEUED ON THE BUS, which drains no
/// outbox of a session that left: to a session here it is refused, and to one on another Mac
/// it goes by direct fleet mail (where the mesh would carry a live sender's too: `fleet_mail`'s
/// `a_leaving_sessions_report_is_driven_to_a_mac_the_mesh_would_carry_it_to`).
#[test]
fn a_report_from_a_session_leaving_goes_by_direct_fleet_mail_alone() {
    crate::with_link_reset(|| {
        crate::note_bridge_supervised();
        let worker = test_handle(1);
        let node = format!("n-{}", "b".repeat(16));
        assert_eq!(
            post_leaving(&*worker.ctx, To::Here("s-2"), "k", "text"),
            Err(Unsent::Leaving)
        );
        assert!(worker.ctx.fabric.lock().posts.is_empty(), "nothing queued");
        let there = post_leaving(
            &*worker.ctx,
            To::There {
                sid: "s-2",
                node: &node,
            },
            "k",
            "text",
        );
        assert!(
            matches!(&there, Err(Unsent::NoDrive { .. })),
            "a fleet drive is what it tries: {there:?}"
        );
        assert!(worker.ctx.fabric.lock().posts.is_empty(), "nothing queued");
        // Control: the same report from a session that stays is queued on the bus.
        assert!(post(&*worker.ctx, To::Here("s-2"), "k", "text").is_ok());
        assert_eq!(worker.ctx.fabric.lock().posts.len(), 1);
    });
}
