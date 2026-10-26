// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! STOPPED WORK FIRST (`docs/DESIGN-harness-autopilot-2026-10-15.md` §14.7), through the App: a
//! Land session's `NEEDS YOU:` report row on the menu bar says what its hold waits on, read off
//! the legs of the hold its glass shows ([`crate::harness_glass::HeldGlass::need_alone`]).

use aterm_agent::harness::acts::{HoldLeg, LandHeld, TaskFace};
use aterm_agent::supervise::mode_words::report_words;

use super::landed_words_tests::{desk, idle, tell};
use crate::App;
use crate::status_item::{StatusRow, compose_status_menu};

const NEED: &str = "rotate the deploy key";

/// Session 0's need, published as its harness writes a `NEEDS YOU:` report.
fn report(app: &mut App) {
    let ctx = app.pool.get(0).expect("session 0").ctx.clone();
    ctx.meta
        .lock()
        .expect("meta")
        .set_attention_kind(
            crate::harness_host::NEEDS_ATTENTION_OWNER,
            Some(format!("claude needs you: {NEED} \u{2014} carrying on")),
            Some(aterm_types::needs::AttentionKind::Ask),
        )
        .expect("a slot");
    app.refresh_presence_session(0);
}

/// Land's hold of session `sid`, its yield read and held by `legs`.
fn hold(app: &mut App, sid: &str, legs: u8, read: bool) {
    let held = LandHeld {
        since_ms: Some(1_792_159_320_000),
        nudges: 1,
        read,
        legs,
        unpushed: u16::from(legs & HoldLeg::Repo.bit() != 0),
        ..LandHeld::default()
    };
    if crate::harness_host::store_land_held(&app.store, sid, Some(held)) {
        app.apply_harness_fact(sid, crate::HarnessFact::LandHeld);
    }
}

/// The report row's tip, as the menu bar composes it from the App's own glance.
fn tip(app: &App) -> Option<String> {
    let now = aterm_time::unix_secs().map_or(0, |s| i64::try_from(s).unwrap_or(0));
    let menu = compose_status_menu(&app.status_glance(), now);
    menu.iter()
        .find_map(|r| match r {
            StatusRow::Action {
                label,
                tip,
                indent: 0,
                ..
            } if label.contains(NEED) => Some(tip.clone()),
            _ => None,
        })
        .unwrap_or_else(|| panic!("the report's row: {menu:#?}"))
}

/// A held Land whose one leg is its needs says the landing waits on this alone; the same hold
/// with a repository leg too (the control), an unread hold, Cruise and no hold say it carries on.
#[test]
fn a_need_that_alone_holds_a_landing_says_so_on_the_apps_menu() {
    let (mut app, sid, _host) = desk("land");
    idle(&app);
    tell(&mut app, &sid, Some(TaskFace::Open));
    report(&mut app);
    let carries = Some(report_words(aterm_phase::Program::Claude.product(), false));
    assert_eq!(tip(&app), carries, "no hold yet");
    hold(&mut app, &sid, HoldLeg::Needs.bit(), true);
    assert!(app.shown_hold(0).is_some(), "PRECONDITION: the hold stands");
    assert_eq!(
        tip(&app),
        Some(report_words(aterm_phase::Program::Claude.product(), true)),
        "the need alone holds the landing"
    );
    hold(
        &mut app,
        &sid,
        HoldLeg::Needs.bit() | HoldLeg::Repo.bit(),
        true,
    );
    assert_eq!(
        tip(&app),
        carries,
        "CONTROL: a hold with a repo leg too says carries on meanwhile"
    );
    hold(&mut app, &sid, HoldLeg::Needs.bit(), false);
    assert_eq!(tip(&app), carries, "an unread yield's legs do not stand");
    hold(&mut app, &sid, HoldLeg::Needs.bit(), true);
    assert_eq!(
        tip(&app),
        Some(report_words(aterm_phase::Program::Claude.product(), true))
    );
    let session = app.pool.get(0).expect("session 0");
    let (term, ctx) = (session.term.clone(), session.ctx.clone());
    let (reply, changed) =
        crate::control::cmd_meta(&term, &app.store, 0, &ctx, "set autopilot cruise");
    assert!(reply.starts_with("OK"), "{reply}");
    if changed {
        app.on_meta_changed(0);
    }
    assert!(app.shown_hold(0).is_none(), "Cruise shows no hold");
    assert_eq!(tip(&app), carries, "no landing to wait");
}
