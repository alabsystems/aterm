// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the remote hold slot across fleet drive operations.

use super::inbox_hold::registered_as;
use super::*;
use crate::test_support::{Store, new_store};

fn mac(node: u8, host: &str) -> Driver {
    Driver {
        node: format!("{node:02x}").repeat(16),
        host: host.to_owned(),
    }
}

fn remote(store: &Store, driver: &Driver, rest: &str) -> String {
    let _driving = DrivingAs::enter(driver.clone());
    cmd_hold(store, rest, HoldIssuer::Remote)
}

fn refusal_to(ctx: &dyn FabricSession, driver: Option<&Driver>, verb: &str) -> Option<String> {
    let _driving = driver.cloned().map(DrivingAs::enter);
    halt_refusal(ctx, verb)
}

#[test]
fn a_nodes_hold_refuses_every_writer_but_that_node() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    let (m03, m09) = (mac(0x03, "mac-03"), mac(0x09, "mac-09"));
    assert!(refusal_to(&ctx, None, "send").is_none(), "control: no hold");
    assert_eq!(
        remote(&store, &m03, &format!("{sid} on reason=build")),
        "OK hold=1\n"
    );
    let holds = ctx.fabric.holds();
    let held = holds.remote().expect("the remote slot");
    assert_eq!(
        (held.node.as_str(), held.host.as_str()),
        (m03.node.as_str(), "mac-03")
    );
    assert_eq!(holds.origins(), REMOTE_ORIGIN);
    assert_eq!(holds.word(), Some(aterm_types::needs::HoldWord::Remote));
    for who in [None, Some(&m09)] {
        let refused = refusal_to(&ctx, who, "send").expect("held from everyone else");
        assert!(refused.contains(" origin=remote"), "{who:?}: {refused}");
    }
    assert!(
        refusal_to(&ctx, Some(&m03), "send").is_none(),
        "the node's own writes pass"
    );
    assert!(refusal_to(&ctx, Some(&m03), "turn").is_none());
    for rest in [format!("{sid} on"), format!("{sid} off")] {
        let reply = remote(&store, &m09, &rest);
        assert!(
            reply.starts_with("ERR halted ") && reply.contains(" origin=remote"),
            "{reply}"
        );
    }
    assert_eq!(
        ctx.fabric.holds().remote().map(|r| r.host.as_str()),
        Some("mac-03")
    );
    assert_eq!(remote(&store, &m03, &format!("{sid} off")), "OK hold=0\n");
    assert!(refusal_to(&ctx, None, "send").is_none());
}

#[test]
fn a_nodes_hold_ends_with_its_grant() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    let (other_sid, other) = registered_as(&store, 2);
    let (m03, m09) = (mac(0x03, "mac-03"), mac(0x09, "mac-09"));
    assert_eq!(remote(&store, &m03, &format!("{sid} on")), "OK hold=1\n");
    assert_eq!(
        remote(&store, &m09, &format!("{other_sid} on")),
        "OK hold=1\n"
    );
    assert_eq!(
        cmd_hold(&store, &format!("{sid} off"), HoldIssuer::Owner),
        "OK hold=1\n",
        "a local off lifts its own slot alone"
    );
    let granted = |node: &str| node == m09.node;
    assert_eq!(lift_ungranted_remote_holds(&store, &granted, &|_| {}), 1);
    assert!(!ctx.fabric.holds().is_held(), "m03 drives no more");
    assert!(refusal_to(&ctx, None, "send").is_none());
    assert_eq!(
        other.fabric.holds().remote().map(|hold| hold.node.clone()),
        Some(m09.node.clone()),
        "a granted node keeps its hold"
    );
    assert_eq!(lift_ungranted_remote_holds(&store, &granted, &|_| {}), 0);
    assert_eq!(lift_ungranted_remote_holds(&store, &|_| false, &|_| {}), 1);
    assert!(!other.fabric.holds().is_held());

    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    assert_eq!(lift_ungranted_remote_holds(&store, &|_| false, &|_| {}), 0);
    assert_eq!(
        ctx.fabric.holds().origins(),
        PERSON_ORIGIN,
        "the person's hold is no node's"
    );
}

#[test]
fn the_persons_take_over_refuses_and_clears_a_nodes_hold() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    let m03 = mac(0x03, "mac-03");
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    let reply = remote(&store, &m03, &format!("{sid} on"));
    assert!(
        reply.starts_with("ERR halted ") && reply.contains(" origin=person"),
        "{reply}"
    );
    assert!(ctx.fabric.holds().remote().is_none(), "nothing set");
    assert_eq!(person_hold(&ctx, false, &|| {}), PersonHoldOutcome::Moved);

    assert_eq!(remote(&store, &m03, &format!("{sid} on")), "OK hold=1\n");
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    assert!(
        ctx.fabric.holds().remote().is_none(),
        "Take Over took the tab from the node"
    );
    let refused = refusal_to(&ctx, Some(&m03), "send").expect("the person's hold binds the node");
    assert!(refused.contains(" origin=person"), "{refused}");
    assert_eq!(remote(&store, &m03, &format!("{sid} off")), "OK hold=1\n");
    assert_eq!(ctx.fabric.holds().origins(), PERSON_ORIGIN);
    assert_eq!(person_hold(&ctx, false, &|| {}), PersonHoldOutcome::Moved);

    assert_eq!(remote(&store, &m03, &format!("{sid} on")), "OK hold=1\n");
    assert_eq!(ctx.fabric.holds().hand_back_origin(), Some(REMOTE_ORIGIN));
    assert_eq!(person_hold(&ctx, false, &|| {}), PersonHoldOutcome::Moved);
    assert!(
        !ctx.fabric.holds().is_held(),
        "Hand Back lifted the node's hold"
    );
}

#[test]
fn no_local_connection_names_the_remote_slot_and_no_carry_writes_it() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    let m03 = mac(0x03, "mac-03");
    for issuer in [HoldIssuer::Owner, HoldIssuer::Bridge] {
        let reply = cmd_hold(&store, &format!("{sid} on origin=remote"), issuer);
        assert!(reply.starts_with("ERR usage"), "{issuer:?}: {reply}");
    }
    assert_eq!(
        cmd_hold(&store, &format!("{sid} on"), HoldIssuer::Remote),
        HOLD_DENIED
    );
    for origin in ["local", "fleet", "remote", "person"] {
        let reply = remote(&store, &m03, &format!("{sid} on origin={origin}"));
        assert!(reply.starts_with("ERR usage"), "origin={origin}: {reply}");
    }
    assert!(!ctx.fabric.holds().is_held(), "nothing moved");

    assert_eq!(remote(&store, &m03, &format!("{sid} on")), "OK hold=1\n");
    assert_eq!(
        cmd_hold(&store, &format!("{sid} on reason=mine"), HoldIssuer::Owner),
        "OK hold=1\n"
    );
    assert_eq!(
        render_holds(&ctx.fabric.holds()),
        vec!["local mine".to_owned()]
    );
    assert_eq!(
        cmd_hold(&store, &format!("{sid} off"), HoldIssuer::Owner),
        "OK hold=1\n"
    );
    assert!(render_holds(&ctx.fabric.holds()).is_empty());
    assert_eq!(ctx.fabric.holds().origins(), REMOTE_ORIGIN);
}

#[test]
fn a_local_off_lifts_a_nodes_hold_once_that_node_may_drive_this_mac_no_more() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    let m03 = mac(0x03, "mac-03 (node 03030303)");
    let off = |holder: Holder| {
        owner_off_remote(
            &store,
            sid.as_str(),
            &|hold: &RemoteHold| {
                assert_eq!(hold.node, m03.node);
                holder.clone()
            },
            &|_| {},
        )
    };
    assert_eq!(off(Holder::Gone("left".into())), None, "no hold stands");
    assert_eq!(remote(&store, &m03, &format!("{sid} on")), "OK hold=1\n");
    let refused = refusal_to(&ctx, None, "send").expect("held");
    assert!(
        refused.starts_with("ERR halted reason=- origin=remote (held by mac-03 (node 03030303)")
            && refused.contains(&format!(
                "`aterm ctl --host <this session's Mac> hold {sid} off` run on mac-03 (node \
                 03030303), or the person at this session's Mac"
            ))
            && refused.contains(&format!(
                "`aterm fleet peers rm {}` there once that node left",
                m03.node
            ))
            && refused.contains(&format!("`aterm ctl hold {sid} off` on this session's Mac")),
        "the refusal names who holds it and what lifts it: {refused}"
    );
    let either_end = |reply: &str| !reply.contains(" here") && !reply.contains("this Mac");
    assert!(either_end(&refused), "{refused}");
    for (holder, says) in [
        (Holder::Drives, "which still drives this session's Mac"),
        (
            Holder::Again("nothing listens".into()),
            "which may still drive this session's Mac: its place did not prove it just now \
             (nothing listens)",
        ),
    ] {
        match off(holder) {
            Some(RemoteOff::Stands(reply)) => assert!(
                reply.starts_with("ERR halted reason=- origin=remote")
                    && reply.contains(says)
                    && either_end(&reply),
                "{reply}"
            ),
            other => panic!("a node that may still drive keeps its hold: {other:?}"),
        }
        assert!(ctx.fabric.holds().remote().is_some());
    }
    assert_eq!(
        cmd_hold(&store, &format!("{sid} on reason=mine"), HoldIssuer::Owner),
        "OK hold=1\n"
    );
    assert_eq!(
        off(Holder::Gone("left".into())),
        None,
        "an off lifts the local slot first"
    );
    assert_eq!(
        cmd_hold(&store, &format!("{sid} off"), HoldIssuer::Owner),
        "OK hold=1\n"
    );
    match off(Holder::Gone("another secret".into())) {
        Some(RemoteOff::Lifted { hold, why, reply }) => {
            assert_eq!(
                (hold.node, why.as_str()),
                (m03.node.clone(), "another secret")
            );
            assert_eq!(reply, "OK hold=0 lifted=remote\n");
        }
        other => panic!("a node that may drive no more keeps its hold: {other:?}"),
    }
    assert!(!ctx.fabric.holds().is_held());
    assert!(refusal_to(&ctx, None, "send").is_none());

    assert_eq!(remote(&store, &m03, &format!("{sid} on")), "OK hold=1\n");
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    assert_eq!(
        off(Holder::Gone("left".into())),
        None,
        "the person's Take Over took the tab"
    );
}
