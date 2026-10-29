// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the `person` hold origin and refusal accounting.

use super::inbox_hold::registered_as;
use super::*;
use crate::test_support::{Store, new_store};

fn owner(store: &Store, rest: &str) -> String {
    cmd_hold(store, rest, HoldIssuer::Owner)
}

fn bridge(store: &Store, rest: &str) -> String {
    cmd_hold(store, rest, HoldIssuer::Bridge)
}

#[test]
fn the_window_sets_origin_person_and_no_socket_names_or_lifts_it() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    let holds = ctx.fabric.holds();
    assert_eq!(holds.person(), Some(TAKEN_OVER));
    assert_eq!(holds.origins(), PERSON_ORIGIN);
    assert_eq!(holds.word(), Some(aterm_types::needs::HoldWord::Person));
    let hold = ctx.fabric.hold().expect("held");
    assert_eq!(
        (hold.origin.as_str(), hold.reason.as_str()),
        (PERSON_ORIGIN, TAKEN_OVER)
    );
    let refusal = halt_refusal(&ctx, "send").expect("a held session refuses send");
    assert!(refusal.starts_with("ERR halted "), "{refusal}");
    assert!(refusal.contains(" origin=person"), "{refusal}");

    assert_eq!(owner(&store, &format!("{sid} off")), "OK hold=1\n");
    assert_eq!(bridge(&store, &format!("{sid} off")), "OK hold=1\n");
    assert_eq!(
        bridge(&store, &format!("{sid} off origin=local")),
        "OK hold=1\n"
    );
    assert_eq!(ctx.fabric.holds().person(), Some(TAKEN_OVER));

    for (who, rest) in [
        ("owner", format!("{sid} off origin=person")),
        ("owner", format!("{sid} on origin=person")),
        ("bridge", format!("{sid} off origin=person")),
        ("bridge", format!("{sid} on reason=x origin=person")),
    ] {
        let reply = if who == "owner" {
            owner(&store, &rest)
        } else {
            bridge(&store, &rest)
        };
        assert!(reply.starts_with("ERR usage"), "{who} {rest}: {reply}");
    }
    assert!(
        cmd_hold(&store, &format!("{sid} off"), HoldIssuer::Person).starts_with("ERR usage"),
        "the person's acts never ride the wire grammar"
    );
    assert_eq!(ctx.fabric.holds().origins(), PERSON_ORIGIN, "nothing moved");

    assert_eq!(
        owner(&store, &format!("{sid} on reason=mine")),
        "OK hold=1\n"
    );
    assert_eq!(ctx.fabric.holds().origins(), "person,local");
    assert_eq!(owner(&store, &format!("{sid} off")), "OK hold=1\n");
    assert_eq!(ctx.fabric.holds().origins(), PERSON_ORIGIN);

    assert_eq!(person_hold(&ctx, false, &|| {}), PersonHoldOutcome::Moved);
    assert!(!ctx.fabric.holds().is_held());
    assert_eq!(
        person_hold(&ctx, false, &|| {}),
        PersonHoldOutcome::Unchanged
    );
    assert!(halt_refusal(&ctx, "send").is_none());
    assert_eq!(owner(&store, &format!("{sid} off")), "OK hold=0\n");
}

#[test]
fn the_person_slot_stands_beside_the_fleets_and_neither_lift_touches_the_other() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    assert_eq!(
        bridge(&store, &format!("{sid} on reason=stop")),
        "OK hold=1\n"
    );
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    assert_eq!(ctx.fabric.holds().origins(), "person,fleet");
    assert_eq!(
        ctx.fabric.hold().map(|h| h.origin),
        Some(PERSON_ORIGIN.to_string())
    );
    assert_eq!(bridge(&store, &format!("{sid} off")), "OK hold=1\n");
    assert_eq!(ctx.fabric.holds().origins(), PERSON_ORIGIN);
    assert_eq!(
        bridge(&store, &format!("{sid} on reason=stop")),
        "OK hold=1\n"
    );
    assert_eq!(person_hold(&ctx, false, &|| {}), PersonHoldOutcome::Moved);
    assert_eq!(ctx.fabric.holds().origins(), "fleet");
}

#[test]
fn the_handoff_export_and_import_round_trip_the_person_slot() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    assert_eq!(
        bridge(&store, &format!("{sid} on reason=stop")),
        "OK hold=1\n"
    );
    assert_eq!(
        owner(&store, &format!("{sid} on reason=mine")),
        "OK hold=1\n"
    );
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    let exported = ctx.fabric.holds();
    let rows = render_holds(&exported);
    assert_eq!(rows.len(), 3);
    assert!(rows[0].starts_with("person "), "{rows:?}");
    let imported = parse_holds(rows.iter().map(String::as_str));
    assert_eq!(imported, exported);

    let (_, successor) = registered_as(&store, 2);
    successor
        .fabric
        .seed_holds(imported.clone(), &successor.timeline);
    assert_eq!(successor.fabric.holds(), exported);
    assert_eq!(successor.fabric.holds().person(), Some(TAKEN_OVER));

    let unreadable = parse_hold("person");
    assert_eq!(
        (unreadable.origin.as_str(), unreadable.reason.as_str()),
        (PERSON_ORIGIN, CARRIED_UNREADABLE)
    );
}

#[test]
fn the_counter_moves_only_on_refusals_under_the_person_hold() {
    let store = new_store();
    let (sid, ctx) = registered_as(&store, 1);
    assert_eq!(
        owner(&store, &format!("{sid} on reason=mine")),
        "OK hold=1\n"
    );
    assert!(halt_refusal(&ctx, "send").is_some());
    assert_eq!(ctx.fabric.person_refusals(), 0);
    assert_eq!(owner(&store, &format!("{sid} off")), "OK hold=0\n");

    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    let _ = (ctx.fabric.holds(), ctx.fabric.hold());
    assert!(
        halt_refusal(&ctx, "status").is_none(),
        "a read is let through"
    );
    assert_eq!(ctx.fabric.person_refusals(), 0, "reads never move it");
    assert!(halt_refusal(&ctx, "send").is_some());
    assert!(halt_refusal(&ctx, "turn").is_some());
    assert_eq!(ctx.fabric.person_refusals(), 2);
    assert!(app_halt_refusal(&store, "invoke").is_some());
    assert!(open_halt_refusal(&store, "open", "menu").is_some());
    assert_eq!(
        ctx.fabric.person_refusals(),
        2,
        "an instance-wide refusal is no tab's"
    );

    assert_eq!(person_hold(&ctx, false, &|| {}), PersonHoldOutcome::Moved);
    assert!(halt_refusal(&ctx, "send").is_none());
    assert_eq!(
        ctx.fabric.person_refusals(),
        2,
        "an unheld verb counts nothing"
    );
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    assert_eq!(
        ctx.fabric.person_refusals(),
        0,
        "a fresh Take Over starts at zero"
    );
}

#[test]
fn a_take_over_under_a_committing_update_answers_busy_at_once() {
    let store = new_store();
    let (_sid, ctx) = registered_as(&store, 1);
    let _ = ctx.fabric.fence_hold(true);
    let blocks = fence_blocks_on_this_thread();
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Busy);
    assert_eq!(fence_blocks_on_this_thread(), blocks, "the press waited");
    assert!(!ctx.fabric.holds().is_held());
    ctx.fabric.lift_hold_fence();
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
}

#[test]
fn a_gateway_card_does_not_open_under_a_hold() {
    let store = new_store();
    let (_sid, ctx) = registered_as(&store, 1);
    let gateways = [
        "menu",
        "palette",
        "tab-menu",
        "overview",
        "connections",
        "autopilot-menu",
        "fact-menu",
    ];
    for target in gateways {
        assert!(
            open_halt_refusal(&store, "open", target).is_none(),
            "{target} unheld"
        );
    }
    assert_eq!(person_hold(&ctx, true, &|| {}), PersonHoldOutcome::Moved);
    for target in gateways {
        let refusal = open_halt_refusal(&store, "open", target).expect("refused");
        assert!(refusal.starts_with("ERR halted "), "{target}: {refusal}");
        assert!(
            open_halt_refusal(&store, "open", &format!("{target} close")).is_none(),
            "{target} close stays answerable"
        );
    }
    assert!(
        open_halt_refusal(&store, "open", "fact-menu path")
            .is_some_and(|refusal| refusal.starts_with("ERR halted ")),
        "a fact's menu named by its fact"
    );
    assert!(
        open_halt_refusal(&store, "open", "autopilot-menu at=20,40 by=pointer")
            .is_some_and(|refusal| refusal.starts_with("ERR halted ")),
        "the Autopilot menu a program asks for at its own cell"
    );
    for other in ["prefs", "about", "update", "", "app settings"] {
        assert!(
            open_halt_refusal(&store, "open", other).is_none(),
            "{other:?}"
        );
    }
    assert!(
        open_halt_refusal(&store, "invoke", "menu").is_none(),
        "not open"
    );
}
