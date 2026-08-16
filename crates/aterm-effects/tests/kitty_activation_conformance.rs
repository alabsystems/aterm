// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tier-1 binding for SING-ALONG activation thresholds.
//!
//! The derived models exhaust the bounded detector/count spaces. These tests
//! drive the genuine shipping state machines with injected clocks, project the
//! post-state at every abstract boundary, and validate representative traces in
//! both the embedded interpreter and external `ty` when it is installed.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use aterm_effects::kitty_cursor::{CursorCat, MIN_RUN_KEYS};
use aterm_effects::kitty_sing::{KittySing, SING_ARM_REPEATS, SING_REPEAT_GAP, SING_WIND_DOWN};
use aterm_spec::derive::{Model, cursor_cat_earn_floor_model, kitty_sing_detector_model};
use aterm_spec::{interp, verify};

type State = BTreeMap<&'static str, i64>;

fn model_const(model: &Model, name: &str) -> i64 {
    model
        .consts
        .iter()
        .find_map(|(candidate, value)| (*candidate == name).then_some(*value))
        .unwrap_or_else(|| panic!("{} has no constant {name}", model.name))
}

fn assert_exact_step(model: &Model, state: &mut State, post: State, action: &str) {
    assert!(
        model.action_enabled(action, state),
        "{}.{action} is disabled at {state:?}",
        model.name,
    );
    let mut expected = state.clone();
    assert!(model.fire(action, &mut expected));
    assert_eq!(
        post, expected,
        "real post-state diverged from {}.{action}",
        model.name,
    );
    *state = post;
}

fn assert_tiered_step(model: &Model, prev: &State, post: &State, action: &str, label: &str) {
    let (ok, why) = verify::validate_transition_tiered(model, &[], prev, post, Some(action), label);
    assert!(ok, "{label}: {why}");
}

fn sing_projection(detector: &KittySing, now: Instant, abstract_count: u32) -> State {
    let armed = detector.is_armed(now);
    let drive_live = detector.drive(now) > 0.0;
    let phase = if armed {
        1
    } else if drive_live {
        2
    } else {
        0
    };
    BTreeMap::from([
        ("phase", phase),
        // The run is semantically consumed once the celebration arms or winds
        // down. `abstract_count` is derived from the real committed presses,
        // while the public lifecycle observations choose the phase.
        (
            "count",
            if phase == 2 {
                0
            } else {
                i64::from(abstract_count)
            },
        ),
        ("drive_live", i64::from(drive_live)),
    ])
}

#[test]
fn real_kitty_sing_arm_break_release_and_finish_conform() {
    let model = kitty_sing_detector_model();
    assert_eq!(
        model_const(&model, "ArmRepeats"),
        i64::from(SING_ARM_REPEATS),
        "the formal arm threshold must be the shipping constant",
    );

    // An unarmed break discards the genuine detector's partial run.
    let mut partial = KittySing::default();
    let t0 = Instant::now();
    let mut partial_spec = model.init_state();
    for i in 1..=3u32 {
        partial.note_char(t0 + Duration::from_millis(u64::from(i) * 30), 7, 'x');
        let post = sing_projection(&partial, t0 + Duration::from_millis(u64::from(i) * 30), i);
        assert_exact_step(&model, &mut partial_spec, post, "Repeat");
    }
    let break_at = t0 + Duration::from_millis(120);
    let before_break = partial_spec.clone();
    partial.note_break(break_at);
    let after_break = sing_projection(&partial, break_at, 0);
    assert_exact_step(&model, &mut partial_spec, after_break.clone(), "Break");
    assert_tiered_step(
        &model,
        &before_break,
        &after_break,
        "Break",
        "real FULL-NYAN partial-run break",
    );

    // A complete held run stays unarmed through press fifteen and arms on the
    // sixteenth. Every post-state is projected from the real detector.
    let mut detector = KittySing::default();
    let t0 = Instant::now();
    let mut spec = model.init_state();
    for i in 1..=SING_ARM_REPEATS {
        let at = t0 + Duration::from_millis(u64::from(i) * 30);
        let prev = spec.clone();
        detector.note_char(at, 11, 'a');
        let post = sing_projection(&detector, at, i);

        if i == 8 {
            // Exact historical mutant: SING-ALONG armed on the eighth press.
            // The transition exists only under Buggy=1, and its post-state
            // demonstrably violates the healthy threshold invariant.
            let early = BTreeMap::from([("phase", 1), ("count", 8), ("drive_live", 1)]);
            assert!(!model.check_invariant("ArmedRequiresCurrentThreshold", &early));
            let (healthy_ok, _) = verify::validate_transition_tiered(
                &model,
                &[],
                &prev,
                &early,
                Some("Repeat"),
                "healthy rejection of the eight-press FULL-NYAN arm",
            );
            assert!(
                !healthy_ok,
                "the healthy detector admitted the old threshold"
            );
            let (buggy_ok, why) = verify::validate_transition_tiered(
                &model,
                &[("Buggy", 1)],
                &prev,
                &early,
                Some("Repeat"),
                "FULL-NYAN eight-press negative control",
            );
            assert!(buggy_ok, "Buggy=1 did not reproduce the old arm: {why}");
            let buggy = interp::with_buggy(&model, 1);
            assert!(!buggy.check_invariant("ArmedRequiresCurrentThreshold", &early));
            assert!(!detector.is_armed(at), "shipping code armed on press eight");
        }

        assert_exact_step(&model, &mut spec, post.clone(), "Repeat");
        if i == SING_ARM_REPEATS {
            assert_tiered_step(
                &model,
                &prev,
                &post,
                "Repeat",
                "real FULL-NYAN sixteenth-press arm",
            );
            assert!(detector.is_armed(at));
        } else {
            assert!(!detector.is_armed(at));
        }
    }

    // Eager release (Backspace) enters the live crossfade immediately.
    let release_at = t0 + Duration::from_millis(u64::from(SING_ARM_REPEATS + 1) * 30);
    let before_release = spec.clone();
    detector.note_backspace(release_at);
    let released = sing_projection(&detector, release_at, 0);
    assert_exact_step(&model, &mut spec, released.clone(), "Release");
    assert_tiered_step(
        &model,
        &before_release,
        &released,
        "Release",
        "real FULL-NYAN eager release",
    );
    assert!(!detector.is_armed(release_at));
    assert!(detector.drive(release_at) > 0.0, "release must crossfade");

    // The host's settle call after the bounded wind-down clears the detector to
    // its public byte-identical idle projection.
    let finish_at = release_at + Duration::from_secs_f32(SING_WIND_DOWN + 0.05);
    let before_finish = spec.clone();
    detector.settle(finish_at);
    let finished = sing_projection(&detector, finish_at, 0);
    assert_exact_step(&model, &mut spec, finished.clone(), "Finish");
    assert_tiered_step(
        &model,
        &before_finish,
        &finished,
        "Finish",
        "real FULL-NYAN wind-down finish",
    );
    assert_eq!(detector.drive(finish_at), 0.0);
}

#[test]
fn real_kitty_sing_lazy_release_maps_to_the_same_bounded_tail() {
    let model = kitty_sing_detector_model();
    let mut detector = KittySing::default();
    let t0 = Instant::now();
    let mut spec = model.init_state();
    let mut last = t0;
    for i in 1..=SING_ARM_REPEATS {
        last = t0 + Duration::from_millis(u64::from(i) * 30);
        detector.note_char(last, 13, 'w');
        let post = sing_projection(&detector, last, i);
        assert_exact_step(&model, &mut spec, post, "Repeat");
    }
    assert!(detector.is_armed(last));

    // No release event arrives when the finger simply lifts. The shipping
    // detector derives the edge at `last + SING_REPEAT_GAP`; project that
    // clock-driven post-state onto the same abstract Release action.
    let timeout = last + SING_REPEAT_GAP;
    let prev = spec.clone();
    let timed_out = sing_projection(&detector, timeout, 0);
    assert!(!detector.is_armed(timeout));
    assert!(detector.drive(timeout) > 0.0);
    assert_exact_step(&model, &mut spec, timed_out.clone(), "Release");
    assert_tiered_step(
        &model,
        &prev,
        &timed_out,
        "Release",
        "real FULL-NYAN lazy timeout release",
    );
}

fn cat_projection(singing: bool, abstract_run: u32, cat: &CursorCat) -> State {
    BTreeMap::from([
        ("singing", i64::from(singing)),
        ("run", i64::from(abstract_run)),
        ("active", i64::from(cat.is_active())),
    ])
}

#[test]
fn real_cursor_cat_singing_bypass_still_requires_sixteen_travel_events() {
    let model = cursor_cat_earn_floor_model();
    assert_eq!(
        model_const(&model, "MinRun"),
        i64::from(MIN_RUN_KEYS),
        "the formal cat floor must be the shipping constant",
    );

    let mut cat = CursorCat::default();
    let t0 = Instant::now();
    let mut spec = model.init_state();

    // Arm the celebration drive. It pins momentum but cannot itself summon.
    let before_begin = spec.clone();
    cat.set_singing(
        t0,
        aterm_effects::kitty_cursor::SingSync {
            drive: 1.0,
            beat: 0.0,
            energy: 0.30,
            class: 0,
            landing: false,
            fill: false,
            bow: 0.0,
        },
    );
    let begun = cat_projection(true, 0, &cat);
    assert_exact_step(&model, &mut spec, begun.clone(), "BeginSing");
    assert_tiered_step(
        &model,
        &before_begin,
        &begun,
        "BeginSing",
        "real cursor-cat singing bypass begin",
    );
    assert!(
        !cat.is_active(),
        "momentum pin alone must not summon the cat"
    );

    for i in 1..=MIN_RUN_KEYS {
        let at = t0 + Duration::from_millis(u64::from(i) * 40);
        let prev = spec.clone();
        cat.on_key(at, true);
        cat.set_singing(
            at,
            aterm_effects::kitty_cursor::SingSync {
                drive: 1.0,
                beat: i as f32 * 0.1,
                energy: 0.30,
                class: 0,
                landing: false,
                fill: false,
                bow: 0.0,
            },
        );
        let post = cat_projection(true, i, &cat);

        if i == 10 {
            // Exact v0.56 mutant: the cat appears at the old ten-key floor.
            let early = BTreeMap::from([("singing", 1), ("run", 10), ("active", 1)]);
            assert!(!model.check_invariant("NoCatBeforeSixteen", &early));
            let (healthy_ok, _) = verify::validate_transition_tiered(
                &model,
                &[],
                &prev,
                &early,
                Some("Qualify"),
                "healthy rejection of the v0.56 cursor-cat floor",
            );
            assert!(!healthy_ok, "the healthy model admitted a ten-key cat");
            let (buggy_ok, why) = verify::validate_transition_tiered(
                &model,
                &[("Buggy", 1)],
                &prev,
                &early,
                Some("Qualify"),
                "cursor-cat v0.56 ten-key negative control",
            );
            assert!(buggy_ok, "Buggy=1 did not reproduce the ten-key cat: {why}");
            let buggy = interp::with_buggy(&model, 1);
            assert!(!buggy.check_invariant("NoCatBeforeSixteen", &early));
            assert!(!cat.is_active(), "shipping cat appeared at the v0.56 floor");
        }

        assert_exact_step(&model, &mut spec, post.clone(), "Qualify");
        if i == MIN_RUN_KEYS {
            assert_tiered_step(
                &model,
                &prev,
                &post,
                "Qualify",
                "real cursor-cat sixteenth travel event",
            );
            assert!(cat.is_active(), "the sixteenth qualifying event summons");
        } else {
            assert!(!cat.is_active(), "cat appeared before event sixteen ({i})");
        }
    }
}
