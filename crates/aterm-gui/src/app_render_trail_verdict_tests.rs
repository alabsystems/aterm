// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{keyed_celebration_completion, verdict_armed, verdict_voice};
use std::time::{Duration, Instant};

#[test]
fn external_celebration_wakes_initial_green_then_only_bars_outro_and_settle() {
    use aterm_effects::kitty_sing::{
        CelebrateOn, SING_BAR_SECONDS, SING_WIND_DOWN, song_signature,
    };
    let mut app = crate::App::headless_for_test();
    let ws = app.windows.get_mut(&crate::WindowId(0)).unwrap();
    ws.focused = true;
    let start = Instant::now();
    let sid = ws.front_terminal().unwrap().session;
    ws.fx
        .companions_mut()
        .kitty_sing
        .arm_external(start, sid, song_signature('C'), 2, CelebrateOn::Green)
        .unwrap();
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(start, false, false, true, false, 10.0, true),
        None
    );
    assert!(keyed_celebration_completion(
        &mut ws.fx.companions_mut().kitty_sing,
        &mut ws.verdict_spent,
        sid,
        Some(start),
        start,
        0,
    ));
    // Real policy gates win even when an external run has an event due.
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(start, false, false, true, false, 10.0, false),
        None
    );
    ws.focused = false;
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(start, false, false, true, false, 10.0, true),
        None
    );
    ws.focused = true;
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(start, false, false, false, false, 10.0, true),
        None
    );
    let interval = crate::effect_present_interval(ws.frame_interval);
    let mut now = start + interval;
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(start, false, false, true, false, 10.0, true),
        Some(now),
        "on=green owns an initial followup at the existing frame cap"
    );
    // A synchronized-output hold can consume the scheduler slot while
    // deferring the song pass. Its still-due event must rearm in the future.
    assert!(ws.service_due_terminal_effect_tick(now));
    assert_eq!(
        ws.fx
            .companions_mut()
            .kitty_sing
            .next_external_deadline(now),
        Some(now)
    );
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(now, false, false, true, false, 10.0, true),
        Some(now + interval),
        "deferred consumption cannot spin on a deadline at now"
    );
    assert!(!ws.service_due_terminal_effect_tick(now));
    now += interval;
    for phase in 0..=3 {
        assert_eq!(
            ws.plan_terminal_effect_lane_with_recording(now, false, false, true, false, 10.0, true),
            Some(now),
            "an already-armed due edge retains its exact deadline"
        );
        assert!(ws.service_due_terminal_effect_tick(now));
        if phase < 2 {
            ws.fx.companions_mut().sing_riff_bar = ws.fx.companions_mut().kitty_sing.bar(now);
            assert_eq!(ws.fx.companions_mut().sing_riff_bar, Some(phase));
            assert_eq!(
                ws.fx
                    .companions_mut()
                    .kitty_sing
                    .take_bar_fan(now)
                    .unwrap()
                    .bar,
                phase
            );
        } else if phase == 2 {
            assert!(ws.fx.companions_mut().kitty_sing.take_outro(now).is_some());
        } else {
            ws.fx.companions_mut().kitty_sing.settle(now);
        }
        let next =
            ws.plan_terminal_effect_lane_with_recording(now, false, false, true, false, 10.0, true);
        assert!(
            !ws.deco_anim_frame_active(now),
            "external scheduling grants no frame train"
        );
        assert!(
            !ws.fx.companion_mut().brain().needs_frames(),
            "a song does not grant a static pet frame debt"
        );
        if phase < 3 {
            let exact = if phase < 2 {
                start + Duration::from_secs_f32((phase + 1) as f32 * SING_BAR_SECONDS)
            } else {
                start
                    + Duration::from_secs_f32(2.0 * SING_BAR_SECONDS)
                    + Duration::from_secs_f32(SING_WIND_DOWN)
            };
            assert_eq!(next, Some(exact));
            assert_eq!(
                ws.plan_terminal_effect_lane_with_recording(
                    now + Duration::from_millis(1),
                    false,
                    false,
                    true,
                    false,
                    10.0,
                    true
                ),
                Some(exact),
                "unrelated event-loop turns keep the same coarse deadline"
            );
            now = exact;
        } else {
            assert_eq!(next, None);
            assert!(
                ws.next_trail_tick.is_none(),
                "settlement parks the real scheduler"
            );
        }
    }
}

fn keyed_celebration_model() -> aterm_spec::derive::Model {
    aterm_spec::ty_model! {
        KeyedCelebrationCompletion {
            const Buggy = 0;
            var credit = 0;
            var live = 0;
            var authorized = 0;
            action Enter when (live == 0 && credit == 0) {
                credit = 1;
            }
            action Fail when (live == 0 && credit == 1) {
                credit = 0;
            }
            action Green when (live == 0 && (credit == 1 || Buggy == 1)) {
                authorized = credit;
                credit = 0;
                live = 1;
            }
            invariant ARunNeedsItsOwnKeyedSuccess: live == 0 || authorized == 1;
            invariant Bounds: credit <= 1 && live <= 1 && authorized <= 1;
        }
    }
}

#[test]
fn fx_celebration_completion_guard_conforms_to_real_detector() {
    use aterm_effects::kitty_sing::{CelebrateOn, KittySing, song_signature};
    let model = keyed_celebration_model();
    let now = Instant::now();
    let sid = 7;
    for credit in [false, true] {
        for failed in [false, true] {
            let mut sing = KittySing::default();
            sing.arm_external(now, sid, song_signature('C'), 2, CelebrateOn::Green)
                .expect("explicit arm admitted");
            let mut spent = (!credit).then_some((sid, now));
            let mut expected = model.init_state();
            if credit {
                assert!(model.fire("Enter", &mut expected));
            }
            let action = if failed { "Fail" } else { "Green" };
            let enabled = model.action_enabled(action, &expected);
            let admitted = keyed_celebration_completion(
                &mut sing,
                &mut spent,
                sid,
                Some(now),
                now,
                i32::from(failed),
            );
            assert_eq!(
                admitted, enabled,
                "shipping boundary guard follows the model"
            );
            if enabled {
                assert!(model.fire(action, &mut expected));
            }
            let mut actual = expected.clone();
            actual.insert("live", i64::from(sing.external_live()));
            assert_eq!(
                actual, expected,
                "the real detector fires only on the licensed success"
            );
            // Negative control: a run forced live with no keyed success (what an unguarded
            // detector would do) breaks the invariant.
            if !credit && !failed {
                actual.insert("live", 1);
                assert!(!model.check_invariant("ARunNeedsItsOwnKeyedSuccess", &actual));
            }
        }
    }
}

#[test]
fn fx_celebration_green_requires_one_fresh_successful_keyed_completion() {
    use aterm_effects::kitty_sing::{CelebrateOn, KittySing, song_signature};
    let now = Instant::now();
    let sid = 7;
    let mut sing = KittySing::default();
    sing.arm_external(now, sid, song_signature('C'), 2, CelebrateOn::Green)
        .expect("the explicit arm is admitted");
    let mut spent = None;
    // Unkeyed marks and another pane's completion leave this arm intact.
    assert!(!keyed_celebration_completion(
        &mut sing, &mut spent, sid, None, now, 0
    ));
    assert!(keyed_celebration_completion(
        &mut sing,
        &mut spent,
        sid + 1,
        Some(now),
        now,
        0
    ));
    assert!(!sing.external_live());
    assert!(sing.armed_external().is_some());
    // A failed command spends its Enter. A second, successful mark for
    // that same Enter must not retroactively celebrate the failure.
    assert!(keyed_celebration_completion(
        &mut sing,
        &mut spent,
        sid,
        Some(now),
        now,
        1
    ));
    assert!(!keyed_celebration_completion(
        &mut sing,
        &mut spent,
        sid,
        Some(now),
        now,
        0
    ));
    assert!(!sing.external_live());
    let next = now + Duration::from_secs(1);
    assert!(keyed_celebration_completion(
        &mut sing,
        &mut spent,
        sid,
        Some(next),
        next,
        0
    ));
    assert!(
        sing.external_live(),
        "positive control reaches the real detector"
    );
    assert!(sing.armed_external().is_none());
    assert_eq!(sing.bar(next), Some(0));
    let later = next + Duration::from_secs(2);
    assert!(!keyed_celebration_completion(
        &mut sing,
        &mut spent,
        sid,
        Some(next),
        later,
        0
    ));
    assert_eq!(
        sing.bar(later),
        Some(1),
        "a repeated mark cannot re-anchor the song"
    );
}

/// A `D` mark speaks only when armed by an accepted plain-Enter turn
/// boundary, and the first `D` after it spends the arm; program output alone
/// never arms it.
#[test]
fn a_command_that_speaks_was_run_by_a_keystroke() {
    let t = Instant::now();
    let mut spent = None;

    assert!(
        !verdict_armed(&mut spent, 7, None),
        "a D with no Enter behind it at all is silent"
    );
    assert_eq!(spent, None, "…and it spends nothing either");

    // You press Enter; the command runs; the `D` comes back.
    let enter = t;
    assert!(
        verdict_armed(&mut spent, 7, Some(enter)),
        "a D behind a keyed Enter speaks"
    );

    // The shell repeats its marks — a redraw, a resumed session, a second
    // `D` for the one command. One Enter, one verdict.
    assert!(
        !verdict_armed(&mut spent, 7, Some(enter)),
        "the same Enter cannot buy a second verdict"
    );

    // Another window's session, same instant: the arm is per session, so
    // a tab switch re-baselines instead of replaying.
    assert!(
        verdict_armed(&mut spent, 9, Some(enter)),
        "another session's Enter is another session's verdict"
    );

    // …and the next real Enter arms again.
    assert!(
        verdict_armed(&mut spent, 9, Some(enter + Duration::from_secs(1))),
        "a new Enter is a new verdict"
    );
}

/// Under two seconds a successful command is silent; a failure always
/// speaks, and a success past half a minute rings the ice bell.
#[test]
fn a_quick_command_is_silent() {
    assert_eq!(
        verdict_voice(false, Some(120)),
        None,
        "`ls` says nothing: it answered, it did not work"
    );
    assert_eq!(
        verdict_voice(false, Some(1_999)),
        None,
        "…and neither does anything under two seconds"
    );
    assert_eq!(
        verdict_voice(false, None),
        None,
        "a shell that reported no timestamps is treated as quick, not long"
    );
    assert_eq!(
        verdict_voice(false, Some(2_000)),
        Some((false, false)),
        "two seconds is the wall, and it speaks the plagal cadence"
    );
    assert_eq!(
        verdict_voice(false, Some(30_000)),
        Some((false, true)),
        "half a minute was a BUILD: the ice bell rings over the resolution"
    );
    assert_eq!(
        verdict_voice(true, Some(8)),
        Some((true, false)),
        "a failure always speaks, however fast — and never rings the bell"
    );
    assert_eq!(
        verdict_voice(true, Some(600_000)),
        Some((true, false)),
        "…including a ten-minute one: the bell is the green cadence's"
    );
}
