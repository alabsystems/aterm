// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use std::collections::BTreeMap;

use crate::geom_24x80_for_test as output_streak_geom;

/// The chrome-decoration lane (Robi, the typed dog, the reduced-motion sing
/// still, the companion's wince follow-up) arms a deadline rather than
/// calling `request_redraw()` from the redraw handler, which winit's Windows
/// backend turns into an immediate re-invalidation and a free-running loop.
///
/// * A level, not a one-shot request: it survives parks
///   (`an_unrelated_wake_between_arm_and_fire_keeps_the_decoration_tick`)
///   but expires, so a decoration that stopped moving stops paying and idle
///   reaches zero.
/// * Phase-locked: the re-arm continues from the slot that fired, so the
///   animation holds the panel cadence instead of sliding by the frame cost.
#[test]
fn a_decoration_latch_is_a_bounded_level_on_the_shared_phase_locked_lane() {
    let mut app = App::headless_for_test();
    let ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless fixture owns window 0");
    assert!(
        ws.front_terminal().is_some(),
        "the fixture must front a terminal, or the arm gate below is vacuous"
    );
    let t0 = Instant::now();
    assert!(
        !ws.deco_anim_frame_active(t0),
        "a resting decoration holds the lane open for nothing: the loop parks"
    );

    // One animating frame. The hold is `DECO_ANIM_LEVEL_FRAMES` periods of
    // the window's LONGEST lane (`deco_anim_hold`), long enough to cover
    // arm -> fire -> next redraw even if a park stretches the lane meanwhile.
    let grace = ws.deco_anim_hold();
    ws.note_deco_animating(t0);
    assert!(ws.deco_anim_frame_active(t0));
    assert!(
        ws.deco_anim_frame_active(t0 + grace - Duration::from_millis(1)),
        "the level must outlive one lane period, or a park that redraws \
         nothing drops the tick the decoration armed"
    );
    assert!(
        !ws.deco_anim_frame_active(t0 + grace),
        "and it must expire, or a decoration that stopped moving would keep \
         the loop awake forever — the idle-to-zero law"
    );

    // Monotonic: a stale/earlier sample never shortens a hold a livelier
    // decoration already extended (two of them can draw on one frame).
    ws.note_deco_animating(t0 + grace);
    ws.note_deco_animating(t0);
    assert!(
        ws.deco_anim_frame_active(t0 + grace + grace - Duration::from_millis(1)),
        "the longer hold wins regardless of the order the arms run in"
    );

    // The wake it buys: the same deadline every other effect takes, phase
    // locked to the fired slot rather than to when this frame finished.
    // Negative control on the second assert — `presented + interval` (tick 6)
    // is exactly the completion-relative slide this lane exists to reject.
    let interval = Duration::from_millis(4);
    let base = Instant::now();
    let fired = base + interval;
    let presented = fired + Duration::from_millis(2);
    assert_eq!(
        phase_locked_effect_deadline(presented, interval, Some(fired), None, true),
        base + Duration::from_millis(8),
    );
    assert_ne!(
        phase_locked_effect_deadline(presented, interval, Some(fired), None, true),
        presented + interval,
    );
}

/// An event-loop turn that redraws some other window, or none, between the
/// arm and the fire keeps the armed decoration tick. The park arm calls
/// `park_terminal_effect_scheduler`, which drops `next_trail_tick`, and
/// `new_events` services deadlines only under `ResumeTimeReached`, so an
/// edge-triggered input would freeze Robi mid-stride (he has no focus-gated
/// or blink wake to revive him). Park repeatedly with nothing else live:
/// the armed slot stays, unmoved, and reaches the fold.
#[test]
fn an_unrelated_wake_between_arm_and_fire_keeps_the_decoration_tick() {
    let mut app = App::headless_for_test();
    let ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless fixture owns window 0");
    assert!(
        ws.front_terminal().is_some(),
        "the fixture must front a terminal, or the arm gate is vacuous"
    );
    let t0 = Instant::now();
    // `has_glass` stands in for `os_window.is_some()`: a test cannot
    // fabricate a winit Window, and every arm is vacuous without one.
    // Baseline: nothing at all is live, so the lane parks and owes no wake.
    assert_eq!(
        ws.plan_terminal_effect_lane(t0, false, true),
        None,
        "the fixture must be quiescent, or the arm below proves nothing"
    );
    assert_eq!(ws.next_trail_tick, None);

    // A redraw draws Robi mid-stride. The next park arms his slot.
    ws.note_deco_animating(t0);
    let armed = ws
        .plan_terminal_effect_lane(t0, false, true)
        .expect("an in-motion decoration owes the loop a frame");
    assert!(armed > t0, "the armed slot is strictly in the future");
    assert_eq!(ws.next_trail_tick, Some(armed));

    // Now the blocker. Five event-loop turns before the slot is due, none of
    // them a redraw of this window and none of them `ResumeTimeReached`, so
    // nothing refreshes the latch and nothing consumes the tick.
    let interval = ws.effect_present_interval();
    for step in 1..=5_u32 {
        let woke = t0 + interval / 8 * step;
        assert!(
            woke < armed,
            "the probe must stay inside the armed interval"
        );
        assert_eq!(
            ws.plan_terminal_effect_lane(woke, false, true),
            Some(armed),
            "an unrelated wake must neither park the decoration's tick nor \
             move it — this is the permanent freeze the review caught"
        );
        assert_eq!(ws.next_trail_tick, Some(armed));
    }

    // And it is bounded, not a train: the walk ends, no redraw refreshes the
    // latch, and the lane parks back to pure `Wait` within the grace.
    let grace = ws.deco_anim_hold();
    assert_eq!(
        ws.plan_terminal_effect_lane(t0 + grace, false, true),
        None,
        "a decoration that stopped moving stops paying"
    );
    assert_eq!(
        ws.next_trail_tick, None,
        "and the park drops the clock, so the loop returns to 0% idle"
    );
}

/// Prism Wake's sound episode outlives its visible comet without keeping
/// the frame train alive: the scheduler takes the engine's analytic Settle
/// crossing as one exact wake, then parks.
#[test]
fn a_parked_output_episode_wakes_once_for_settle() {
    let mut app = App::headless_for_test();
    let ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless fixture owns window 0");
    let t0 = Instant::now();
    assert_eq!(ws.plan_terminal_effect_lane(t0, false, true), None);

    let cfg = crate::output_streak::StreakConfig {
        enabled: true,
        sound: true,
        idle_secs: 10.0,
        ..crate::output_streak::StreakConfig::default()
    };
    let geom = output_streak_geom();
    let mut engine = crate::output_streak::OutputStreak::new(23);
    assert!(!engine.note_output(1, &[(4, 0, 40)], t0, false));
    let spawned = t0 + Duration::from_millis(350);
    assert!(engine.note_output(2, &[(4, 0, 40)], spawned, false));
    let mut quads = Vec::new();
    assert_eq!(
        engine
            .tick(spawned, geom, &cfg, &mut quads)
            .cue
            .map(|cue| cue.sound),
        Some(crate::output_streak::StreakSound::Shimmer)
    );
    for step in 1..=4 {
        quads.clear();
        engine.tick(
            spawned + Duration::from_millis(250 * step),
            geom,
            &cfg,
            &mut quads,
        );
    }
    let parked = spawned + Duration::from_secs(1);
    assert!(!engine.is_active());
    let settle = engine
        .next_change_deadline(parked, cfg.idle_secs)
        .expect("the open episode owes its closing cue");
    assert!(settle > parked);
    *ws.fx.streak_slot() = Some(Box::new(engine));
    ws.deco_anim_until = None;

    assert_eq!(
        ws.plan_terminal_effect_lane(parked, false, true),
        Some(settle),
        "the quiet interval owns one exact deadline, not frame cadence"
    );
    assert!(ws.service_due_terminal_effect_tick(settle));

    let fired = settle + Duration::from_millis(1);
    quads.clear();
    let frame = ws
        .fx
        .streak_slot()
        .as_deref_mut()
        .expect("engine survives until its promised wake")
        .tick(fired, geom, &cfg, &mut quads);
    assert_eq!(
        frame.cue.map(|cue| cue.sound),
        Some(crate::output_streak::StreakSound::Settle)
    );
    assert_eq!(
        ws.plan_terminal_effect_lane(fired, false, true),
        None,
        "after Settle the lane returns to pure Wait"
    );
}

/// Single-pane and composed Prism Wake engines are retained separately, and
/// only the route on glass contributes a deadline: a hidden route's comet
/// is not ticked, so its `now` deadline would loop without draining.
#[test]
fn a_hidden_output_streak_route_cannot_pin_the_scheduler() {
    fn active_engine(now: Instant, seed: u64) -> crate::output_streak::OutputStreak {
        let mut engine = crate::output_streak::OutputStreak::new(seed);
        let cfg = crate::output_streak::StreakConfig {
            enabled: true,
            ..crate::output_streak::StreakConfig::default()
        };
        assert!(!engine.note_output(1, &[(4, 0, 40)], now, false));
        let spawned = now + Duration::from_millis(350);
        assert!(engine.note_output(2, &[(4, 0, 40)], spawned, false));
        engine.tick(spawned, output_streak_geom(), &cfg, &mut Vec::new());
        assert!(engine.is_active());
        engine
    }

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = Instant::now() + Duration::from_secs(1);
    {
        let ws = app.windows.get_mut(&wid).expect("headless window");
        assert!(!ws.is_split());
        ws.fx
            .streak_panes_mut()
            .insert((99, 0), active_engine(now, 1));
        assert_eq!(
            ws.output_streak_next_change_deadline(now, 10.0),
            None,
            "composed state is hidden on the single-pane route"
        );
        *ws.fx.streak_slot() = Some(Box::new(active_engine(now, 2)));
        assert_eq!(ws.output_streak_next_change_deadline(now, 10.0), Some(now));
    }

    app.split_active_stub_tab(wid);
    let ws = app.windows.get_mut(&wid).expect("headless window");
    assert!(ws.is_split());
    ws.fx.clear_streak_panes();
    assert_eq!(
        ws.output_streak_next_change_deadline(now, 10.0),
        None,
        "single-pane state is hidden on the composed route"
    );
}

/// The frame-cadence arm clamps an inherited `next_trail_tick` to its own
/// cadence. The else-if arm can leave a slot bounded only by the
/// reduced-motion cat's `discovery_until` (seconds out), which would pace the
/// decoration at seconds and outlive `deco_anim_until`, so a later park would
/// drop it and freeze the lane. Clamped, the tick that refreshes the latch
/// is always due inside it.
#[test]
fn a_decoration_arm_never_inherits_a_slot_that_outlives_its_own_latch() {
    let mut app = App::headless_for_test();
    let ws = app
        .windows
        .get_mut(&WindowId(0))
        .expect("the headless fixture owns window 0");
    let t0 = Instant::now();
    let interval = ws.effect_present_interval();

    // Stand in for the reduced-motion cat's one-shot erase, armed by the
    // else-if on an earlier park and still pending when a decoration moves.
    let far = t0 + Duration::from_secs(3);
    ws.next_trail_tick = Some(far);
    ws.note_deco_animating(t0);
    let d = ws
        .plan_terminal_effect_lane(t0, false, true)
        .expect("an in-motion decoration owes the loop a frame");
    assert!(
        d <= t0 + interval,
        "a frame-cadence arm may not inherit a coarser slot"
    );
    assert!(
        d < far,
        "the bounded erase can only land EARLIER than it asked, never later"
    );
    assert_eq!(ws.next_trail_tick, Some(d));
    assert!(
        ws.deco_anim_frame_active(d),
        "and the latch must still be live when its own slot fires — that is \
         the coupling the clamp exists to guarantee"
    );
}

/// The decoration lane arms before the RepaintKey early-out, so it arms a
/// deadline instead of re-requesting a redraw: a walk step that bakes to the
/// same sprite is correctly not presented, and a wake requested only after a
/// present would be lost on exactly those frames. The deadline is armed by
/// the frame that drew him, presented or not.
///
/// Scope: the pure helpers only (`take_due_trail_tick`,
/// `rebase_pending_trail_tick_after_present`, `phase_locked_effect_deadline`),
/// pinning that no present is required to keep the train phased. The
/// arm/park branch is covered by
/// `an_unrelated_wake_between_arm_and_fire_keeps_the_decoration_tick`.
#[test]
fn an_early_out_frame_still_owes_the_decoration_its_next_slot() {
    let interval = Duration::from_millis(4);
    let base = Instant::now();
    let fired = base + interval;

    // `new_events` fired the slot and handed ownership to the anchor; the
    // redraw it requested then early-outed, so no present ever ran and
    // `rebase_pending_trail_tick_after_present` had nothing to consume.
    let mut next = Some(fired);
    let mut anchor = None;
    assert!(take_due_trail_tick(&mut next, &mut anchor, fired));
    let early_out_at = fired + Duration::from_millis(1);
    rebase_pending_trail_tick_after_present(&mut next, &mut anchor, early_out_at);
    assert_eq!(
        (next, anchor),
        (None, Some(fired)),
        "an early-out leaves the fired anchor exactly as the timer set it"
    );

    // `about_to_wait` re-arms from the decoration request alone. Same slot,
    // same cadence a presented frame would have produced — the animation
    // cannot stall on a repeated fingerprint.
    let last_fire = anchor.take();
    assert_eq!(
        phase_locked_effect_deadline(early_out_at, interval, last_fire, None, true),
        base + Duration::from_millis(8),
    );
}

#[test]
fn only_a_due_tick_transfers_ownership_to_the_fired_anchor() {
    let base = Instant::now();
    let fired = base + Duration::from_millis(2);
    let mut next = Some(fired);
    let mut anchor = None;
    assert!(!take_due_trail_tick(&mut next, &mut anchor, base));
    assert_eq!((next, anchor), (Some(fired), None));
    assert!(take_due_trail_tick(&mut next, &mut anchor, fired));
    assert_eq!((next, anchor), (None, Some(fired)));
    assert!(!take_due_trail_tick(
        &mut next,
        &mut anchor,
        fired + Duration::from_millis(1)
    ));
    assert_eq!((next, anchor), (None, Some(fired)));
}

#[test]
fn shared_success_ack_rebases_timers_but_content_gates_the_pacing_stamp() {
    let mut app = App::headless_for_test();
    let id = WindowId(0);
    let base = Instant::now();
    let previous_content = base;
    let frame = base + Duration::from_millis(10);
    let presented = frame + Duration::from_millis(2);
    let pending_tick = frame + Duration::from_millis(1);
    let monitor_interval = Duration::from_millis(8);
    let app_interval = Duration::from_millis(16);

    let state = app.windows.get_mut(&id).expect("bootstrap window");
    state.last_present_at = Some(previous_content);
    state.content_pending = false;
    state.input_hot = true;
    state.next_trail_tick = Some(pending_tick);
    state.last_trail_fire = Some(base);
    state.capture_present_serial = 9;
    state.frame_interval = Some(monitor_interval);
    assert!(
        state
            .present_retry
            .on_drop(metrics::PresentDropReason::CpuAcquire, base)
            .is_some(),
        "prime a retry episode"
    );

    let outcome = acknowledge_successful_present(state, app_interval, frame, presented);
    assert_eq!(
        outcome.frame_interval, monitor_interval,
        "the source window owns the load-shed budget"
    );
    assert!(!outcome.offscreen);
    assert_eq!(state.next_trail_tick, None);
    assert_eq!(
        state.last_trail_fire,
        Some(frame),
        "a useful present consumes and rebases the pending effect tick"
    );
    assert_eq!(
        state.last_present_at,
        Some(previous_content),
        "an effect/native-only frame cannot poison PTY output pacing"
    );
    assert!(
        state.input_hot,
        "the keystroke bypass remains armed until its content is displayed"
    );
    assert_eq!(state.capture_present_serial, 10);
    assert_eq!(
        state.present_retry,
        crate::PresentRetry::default(),
        "any real successful submission acknowledges retry recovery"
    );

    let second_frame = frame + Duration::from_millis(20);
    let second_presented = second_frame + Duration::from_millis(3);
    state.content_pending = true;
    state.input_hot = true;
    let serial = state.capture_present_serial;
    let fired_anchor = state.last_trail_fire;
    acknowledge_successful_present(state, app_interval, second_frame, second_presented);
    assert_eq!(
        state.last_trail_fire, fired_anchor,
        "a timer-driven frame with no pending slot preserves its fired anchor"
    );
    assert_eq!(state.last_present_at, Some(second_presented));
    assert!(!state.content_pending);
    assert!(!state.input_hot);
    assert_eq!(state.capture_present_serial, serial + 1);
}

#[test]
fn every_successful_route_replaces_capture_visuals_instead_of_inheriting_them() {
    let mut app = App::headless_for_test();
    let id = WindowId(0);
    let frame = Instant::now();
    let stale = OverlayGlow {
        accent: 0x0012_3456,
        wash_a: 17,
        border_a: 203,
        border_scale_q4: 0,
    };
    let plan = app.active_visible_leaf_plan(id).expect("visible plan");

    app.finalize_successful_present(
        id,
        crate::metrics::StartupPresentTiming::collapsed(frame),
        0,
        None,
        SuccessfulPresentRoute::Terminal,
        crate::VisibleContentRoute::Terminal { composed: false },
        HostVisualState {
            invert: true,
            overlay: Some(stale),
        },
        &plan,
    );
    let terminal_serial = app.windows[&id].capture_present_serial;
    assert!(app.windows[&id].capture_present_invert);
    assert_eq!(app.windows[&id].capture_present_overlay, Some(stale));

    app.finalize_successful_present(
        id,
        crate::metrics::StartupPresentTiming::collapsed(frame + Duration::from_millis(1)),
        0,
        None,
        SuccessfulPresentRoute::Heterogeneous,
        crate::VisibleContentRoute::Heterogeneous,
        HostVisualState::default(),
        &plan,
    );
    let state = &app.windows[&id];
    assert_eq!(state.capture_present_serial, terminal_serial + 1);
    assert!(
        !state.capture_present_invert && state.capture_present_overlay.is_none(),
        "a later mixed/native frame explicitly clears stale terminal chrome"
    );
}

/// The pool-park latch hears both routes that draw cursor effects: a mixed
/// native + terminal tab whose focused leaf is a terminal runs the same
/// effect lane and drawable pool, so a heterogeneous present that waited
/// [`crate::EFFECT_LANE_ACQUIRE_PARK_NS`] for a drawable leaves the lane at
/// the halving as a terminal present does. Driven through the shared
/// success boundary with the wait injected; one nanosecond short of the
/// threshold is the negative control on each route.
#[test]
fn both_effect_drawing_routes_feed_the_pool_park_latch() {
    struct ClearInjectedWait;
    impl Drop for ClearInjectedWait {
        fn drop(&mut self) {
            super::ACQUIRE_WAIT_NS_FOR_TEST.with(|wait| wait.set(None));
        }
    }
    let _clear = ClearInjectedWait;
    let hz60 = Duration::from_nanos(1_000_000_000_000 / 60_000);
    let halved =
        crate::effect_present_interval_with(Some(hz60), crate::EFFECT_PRESENT_PANEL_PERIODS);
    let routes = [
        (
            SuccessfulPresentRoute::Terminal,
            crate::VisibleContentRoute::Terminal { composed: false },
        ),
        (
            SuccessfulPresentRoute::Heterogeneous,
            crate::VisibleContentRoute::Heterogeneous,
        ),
    ];
    for (route, visible_route) in routes {
        for (wait_ns, parks) in [
            (crate::EFFECT_LANE_ACQUIRE_PARK_NS - 1, false),
            (crate::EFFECT_LANE_ACQUIRE_PARK_NS, true),
        ] {
            let mut app = App::headless_for_test();
            let id = WindowId(0);
            app.windows
                .get_mut(&id)
                .expect("the headless fixture owns window 0")
                .frame_interval = Some(hz60);
            let before = app.windows[&id].effect_present_interval();
            let plan = app.active_visible_leaf_plan(id).expect("visible plan");
            super::ACQUIRE_WAIT_NS_FOR_TEST.with(|wait| wait.set(Some(wait_ns)));
            app.finalize_successful_present(
                id,
                crate::metrics::StartupPresentTiming::collapsed(Instant::now()),
                0,
                None,
                route,
                visible_route,
                HostVisualState::default(),
                &plan,
            );
            let state = &app.windows[&id];
            assert_eq!(
                state.effect_lane_parked, parks,
                "{route:?}: a present whose acquire waited {wait_ns} ns"
            );
            assert_eq!(
                state.effect_present_interval(),
                if parks { halved } else { before },
                "{route:?}: a parked present leaves the lane at the halving on every host"
            );
        }
    }
}

#[test]
fn coarse_engine_deadline_is_preserved_exactly() {
    let now = Instant::now();
    let coarse = now + Duration::from_millis(90);
    assert_eq!(
        phase_locked_effect_deadline(
            now,
            Duration::from_millis(16),
            Some(now),
            Some(coarse),
            false,
        ),
        coarse
    );
}

#[test]
fn content_present_rebase_conforms_to_derived_model() {
    let model = aterm_spec::derive::effect_present_rebase_model();
    let state = |pending: i64, anchor: i64, after_content: i64, steps: i64| {
        BTreeMap::from([
            ("pending", pending),
            ("anchor", anchor),
            ("after_content", after_content),
            ("steps", steps),
        ])
    };
    let base = Instant::now();
    let frame = base + Duration::from_millis(2);
    let mut next = None;
    let mut anchor = None;

    // Reach the pending state through the model's real Arm transition;
    // the former test started at pending=1,steps=0, which is unreachable.
    let initial = state(0, 0, 0, 0);
    assert_eq!(model.init_state(), initial);
    assert_eq!((next, anchor), (None, None));
    next = Some(base + Duration::from_millis(3));
    let armed = state(1, 0, 0, 1);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &initial,
        &armed,
        Some("Arm"),
        "effect-present arm conformance",
    );
    assert!(ok, "shipping arm transition rejected: {why}");

    rebase_pending_trail_tick_after_present(&mut next, &mut anchor, frame);
    let anchor_tick = anchor
        .expect("a useful present becomes the cadence anchor")
        .duration_since(base)
        .as_millis() as i64;
    let projected = state(i64::from(next.is_some()), anchor_tick, 1, 2);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &armed,
        &projected,
        Some("ContentPresent"),
        "effect-present rebase conformance",
    );
    assert!(ok, "shipping rebase transition rejected: {why}");

    let stale = state(1, 2, 1, 2);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &armed,
        &stale,
        Some("ContentPresent"),
        "effect-present rebase negative control",
    );
    assert!(!ok, "leaving the stale timer armed must fail conformance");

    let unanchored = state(0, 0, 1, 2);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &armed,
        &unanchored,
        Some("ContentPresent"),
        "effect-present anchor negative control",
    );
    assert!(
        !ok,
        "clearing the timer without rebasing its anchor must fail"
    );
}

#[test]
fn phase_locked_deadline_conforms_to_derived_model() {
    let model = aterm_spec::derive::effect_phase_lock_model();
    let state = |now: i64,
                 anchor: i64,
                 schedule_base: i64,
                 deadline: i64,
                 pending: i64,
                 rendering: i64,
                 phase_locked: i64,
                 fresh: i64| {
        BTreeMap::from([
            ("now", now),
            ("anchor", anchor),
            ("schedule_base", schedule_base),
            ("deadline", deadline),
            ("pending", pending),
            ("rendering", rendering),
            ("phase_locked", phase_locked),
            ("fresh", fresh),
        ])
    };
    let base = Instant::now();
    let interval = Duration::from_millis(2);
    let actual = phase_locked_effect_deadline(
        base + Duration::from_millis(3),
        interval,
        Some(base + Duration::from_millis(2)),
        None,
        true,
    );
    let actual_tick = actual.duration_since(base).as_millis() as i64;
    let prev = state(3, 2, 0, 2, 0, 1, 0, 0);
    let next = state(3, 2, 2, actual_tick, 1, 0, 1, 1);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &prev,
        &next,
        Some("Rearm"),
        "effect phase-lock conformance",
    );
    assert!(ok, "shipping phase-lock transition rejected: {why}");

    let slid = state(3, 2, 3, 5, 1, 0, 1, 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &prev,
        &slid,
        Some("Rearm"),
        "effect phase-lock negative control",
    );
    assert!(!ok, "render-cost cadence slide must fail conformance");
}

#[test]
fn event_loop_fire_present_rearm_trace_conforms_end_to_end() {
    let model = aterm_spec::derive::effect_phase_lock_model();
    let state = |now: i64,
                 anchor: i64,
                 schedule_base: i64,
                 deadline: i64,
                 pending: i64,
                 rendering: i64,
                 phase_locked: i64,
                 fresh: i64| {
        BTreeMap::from([
            ("now", now),
            ("anchor", anchor),
            ("schedule_base", schedule_base),
            ("deadline", deadline),
            ("pending", pending),
            ("rendering", rendering),
            ("phase_locked", phase_locked),
            ("fresh", fresh),
        ])
    };
    let validate = |before: &BTreeMap<&'static str, i64>,
                    after: &BTreeMap<&'static str, i64>,
                    action: &'static str,
                    label: &'static str| {
        let (ok, why) = aterm_spec::verify::validate_transition_tiered(
            &model,
            &[],
            before,
            after,
            Some(action),
            label,
        );
        assert!(ok, "shipping {action} transition rejected: {why}");
    };

    let base = Instant::now();
    let interval = Duration::from_millis(2);
    let fired = base + interval;
    let mut next = Some(fired);
    let mut anchor = None;

    let mut reachable = model.init_state();
    assert!(model.fire("Tick", &mut reachable));
    assert!(model.fire("Tick", &mut reachable));
    let before_fire = state(2, 0, 0, 2, 1, 0, 1, 0);
    assert_eq!(
        reachable, before_fire,
        "the on-time Fire prestate is reachable"
    );

    // `new_events`: transfer the due timer's exact deadline into the
    // cadence anchor before requesting the redraw.
    assert!(take_due_trail_tick(&mut next, &mut anchor, fired));
    assert_eq!((next, anchor), (None, Some(fired)));
    let after_fire = state(2, 2, 0, 2, 0, 1, 0, 0);
    validate(
        &before_fire,
        &after_fire,
        "Fire",
        "real event-loop trail fire",
    );

    // The actual present path runs while the timer is disarmed. Its
    // post-present rebase must preserve the fired anchor.
    let presented = fired + Duration::from_millis(1);
    rebase_pending_trail_tick_after_present(&mut next, &mut anchor, presented);
    assert_eq!((next, anchor), (None, Some(fired)));
    let after_render = state(3, 2, 0, 2, 0, 1, 0, 0);
    validate(
        &after_fire,
        &after_render,
        "RenderCost",
        "real timer-driven present cost",
    );

    // `about_to_wait`: consume the transferred anchor and arm the next
    // phase-locked slot. A completion-relative implementation would return
    // tick 5 here and is rejected by the model.
    let last_fire = anchor.take();
    let deadline = phase_locked_effect_deadline(presented, interval, last_fire, None, true);
    next = Some(deadline);
    assert_eq!(next, Some(deadline));
    assert_eq!(deadline, base + Duration::from_millis(4));
    let rearmed = state(3, 2, 2, 4, 1, 0, 1, 1);
    validate(
        &after_render,
        &rearmed,
        "Rearm",
        "real event-loop trail rearm",
    );

    let slid = state(3, 2, 3, 5, 1, 0, 1, 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &after_render,
        &slid,
        Some("Rearm"),
        "event-loop phase-slide negative control",
    );
    assert!(!ok, "completion-relative event-loop rearm must be rejected");

    // Late delivery plus an overloaded present is the other real branch.
    // The due helper retains the original deadline as anchor, while re-arm
    // starts a fresh full interval once the next phase slot is already past.
    let late_base = Instant::now();
    let late_fired = late_base + interval;
    let delivered = late_base + Duration::from_millis(5);
    let mut late_next = Some(late_fired);
    let mut late_anchor = None;
    let mut late_reachable = model.init_state();
    for _ in 0..5 {
        assert!(model.fire("Tick", &mut late_reachable));
    }
    let late_before_fire = state(5, 0, 0, 2, 1, 0, 1, 0);
    assert_eq!(
        late_reachable, late_before_fire,
        "the late Fire prestate is reachable"
    );
    assert!(take_due_trail_tick(
        &mut late_next,
        &mut late_anchor,
        delivered
    ));
    assert_eq!((late_next, late_anchor), (None, Some(late_fired)));
    let late_after_fire = state(5, 2, 0, 2, 0, 1, 0, 0);
    validate(
        &late_before_fire,
        &late_after_fire,
        "Fire",
        "late event-loop trail fire",
    );

    let late_presented = late_base + Duration::from_millis(6);
    rebase_pending_trail_tick_after_present(&mut late_next, &mut late_anchor, late_presented);
    assert_eq!((late_next, late_anchor), (None, Some(late_fired)));
    let late_after_render = state(6, 2, 0, 2, 0, 1, 0, 0);
    validate(
        &late_after_fire,
        &late_after_render,
        "RenderCost",
        "overloaded timer-driven present cost",
    );

    let late_last_fire = late_anchor.take();
    let late_deadline =
        phase_locked_effect_deadline(late_presented, interval, late_last_fire, None, true);
    late_next = Some(late_deadline);
    assert_eq!(late_next, Some(late_deadline));
    assert_eq!(
        late_deadline,
        late_base + Duration::from_millis(8),
        "overload starts one fresh full interval"
    );
    let late_rearmed = state(6, 2, 6, 8, 1, 0, 0, 1);
    validate(
        &late_after_render,
        &late_rearmed,
        "Rearm",
        "overloaded event-loop trail rearm",
    );

    // A catch-up rearm at the already-passed phase slot is the overload
    // counterpart to the completion-relative slide above.
    let catch_up = state(6, 2, 2, 4, 1, 0, 0, 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &late_after_render,
        &catch_up,
        Some("Rearm"),
        "event-loop catch-up negative control",
    );
    assert!(!ok, "an already-due catch-up rearm must be rejected");
}
