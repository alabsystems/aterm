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

/// `rest` as `driver`'s drive connection sends it.
fn remote(store: &Store, driver: &Driver, rest: &str) -> String {
    let _driving = DrivingAs::enter(driver.clone());
    cmd_hold(store, rest, HoldIssuer::Remote)
}

/// `verb`'s halt refusal on `ctx`, as `driver`'s drive connection meets it
/// (`None`: any other writer's).
fn refusal_to(ctx: &dyn FabricSession, driver: Option<&Driver>, verb: &str) -> Option<String> {
    let _driving = driver.cloned().map(DrivingAs::enter);
    halt_refusal(ctx, verb)
}

/// A NODE'S HOLD KEEPS EVERYONE OUT BUT THE NODE. Its `hold on` fills the
/// remote slot with its node and host; every other writer — a local
/// client, the harness, another node — meets `ERR halted … origin=remote`;
/// the node's own writes pass; and its `off` lifts it. CONTROL: with no
/// hold, nobody is refused.
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

/// THE PERSON WINS. A node's `hold on` under the person's Take Over is
/// refused naming the person's hold; the person's Take Over takes a node's
/// slot away, and while it stands the node's writes are refused too; Hand
/// Back, with no person's or client's hold, lifts a node's.
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

/// THE REMOTE SLOT IS ITS NODE'S. No local connection names it — Owner and
/// bridge `origin=remote` are `ERR usage`, and a remote act off a drive
/// connection's thread is `ERR denied` — and the node names no origin. No
/// carry writes it, so a successor never reads it as a local hold.
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
