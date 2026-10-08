// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CursorFxInputs, PERF_HYSTERESIS_FRAMES, causal_render_cost_ns};
use crate::App;
use crate::motion::{MotionEffect, MotionPolicy};
use std::time::{Duration, Instant};

/// W11(a+b): `App::motion_policy` is the one call every animation consumer
/// makes — it folds config `motion`, the live OS "Reduce Motion" flag, and
/// the window focus. `auto` (default) follows the system flag; `full`
/// overrides it; nothing overrides the unfocused demotion.
#[test]
fn app_motion_policy_folds_config_flag_and_focus() {
    let mut app = App::headless_for_test();
    // Default: auto + no system reduce + focused ⇒ Full.
    assert_eq!(app.motion_policy(true), MotionPolicy::Full);
    // The OS flag flips (what the Wake::ReduceMotionChanged handler stores).
    app.system_reduce_motion = true;
    assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
    assert_eq!(
        app.motion_policy(true).amplitude(MotionEffect::CursorGlow),
        0.0,
        "a Reduced policy zeroes the aurora amplitude"
    );
    // motion = "full" overrides the OS flag…
    app.config.motion = Some("full".into());
    assert_eq!(app.motion_policy(true), MotionPolicy::Full);
    // …but never the unfocused demotion (W11b).
    assert_eq!(app.motion_policy(false), MotionPolicy::Reduced);
    // motion = "reduced" forces static even with the OS flag off.
    app.system_reduce_motion = false;
    app.config.motion = Some("reduced".into());
    assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
}

/// The typed wake (`App::cursor_fx_focus`): a key/text egress into a window
/// pins the motion policy's focus input for that window alone while the
/// wake is live. It never touches the mode inputs: OS Reduce Motion still
/// demotes a woken window.
#[test]
fn typed_wake_pins_cursor_fx_focus() {
    use std::time::{Duration, Instant};

    use crate::WindowId;
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let t0 = Instant::now();
    // Idle + unfocused: the W11b baseline demotion holds.
    assert!(!app.cursor_fx_focus(wid, false, t0));
    assert_eq!(
        app.motion_policy(app.cursor_fx_focus(wid, false, t0)),
        MotionPolicy::Reduced,
        "an idle unfocused window stays demoted"
    );
    // One key egress stamps the wake (`last_key_at`, both input sources)…
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
    // …for the typed window only (a sibling stays demoted)…
    assert!(
        !app.cursor_fx_focus(WindowId(1), false, t0 + Duration::from_millis(1)),
        "the wake is per-window, not app-global"
    );
    // …and it EXPIRES: past the hold the demotion re-engages, so an idle
    // unfocused window still does no decorative work (FL-1's settle).
    assert!(
        !app.cursor_fx_focus(
            wid,
            false,
            t0 + super::CURSOR_FX_TYPED_WAKE + Duration::from_millis(1)
        ),
        "an expired wake demotes again"
    );
    // The wake never overrides the accessibility mode.
    app.system_reduce_motion = true;
    assert_eq!(
        app.motion_policy(app.cursor_fx_focus(wid, false, t0 + Duration::from_millis(1))),
        MotionPolicy::Reduced,
        "OS Reduce-Motion still demotes a typed-into window"
    );
}

/// Tier-1 for the blackout itself, at the real seam: `cursor_frame` on
/// an unfocused window with a live typed wake emits real comet cells into
/// `trail_scratch` — the exact frame shape v0.48–v0.50 composed as zero
/// pixels — and an idle unfocused tick past the wake's expiry drains back
/// to the demoted-static state (no resident trail), keeping FL-1's
/// settled-window early-out honest.
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

    // Seed the engines on the unfocused window (no wake yet): demoted.
    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    let seeded = app.cursor_frame(wid, seed).expect("seed cursor engines");
    assert!(
        !seeded.win_focused,
        "unfocused + idle: W11b demotes (negative control)"
    );

    // A keystroke arrives: the input path stamps `last_key_at` and heats
    // the cadence (`App::input`'s egress arm — both sources), and the
    // engines see a typed move. The next tick must paint the trail even
    // though the window never held OS key focus.
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
        !ws.fx.cursor().trail_scratch.is_empty(),
        "an unfocused window paints the trail its typing earned \
         (the v0.48–v0.50 real-window blackout frame)"
    );

    // Past the wake's hold (which outlives every effect decay) the
    // demotion re-engages and the engines drain to proven-zero.
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
        ws.fx.cursor().trail_scratch.is_empty(),
        "the drained unfocused window holds no resident trail"
    );
}

/// The typed wake's scheduler half: `terminal_effect_frame_active` folds the
/// same predicate as `cursor_fx_focus`, so an unfocused typed window keeps
/// getting frames and its comet does not freeze mid-decay between echoes.
/// Headless captures supply their own clock, so only this test sees it.
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

    // Seed the engines' position the way a real window does, so the live
    // tick below is a genuine move rather than a first sighting.
    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("seed cursor engines");

    // Charge the engines the way the typed window really does: a keystroke
    // stamps the wake, the engines see the move, one tick commits it.
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

    // W11b, unchanged: the same live engines on an unwatched window park.
    // Asserted against a charged engine set, so the park is the focus fold's
    // doing and not an empty lane.
    app.windows.get_mut(&wid).expect("window").last_key_at = None;
    let ws = app.windows.get(&wid).expect("window");
    assert!(ws.cursor_fx_active(at, false), "the engines are still live");
    assert!(
        !ws.terminal_effect_frame_active(at, false),
        "unfocused and untyped: W11b parks the 60 fps lane"
    );

    // And the wake expires with the policy: past the hold the lane parks
    // again, again asserted against a live engine set.
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

    // The two halves agree on the same instant — the invariant the fold's
    // doc-comment claims. A wake that painted but never scheduled, or
    // scheduled but never painted, is the drift that blacked the trail out.
    for probe in [t0, typed, typed + Duration::from_secs(1), late] {
        let ws = app.windows.get(&wid).expect("window");
        assert_eq!(
            ws.cursor_fx_typed_wake(probe),
            app.cursor_fx_focus(wid, false, probe),
            "policy and scheduler read one predicate at {probe:?}"
        );
    }
}

/// The `trail status` row resolves focus through the frame path's own fold
/// (`cursor_fx_focus`), not the bare `motion_focus` seam — the difference
/// being the exact window the v0.48–v0.50 blackout hid in: unfocused, typed
/// into over the control socket, and painting. A sensor that answered
/// `focused=false motion_stage=reduced intensity=0.00` there would accuse
/// the motion gate while the frames were lit, which is worse than no
/// sensor: the verb's whole promise is that the first field reading wrong
/// names the bug.
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

    // Unfocused and idle — W11b's demotion is real, and the row says so.
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.focused = false;
        ws.last_key_at = None;
    }
    let idle = app.trail_status().expect("a front window exists");
    for key in [" focused=false", " motion_stage=reduced", " intensity=0.00"] {
        assert!(idle.contains(key), "missing {key:?} in {idle}");
    }

    // …then one keystroke lands on that same unfocused window. The frame
    // path now composes at full amplitude — `typed_wake_lets_an_unfocused_
    // window_paint_its_trail` proves the cells — so the sensor must agree
    // with the glass rather than re-deriving a darker answer.
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

/// `intensity` folds the enable gate beside the policy and shed multiplies:
/// `enabled = false` emits no geometry whatever the amplitude, so the row
/// must not print a live intensity beside `glow_active=false`.
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

    // Negative control: with the knob on, the same window reports the live
    // amplitude again.
    app.config.cursor_trail = Some(true);
    let lit = app.trail_status().expect("a front window exists");
    assert!(lit.contains(" effective=true"), "{lit}");
    assert!(
        !lit.contains(" intensity=0.00"),
        "an enabled, focused, full-motion trail is not dark: {lit}"
    );
}

/// The `motion_focus` RECORDING PIN: an in-flight `video` capture feeds
/// `true` into the policy's focus input for the RECORDED window only — a
/// control-socket recording is a watcher, so the unfocused demotion (W11b)
/// must not zero the very effects the recording was started to observe.
/// The pin never touches the MODE inputs: OS Reduce-Motion still demotes
/// a recorded window exactly as `MotionPolicy::resolve` proves.
#[test]
fn video_recording_pins_motion_focus() {
    use crate::WindowId;
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").focused = false;
    // Unfocused, no recording: the W11b baseline demotion holds.
    assert!(!app.motion_focus(wid, false));
    assert_eq!(
        app.motion_policy(app.motion_focus(wid, false)),
        MotionPolicy::Reduced,
        "unfocused + unrecorded stays demoted"
    );
    // Start a recording of this window: the focus input pins true…
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
        trail: false,
        trail_log: Vec::new(),
        trail_seen: 0,
        trail_lost: 0,
        pace_ticks: Vec::new(),
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
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

    // Cross the policy seam into the real cursor-effect scheduler. The
    // recording watcher that granted Full motion must also keep live light
    // on a frame-cadence deadline even though VideoRec itself owns no
    // optional `pace` clock.
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
    // …for the recorded window only (a sibling stays demoted)…
    assert!(
        !app.motion_focus(WindowId(1), false),
        "the pin is per-window, not app-global"
    );
    // …and never overrides the accessibility mode.
    app.system_reduce_motion = true;
    assert_eq!(
        app.motion_policy(app.motion_focus(wid, false)),
        MotionPolicy::Reduced,
        "OS Reduce-Motion still demotes a recorded window"
    );
    drop(app);
    let _ = std::fs::remove_dir_all(root);
}

/// Load-adaptive shedding: the EMA + hysteresis latch sets `perf_reduced`
/// only after K consecutive over-budget content presents and clears it only
/// after K under-budget ones. Under the default (auto) policy a set latch
/// forces `Reduced`; `motion = "full"` and `load_adaptive_motion = false`
/// keep animating. The render cost is injected.
#[test]
fn perf_reduced_hysteresis_and_motion_fold() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    // Default motion (auto) + focused + no OS reduce would OTHERWISE animate at full
    // amplitude; the shed latch is what forces Reduced under this default policy.
    app.config.motion = Some("auto".into());
    app.system_reduce_motion = false;
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Full,
        "baseline: an unloaded focused window animates"
    );

    // 60 Hz budget: shed above 1.5×16 = 24 ms, re-engage below 0.8×16 = 12.8 ms.
    let fi = Duration::from_millis(16);
    let slow = 25_000_000u64; // 25 ms render — just over the 24 ms shed threshold.
    let fast = 0u64; // an instant render — well under the 12.8 ms clear threshold.
    // Synthetic frame clock (the timing is INJECTED — no wall-clock in the test).
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    // A single slow frame must not trip the latch (hysteresis).
    assert!(
        !app.note_present_cost(slow, fi, at(0)),
        "one slow frame is not a transition"
    );
    assert!(
        !app.perf_reduced,
        "hysteresis: a lone slow present does not shed"
    );
    assert_eq!(app.motion_policy(true), MotionPolicy::Full);
    // The 2nd extends the run; the 3rd (K = 3) trips it and reports the edge.
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

    // Under the DEFAULT (auto) policy a load-shed window forces Reduced …
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

    // … but the user's EXPLICIT intent overrides the shed while it is still latched.
    // `motion = "full"` means "always animate":
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
    // …and opting out of the heuristic (auto + load_adaptive_motion = false) does too:
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
    // Restore the default (auto + shedding on): the latch is still set, so Reduced.
    app.config.load_adaptive_motion = None;
    assert_eq!(
        app.motion_policy(true),
        MotionPolicy::Reduced,
        "default policy still sheds while latched"
    );
    assert!(app.load_shed_active());

    // A single fast frame must not clear the latch (hysteresis the other way).
    assert!(
        !app.note_present_cost(fast, fi, at(48)),
        "one fast frame is not a transition"
    );
    assert!(
        app.perf_reduced,
        "hysteresis: a lone fast present does not re-engage"
    );
    assert_eq!(app.motion_policy(true), MotionPolicy::Reduced);
    // Even a sustained fast run does not clear before the dwell: the
    // post-shed cost lacks the effects' own cost.
    assert!(
        !app.note_present_cost(fast, fi, at(64)),
        "second fast frame: still shed"
    );
    assert!(
        !app.note_present_cost(fast, fi, at(80)),
        "K-th fast frame INSIDE the dwell must not clear"
    );
    assert!(app.perf_reduced, "dwell holds the latch shed");
    // Past the dwell the accumulated fast run clears it on the next sample.
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

/// One huge present never sheds, however huge: a spike ~8× the threshold
/// (a theme switch invalidating every cache) keeps the decaying EMA over
/// threshold for a few frames, and the shed qualifier reads the raw sample
/// (owner, 2026-08-15).
#[test]
fn a_single_spike_present_never_sheds() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    app.config.motion = Some("auto".into());
    app.system_reduce_motion = false;

    // 60 Hz budget: shed threshold 1.5×16 ≈ 24 ms.
    let fi = Duration::from_millis(16);
    let steady = 1_000_000u64; // 1 ms — a healthy frame.
    let spike = 250_000_000u64; // 250 ms — a theme-reload invalidation storm.
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    // A settled, healthy cadence…
    for i in 0..4 {
        assert!(!app.note_present_cost(steady, fi, at(i * 16)));
    }
    // …one invalidation-storm present…
    assert!(
        !app.note_present_cost(spike, fi, at(64)),
        "the spike itself is not a transition"
    );
    // …and the EMA's decay tail (125 → 63 → 31 ms, all over the 24 ms
    // threshold) must not be mistaken for a sustained overload once the
    // per-frame cost is instantly healthy again.
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

/// The adaptive loop must include CPU work associated with effect-heavy GPU
/// submission. A compose-only sample is blind to that slice; folding the
/// injected encode/submit CPU wall time into the causal sample trips the
/// latch, and the cheaper reduced frame then provides negative feedback.
/// This test intentionally makes no claim about completed shader execution.
#[test]
fn gpu_submit_cpu_work_is_part_of_the_load_shed_feedback_signal() {
    use std::time::Duration;

    let fi = Duration::from_millis(16);
    let compose_ns = 2_000_000;
    let heavy_gpu_submit_cpu_ns = 28_000_000;
    let reduced_gpu_submit_cpu_ns = 1_000_000;
    let t0 = std::time::Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    // A compose-only signal stays below the 24 ms threshold even while fake
    // GPU encode/submit CPU work costs 28 ms.
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

    // In production the latch removes decorative passes and therefore can
    // lower their command-encoding/submit CPU cost. Model that consequence
    // directly: the sample falls immediately, and EMA + clear hysteresis —
    // once the anti-flap dwell has elapsed — restore the full policy.
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

/// The shed metric includes the effects' cost only while they run, so an
/// effect-heavy session would oscillate between shed and restore. The dwell
/// and flap backoff bound the flip rate: each re-shed inside the quick
/// window doubles the next dwell.
#[test]
fn shed_latch_flap_backoff_bounds_transitions() {
    use std::time::Duration;
    let mut app = App::headless_for_test();
    let fi = Duration::from_millis(16);
    let t0 = std::time::Instant::now();
    // Worst-case oscillator input: over budget whenever effects are on,
    // instantly under budget whenever they are shed.
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

/// The shed clears at the sparse cadence a shed window presents at: with
/// cheap presents every 400 ms (the first inside the wall-clock dwell), the
/// latch clears within the dwell plus `PERF_HYSTERESIS_FRAMES` presents and
/// motion returns.
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

/// The load-shed latch keeps its fold for the flying head and every
/// decoration, and never reaches the resident pet: its posture and its
/// scheduler gate resolve through the accessibility fold alone, which
/// still pins it.
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

/// A latch flip drives the cursor glow/trail through the soft envelope —
/// ramping over the fade window, anchored mid-fade on reversal — instead of
/// the old hard step that cleared the engines' buffers the same frame.
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
    // Fade-out: still audible at the flip, ~half at half the window, 0 after.
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
    // Restore past the dwell: the envelope ramps back up from where it was.
    // (Several fast frames — the EMA must decay below the clear threshold
    // and then hold there for the debounce run.)
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

/// Smooth scroll survives load shedding. The latch is engaged as production
/// engages it (`PERF_HYSTERESIS_FRAMES` presents over 1.5× a 120 Hz budget)
/// and sheds decoration; a wheel notch in a focused window still arms a
/// glide, the mid-ease tick keeps it armed, the compose tail presents the
/// sub-row band, and the settle leaves it alone. OS Reduce Motion still
/// lands the glide and zeroes the band (owner, 2026-09-22).
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

    // Engage the latch the way production does: K consecutive presents
    // over 1.5× the 8.33 ms budget of a 120 Hz panel (13 ms > 12.5 ms).
    let fi = Duration::from_micros(8_333);
    let t0 = Instant::now();
    for frame in 0..PERF_HYSTERESIS_FRAMES {
        app.note_present_cost(
            13_000_000,
            fi,
            t0 + Duration::from_millis(u64::from(frame) * 8),
        );
    }
    // CONTROLS: the latch is engaged and decoration is shed.
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
    // The functional pair resolves through accessibility alone.
    assert_eq!(
        app.effect_policy(MotionEffect::SmoothScroll, true),
        MotionPolicy::Full
    );
    assert_eq!(
        app.effect_policy(MotionEffect::ScrollPill, true),
        MotionPolicy::Full
    );
    assert!(app.smooth_scroll_animates(wid));

    // A wheel notch under the latch ARMS a glide (it snapped before).
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
    // Mid-ease (≤ 90 ms of a 180 ms glide) the tick samples and keeps the
    // glide armed instead of landing it.
    app.tick_scroll_glide(wid, wheel_at + Duration::from_millis(90));
    assert!(
        app.windows[&wid].scroll_glide.is_some(),
        "the mid-ease tick must not land the glide under the latch"
    );
    // The compose tail PRESENTS whatever the bank holds (a known residual,
    // so the assertion does not depend on the ease's wall-clock sample).
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
    // A latch edge under a live glide leaves it armed: clear the latch (K
    // fast presents after the 1.5 s dwell) and re-shed with the glide in
    // flight.
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

    // The exemption is narrow: accessibility still demotes.
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
    // …and so does the unfocused demotion (W11b), latch or no latch.
    app.system_reduce_motion = false;
    assert_eq!(
        app.effect_policy(MotionEffect::SmoothScroll, false),
        MotionPolicy::Reduced
    );
}
