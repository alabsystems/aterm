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
        let ws = app.windows.get_mut(&wid).expect("window");
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
        !ws.fx.cursor().glow.under_quads().is_empty(),
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
        ws.fx.cursor().glow.under_quads().is_empty() && ws.fx.cursor().glow_scratch.is_empty(),
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
        let ws = app.windows.get_mut(&wid).expect("window");
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
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    let wid = WindowId(0);
    assert_eq!(
        app.frontmost_window,
        Some(wid),
        "the fixture's front window"
    );

    {
        let ws = app.windows.get_mut(&wid).expect("window");
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
    app.config.cursor_trail_style = Some("rainbow kitty".into());
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
    app.config.cursor_trail_style = Some("rainbow kitty".into());
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
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.fx.cursor_mut().glow.note_typed_cells(tick_at, 1);
        ws.fx.cursor_mut().trail.note_typed(tick_at);
    }
    let mut moved = CursorFxInputs::sample_for_test(tick_at + Duration::from_millis(1));
    moved.cur = Some((2, 4));
    app.cursor_frame(wid, moved).expect("licensed tick");
    let ws = app.windows.get_mut(&wid).expect("window");
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

#[test]
fn shed_latch_flap_backoff_bounds_transitions() {
    use std::time::Duration;
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
    assert!(
        transitions <= 14,
        "flap backoff must bound the limit cycle (got {transitions} transitions/min)"
    );
    assert!(
        shed_frames > total_frames * 3 / 4,
        "a genuinely overloaded session converges to mostly-shed \
         ({shed_frames}/{total_frames} frames shed)"
    );
    assert!(
        app.perf_shed_dwell > crate::app_render::PERF_SHED_DWELL_MIN,
        "repeated flaps must have backed the dwell off"
    );
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
