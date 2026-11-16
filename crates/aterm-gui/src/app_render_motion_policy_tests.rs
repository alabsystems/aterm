// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CursorFxInputs, PERF_HYSTERESIS_FRAMES, causal_render_cost_ns};
use crate::App;
use crate::motion::{MotionEffect, MotionPolicy};
use std::time::{Duration, Instant};

#[test]
fn app_motion_policy_folds_config_flag_and_focus() {
    let mut app = App::headless_for_test();
    assert_eq!(app.motion_policy(true), MotionPolicy::Full);
    app.system_reduce_motion = true;
    assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
    assert_eq!(
        app.motion_policy(true).amplitude(MotionEffect::CursorGlow),
        0.0,
        "a Reduced policy zeroes the aurora amplitude"
    );
    app.config.motion = Some("full".into());
    assert_eq!(app.motion_policy(true), MotionPolicy::Full);
    assert_eq!(app.motion_policy(false), MotionPolicy::Reduced);
    app.system_reduce_motion = false;
    app.config.motion = Some("reduced".into());
    assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
}

#[test]
fn typed_wake_pins_cursor_fx_focus() {
    use std::time::{Duration, Instant};

    use crate::WindowId;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    assert!(!app.cursor_fx_focus(wid, false, t0));
    assert_eq!(
        app.motion_policy(app.cursor_fx_focus(wid, false, t0)),
        MotionPolicy::Reduced,
        "an idle unfocused window stays demoted"
    );
    app.windows.get_mut(&wid).expect("window").last_key_at = Some(t0);
    assert!(
        app.cursor_fx_focus(wid, false, t0 + Duration::from_millis(1)),
        "a live typed wake pins the focus input"
    );
    assert_eq!(
        app.motion_policy(app.cursor_fx_focus(wid, false, t0 + Duration::from_millis(1))),
        MotionPolicy::Full,
        "a typed-into unfocused window animates its earned trail"
    );
    assert!(
        !app.cursor_fx_focus(WindowId(1), false, t0 + Duration::from_millis(1)),
        "the wake is per-window, not app-global"
    );
    assert!(
        !app.cursor_fx_focus(
            wid,
            false,
            t0 + super::CURSOR_FX_TYPED_WAKE + Duration::from_millis(1)
        ),
        "an expired wake demotes again"
    );
    app.system_reduce_motion = true;
    assert_eq!(
        app.motion_policy(app.cursor_fx_focus(wid, false, t0 + Duration::from_millis(1))),
        MotionPolicy::Reduced,
        "OS Reduce-Motion still demotes a typed-into window"
    );
}

#[test]
fn typed_wake_lets_an_unfocused_window_paint_its_trail() {
    use std::time::{Duration, Instant};

    use super::CursorFxInputs;
    use crate::WindowId;
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("comet".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    let t0 = Instant::now();
    app.windows.get_mut(&wid).expect("window").focused = false;

    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    let seeded = app.cursor_frame(wid, seed).expect("seed cursor engines");
    assert!(
        !seeded.win_focused,
        "unfocused + idle: W11b demotes (negative control)"
    );

    let typed = t0 + Duration::from_millis(1);
    {
        let mut ws = app.windows.get_mut(&wid).expect("window");
        ws.last_key_at = Some(typed);
        ws.fx.cursor_mut().cadence.on_keystroke(typed);
        ws.fx.cursor_mut().glow.note_synthetic_move(typed);
        ws.fx.cursor_mut().trail.note_synthetic_move(typed);
    }
    let mut live = CursorFxInputs::sample_for_test(t0 + Duration::from_millis(2));
    live.cur = Some((2, 5));
    let live_tick = app.cursor_frame(wid, live).expect("live cursor tick");
    assert!(
        live_tick.win_focused,
        "the typed wake pins the tick's focus fold"
    );
    assert_eq!(live_tick.motion, MotionPolicy::Full);
    let ws = app.windows.get(&wid).expect("window");
    assert!(
        !ws.fx.cursor().glow.under_laid().is_empty(),
        "an unfocused window paints the trail its typing earned \
         (the v0.48–v0.50 real-window blackout frame)"
    );

    let mut idle = CursorFxInputs::sample_for_test(
        typed + super::CURSOR_FX_TYPED_WAKE + Duration::from_millis(1),
    );
    idle.cur = Some((2, 5));
    let idle_tick = app.cursor_frame(wid, idle).expect("idle tick");
    assert!(
        !idle_tick.win_focused,
        "an idle unfocused window demotes again (FL-1 settle intact)"
    );
    let ws = app.windows.get(&wid).expect("window");
    assert!(
        ws.fx.cursor().glow.under_laid().is_empty() && ws.fx.cursor().glow_laid().is_empty(),
        "the drained unfocused window holds no resident trail"
    );
}

#[test]
fn typed_wake_arms_the_frame_train_on_an_unfocused_window() {
    use std::time::{Duration, Instant};

    use super::CursorFxInputs;
    use crate::WindowId;
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("comet".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    let t0 = Instant::now();
    app.windows.get_mut(&wid).expect("window").focused = false;

    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("seed cursor engines");

    let typed = t0 + Duration::from_millis(1);
    {
        let mut ws = app.windows.get_mut(&wid).expect("window");
        ws.last_key_at = Some(typed);
        ws.fx.cursor_mut().cadence.on_keystroke(typed);
        ws.fx.cursor_mut().glow.note_synthetic_move(typed);
        ws.fx.cursor_mut().trail.note_synthetic_move(typed);
    }
    let mut live = CursorFxInputs::sample_for_test(t0 + Duration::from_millis(2));
    live.cur = Some((2, 5));
    app.cursor_frame(wid, live).expect("live cursor tick");

    let at = t0 + Duration::from_millis(3);
    let ws = app.windows.get(&wid).expect("window");
    assert!(
        ws.cursor_fx_active(at, false),
        "precondition: the lane HAS work to schedule (glow={} trail={})",
        ws.fx.cursor().glow.is_active(),
        ws.fx.cursor().trail.is_active(),
    );
    assert!(
        ws.terminal_effect_frame_active(at, false),
        "the typed wake arms the frame train, not just the paint policy"
    );

    app.windows.get_mut(&wid).expect("window").last_key_at = None;
    let ws = app.windows.get(&wid).expect("window");
    assert!(ws.cursor_fx_active(at, false), "the engines are still live");
    assert!(
        !ws.terminal_effect_frame_active(at, false),
        "unfocused and untyped: W11b parks the 60 fps lane"
    );

    let late = typed + super::CURSOR_FX_TYPED_WAKE + Duration::from_millis(1);
    app.windows.get_mut(&wid).expect("window").last_key_at = Some(typed);
    let ws = app.windows.get(&wid).expect("window");
    assert!(
        ws.cursor_fx_active(late, false),
        "the engine set is still charged at the expiry instant"
    );
    assert!(
        !ws.terminal_effect_frame_active(late, false),
        "an expired wake parks the lane again (FL-1's settle)"
    );

    for probe in [t0, typed, typed + Duration::from_secs(1), late] {
        let ws = app.windows.get(&wid).expect("window");
        assert_eq!(
            ws.cursor_fx_typed_wake(probe),
            app.cursor_fx_focus(wid, false, probe),
            "policy and scheduler read one predicate at {probe:?}"
        );
    }
}

#[test]
fn trail_status_reads_the_focus_the_frame_path_used() {
    use std::time::Instant;

    use crate::WindowId;
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    let wid = WindowId(0);
    assert_eq!(
        app.frontmost_window,
        Some(wid),
        "the fixture's front window"
    );

    {
        let mut ws = app.windows.get_mut(&wid).expect("window");
        ws.focused = false;
        ws.last_key_at = None;
    }
    let idle = app.trail_status().expect("a front window exists");
    for key in [" focused=false", " motion_stage=reduced", " intensity=0.00"] {
        assert!(idle.contains(key), "missing {key:?} in {idle}");
    }

    app.windows.get_mut(&wid).expect("window").last_key_at = Some(Instant::now());
    let typed = app.trail_status().expect("a front window exists");
    for key in [" focused=true", " motion_stage=full"] {
        assert!(typed.contains(key), "missing {key:?} in {typed}");
    }
    assert!(
        !typed.contains(" intensity=0.00"),
        "a typed-into window is not dark: {typed}"
    );
}

#[test]
fn trail_status_intensity_folds_the_enable_gate() {
    use crate::WindowId;
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail_style = Some("rainbow".into());
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").focused = true;

    app.config.cursor_trail = Some(false);
    let dark = app.trail_status().expect("a front window exists");
    for key in [
        " config_enabled=false",
        " effective=false",
        " intensity=0.00",
        " glow_active=false",
    ] {
        assert!(dark.contains(key), "missing {key:?} in {dark}");
    }

    app.config.cursor_trail = Some(true);
    let lit = app.trail_status().expect("a front window exists");
    assert!(lit.contains(" effective=true"), "{lit}");
    assert!(
        !lit.contains(" intensity=0.00"),
        "an enabled, focused, full-motion trail is not dark: {lit}"
    );
}

#[test]
fn video_recording_pins_motion_focus() {
    use crate::WindowId;
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").focused = false;
    assert!(!app.motion_focus(wid, false));
    assert_eq!(
        app.motion_policy(app.motion_focus(wid, false)),
        MotionPolicy::Reduced,
        "unfocused + unrecorded stays demoted"
    );
    let root =
        std::env::temp_dir().join(format!("aterm-motion-focus-video-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    crate::control_auth::ensure_private_dir(&root).unwrap();
    let dir = crate::control_auth::confine_video_dir(&root).unwrap();
    let (reply, _rx) = std::sync::mpsc::channel();
    app.video_rec = Some(crate::VideoRec {
        window: wid,
        deadline: std::time::Instant::now(),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        unlogged_over_cap: 0,
        mode: crate::VideoMode::SwapchainTap,
        next_frame: None,
        presented: None,
        dir,
        handoff: None,
        cancel: crate::VideoCancellation::new(),
        reply,
    });
    assert!(
        app.motion_focus(wid, false),
        "recording pins the focus input"
    );
    assert_eq!(
        app.motion_policy(app.motion_focus(wid, false)),
        MotionPolicy::Full,
        "a recorded unfocused window animates"
    );
    assert!(
        app.video_rec
            .as_ref()
            .is_some_and(|rec| rec.next_frame.is_none()),
        "fixture is the formerly clockless unpaced windowed recording"
    );

    let tick_at = Instant::now();
    let mut seed = CursorFxInputs::sample_for_test(tick_at);
    seed.cur = Some((2, 3));
    app.cursor_frame(wid, seed).expect("seed tick");
    {
        let mut ws = app.windows.get_mut(&wid).expect("window");
        ws.fx.cursor_mut().glow.note_typed_cells(tick_at, 1);
        ws.fx.cursor_mut().trail.note_typed(tick_at);
    }
    let mut moved = CursorFxInputs::sample_for_test(tick_at + Duration::from_millis(1));
    moved.cur = Some((2, 4));
    app.cursor_frame(wid, moved).expect("licensed tick");
    let mut ws = app.windows.get_mut(&wid).expect("window");
    assert!(ws.fx.cursor_mut().glow.is_active() || ws.fx.cursor_mut().trail.is_active());
    assert!(
        ws.plan_terminal_effect_lane_with_recording(
            tick_at + Duration::from_millis(2),
            false,
            false,
            true,
            true,
            10.0,
            true,
        )
        .is_some(),
        "the motion-focus watcher owns a matching frame clock"
    );
    ws.park_terminal_effect_scheduler();
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(
            tick_at + Duration::from_millis(2),
            false,
            false,
            true,
            false,
            10.0,
            true,
        ),
        None,
        "without OS focus, typed wake, or recording watcher no new clock is minted"
    );
    drop(ws);
    assert!(
        !app.motion_focus(WindowId(1), false),
        "the pin is per-window, not app-global"
    );
    app.system_reduce_motion = true;
    assert_eq!(
        app.motion_policy(app.motion_focus(wid, false)),
        MotionPolicy::Reduced,
        "OS Reduce-Motion still demotes a recorded window"
    );
    drop(app);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn the_load_shed_switch_is_inert_exactly_where_the_mode_refuses_the_shed() {
    use crate::motion::MotionMode;
    let now = std::time::Instant::now();
    for mode in [MotionMode::Auto, MotionMode::Full, MotionMode::Reduced] {
        let gate = |adaptive: bool| {
            let mut app = App::headless_for_test();
            app.perf_reduced = true;
            app.config.load_adaptive_motion = Some(adaptive);
            (
                app.load_shed_active_with(mode),
                app.effective_shed_envelope(mode, now).to_bits(),
            )
        };
        assert_eq!(
            gate(true) == gate(false),
            !mode.admits_load_shed(),
            "{mode:?}: the switch is inert exactly where the mode refuses the shed"
        );
    }
}

#[test]
fn perf_reduced_hysteresis_and_motion_fold() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    app.config.motion = Some("auto".into());
    app.system_reduce_motion = false;
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Full,
        "baseline: an unloaded focused window animates"
    );

    let fi = Duration::from_millis(16);
    let slow = 25_000_000u64; // 25 ms render — just over the 24 ms shed threshold.
    let fast = 0u64; // an instant render — well under the 12.8 ms clear threshold.
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    assert!(
        !app.note_present_cost(slow, fi, at(0)),
        "one slow frame is not a transition"
    );
    assert!(
        !app.perf_reduced,
        "hysteresis: a lone slow present does not shed"
    );
    assert_eq!(app.motion_policy(true), MotionPolicy::Full);
    assert!(
        !app.note_present_cost(slow, fi, at(16)),
        "second slow frame: still no transition"
    );
    assert!(!app.perf_reduced);
    assert!(
        app.note_present_cost(slow, fi, at(32)),
        "K-th slow frame flips the latch"
    );
    assert!(app.perf_reduced, "sustained overload sheds effects");
    assert!(
        app.load_shed_active(),
        "the raw latch is effective under default auto policy"
    );

    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Reduced,
        "a load-shed window forces Reduced under the default (auto) motion policy"
    );
    assert_eq!(
        app.motion_policy(true).amplitude(MotionEffect::CursorGlow),
        0.0,
        "shed ⇒ the aurora amplitude is exactly 0 (the proven Reduced state)"
    );

    app.config.motion = Some("full".into());
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Full,
        "motion = \"full\" overrides the shed latch (explicit always-animate)"
    );
    assert!(
        !app.load_shed_active(),
        "direct effect gates must honor the same Full override"
    );
    app.config.motion = Some("auto".into());
    app.config.load_adaptive_motion = Some(false);
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Full,
        "load_adaptive_motion = false opts out of shedding entirely"
    );
    assert!(
        !app.load_shed_active(),
        "direct effect gates must honor the adaptive opt-out"
    );
    app.config.load_adaptive_motion = None;
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Reduced,
        "default policy still sheds while latched"
    );
    assert!(app.load_shed_active());

    assert!(
        !app.note_present_cost(fast, fi, at(48)),
        "one fast frame is not a transition"
    );
    assert!(
        app.perf_reduced,
        "hysteresis: a lone fast present does not re-engage"
    );
    assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
    assert!(
        !app.note_present_cost(fast, fi, at(64)),
        "second fast frame: still shed"
    );
    assert!(
        !app.note_present_cost(fast, fi, at(80)),
        "K-th fast frame INSIDE the dwell must not clear"
    );
    assert!(app.perf_reduced, "dwell holds the latch shed");
    assert!(
        app.note_present_cost(fast, fi, at(32 + 1600)),
        "a fast frame past the dwell clears the latch"
    );
    assert!(!app.perf_reduced, "sustained recovery re-engages effects");
    assert!(!app.load_shed_active());
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Full,
        "cleared latch restores the config-resolved policy"
    );
}

#[test]
fn a_single_spike_present_never_sheds() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    app.config.motion = Some("auto".into());
    app.system_reduce_motion = false;

    let fi = Duration::from_millis(16);
    let steady = 1_000_000u64; // 1 ms — a healthy frame.
    let spike = 250_000_000u64; // 250 ms — a theme-reload invalidation storm.
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    for i in 0..4 {
        assert!(!app.note_present_cost(steady, fi, at(i * 16)));
    }
    assert!(
        !app.note_present_cost(spike, fi, at(64)),
        "the spike itself is not a transition"
    );
    for i in 0..5 {
        assert!(
            !app.note_present_cost(steady, fi, at(80 + i * 16)),
            "a healthy frame in the EMA's decay tail is not a transition"
        );
        assert!(
            !app.perf_reduced,
            "a lone spike must never latch the shed, however large"
        );
        assert_eq!(
            app.motion_policy(true),
            MotionPolicy::Full,
            "a theme switch never costs the kitty her rainbow"
        );
    }
}

#[test]
fn gpu_submit_cpu_work_is_part_of_the_load_shed_feedback_signal() {
    use std::time::Duration;

    let fi = Duration::from_millis(16);
    let compose_ns = 2_000_000;
    let heavy_gpu_submit_cpu_ns = 28_000_000;
    let reduced_gpu_submit_cpu_ns = 1_000_000;
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    let mut compose_only = App::headless_for_test();
    for frame in 0..PERF_HYSTERESIS_FRAMES {
        assert!(!compose_only.note_present_cost(compose_ns, fi, at(u64::from(frame) * 16)));
    }
    assert!(
        !compose_only.perf_reduced,
        "compose-only accounting is blind to GPU-submit CPU work"
    );

    let mut app = App::headless_for_test();
    let heavy = causal_render_cost_ns(compose_ns, heavy_gpu_submit_cpu_ns);
    assert_eq!(heavy, 30_000_000);
    for frame in 1..=PERF_HYSTERESIS_FRAMES {
        assert_eq!(
            app.note_present_cost(heavy, fi, at(u64::from(frame) * 16)),
            frame == PERF_HYSTERESIS_FRAMES,
            "only the debounced final heavy frame may flip the latch"
        );
    }
    assert!(
        app.perf_reduced,
        "causal GPU-submit CPU work trips effect shedding"
    );

    let reduced = causal_render_cost_ns(compose_ns, reduced_gpu_submit_cpu_ns);
    assert!(reduced < heavy, "shedding must lower its own causal signal");
    let mut cleared = false;
    for i in 0..8u64 {
        if app.note_present_cost(reduced, fi, at(2000 + i * 16)) {
            cleared = true;
            break;
        }
    }
    assert!(cleared, "sustained causal recovery clears the shed latch");
    assert!(!app.perf_reduced);
}

/// NEGATIVE CONTROL: a re-shed at or past `PERF_RESHED_QUICK_WINDOW` resets the dwell to
/// `PERF_SHED_DWELL_MIN`, while quick re-sheds double up to `PERF_SHED_DWELL_MAX`, giving an exact
/// closed-form transition and unshed-frame trajectory over a sustained oscillator.
#[test]
fn shed_latch_flap_backoff_bounds_transitions() {
    use crate::app_render::{
        PERF_HYSTERESIS_FRAMES, PERF_RESHED_QUICK_WINDOW, PERF_SHED_DWELL_MAX, PERF_SHED_DWELL_MIN,
        next_shed_dwell,
    };
    use std::time::Duration;

    assert_eq!(
        next_shed_dwell(PERF_SHED_DWELL_MAX, None),
        PERF_SHED_DWELL_MIN
    );
    assert_eq!(
        next_shed_dwell(
            PERF_SHED_DWELL_MIN,
            Some(PERF_RESHED_QUICK_WINDOW - Duration::from_nanos(1)),
        ),
        PERF_SHED_DWELL_MIN.saturating_mul(2),
    );
    assert_eq!(
        next_shed_dwell(Duration::MAX, Some(Duration::ZERO)),
        PERF_SHED_DWELL_MAX,
    );
    assert_eq!(
        next_shed_dwell(PERF_SHED_DWELL_MAX, Some(PERF_RESHED_QUICK_WINDOW)),
        PERF_SHED_DWELL_MIN,
    );

    let mut app = App::headless_for_test();
    let fi = Duration::from_millis(16);
    let t0 = std::time::Instant::now();
    let heavy = 30_000_000u64;
    let cheap = 1_000_000u64;
    let mut transitions = 0u32;
    let mut shed_frames = 0u32;
    let total_frames = 60_000 / 16; // one simulated minute at 60 Hz
    for frame in 0..total_frames {
        let now = t0 + Duration::from_millis(u64::from(frame) * 16);
        let cost = if app.perf_reduced { cheap } else { heavy };
        if app.note_present_cost(cost, fi, now) {
            transitions += 1;
        }
        shed_frames += u32::from(app.perf_reduced);
    }

    let mut expected_frame = PERF_HYSTERESIS_FRAMES - 1;
    let mut expected_transitions = 1u32;
    let mut expected_unshed_frames = PERF_HYSTERESIS_FRAMES - 1;
    let mut expected_dwell = next_shed_dwell(PERF_SHED_DWELL_MIN, None);
    let fi_ns = fi.as_nanos();
    loop {
        let dwell_frames = u32::try_from(expected_dwell.as_nanos().div_ceil(fi_ns)).unwrap();
        let clear_frame = expected_frame + dwell_frames;
        if clear_frame >= total_frames {
            break;
        }
        expected_transitions += 1;
        expected_unshed_frames += PERF_HYSTERESIS_FRAMES.min(total_frames - clear_frame);
        let reshed_frame = clear_frame + PERF_HYSTERESIS_FRAMES;
        if reshed_frame >= total_frames {
            break;
        }
        expected_transitions += 1;
        expected_dwell = next_shed_dwell(expected_dwell, Some(fi * PERF_HYSTERESIS_FRAMES));
        expected_frame = reshed_frame;
    }

    assert_eq!(
        transitions, expected_transitions,
        "oscillator transitions match the closed-form backoff trajectory"
    );
    assert_eq!(
        shed_frames,
        total_frames - expected_unshed_frames,
        "shed frame count matches total frames minus hysteresis unshed windows"
    );
    assert_eq!(
        app.perf_shed_dwell, PERF_SHED_DWELL_MAX,
        "repeated quick flaps saturate the dwell at PERF_SHED_DWELL_MAX"
    );
    assert_eq!(app.perf_shed_dwell, expected_dwell);
}

#[test]
fn a_shed_clears_at_a_sparse_keystroke_echo_cadence() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    app.config.motion = Some("auto".into());
    app.system_reduce_motion = false;
    let fi = Duration::from_millis(16);
    let t0 = std::time::Instant::now();
    for frame in 0..u64::from(PERF_HYSTERESIS_FRAMES) {
        app.note_present_cost(30_000_000, fi, t0 + Duration::from_millis(frame * 16));
    }
    assert!(
        app.perf_reduced,
        "fixture: sustained overload latched the shed"
    );
    assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
    let shed_at = app.perf_flip_at.expect("the latch records its flip");

    let cadence = Duration::from_millis(400);
    let deadline =
        shed_at + crate::app_render::PERF_SHED_DWELL_MIN + cadence * PERF_HYSTERESIS_FRAMES;
    let mut at = shed_at + cadence;
    assert!(
        at < shed_at + crate::app_render::PERF_SHED_DWELL_MIN,
        "fixture: the first echo lands inside the dwell"
    );
    let mut cleared_at = None;
    while at <= deadline {
        if app.note_present_cost(1_000_000, fi, at) {
            cleared_at = Some(at);
            break;
        }
        at += cadence;
    }
    let cleared_at = cleared_at.unwrap_or_else(|| {
        panic!("sparse echoes never cleared the shed within dwell + hysteresis")
    });
    assert!(
        cleared_at >= shed_at + crate::app_render::PERF_SHED_DWELL_MIN,
        "…and never before the anti-flap dwell"
    );
    assert!(!app.perf_reduced);
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Full,
        "motion is handed back once the latch clears"
    );
}

#[test]
fn a_performance_shed_never_pins_the_pet_and_preserves_head_policy() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    app.config.motion = Some("auto".into());
    app.system_reduce_motion = false;
    let fi = Duration::from_millis(16);
    let t0 = std::time::Instant::now();
    for frame in 0..u64::from(PERF_HYSTERESIS_FRAMES) {
        app.note_present_cost(30_000_000, fi, t0 + Duration::from_millis(frame * 16));
    }
    assert!(app.load_shed_active(), "fixture: the latch engaged");
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Reduced,
        "the head's fold"
    );
    assert_eq!(app.accessibility_motion_policy(true), MotionPolicy::Full);
    assert!(
        app.resident_pet_motion(),
        "the shed never pins the resident"
    );

    app.system_reduce_motion = true;
    assert!(!app.resident_pet_motion(), "OS Reduce Motion pins it");
    app.system_reduce_motion = false;
    app.config.motion = Some("reduced".into());
    assert!(!app.resident_pet_motion(), "motion = reduced pins it");
    app.config.motion = Some("auto".into());
    app.set_serious_mode(true);
    assert!(!app.resident_pet_motion(), "serious mode retires it");
}

#[test]
fn shed_envelope_fades_and_anchors_mid_fade() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    let fi = Duration::from_millis(16);
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    assert_eq!(app.shed_envelope(at(0)), 1.0, "no flip yet: full amplitude");
    for f in 0..3u64 {
        app.note_present_cost(30_000_000, fi, at(f * 16));
    }
    assert!(app.perf_reduced, "latched at t=32ms");
    assert_eq!(app.shed_envelope(at(32)), 1.0);
    let mid = app.shed_envelope(at(32 + 225));
    assert!(
        (0.3..0.7).contains(&mid),
        "mid-fade is a partial amplitude, not a step (got {mid})"
    );
    assert_eq!(
        app.shed_envelope(at(32 + 1000)),
        0.0,
        "fade-out completes to the proven zero"
    );
    for f in 0..8u64 {
        app.note_present_cost(1_000_000, fi, at(2000 + f * 16));
    }
    assert!(!app.perf_reduced, "restored past the dwell");
    let rising = app.shed_envelope(at(2128 + 100));
    assert!(
        rising > 0.0 && rising < 1.0,
        "restore ramps back up (got {rising})"
    );
    assert_eq!(
        app.shed_envelope(at(2032 + 1000)),
        1.0,
        "fade-in completes to full amplitude"
    );
}

#[test]
fn effective_shed_envelope_preserves_mid_fade_reversal_and_user_opt_outs() {
    use std::time::Duration;

    let mut app = App::headless_for_test();
    app.config.motion = Some("auto".into());
    app.config.load_adaptive_motion = Some(true);
    let t0 = std::time::Instant::now();
    let mid = t0 + Duration::from_millis(180);
    let auto = crate::motion::MotionMode::Auto;

    app.flip_shed_latch(t0, true);
    let falling = app.effective_shed_envelope(auto, mid);
    assert!(falling > 0.0 && falling < 1.0, "fixture is mid-fade");
    let tick = app
        .cursor_frame(crate::WindowId(0), CursorFxInputs::sample_for_test(mid))
        .expect("shared cursor-family tick");
    assert_eq!(tick.shed_envelope.to_bits(), falling.to_bits());
    assert_eq!(
        tick.cursor_motion,
        MotionPolicy::Full,
        "accessibility/focus motion stays live while adaptive shed rides the envelope"
    );
    assert_eq!(
        tick.motion,
        MotionPolicy::Reduced,
        "non-cursor decorations retain the adaptive latch's hard shed"
    );

    app.flip_shed_latch(mid, false);
    assert_eq!(
        app.effective_shed_envelope(auto, mid).to_bits(),
        falling.to_bits(),
        "a reversal anchors at the exact presented amplitude"
    );
    assert!(
        app.effective_shed_envelope(auto, mid + Duration::from_millis(40)) > falling,
        "the reversed envelope rises continuously"
    );

    app.config.motion = Some("full".into());
    assert_eq!(
        app.effective_shed_envelope(crate::motion::MotionMode::Full, mid),
        1.0
    );
    app.config.motion = Some("auto".into());
    app.config.load_adaptive_motion = Some(false);
    assert_eq!(app.effective_shed_envelope(auto, mid), 1.0);
}

#[test]
fn motion_transition_settlement_invalidates_retained_native_frame() {
    use crate::WindowId;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
    let stamp = app.native_ui_compile_stamp(wid).unwrap();
    let compiled = app.compiled_native_ui(wid).unwrap();
    app.windows.get_mut(&wid).unwrap().native_ui_compiled =
        Some(crate::app_native::NativeCompiledFrame {
            stamp,
            phase: crate::app_native::NativeCompiledPhase::Presented,
            compiled,
        });

    app.settle_motion_policy_transition();

    assert!(
        app.windows.get(&wid).unwrap().native_ui_compiled.is_none(),
        "the post-present latch edge must not retain a full-motion native frame"
    );
}

#[test]
fn load_shed_latch_never_demotes_smooth_scroll() {
    use crate::{WindowId, term_lock};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.motion = Some("auto".into());
    app.config.load_adaptive_motion = None;
    app.system_reduce_motion = false;
    app.windows.get_mut(&wid).unwrap().focused = true;
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    {
        let mut t = term_lock(&term);
        for i in 0..60 {
            t.process(format!("line {i}\r\n").as_bytes());
        }
        assert!(
            t.grid().scrollback_lines() >= 10,
            "test needs real scrollback"
        );
    }

    let fi = Duration::from_micros(8_333);
    let t0 = Instant::now();
    for frame in 0..PERF_HYSTERESIS_FRAMES {
        app.note_present_cost(
            13_000_000,
            fi,
            t0 + Duration::from_millis(u64::from(frame) * 8),
        );
    }
    assert!(app.load_shed_active(), "control: the latch engaged");
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Reduced,
        "control: the decorative fold is Reduced under the latch"
    );
    assert_eq!(
        app.effect_policy(MotionEffect::CursorGlow, true),
        MotionPolicy::Reduced,
        "control: a decorative effect resolves through the latch"
    );
    assert_eq!(
        app.effect_policy(MotionEffect::SmoothScroll, true),
        MotionPolicy::Full
    );
    assert_eq!(
        app.effect_policy(MotionEffect::ScrollPill, true),
        MotionPolicy::Full
    );
    assert!(app.smooth_scroll_animates(wid));

    let offset_before = term_lock(&term).grid().display_offset();
    let wheel_at = Instant::now();
    app.scroll_wheel_animated(wid, &term, 3);
    assert!(
        app.windows[&wid].scroll_glide.is_some(),
        "a wheel notch under the load-shed latch must arm the glide"
    );
    assert_eq!(
        term_lock(&term).grid().display_offset(),
        offset_before,
        "the notch eases; it does not snap the engine whole-row"
    );
    app.tick_scroll_glide(wid, wheel_at + Duration::from_millis(90));
    assert!(
        app.windows[&wid].scroll_glide.is_some(),
        "the mid-ease tick must not land the glide under the latch"
    );
    app.windows.get_mut(&wid).unwrap().scroll_frac_px = 5;
    app.set_scroll_band(wid);
    assert_eq!(
        app.windows[&wid].input_scratch.scroll_frac_px, 5,
        "the sub-row band must be presented under the latch"
    );
    assert!(
        !app.settle_scroll_motion_if_reduced(wid, Instant::now()),
        "the shared settle resolves Full and leaves the glide alone"
    );
    for frame in 0..PERF_HYSTERESIS_FRAMES {
        app.note_present_cost(
            0,
            fi,
            t0 + Duration::from_millis(2_000 + u64::from(frame) * 8),
        );
    }
    assert!(!app.load_shed_active(), "control: the latch cleared");
    for frame in 0..PERF_HYSTERESIS_FRAMES {
        app.note_present_cost(
            13_000_000,
            fi,
            t0 + Duration::from_millis(2_100 + u64::from(frame) * 8),
        );
    }
    assert!(app.load_shed_active(), "control: the latch re-engaged");
    assert!(
        app.windows[&wid].scroll_glide.is_some(),
        "a shed edge must not land a glide in flight"
    );

    app.system_reduce_motion = true;
    assert_eq!(
        app.effect_policy(MotionEffect::SmoothScroll, true),
        MotionPolicy::Reduced
    );
    assert!(
        app.settle_scroll_motion_if_reduced(wid, Instant::now()),
        "OS Reduce Motion lands the glide"
    );
    assert!(app.windows[&wid].scroll_glide.is_none());
    assert_eq!(app.windows[&wid].scroll_frac_px, 0);
    app.set_scroll_band(wid);
    assert_eq!(app.windows[&wid].input_scratch.scroll_frac_px, 0);
    app.system_reduce_motion = false;
    assert_eq!(
        app.effect_policy(MotionEffect::SmoothScroll, false),
        MotionPolicy::Reduced
    );
}

#[cfg(a11y_tree)]
#[test]
fn a_load_flip_at_a_present_keeps_the_page_for_a_client() {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    app.first_present_done = true;
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::About));
    let (_, view) = app.active_native_view(wid).unwrap();
    app.windows.get_mut(&wid).unwrap().a11y_active = true;
    assert!(app.prepare_native_input_scratch(wid));
    let (plan, route) = app.active_visible_frame_layout(wid).unwrap();
    let presented_stamp = app.windows[&wid]
        .native_ui_compiled
        .as_ref()
        .map(|frame| frame.stamp);
    app.perf_run = PERF_HYSTERESIS_FRAMES - 1;
    app.finalize_successful_present(
        wid,
        crate::metrics::StartupPresentTiming::collapsed(Instant::now()),
        Duration::from_secs(1).as_nanos() as u64,
        None,
        super::SuccessfulPresentRoute::Native {
            view,
            presented_stamp,
        },
        route,
        super::HostVisualState::default(),
        None,
        &plan,
    );
    assert!(app.perf_reduced, "this present flipped the load policy");
    assert!(
        app.windows[&wid].native_a11y_published.is_some(),
        "a screen reader keeps the page through the flip"
    );
}

/// A present nobody asked for moves the EMA and may count toward a clear, but never toward a
/// set: three costly ones leave the latch clear and the run where it was, while the same three
/// asked for set it; one between asked costly presents neither counts nor breaks their run;
/// under a shed, past the dwell, cheap ones clear it.
///
/// NEGATIVE CONTROL: the ask ignored (`} else if ask == PresentAsk::Asked {` made `} else {`):
/// "three costly presents nobody asked for set nothing" failed (the third flipped).
/// NEGATIVE CONTROL: an unasked present resets the set run (`else { self.perf_run = 0; }` after
/// the asked arm): "an unasked present neither counts nor resets" failed (left 0, right 1).
#[test]
fn an_unasked_present_may_clear_the_shed_but_never_set_it() {
    use super::PresentAsk::{Asked, Unasked};
    let fi = Duration::from_millis(16);
    let costly = 2 * fi.as_nanos() as u64;
    let cheap = 1_000_000_u64;
    let t0 = Instant::now();
    let at = |frame: u64| t0 + fi * u32::try_from(frame).expect("a small frame count");

    let mut app = App::headless_for_test();
    for frame in 0..u64::from(PERF_HYSTERESIS_FRAMES) {
        assert!(
            !app.note_present_cost_as(costly, fi, at(frame), Unasked),
            "three costly presents nobody asked for set nothing"
        );
    }
    assert!(
        !app.perf_reduced && app.perf_run == 0,
        "…and leave the set run at zero"
    );
    assert_eq!(
        app.present_cost_ema_ns,
        Some(costly as f64),
        "they still move the EMA"
    );
    for k in 0..u64::from(PERF_HYSTERESIS_FRAMES) {
        let frame = u64::from(PERF_HYSTERESIS_FRAMES) + k;
        assert_eq!(
            app.note_present_cost_as(costly, fi, at(frame), Asked),
            k + 1 == u64::from(PERF_HYSTERESIS_FRAMES),
            "the same three asked for: only the third flips"
        );
    }
    assert!(app.perf_reduced, "the same three asked for set it");

    let mut app = App::headless_for_test();
    assert!(!app.note_present_cost_as(costly, fi, at(0), Asked));
    assert!(!app.note_present_cost_as(cheap, fi, at(1), Unasked));
    assert_eq!(
        app.perf_run, 1,
        "an unasked present neither counts nor resets"
    );
    assert!(!app.note_present_cost_as(costly, fi, at(2), Asked));
    assert!(
        app.note_present_cost_as(costly, fi, at(3), Asked),
        "an unasked present between asked costly ones breaks no run: the third asked sets"
    );
    let shed_at = app.perf_flip_at.expect("the latch records its flip");
    let past = shed_at + crate::app_render::PERF_SHED_DWELL_MIN;
    // The EMA stands at 28.125 ms after the shed (32 ms, 1 ms, 32 ms, 32 ms at PERF_EMA_ALPHA
    // 0.5). The first cheap sample brings it to 14.5625 ms, still over 0.8·fi (12.8 ms), so the
    // run under it starts at the second, and its PERF_HYSTERESIS_FRAMES-th sample, the
    // (PERF_HYSTERESIS_FRAMES + 1)-th cheap one, clears the latch.
    let clears = u64::from(PERF_HYSTERESIS_FRAMES) + 1;
    for k in 1..=clears {
        assert_eq!(
            app.note_present_cost_as(
                cheap,
                fi,
                past + fi * u32::try_from(k).expect("a small frame count"),
                Unasked
            ),
            k == clears,
            "cheap presents nobody asked for, past the dwell: the run under budget clears"
        );
    }
    assert!(!app.perf_reduced, "an unasked present may clear the shed");
}

/// Whether three costly presents of the headless window, judged as the redraw judges them,
/// set the shed: a window touched within its attention or not (`attended`), resting or not,
/// each present carrying content or not.
fn three_costly_presents_shed(attended: bool, resting: bool, content: bool) -> bool {
    let mut app = App::headless_for_test();
    let wid = crate::WindowId(0);
    let t0 = Instant::now() + crate::window_rest::ATTENTION;
    let mut ws = app.windows.get_mut(&wid).expect("the headless window");
    if !attended {
        assert!(
            ws.rest
                .settle_attended(t0, Some(crate::window_rest::ATTENTION))
        );
    }
    if resting {
        assert!(ws.rest.settle(t0, Some(Duration::from_secs(2))));
    }
    assert_eq!(
        ws.person_here(),
        attended && !resting,
        "fixture: who is here"
    );
    drop(ws);
    let fi = Duration::from_millis(16);
    let present = super::SuccessfulPresentWindowOutcome {
        frame_interval: fi,
        offscreen: false,
        content_presented: content,
    };
    for frame in 0..PERF_HYSTERESIS_FRAMES {
        app.judge_window_present(wid, 2 * fi.as_nanos() as u64, present, t0 + fi * frame);
    }
    app.perf_reduced
}

/// A present is asked for by the content it carried or by a person in its window: costly
/// presents of an attended window, and an unattended window's output frames, set the shed; an
/// unattended or resting window's still frames do not.
///
/// NEGATIVE CONTROL: the ask is `person_here` alone (`present.content_presented ||` dropped):
/// "an unattended window's output frame is judged" failed.
/// NEGATIVE CONTROL: the ask is always `Asked` (its condition made `true`): "a lapsed window's
/// still frames never set the shed" failed.
#[test]
fn a_present_is_asked_by_its_content_or_a_person_here() {
    assert!(
        three_costly_presents_shed(true, false, false),
        "an attended window's still frames are judged"
    );
    assert!(
        three_costly_presents_shed(false, false, true),
        "an unattended window's output frame is judged"
    );
    assert!(
        !three_costly_presents_shed(false, false, false),
        "a lapsed window's still frames never set the shed"
    );
    assert!(
        !three_costly_presents_shed(true, true, false),
        "a resting window's still frames never set the shed"
    );
}

/// A raw flip that changes whether the shed applies asks every window for one frame; one that
/// changes no policy (`motion = "full"`, `load_adaptive_motion = false`) is logged and counted
/// and asks none of a window that shows no native page.
///
/// NEGATIVE CONTROL: the settle on every raw flip (`if shed != shed_before` made always
/// true): "a flip under motion = \"full\" asks no frame" failed (left (false, 1), right
/// (false, 0)).
#[test]
fn a_flip_that_changes_no_policy_asks_no_frame() {
    let fi = Duration::from_millis(16);
    let present = super::SuccessfulPresentWindowOutcome {
        frame_interval: fi,
        offscreen: false,
        content_presented: true,
    };
    let flip = |motion: &str, adaptive: bool| {
        let mut app = App::headless_for_test();
        app.config.motion = Some(motion.into());
        app.config.load_adaptive_motion = Some(adaptive);
        let asks = crate::frame_asks_for_test(&app);
        let t0 = Instant::now();
        let flips: Vec<bool> = (0..PERF_HYSTERESIS_FRAMES)
            .map(|frame| {
                app.judge_window_present(
                    crate::WindowId(0),
                    2 * fi.as_nanos() as u64,
                    present,
                    t0 + fi * frame,
                )
            })
            .collect();
        assert_eq!(
            flips,
            [false, false, true],
            "{motion}: the third present flips"
        );
        assert!(app.perf_reduced, "{motion}: the raw latch is shed");
        (
            app.load_shed_active(),
            crate::frame_asks_for_test(&app) - asks,
        )
    };
    assert_eq!(
        flip("full", true),
        (false, 0),
        "a flip under motion = \"full\" asks no frame"
    );
    assert_eq!(
        flip("auto", false),
        (false, 0),
        "a flip with adaptive motion off asks no frame"
    );
    assert_eq!(
        flip("auto", true),
        (true, 1),
        "a flip that sheds asks every window once"
    );
}

/// A raw flip that changes no policy still changes what a native page draws, because the page
/// reads the raw latch: under `motion = "full"`, the window showing Settings loses its compiled
/// native frame and is asked for exactly one frame of its own, and no ask goes to every window.
///
/// NEGATIVE CONTROL: no settle on a flip that left the shed as it was (`judge_window_present`'s
/// `else { self.settle_raw_latch_flip(); }` removed): "a raw flip drops the native page's
/// compiled frame" failed (the frame was kept).
#[test]
fn a_flip_that_changes_no_policy_repaints_the_native_page_that_reads_it() {
    let wid = crate::WindowId(0);
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::CursorMotion));
    let stamp = app
        .native_ui_compile_stamp(wid)
        .expect("fixture: the Settings page compiles");
    let compiled = app
        .compiled_native_ui(wid)
        .expect("fixture: the Settings page's frame");
    app.windows
        .get_mut(&wid)
        .expect("the headless window")
        .native_ui_compiled = Some(crate::app_native::NativeCompiledFrame {
        stamp,
        phase: crate::app_native::NativeCompiledPhase::Presented,
        compiled,
    });
    let own = app.windows[&wid].redraw_requests_for_test.get();
    let all = crate::redraw_all_requests_on_this_thread();
    let fi = Duration::from_millis(16);
    let present = super::SuccessfulPresentWindowOutcome {
        frame_interval: fi,
        offscreen: false,
        content_presented: true,
    };
    let t0 = Instant::now();
    let flips: Vec<bool> = (0..PERF_HYSTERESIS_FRAMES)
        .map(|frame| {
            app.judge_window_present(wid, 2 * fi.as_nanos() as u64, present, t0 + fi * frame)
        })
        .collect();
    assert_eq!(
        flips,
        [false, false, true],
        "fixture: the third present flips"
    );
    assert!(
        !app.load_shed_active(),
        "fixture: under motion = \"full\" the shed does not apply"
    );
    assert_ne!(
        app.native_ui_compile_stamp(wid),
        Ok(stamp),
        "premise: the page's compile stamp reads the raw latch"
    );
    assert!(
        app.windows[&wid].native_ui_compiled.is_none(),
        "a raw flip drops the native page's compiled frame"
    );
    assert_eq!(
        (
            app.windows[&wid].redraw_requests_for_test.get() - own,
            crate::redraw_all_requests_on_this_thread() - all,
        ),
        (1, 0),
        "a raw flip asks the window showing a native page for one frame, and no ask goes to \
         every window"
    );
}

/// A window nobody attends applies the load-shed fade's end, never its middle: while a fade
/// runs, an attended window applies the App's envelope bit for bit, still fading, and a lapsed
/// one 0 under the shed and 1 once it clears; under `motion = "full"` and with adaptive motion
/// off both apply 1.
///
/// NEGATIVE CONTROL: the seam returned the App's envelope for every window
/// (`window_shed_envelope`'s `ws.person_here()` made `ws.person_here() || true`): "a lapsed
/// window applies the shed fade's end" failed (left 0.5555555, right 0.0).
#[test]
fn an_unattended_window_applies_the_fades_end() {
    use crate::motion::MotionMode;
    use crate::window_rest::ATTENTION;
    use aterm_effects::companion::shed_envelope_transitioning;
    let wid = crate::WindowId(0);
    let fi = Duration::from_millis(16);
    let mut app = App::headless_for_test();
    let t0 = Instant::now();
    let attend = |app: &mut App, here: bool, at: Instant| {
        let rest = &mut app.windows.get_mut(&wid).expect("the headless window").rest;
        if here {
            let _ = rest.touch(at);
        } else {
            let _ = rest.settle_attended(at + ATTENTION, Some(ATTENTION));
        }
        assert_eq!(rest.attended(), here, "fixture: attended or lapsed");
    };
    for frame in 0..PERF_HYSTERESIS_FRAMES {
        app.note_present_cost(30_000_000, fi, t0 + fi * frame);
    }
    let shed_at = app.perf_flip_at.expect("fixture: the latch shed");
    let auto = MotionMode::Auto;
    let mid = shed_at + Duration::from_millis(200);
    let app_env = app.effective_shed_envelope(auto, mid);
    assert!(
        shed_envelope_transitioning(true, app_env),
        "fixture: the App's envelope is mid-fade"
    );
    attend(&mut app, true, mid);
    let here = app.window_shed_envelope(wid, auto, mid);
    assert_eq!(
        here.to_bits(),
        app_env.to_bits(),
        "an attended window applies the App's envelope bit for bit"
    );
    assert!(shed_envelope_transitioning(true, here), "…and still fades");
    attend(&mut app, false, mid);
    let lapsed = app.window_shed_envelope(wid, auto, mid);
    assert_eq!(lapsed, 0.0, "a lapsed window applies the shed fade's end");
    assert!(
        !shed_envelope_transitioning(true, lapsed),
        "…which owes no further frame"
    );

    for mode_case in ["full", "adaptive-off"] {
        if mode_case == "full" {
            app.config.motion = Some("full".into());
        } else {
            app.config.motion = Some("auto".into());
            app.config.load_adaptive_motion = Some(false);
        }
        let mode = app.config.motion_mode();
        for here in [true, false] {
            attend(&mut app, here, mid);
            assert_eq!(
                app.window_shed_envelope(wid, mode, mid),
                1.0,
                "{mode_case}: attended={here} applies no shed"
            );
        }
    }
    app.config.motion = Some("auto".into());
    app.config.load_adaptive_motion = Some(true);

    let past = shed_at + crate::app_render::PERF_SHED_DWELL_MIN;
    let mut cleared = None;
    for k in 1..=8_u32 {
        if app.note_present_cost(0, fi, past + fi * k) {
            cleared = Some(past + fi * k);
            break;
        }
    }
    let cleared = cleared.expect("fixture: cheap presents past the dwell clear the latch");
    let rising = cleared + Duration::from_millis(100);
    let app_env = app.effective_shed_envelope(auto, rising);
    assert!(
        !app.load_shed_active() && shed_envelope_transitioning(false, app_env),
        "fixture: the App's envelope is mid fade-in"
    );
    attend(&mut app, false, rising);
    assert_eq!(
        app.window_shed_envelope(wid, auto, rising),
        1.0,
        "a lapsed window applies the clear fade's end"
    );
    attend(&mut app, true, rising);
    assert_eq!(
        app.window_shed_envelope(wid, auto, rising).to_bits(),
        app_env.to_bits(),
        "an attended window rises with the App"
    );
}
