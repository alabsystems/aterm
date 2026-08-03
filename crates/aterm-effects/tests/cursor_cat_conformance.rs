// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding for the collectible cursor-cat lifecycle.
//!
//! The derived model is exhaustive over its bounded clock. This test drives the
//! real `CursorCat` with an injected `Instant`, projects public frame state onto
//! that clock, and asks both the in-process interpreter and `ty trace validate`
//! to admit every shipping transition.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_effects::kitty_cursor::{CatFrame, CatReaction, CursorCat};
use aterm_effects::kitty_registry::KittyLook;
use aterm_spec::derive::{Model, cursor_cat_curse_wince_model, cursor_cat_model};
use aterm_spec::verify;

type State = BTreeMap<&'static str, i64>;

fn project(
    frame: &CatFrame,
    active: bool,
    elapsed: i64,
    presented: i64,
    hidden: i64,
    presentable: bool,
) -> State {
    let phase = if !active {
        0 // Hidden
    } else if frame.discovery {
        1 // guaranteed Discovery hold
    } else {
        2 // bounded Fade after the guarantee expires
    };
    [
        ("phase", phase),
        ("elapsed", elapsed),
        ("presented", presented),
        ("hidden", hidden),
        ("presentable", i64::from(presentable)),
        ("collections", 1),
        ("visible", i64::from(presentable && frame.alpha > 0)),
        ("forced", i64::from(frame.discovery)),
        ("presented_once", 1),
        ("wall_expired", 0),
        ("trail_master", 0),
        ("ordinary_armed", 0),
        ("ordinary_visible", 0),
    ]
    .into_iter()
    .collect()
}

#[test]
fn real_cursor_cat_long_presentable_gap_settles_without_a_deferred_fade() {
    let model = cursor_cat_model();
    let mut cat = CursorCat::default();
    let look = KittyLook::default();
    let t0 = Instant::now();

    // The Collect transition is bound to the first ordinary present rather
    // than the alpha-zero event timestamp: at least one discovery frame was
    // genuinely visible before the host experienced a long drawable gap.
    cat.on_collect(t0, look);
    let early_frame = cat.frame(t0 + Duration::from_millis(16));
    assert!(early_frame.alpha > 0);
    assert!(early_frame.discovery);
    assert!(cat.is_active());
    let early = project(&early_frame, cat.is_active(), 0, 0, 0, true);
    let init = model.init_state();
    assert_step(&model, &init, &early, "Collect");
    let mut spec = init;
    assert!(model.fire("Collect", &mut spec));
    assert_eq!(early, spec);

    // Negative control: the old callback-relative fade begins a brand-new,
    // fully visible Fade at the delayed callback. Buggy=1 must admit exactly
    // that state; the healthy model must reject it before production is bound.
    let mut deferred_fade = project(&early_frame, true, 5, 1, 0, true);
    deferred_fade.insert("phase", 2);
    deferred_fade.insert("visible", 1);
    deferred_fade.insert("forced", 0);
    deferred_fade.insert("wall_expired", 1);
    let (bug_admits, bug_why) = verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &early,
        &deferred_fade,
        Some("LongPresentableGap"),
        "CursorCat deferred-fade negative control",
    );
    assert!(
        bug_admits,
        "Buggy=1 must reproduce the deferred visible fade: {bug_why}"
    );
    let (healthy_admits, healthy_why) = verify::validate_transition_tiered(
        &model,
        &[],
        &early,
        &deferred_fade,
        Some("LongPresentableGap"),
        "CursorCat healthy rejection of deferred fade",
    );
    assert!(
        !healthy_admits,
        "Buggy=0 accepted the deferred-fade negative control: {healthy_why}"
    );

    // Ten seconds is beyond the complete discovery hold plus fade. A single
    // presentable callback consumes that elapsed tail directly: no opaque snap,
    // no newly started fade timer, and no additional 60 Hz animation wakeups.
    let settled_frame = cat.frame(t0 + Duration::from_secs(10));
    assert_eq!(settled_frame.alpha, 0);
    assert!(!settled_frame.discovery);
    assert!(!settled_frame.collection_hello);
    assert!(
        !cat.is_active(),
        "an expired hello must not defer its fade until the next callback"
    );
    let mut settled = project(&settled_frame, cat.is_active(), 5, 1, 0, true);
    settled.insert("wall_expired", 1);
    assert_step(&model, &early, &settled, "LongPresentableGap");
    assert!(model.fire("LongPresentableGap", &mut spec));
    assert_eq!(settled, spec);
    assert!(model.check_invariant("LongGapSettlesHidden", &spec));
}

fn assert_step(model: &Model, prev: &State, next: &State, action: &str) {
    let (ok, why) = verify::validate_transition_tiered(
        model,
        &[],
        prev,
        next,
        Some(action),
        "real CursorCat collectible lifecycle",
    );
    assert!(ok, "model rejected real {action} transition: {why}");
}

fn wince_project(
    active: bool,
    prefixes: i64,
    completions: i64,
    winces: i64,
    chain: i64,
    reaction: bool,
) -> State {
    [
        ("active", i64::from(active)),
        ("prefixes", prefixes),
        ("completions", completions),
        ("winces", winces),
        ("chain", chain),
        ("reaction", i64::from(reaction)),
    ]
    .into_iter()
    .collect()
}

#[test]
fn real_cursor_cat_winces_only_for_complete_cues_and_repeats_build_force() {
    let model = cursor_cat_curse_wince_model();
    let t0 = Instant::now();
    let mut cat = CursorCat::default();
    cat.on_collect(t0, KittyLook::default());
    let _ = cat.frame(t0 + Duration::from_millis(16));

    // `fuc` produces no shipping cue, so the real cat remains in its existing
    // discovery expression while the model records an inert prefix.
    let mut spec = model.init_state();
    let prev = spec.clone();
    assert!(model.fire("TypeFuc", &mut spec));
    let prefix = cat.frame(t0 + Duration::from_millis(80));
    assert_ne!(prefix.reaction, CatReaction::Wince);
    let real = wince_project(true, 1, 0, 0, 0, false);
    assert_step(&model, &prev, &real, "TypeFuc");
    assert_eq!(real, spec);

    // The completed token is the first accepted kick.
    let first_at = t0 + Duration::from_millis(100);
    assert!(cat.on_curse(first_at, 1));
    let first = cat.frame(first_at + Duration::from_millis(90));
    assert_eq!(first.reaction, CatReaction::Wince);
    let prev = spec.clone();
    let real = wince_project(true, 1, 1, 1, 1, true);
    assert_step(&model, &prev, &real, "Complete");
    assert!(model.fire("Complete", &mut spec));
    assert_eq!(real, spec);

    // A second complete word inside the phrase window is a distinct, stronger
    // beat rather than a hold extension.
    let second_at = t0 + Duration::from_millis(320);
    assert!(cat.on_curse(second_at, 1));
    let second = cat.frame(second_at + Duration::from_millis(90));
    assert_eq!(second.reaction, CatReaction::Wince);
    assert!(
        second.pose.lead < first.pose.lead || second.pose.scale_y < first.pose.scale_y,
        "the second curse must build visible force: first={:?}, second={:?}",
        first.pose,
        second.pose
    );
    let prev = spec.clone();
    let real = wince_project(true, 1, 2, 2, 2, true);
    assert_step(&model, &prev, &real, "Complete");
    assert!(model.fire("Complete", &mut spec));
    assert_eq!(real, spec);

    // The reaction seam cannot summon a hidden companion.
    let mut hidden = CursorCat::default();
    assert!(!hidden.on_curse(first_at, 1));
    assert!(!hidden.is_active());
    let mut hidden_spec = model.init_state();
    let prev = hidden_spec.clone();
    assert!(model.fire("Hide", &mut hidden_spec));
    let hidden_real = wince_project(false, 0, 0, 0, 0, false);
    assert_step(&model, &prev, &hidden_real, "Hide");
    assert_eq!(hidden_real, hidden_spec);
    let prev = hidden_spec.clone();
    assert!(model.fire("HiddenComplete", &mut hidden_spec));
    let hidden_real = wince_project(false, 0, 1, 0, 0, false);
    assert_step(&model, &prev, &hidden_real, "HiddenComplete");
    assert_eq!(hidden_real, hidden_spec);
}

#[test]
fn real_cursor_cat_collectible_lifecycle_conforms_and_hidden_expiry_is_caught() {
    let model = cursor_cat_model();
    let mut cat = CursorCat::default();
    let look = KittyLook::default();
    let t0 = Instant::now();

    let mut spec = model.init_state();
    assert!(!cat.is_active(), "a fresh cursor cat is fully idle");

    // Collect -> Discovery. Fade-in starts at alpha zero at the exact event
    // timestamp, so bind visibility at the first ordinary 60 Hz present.
    let prev = spec.clone();
    cat.on_collect(t0, look);
    let collected = cat.frame(t0 + Duration::from_millis(16));
    assert!(cat.is_active(), "collection arms the render lifecycle");
    assert!(
        collected.alpha > 0,
        "the first present draws the collected cat"
    );
    assert!(
        collected.discovery,
        "collection enters the guaranteed hello"
    );
    assert_eq!(collected.reaction, CatReaction::Discovery);
    assert_eq!(
        collected.look,
        look.normalized(),
        "the collected identity survives"
    );
    let mut real = project(&collected, cat.is_active(), 0, 0, 0, true);
    assert_step(&model, &prev, &real, "Collect");
    assert!(model.fire("Collect", &mut spec));
    assert_eq!(real, spec);

    // The first two abstract ticks remain strictly inside the 2.8 s hold.
    // Momentum has decayed below the ordinary survival threshold by the second
    // sample; discovery alone is therefore what keeps this shipping cat live.
    for (elapsed, millis) in [(1, 1_000), (2, 2_799)] {
        let prev = real;
        let frame = cat.frame(t0 + Duration::from_millis(millis));
        assert!(frame.alpha > 0 && frame.discovery && cat.is_active());
        real = project(&frame, cat.is_active(), elapsed, elapsed, 0, true);
        assert_step(&model, &prev, &real, "Tick");
        assert!(model.fire("Tick", &mut spec));
        assert_eq!(real, spec);
    }

    // Negative control: after two real presented samples, a hidden wall-clock
    // sample consumes the remaining hold only under Buggy=1. Production must
    // preserve elapsed/presented/forced while making the host draw nothing.
    let before_hidden = real.clone();
    let mut expired_while_hidden = before_hidden.clone();
    expired_while_hidden.insert("elapsed", 3);
    expired_while_hidden.insert("hidden", 1);
    expired_while_hidden.insert("presentable", 0);
    expired_while_hidden.insert("visible", 0);
    expired_while_hidden.insert("phase", 2);
    expired_while_hidden.insert("forced", 0);
    let (bug_admits, bug_why) = verify::validate_transition_tiered(
        &model,
        &[("Buggy", 1)],
        &before_hidden,
        &expired_while_hidden,
        Some("HiddenTick"),
        "CursorCat hidden-expiry negative control",
    );
    assert!(
        bug_admits,
        "Buggy=1 must reproduce hidden expiry: {bug_why}"
    );
    let (healthy_admits, healthy_why) = verify::validate_transition_tiered(
        &model,
        &[],
        &before_hidden,
        &expired_while_hidden,
        Some("HiddenTick"),
        "CursorCat healthy rejection of hidden expiry",
    );
    assert!(
        !healthy_admits,
        "Buggy=0 accepted the hidden-expiry negative control: {healthy_why}"
    );

    let pause_at = t0 + Duration::from_millis(2_799);
    cat.set_collection_presentable(pause_at, false);
    let hidden_at = pause_at + Duration::from_secs(30);
    let hidden_frame = cat.frame(hidden_at);
    assert!(hidden_frame.discovery && cat.is_active());
    let prev = real;
    real = project(&hidden_frame, cat.is_active(), 2, 2, 1, false);
    assert_step(&model, &prev, &real, "HiddenTick");
    assert!(model.fire("HiddenTick", &mut spec));
    assert_eq!(real, spec);

    let resumed_at = hidden_at;
    cat.set_collection_presentable(resumed_at, true);

    // At the exact hold deadline the real machine may begin fading, remains
    // drawable during that fade, then returns all the way to zero-idle.
    for (elapsed, millis, should_be_active) in [(3, 1, true), (4, 401, true), (5, 801, false)] {
        let prev = real;
        let frame = cat.frame(resumed_at + Duration::from_millis(millis));
        assert!(!frame.discovery, "the forced hold is bounded");
        assert_eq!(cat.is_active(), should_be_active);
        assert_eq!(frame.alpha > 0, should_be_active);
        assert_eq!(
            frame.collection_hello, should_be_active,
            "the non-Nyan collection presentation remains drawable through fade"
        );
        real = project(&frame, cat.is_active(), elapsed, elapsed, 1, true);
        assert_step(&model, &prev, &real, "Tick");
        assert!(model.fire("Tick", &mut spec));
        assert_eq!(real, spec);
    }

    assert_eq!(real[&"phase"], 0);
    assert_eq!(real[&"visible"], 0);
    assert_eq!(real[&"forced"], 0);
    assert!(
        !cat.is_active(),
        "the bounded hello settles back to zero-idle"
    );
}
