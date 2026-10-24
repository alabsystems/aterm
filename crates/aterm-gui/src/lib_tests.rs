// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    App, DEMO_TICK_INTERVAL, NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, WindowId, find_url_span,
    is_safe_url, native_preview_deadline, native_preview_may_tick, run_capture_present_barrier,
    should_dispatch_boot_health_confirmation,
};
use crate::app_config::Config;

#[test]
fn headless_confirms_boot_health_at_the_control_socket_boundary() {
    use std::sync::atomic::Ordering;

    fn would_dispatch(app: &App) -> bool {
        should_dispatch_boot_health_confirmation(
            app.headless,
            app.headless_boot_health_checkpoint(),
            app.first_present_done,
            app.boot_health_confirmation_dispatched,
            true,
            app.windows.values().any(|ws| ws.os_window.is_some()),
            app.incoming_handoff_pending,
        )
    }

    let mut app = App::headless_for_test();
    assert!(
        app.headless,
        "negative control: this fixture IS the headless lane"
    );
    assert!(
        !app.first_present_done && !app.windows.values().any(|ws| ws.os_window.is_some()),
        "negative control: the windowed checkpoint is unreachable here, so it \
         cannot be what disarms the sentinel"
    );

    assert!(app.sock_plan.is_none());
    assert!(
        would_dispatch(&app),
        "a socketless headless build must still confirm"
    );

    app.sock_plan = Some(crate::control_auth::SocketPlan {
        sock_path: "/tmp/aterm-boot-health-test.sock".into(),
        token_path: std::path::PathBuf::from("/tmp/aterm-boot-health-test.token"),
        latest_link: None,
    });
    app.sock_bound.store(false, Ordering::Release);
    assert!(
        !would_dispatch(&app),
        "an unbound control socket is not a working engine"
    );

    app.sock_bound.store(true, Ordering::Release);
    assert!(
        would_dispatch(&app),
        "a bound control socket is the headless health proof"
    );

    app.boot_health_confirmation_dispatched = true;
    assert!(
        !would_dispatch(&app),
        "the same boundary cannot enqueue a second confirmation"
    );
}

#[test]
fn boot_health_never_folds_a_past_deadline() {
    use std::time::{Duration, Instant};
    let mut app = App::headless_for_test();
    let now = Instant::now();
    let past = now.checked_sub(Duration::from_secs(5)).unwrap_or(now);
    app.boot_health_confirmation_retry_at = Some(past);
    assert!(
        app.boot_health_guard_open(),
        "fixture: a socketless headless boot's guard is open"
    );
    assert!(
        app.boot_health_retry_deadline(now).is_some_and(|d| d > now),
        "an open guard folds the retry floored into the future"
    );

    app.incoming_handoff_pending = true;
    assert_eq!(
        app.boot_health_retry_deadline(now),
        None,
        "a retry the guard would refuse must not be folded"
    );
    app.incoming_handoff_pending = false;

    app.note_boot_health_dispatch(crate::app_native::NativeUpdateDispatch::Saturated, now);
    assert_eq!(
        app.boot_health_confirmation_retry_at,
        Some(now + Duration::from_secs(1))
    );
    app.note_boot_health_dispatch(crate::app_native::NativeUpdateDispatch::Saturated, now);
    assert_eq!(
        app.boot_health_confirmation_retry_at,
        Some(now + Duration::from_secs(2)),
        "consecutive saturation doubles the wait"
    );
    assert!(app.boot_health_retry_deadline(now).is_some_and(|d| d > now));

    app.note_boot_health_dispatch(crate::app_native::NativeUpdateDispatch::Queued, now);
    assert!(app.boot_health_confirmation_dispatched);
    assert_eq!(app.boot_health_retry_deadline(now), None);
    app.finish_native_boot_health_confirmation(false, now);
    assert_eq!(
        app.boot_health_confirmation_retry_at,
        Some(now + Duration::from_secs(4))
    );
}

#[test]
fn serious_mode_drains_fun_state_without_touching_functional_ui() {
    use aterm_effects::kitty_sing::SING_ARM_REPEATS;
    use std::time::{Duration, Instant};

    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("comet".into());
    app.set_session_rain_override(0, Some(true));

    assert!(app.glow_config().enabled);
    assert!(app.trail_config().enabled);
    assert!(app.session_rain_enabled(0));
    assert_eq!(
        app.motion_policy(true),
        crate::motion::MotionPolicy::Full,
        "the functional-motion policy starts fully enabled"
    );

    let now = Instant::now();
    let blink_deadline = now + Duration::from_secs(2);
    let mut trail_cfg = app.trail_config();
    trail_cfg.intensity = 1.0;
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.fx.cursor_mut().glow.note_kill(now, true);
    let mut emitted_trail = Vec::new();
    ws.fx
        .cursor_mut()
        .trail
        .tick(Some((0, 0)), now, &trail_cfg, &mut emitted_trail);
    ws.fx.cursor_mut().trail.note_synthetic_move(now);
    ws.fx.cursor_mut().trail.tick(
        Some((0, 4)),
        now + Duration::from_millis(1),
        &trail_cfg,
        &mut emitted_trail,
    );
    assert!(
        ws.fx.cursor_mut().glow.is_active(),
        "negative-control live glow"
    );
    assert!(
        ws.fx.cursor_mut().trail.is_active(),
        "negative-control live trail"
    );

    let sing_start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
    for i in 0..SING_ARM_REPEATS {
        ws.fx.companions_mut().kitty_sing.note_char(
            sing_start + Duration::from_millis(u64::from(i) * 10),
            0,
            'a',
        );
    }
    assert!(
        ws.fx.companions_mut().kitty_sing.is_armed(now),
        "negative-control live singer"
    );
    let c = ws.fx.companions_mut();
    c.music_notes.update(now, true, c.kitty_sing.beat(now));
    assert!(
        ws.fx.companions_mut().music_notes.is_active(),
        "negative-control live notes"
    );
    let pet = crate::app_render::pet_tick_for_test(
        ws,
        aterm_effects::host::CaptureMode::StaticCapture,
        aterm_effects::kitty_pet::PetSense {
            caret_drawn: true,
            now,
            caret: Some((0, 0)),
            rows: 24,
            cols: 80,
            cell_w: 8,
            cell_h: 16,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        },
    );
    assert!(
        pet.alpha > 0 && ws.fx.companion_mut().brain().is_active(),
        "negative-control live pet"
    );
    ws.fx.companions_mut().sing_riff_bar = Some(7);
    assert!(
        ws.fx.companion_mut().hit_rect().is_some(),
        "the live resident owns this frame's hit target"
    );

    ws.next_trail_tick = Some(now);
    ws.next_rain_tick = Some(now);
    ws.next_native_preview_tick = Some(now);
    ws.input_scratch.cursor_trail.push(aterm_render::TrailCell {
        row: 0,
        col: 0,
        alpha: 255,
    });
    ws.input_scratch
        .cursor_glow_add
        .push(aterm_render::GlowQuad::default());
    ws.input_scratch
        .glow_halo
        .push(aterm_render::RainHalo::default());
    ws.input_scratch.cursor_effect_style_override = Some(aterm_core::terminal::CursorStyle::Bolt);
    ws.input_scratch.cursor_fill_override = Some(0x00FF_00FF);
    ws.composed_cursor_effect_valid = true;
    ws.composed_cursor_effect_session = Some(0);
    ws.composed_cursor_fill = Some(0x0011_2233);
    ws.composed_cursor_style_override = Some(aterm_core::terminal::CursorStyle::Bolt);
    ws.composed_cursor_trail_color = 0x0044_5566;
    let tiles = {
        let rom = aterm_effects::matrix_rain::rom::rasterize_master();
        let mut tiles = aterm_effects::matrix_rain::RainBaker::default();
        tiles.begin_frame(8, 16);
        while !tiles.complete() {
            tiles.bake_tiles(&rom);
        }
        tiles
    };
    let cell = aterm_effects::matrix_rain::RainCell {
        w: 8,
        h: 16,
        tint: 0x0028_D75F,
        alpha: 96,
        ..Default::default()
    };
    let body = tiles.tile(1).expect("baked").clone();
    ws.fx.publish_for_test(
        &[aterm_render::sprites::FreeSprite::whole(0, 0, body)],
        (&[cell], &tiles),
        &mut ws.input_scratch,
    );
    let rain_rows = |input: &aterm_render::RenderInput| {
        let layers = input.fx_layers.iter();
        layers
            .filter(|l| aterm_effects::matrix_rain::is_rain_row(l))
            .count()
    };
    let sprites = |input: &aterm_render::RenderInput| {
        aterm_render::sprites::published(&input.fx_layers).len()
    };
    assert_eq!(
        (rain_rows(&ws.input_scratch), sprites(&ws.input_scratch)),
        (1, 1),
        "negative-control: the window's publishers put a rain row and a companion in the frame"
    );

    ws.bell_flash.ring(now);
    let bell_deadline = ws.bell_flash.deadline();
    ws.next_blink = Some(blink_deadline);
    ws.scroll_frac_px = 7;
    ws.pill_shown = true;
    ws.focused = false;
    ws.last_key_at = Some(now);

    assert!(app.set_serious_mode(true));
    assert!(app.serious_mode_enabled());
    assert_eq!(app.config.serious_mode, Some(true));
    assert!(
        app.notify_silent.load(std::sync::atomic::Ordering::Acquire),
        "desktop notification delivery must observe the global mute"
    );
    assert!(app.trail_audio.is_inert_for_test());
    assert!(!app.glow_config().enabled);
    assert!(!app.trail_config().enabled);
    assert!(!app.session_rain_enabled(0));
    assert_eq!(app.pool.rain_override(0), Some(true));
    assert_eq!(app.config.cursor_trail, Some(true));

    let ws = app.windows.get(&WindowId(0)).expect("test window");
    assert!(!ws.fx.cursor().glow.is_active());
    assert!(!ws.fx.cursor().trail.is_active());
    assert!(!ws.fx.companions().kitty_sing.is_armed(now));
    assert_eq!(ws.fx.companions().kitty_sing.drive(now), 0.0);
    assert!(!ws.fx.companions().music_notes.is_active());
    assert!(!ws.fx.companion().brain().is_active());
    assert!(ws.fx.companions().sing_riff_bar.is_none());
    assert!(ws.fx.companion().hit_rect().is_none());
    assert!(ws.fx.rain().is_none());
    assert!(ws.next_trail_tick.is_none());
    assert!(ws.next_rain_tick.is_none());
    assert!(ws.next_native_preview_tick.is_none());
    assert!(ws.input_scratch.cursor_trail.is_empty());
    assert!(ws.input_scratch.cursor_glow_add.is_empty());
    assert!(ws.input_scratch.glow_halo.is_empty());
    assert!(ws.input_scratch.cursor_effect_style_override.is_none());
    assert!(ws.input_scratch.cursor_fill_override.is_none());
    assert_eq!(
        (rain_rows(&ws.input_scratch), sprites(&ws.input_scratch)),
        (0, 0),
        "serious mode takes the rain rows and the companions out of the frame"
    );
    assert!(!ws.composed_cursor_effect_valid);
    assert!(ws.composed_cursor_effect_session.is_none());
    assert!(ws.composed_cursor_fill.is_none());
    assert!(ws.composed_cursor_style_override.is_none());
    assert_eq!(ws.composed_cursor_trail_color, 0);

    assert!(ws.bell_flash.is_active(now));
    assert_eq!(ws.bell_flash.deadline(), bell_deadline);
    assert_eq!(ws.next_blink, Some(blink_deadline));
    assert_eq!(ws.scroll_frac_px, 7);
    assert!(ws.pill_shown);
    assert_eq!(
        app.motion_policy(true),
        crate::motion::MotionPolicy::Full,
        "serious mode is not reduced motion; smooth functional UI remains enabled"
    );

    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.drain_serious_effects();
    assert_eq!(
        ws.last_key_at,
        Some(now),
        "the drain must not erase the classifier's typing evidence"
    );
    assert!(
        ws.cursor_fx_typed_wake(now),
        "negative control: the typed wake is live on this unfocused window"
    );
    assert!(
        !ws.cursor_fx_active(now, true),
        "…and no cursor effect is resident for it to animate"
    );
    assert_eq!(
        ws.plan_terminal_effect_lane(now, true, true),
        None,
        "a live typed wake under serious mode arms no effect frame"
    );
}

#[test]
fn a_shed_resident_keeps_moving_on_its_coarse_offer_never_the_train() {
    use aterm_effects::companion::RESIDENT_SHED_CADENCE;
    let mut app = App::headless_for_test();
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.focused = true;
    let mut now = Instant::now();
    let present = |ws: &mut crate::WindowState, now, col, shed| {
        let caret = Some((4, col));
        let tick = crate::app_render::pet_prepare_shed_for_test(ws, now, 0, caret, None, shed);
        ws.fx.companion_mut().tick(tick)
    };
    for _ in 0..40 {
        now += Duration::from_millis(16);
        present(ws, now, 10, (1.0, false));
    }
    now += Duration::from_millis(16);
    present(ws, now, 60, (1.0, false));
    assert!(
        ws.fx.companion().needs_frames() && ws.cursor_dependents_need_frame_cadence(now, true),
        "fixture: a resident running after a far caret claims the train"
    );

    now += Duration::from_millis(16);
    present(ws, now, 60, (1.0, true));
    assert!(
        ws.fx.companion().brain().needs_frames(),
        "the body is still moving"
    );
    assert!(
        !ws.fx.companion().needs_frames() && !ws.cursor_dependents_need_frame_cadence(now, false),
        "…and off the frame train"
    );
    let offer = now + RESIDENT_SHED_CADENCE;
    assert_eq!(ws.fx.companion().next_change_deadline(now), Some(offer));
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(now, false, true, true, false, 10.0, true),
        Some(offer),
        "the shed-free pet gate arms exactly the coarse offer"
    );
    ws.park_terminal_effect_scheduler();
    assert_eq!(
        ws.plan_terminal_effect_lane_with_recording(now, false, false, true, false, 10.0, true),
        None,
        "negative control: a pet gate carrying the latch strands the moving pet"
    );

    let mut settled = false;
    for _ in 0..400 {
        if !ws.fx.companion().brain().needs_frames() {
            settled = true;
            break;
        }
        now = ws
            .fx
            .companion()
            .next_change_deadline(now)
            .expect("a moving pet is offered");
        present(ws, now, 60, (0.0, true));
    }
    assert!(settled, "fixture: the offers carry it home");
    assert!(!ws.fx.companion().needs_frames());
    assert_eq!(
        ws.fx.companion().next_change_deadline(now),
        ws.fx
            .companion()
            .brain()
            .next_change_deadline(now)
            .map(|step| step.max(now + RESIDENT_SHED_CADENCE)),
        "a settled resident under the shed owes only its own next visible step, \
         no sooner than the shed cadence"
    );

    now += Duration::from_millis(16);
    present(ws, now, 30, (0.4, false));
    assert!(
        ws.fx.companion().needs_frames() && ws.cursor_dependents_need_frame_cadence(now, true),
        "the cleared latch hands the resident its train back"
    );
}

#[test]
fn wake_expiry_preserves_exactly_one_cursor_effect_settle_present() {
    use std::time::{Duration, Instant};

    let mut app = App::headless_for_test();
    let now = Instant::now();
    let deadline = now + Duration::from_millis(12);
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.focused = false;
    ws.last_key_at =
        now.checked_sub(crate::app_render::CURSOR_FX_TYPED_WAKE + Duration::from_millis(1));
    ws.fx.cursor_mut().glow.note_kill(now, true);
    ws.next_trail_tick = Some(deadline);
    assert!(!ws.cursor_fx_typed_wake(now), "the typed wake has expired");
    assert!(
        ws.cursor_fx_active(now, true),
        "negative control: animated light is still resident"
    );

    assert_eq!(
        ws.plan_terminal_effect_lane(now, true, true),
        Some(deadline),
        "an unrelated park before the due tick must preserve the inherited settle slot"
    );
    assert_eq!(
        ws.plan_terminal_effect_lane(now + Duration::from_millis(1), true, true),
        Some(deadline),
        "the settle route is a level until its one deadline fires"
    );

    let model = aterm_models_effects::effect_presentability_settle_model();
    let mut expired = model.init_state();
    assert!(model.fire("ExpireWake", &mut expired));
    let mut retained = expired.clone();
    assert!(model.fire("UnrelatedPark", &mut retained));
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &expired,
        &retained,
        Some("UnrelatedPark"),
        "real wake-expiry settle-slot retention",
    );
    assert!(ok, "settle-slot retention rejected: {why}");
    let mut stranded = retained.clone();
    stranded.insert("pending", 0);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &expired,
        &stranded,
        Some("UnrelatedPark"),
        "forged wake-expiry park",
    );
    assert!(!ok, "dropping the only settle route must be rejected");

    assert!(
        !ws.service_due_terminal_effect_tick(deadline - Duration::from_millis(1)),
        "an arbitrary early event-loop wake must not consume the clock"
    );
    assert!(
        ws.service_due_terminal_effect_tick(deadline),
        "the due clock is consumed independently of the event-loop wake cause"
    );
    assert!(ws.next_trail_tick.is_none(), "the settle slot is one-shot");
    assert_eq!(
        ws.plan_terminal_effect_lane(deadline, true, true),
        None,
        "after consumption, still-resident engine state cannot re-arm an unfocused lane"
    );

    let mut settled = retained;
    assert!(model.fire("PaintSettle", &mut settled));
    assert_eq!(settled["pixels_lit"], 0);
    assert_eq!(settled["effect_active"], 0);
}

use std::time::{Duration, Instant};

/// One gesture of the confirmation capture's schedule.
#[derive(Clone, Copy, Debug)]
enum LaneGesture {
    Key,
    JumpTo(u16),
    Backspace,
    Enter,
}

fn lane_census_schedule() -> Vec<(u64, LaneGesture)> {
    let mut s = Vec::new();
    let mut t = 0u64;
    for _ in 0..35 {
        s.push((t, LaneGesture::Key));
        t += 70;
    }
    t += 1_500 - 70;
    for _ in 0..9 {
        s.push((t, LaneGesture::Key));
        t += 35;
    }
    t += 1_000 - 35;
    for k in 0..4u64 {
        let to = if k % 2 == 0 { 0 } else { 48 };
        s.push((t, LaneGesture::JumpTo(to)));
        t += 500;
    }
    for _ in 0..4 {
        s.push((t, LaneGesture::Backspace));
        t += 100;
    }
    t += 400;
    s.push((t, LaneGesture::Enter));
    s
}

/// The census of one scripted session through the real lane planner.
#[derive(Debug, Default)]
struct LaneCensus {
    /// Arms after the last gesture.
    tail_arms: u32,
    /// ms after the last gesture at which the planner first parked (and
    /// stayed parked); `None` if it never did inside the tail.
    park_ms: Option<u64>,
}

fn lane_wake_census() -> LaneCensus {
    use crate::cursor_glow::Geom;
    use crate::cursor_rainbow::RainbowConfig;

    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".to_string());
    let glow_cfg = app.glow_config();
    assert!(matches!(
        glow_cfg.style,
        crate::cursor_glow::GlowStyle::RainbowKitty
    ));
    let geom = Geom {
        cw: 18,
        ch: 40,
        rows: 26,
        cols: 190,
        origin_x: 0,
        origin_y: 0,
        win_w: 3420,
        win_h: 1040,
        head: 0,
    };
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    let mut census = LaneCensus::default();
    let script = lane_census_schedule();
    let t0 = Instant::now();
    let last_at = t0 + Duration::from_millis(script.last().map_or(0, |s| s.0));
    let end = last_at + Duration::from_secs(6);
    let mut cell = (12u16, 4u16);
    let mut next_ev = 0usize;
    let mut out = Vec::new();
    let mut now = t0 - Duration::from_millis(50);
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some(cell), now, &glow_cfg, geom, &mut out);
    let charge = |ws: &mut super::WindowState, now: Instant| {
        ws.plan_terminal_effect_lane(now, true, true).is_some()
    };
    loop {
        let ev_at = script
            .get(next_ev)
            .map(|(ms, _)| t0 + Duration::from_millis(*ms));
        let wake = match (ws.next_trail_tick, ev_at) {
            (Some(d), Some(e)) => d.min(e),
            (Some(d), None) => d,
            (None, Some(e)) => e,
            (None, None) => break,
        };
        if wake > end {
            break;
        }
        assert!(
            wake > now || ev_at == Some(wake),
            "the lane armed a deadline at `now` — a busy re-arm"
        );
        now = wake;
        if ev_at == Some(now) {
            match script[next_ev].1 {
                LaneGesture::Key => {
                    ws.fx.cursor_mut().glow.note_synthetic_typed(now, 1);
                    ws.fx.cursor_mut().cadence.on_keystroke(now);
                    cell.1 += 1;
                }
                LaneGesture::JumpTo(col) => {
                    ws.fx.cursor_mut().glow.note_motion(now);
                    cell.1 = col;
                }
                LaneGesture::Backspace => {
                    ws.fx.cursor_mut().glow.note_backspace(now);
                    cell.1 = cell.1.saturating_sub(1);
                }
                LaneGesture::Enter => {
                    ws.fx.cursor_mut().glow.note_return(now);
                    cell = (cell.0 + 1, 0);
                }
            }
            ws.last_key_at = Some(now);
            next_ev += 1;
        }
        ws.service_due_terminal_effect_tick(now);
        let a = charge(ws, now);
        ws.fx
            .cursor_mut()
            .glow
            .tick(Some(cell), now, &glow_cfg, geom, &mut out);
        ws.fx.cursor_mut().glow.drain_sound_cues().for_each(drop);
        let caret_cfg = RainbowConfig {
            enabled: false,
            intensity: 1.0,
            blinking: false,
            base: None,
            head_rgb: ws.fx.cursor_mut().glow.rainbow_head_rgb(&glow_cfg),
            paint: Some(ws.fx.cursor_mut().glow.caret_paint(now)),
            ground: Some(glow_cfg.theme_bg),
            flare_at: ws.fx.cursor_mut().glow.caret_flare_at(),
        };
        let (energy, _) = ws.fx.cursor_mut().cadence.sample(now);
        let (phase, field) = (
            ws.fx.cursor_mut().glow.rainbow_phase(),
            ws.fx.cursor_mut().glow.rainbow_field(),
        );
        ws.fx.cursor_mut().rainbow.tick_with_family_phase(
            Some(cell),
            now,
            energy,
            phase,
            field,
            false,
            true,
            geom,
            &caret_cfg,
            &mut out,
        );
        let b = charge(ws, now);
        if now >= last_at {
            let since = now.saturating_duration_since(last_at).as_millis() as u64;
            census.tail_arms += u32::from(a) + u32::from(b);
            if b {
                census.park_ms = None;
            } else if census.park_ms.is_none() {
                census.park_ms = Some(since);
            }
        }
    }
    census
}

#[test]
fn the_effect_lane_parks_within_two_seconds_of_the_last_key_under_v2() {
    let v2 = lane_wake_census();
    let park = v2
        .park_ms
        .expect("the lane never parked after the last key");
    assert!(
        park <= 2_000,
        "the lane parked {park} ms after the last key"
    );
    assert!(v2.tail_arms > 0, "the tail owed at least the swoosh");
}

#[test]
fn the_settling_rainbow_caret_keeps_the_lane_to_its_last_step() {
    use aterm_effects::cursor_glow::Geom;

    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 384,
        head: 0,
    };
    let spine = |secs: f32| {
        let mut disp = 1.0_f32;
        let mut t = 0.0_f32;
        while t < secs {
            let target = (-(t + 0.001) / 2.0).exp();
            let rise = target - disp;
            let ease = if rise > 0.0 { 0.10 } else { 0.85 };
            disp += rise * (1.0 - (-0.001_f32 / ease).exp());
            if disp < 0.005 {
                disp = 0.0;
            }
            t += 0.001;
        }
        disp
    };
    let heat = |secs: f32| 0.5_f32.powf(secs / 0.22);

    let mut app = App::headless_for_test();
    let t0 = Instant::now();
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.focused = true;
    ws.frame_interval = Some(Duration::from_nanos(8_333_333));
    let interval = ws.effect_present_interval();
    let mut halo = Vec::new();
    let tick_caret = |ws: &mut super::WindowState, at: Instant, halo: &mut Vec<_>| {
        let secs = at.saturating_duration_since(t0).as_secs_f32();
        halo.clear();
        ws.fx.cursor_mut().rainbow.tick(
            Some((4, 12)),
            at,
            heat(secs),
            false,
            true,
            geom,
            &crate::cursor_rainbow::RainbowConfig {
                enabled: true,
                intensity: 1.0,
                blinking: false,
                base: Some(0x0050_FA7B),
                head_rgb: None,
                paint: Some(spine(secs)),
                ground: None,
                flare_at: None,
            },
            halo,
        );
    };

    let mut t = t0;
    loop {
        tick_caret(ws, t, &mut halo);
        if ws.fx.cursor().rainbow.settling() {
            break;
        }
        t += interval;
        assert!(
            t.saturating_duration_since(t0) < Duration::from_secs(10),
            "fixture: the caret's frames never stopped moving"
        );
    }
    assert!(
        !ws.cursor_dependents_need_frame_cadence(t, false),
        "negative control: the frame-cadence set alone parks the lane here, mid-fade"
    );
    assert!(
        ws.cursor_fx_active(t, false),
        "…but the release is live effect state"
    );
    let first = ws
        .plan_terminal_effect_lane(t, false, true)
        .expect("the settling caret owns a deadline");
    assert!(first > t, "the deadline is in the future");

    let horizon = t + Duration::from_secs(20);
    let mut wakes = 0_usize;
    let mut deadline = first;
    let released_at = loop {
        assert!(
            deadline <= horizon,
            "the lane never released: {wakes} wakes"
        );
        assert!(
            ws.service_due_terminal_effect_tick(deadline),
            "the armed clock is due at its deadline"
        );
        tick_caret(ws, deadline, &mut halo);
        wakes += 1;
        match ws.plan_terminal_effect_lane(deadline, false, true) {
            Some(next) => {
                assert!(next > deadline, "a deadline must advance");
                deadline = next;
            }
            None => break deadline,
        }
    };
    assert!(
        !ws.fx.cursor().rainbow.is_active() && !ws.fx.cursor().rainbow.settling(),
        "the lane parks only once the caret itself releases"
    );
    let window = released_at.saturating_duration_since(t);
    let frame_train = (window.as_secs_f32() / interval.as_secs_f32()) as usize;
    println!(
        "settling rainbow caret: {wakes} wakes over {window:?} (the frame train: \
         {frame_train} at {interval:?})"
    );
    assert!(
        wakes <= frame_train + 1,
        "{wakes} wakes over {window:?} against the frame train's {frame_train}"
    );
    assert!(
        wakes > 20,
        "non-vacuous: the fade's own steps asked for frames ({wakes})"
    );
}

#[test]
fn the_settling_momentum_glow_keeps_the_lane_to_its_last_frame() {
    use aterm_effects::cursor_glow::Geom;
    use aterm_effects::cursor_momentum::{
        MOMENTUM_GLOW_RADIUS_CELLS, MOMENTUM_GLOW_TAU_S, MomentumGlowConfig, MomentumGlowFrame,
    };

    let geom = Geom {
        cw: 8,
        ch: 16,
        rows: 24,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 640,
        win_h: 384,
        head: 0,
    };
    let cfg = MomentumGlowConfig {
        enabled: true,
        intensity: 1.0,
        tau_s: MOMENTUM_GLOW_TAU_S,
        radius_cells: MOMENTUM_GLOW_RADIUS_CELLS,
        base: 0x0050_FA7B,
        dark_theme: true,
        hue: 0x0050_FA7B,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        block: true,
        glyph_span: None,
    };
    let mut app = App::headless_for_test();
    let t0 = Instant::now();
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    ws.focused = true;
    ws.frame_interval = Some(Duration::from_nanos(8_333_333));
    let interval = ws.effect_present_interval();
    let mut halo = Vec::new();
    let tick = |ws: &mut super::WindowState,
                at: Instant,
                col: u16,
                halo: &mut Vec<_>|
     -> MomentumGlowFrame {
        halo.clear();
        ws.fx
            .cursor_mut()
            .momentum
            .tick(Some((4, col)), at, geom, &cfg, halo, &mut Vec::new())
    };

    let mut t = t0;
    let mut col = 10u16;
    for k in 0..24u16 {
        t = t0 + Duration::from_millis(60 * u64::from(k));
        col = 10 + k;
        ws.fx.cursor_mut().momentum.on_key(t, MOMENTUM_GLOW_TAU_S);
        tick(ws, t, col, &mut halo);
    }
    let last_key = t;
    while !ws.fx.cursor().momentum.settling() {
        t += interval;
        tick(ws, t, col, &mut halo);
        assert!(
            t.saturating_duration_since(t0) < Duration::from_secs(10),
            "fixture: the glow's frames never stopped moving"
        );
    }
    assert!(!halo.is_empty(), "fixture: the halo is still on glass");
    assert!(
        !ws.cursor_dependents_need_frame_cadence(t, false),
        "negative control: the frame-cadence set alone parks the lane here, mid-cool-down"
    );
    assert!(
        ws.cursor_fx_active(t, false),
        "…but the cool-down is live effect state"
    );
    let first = ws
        .plan_terminal_effect_lane(t, false, true)
        .expect("the settling glow owns a deadline");
    assert!(first > t, "the deadline is in the future");

    let horizon = t + Duration::from_secs(20);
    let mut wakes = 0_usize;
    let mut deadline = first;
    let (released_at, last) = loop {
        assert!(
            deadline <= horizon,
            "the lane never released: {wakes} wakes"
        );
        assert!(
            ws.service_due_terminal_effect_tick(deadline),
            "the armed clock is due at its deadline"
        );
        let frame = tick(ws, deadline, col, &mut halo);
        wakes += 1;
        match ws.plan_terminal_effect_lane(deadline, false, true) {
            Some(next) => {
                assert!(next > deadline, "a deadline must advance");
                deadline = next;
            }
            None => break (deadline, frame),
        }
    };
    assert!(
        halo.is_empty() && last.fill.is_none() && !last.hot,
        "the lane parks only on a frame that drew nothing ({} halo quads)",
        halo.len()
    );
    assert!(
        !ws.fx.cursor().momentum.is_active() && !ws.fx.cursor().momentum.settling(),
        "the lane parks only once the glow itself is cold"
    );
    let window = released_at.saturating_duration_since(t);
    let frame_train = (window.as_secs_f32() / interval.as_secs_f32()) as usize;
    println!(
        "settling momentum glow: {wakes} wakes over {window:?} after the frame train let go \
         ({:?} after the last key; the frame train: {frame_train} at {interval:?})",
        released_at.saturating_duration_since(last_key)
    );
}

#[test]
fn static_collection_hello_uses_cursor_effect_presentability() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = std::time::Instant::now();
    {
        let ws = app.windows.get_mut(&wid).expect("test window");
        ws.fx
            .companions_mut()
            .cursor_cat
            .on_collect(now, aterm_effects::kitty_registry::KittyLook::default());
        ws.last_key_at = Some(now);
    }

    app.on_focus(wid, false);
    let ws = app.windows.get(&wid).expect("test window");
    assert!(!ws.focused);
    assert!(ws.cursor_fx_typed_wake(now));
    assert!(
        ws.fx.companions().cursor_cat.static_deadline(now).is_some(),
        "typed-wake focus loss must not pause an on-glass hello"
    );
    assert!(
        ws.static_cursor_cat_deadline(now, false, false).is_some(),
        "the reduced-motion scheduler must fold that same typed wake"
    );

    let ws = app.windows.get(&wid).expect("test window");
    assert!(
        ws.static_cursor_cat_deadline(now, false, true).is_some(),
        "a recorded static hello also owns its bounded erase wake"
    );

    let mut paused = App::headless_for_test();
    {
        let ws = paused.windows.get_mut(&wid).expect("test window");
        ws.fx
            .companions_mut()
            .cursor_cat
            .on_collect(now, aterm_effects::kitty_registry::KittyLook::default());
        ws.fx
            .companions_mut()
            .cursor_cat
            .set_collection_presentable(now, false);
        ws.last_key_at = Some(now);
        assert!(
            ws.fx
                .companions_mut()
                .cursor_cat
                .static_deadline(now)
                .is_none()
        );
    }
    paused.on_focus(wid, false);
    assert!(
        paused.windows[&wid]
            .fx
            .companions()
            .cursor_cat
            .static_deadline(now)
            .is_none(),
        "a typed focus pin preserves, but never overrides, a prior presentation pause"
    );
}

#[test]
fn disabling_serious_mode_restores_latest_underlying_requests() {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("comet".into());
    app.set_session_rain_override(0, Some(true));

    assert!(app.toggle_serious_mode());
    assert!(!app.trail_config().enabled);
    assert!(!app.session_rain_enabled(0));

    assert!(!app.toggle_serious_mode());
    assert!(!app.serious_mode_enabled());
    assert_eq!(app.config.serious_mode, Some(false));
    assert!(!app.notify_silent.load(std::sync::atomic::Ordering::Acquire));
    assert!(app.glow_config().enabled);
    assert!(app.trail_config().enabled);
    assert!(app.session_rain_enabled(0));
    assert!(
        app.trail_audio.is_inert_for_test(),
        "headless stays device-free"
    );

    assert!(app.set_serious_mode(true));
    app.config.cursor_trail = Some(false);
    assert!(!app.set_serious_mode(false));
    assert!(!app.glow_config().enabled);
    assert!(!app.trail_config().enabled);
    assert!(app.session_rain_enabled(0));
    assert_eq!(app.config.cursor_trail, Some(false));
    assert_eq!(app.pool.rain_override(0), Some(true));
}

#[test]
fn a_windowed_bell_plays_one_alert_sound_per_lane() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let now = std::time::Instant::now();
    let before = crate::alert_sounds_on_this_thread();

    app.on_bell(WindowId(0), 0, now);
    app.on_bell(WindowId(0), 0, now);

    assert_eq!(
        crate::alert_sounds_on_this_thread() - before,
        1,
        "two bells inside one lane interval play exactly one alert sound",
    );
    assert!(
        !app.bell_beep.try_fire(now),
        "the bell that sounded burned the lane's token",
    );
}

#[test]
fn serious_mode_bell_never_consumes_the_audible_rate_limit_lane() {
    let mut app = App::headless_for_test();
    app.headless = false;
    assert!(app.set_serious_mode(true));
    let now = std::time::Instant::now();
    let before = crate::alert_sounds_on_this_thread();

    app.on_bell(WindowId(0), 0, now);

    assert_eq!(
        crate::alert_sounds_on_this_thread(),
        before,
        "a serious-mode BEL plays no alert sound",
    );
    assert!(!app.set_serious_mode(false));
    assert!(
        app.bell_beep.try_fire(now),
        "a serious-mode BEL must not consume the audible rate-limit lane"
    );
}

#[test]
fn a_silenced_bell_never_consumes_the_audible_rate_limit_lane() {
    let mut app = App::headless_for_test();
    app.headless = false;
    assert!(
        app.config.bell_sound_or_default(),
        "the audible bell ships ON; this switch is the opt-OUT",
    );
    let now = std::time::Instant::now();
    let before = crate::alert_sounds_on_this_thread();

    app.config.bell_sound = Some(false);
    app.on_bell(WindowId(0), 0, now);
    assert_eq!(
        crate::alert_sounds_on_this_thread(),
        before,
        "a silenced BEL plays no alert sound",
    );
    assert!(
        app.bell_beep.try_fire(now),
        "a silenced BEL must not consume the audible rate-limit lane",
    );
}

#[test]
fn a_headless_bell_never_reaches_the_alert_sound() {
    let mut app = App::headless_for_test();
    assert!(app.headless, "the fixture is a headless instance");
    assert!(
        app.config.bell_sound_or_default(),
        "the audible bell ships ON, so the switch is not what silences it here",
    );
    assert!(
        app.serious_mode_policy()
            .allows(crate::motion::SeriousEffect::TerminalSound),
        "serious mode is off, so it is not what silences it here either",
    );
    let now = std::time::Instant::now();
    let before = crate::alert_sounds_on_this_thread();

    app.on_bell(WindowId(0), 0, now);
    assert_eq!(
        crate::alert_sounds_on_this_thread(),
        before,
        "a headless BEL must never reach the alert sound",
    );
    assert!(
        app.bell_beep.try_fire(now),
        "a headless BEL must never burn the alert sound's lane",
    );
}

fn status_field<'a>(record: &'a str, key: &str) -> Option<&'a str> {
    record
        .split(' ')
        .find_map(|kv| kv.strip_prefix(key)?.strip_prefix('='))
}

#[test]
fn a_bell_is_kept_on_the_tab_that_rang_it_and_on_no_other() {
    let mut app = App::headless_for_test();
    app.headless = false;
    let quiet = app.next_session_id;
    app.push_stub_tab(WindowId(0), crate::stub_session(quiet));
    {
        let ctx = app.pool.get(0).expect("the ringing tab").ctx.clone();
        let mut timeline = ctx.timeline.lock().unwrap();
        timeline.note_foreground_group(4242);
        timeline.set_program(4242, Some("vim".into()));
    }
    let now = std::time::Instant::now();
    let before = crate::alert_sounds_on_this_thread();

    app.on_bell(WindowId(0), 0, now);
    app.on_bell(WindowId(0), 0, now);
    app.config.bell_sound = Some(false);
    app.on_bell(WindowId(0), 0, now);

    assert_eq!(
        crate::alert_sounds_on_this_thread() - before,
        1,
        "the record moves no gate: one sound for the three bells",
    );
    let rang = app.session_status_record(0).expect("the ringing tab");
    assert_eq!(status_field(&rang, "bells"), Some("3"), "{rang}");
    assert_eq!(status_field(&rang, "bells_sounded"), Some("1"), "{rang}");
    let last: Vec<(&str, &str)> = status_field(&rang, "last_bell")
        .expect("last_bell=")
        .split(',')
        .map(|entry| {
            let mut parts = entry.split('/');
            let age = parts.next().expect("an age");
            assert!(age.parse::<u64>().is_ok(), "{entry}");
            (
                parts.next().expect("a sound"),
                parts.next().expect("a program"),
            )
        })
        .collect();
    assert_eq!(
        last,
        [("off", "vim"), ("limited", "vim"), ("sounded", "vim")],
        "the newest first: {rang}"
    );

    let silent = app.session_status_record(quiet).expect("the quiet tab");
    assert!(
        silent.contains(" bells=0 bells_sounded=0 last_bell=- "),
        "a tab that rang nothing says so: {silent}"
    );
}

#[test]
fn a_quiet_bell_is_kept_with_the_gate_that_quieted_it() {
    let mut app = App::headless_for_test();
    let now = std::time::Instant::now();
    app.on_bell(WindowId(0), 0, now);
    app.headless = false;
    assert!(app.set_serious_mode(true));
    app.on_bell(WindowId(0), 0, now);
    let record = app.session_status_record(0).expect("live session");
    assert_eq!(status_field(&record, "bells"), Some("2"), "{record}");
    assert_eq!(
        status_field(&record, "bells_sounded"),
        Some("0"),
        "{record}"
    );
    let sounds: Vec<&str> = status_field(&record, "last_bell")
        .expect("last_bell=")
        .split(',')
        .map(|entry| entry.split('/').nth(1).expect("a sound"))
        .collect();
    assert_eq!(sounds, ["serious", "headless"], "{record}");
}

#[test]
fn capture_present_barrier_is_bounded_and_fail_closed() {
    let drive = |sequence: &[bool]| {
        let mut calls = 0;
        let captured = run_capture_present_barrier(NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, || {
            let value = sequence.get(calls).copied().unwrap_or(false);
            calls += 1;
            value
        });
        (captured, calls)
    };

    assert_eq!(drive(&[true]), (true, 1));
    assert_eq!(drive(&[false, false, true]), (true, 3));
    assert_eq!(drive(&[false, false, false]), (false, 3));
    assert_eq!(drive(&[]), (false, 3));
    assert!(!run_capture_present_barrier(0, || panic!(
        "zero bound must not attempt a present"
    )));
}

#[test]
fn native_preview_deadlines_preserve_idle_and_blink_edges() {
    use crate::settings_preview::PreviewAnimation;

    let now = std::time::Instant::now();
    assert_eq!(
        native_preview_deadline(now, None, PreviewAnimation::None),
        None
    );
    assert_eq!(
        native_preview_deadline(now, None, PreviewAnimation::Continuous),
        Some(now + DEMO_TICK_INTERVAL)
    );

    let already_armed = now + std::time::Duration::from_millis(7);
    assert_eq!(
        native_preview_deadline(now, Some(already_armed), PreviewAnimation::Continuous),
        Some(already_armed),
        "unrelated events must not postpone the existing frame deadline"
    );
    assert_eq!(
        native_preview_deadline(
            now,
            Some(already_armed),
            PreviewAnimation::BlinkEdge { after_ms: 430 },
        ),
        Some(now + std::time::Duration::from_millis(430)),
        "blink-only mode replaces a stale frame cadence with its visual edge"
    );

    assert!(native_preview_may_tick(true, true, false, false));
    assert!(
        native_preview_may_tick(true, false, true, false),
        "an unfocused recorded window still has a real watcher"
    );
    assert!(
        native_preview_may_tick(false, false, true, false),
        "a headless Present-Real recording must advance semantic effects"
    );
    for blocked in [
        (false, true, false, false),
        (true, false, false, false),
        (true, true, false, true),
        (true, false, true, true),
    ] {
        assert!(
            !native_preview_may_tick(blocked.0, blocked.1, blocked.2, blocked.3),
            "unwatched and modal-obscured previews stay parked: {blocked:?}"
        );
    }
}

fn url_at(line: &str, col: usize) -> Option<String> {
    let chars: Vec<char> = line.chars().collect();
    find_url_span(&chars, col).map(|(u, _, _)| u)
}

#[test]
fn config_parsing() {
    let c: Config =
        aterm_toml::from_str("font_px = 24.0\ngpu = true\nscrollback_lines = 5000").unwrap();
    assert_eq!(c.font_px, Some(24.0));
    assert_eq!(c.gpu, Some(true));
    assert_eq!(c.scrollback_lines, Some(5000));
    assert_eq!(c.applied_terminal_config().scrollback_limit, Some(5000));
    let unlimited: Config = aterm_toml::from_str("scrollback_lines = 0").unwrap();
    assert_eq!(unlimited.applied_terminal_config().scrollback_limit, None);
    assert_eq!(
        aterm_toml::from_str::<Config>("font_px = 18.0")
            .unwrap()
            .applied_terminal_config(),
        Config::default().applied_terminal_config()
    );
    use aterm_core::terminal::CursorStyle;
    let c: Config = aterm_toml::from_str("cursor_style = \"bar\"\ncursor_blink = false").unwrap();
    assert_eq!(
        c.applied_terminal_config().cursor_style,
        CursorStyle::SteadyBar
    );
    let c: Config = aterm_toml::from_str("cursor_style = \"weird\"").unwrap();
    assert_eq!(
        c.applied_terminal_config().cursor_style,
        CursorStyle::BlinkingBlock
    );
    let c: Config =
        aterm_toml::from_str("gpu = false\nfuture_key = \"x\"\n[unknown]\nk = 1").unwrap();
    assert_eq!(c.font_px, None);
    assert_eq!(c.gpu, Some(false));
    let c: Config = aterm_toml::from_str("").unwrap();
    assert!(c.font_px.is_none() && c.gpu.is_none());
    assert!(aterm_toml::from_str::<Config>("font_px = \"big\"").is_err());
    let c: Config = aterm_toml::from_str("columns = 120\nlines = 40").unwrap();
    assert_eq!((c.columns, c.lines), (Some(120), Some(40)));
    use crate::app_config::search_index_depth;
    assert_eq!(
        search_index_depth(&Config::default()),
        aterm_core::search::DEFAULT_MAX_CACHED_LINES
    );
    let c: Config = aterm_toml::from_str("search_history_lines = 50000").unwrap();
    assert_eq!(
        (c.search_history_lines, search_index_depth(&c)),
        (Some(50_000), 50_000)
    );
    let c: Config = aterm_toml::from_str("search_history_lines = 0").unwrap();
    assert_eq!(search_index_depth(&c), 0);
    let c: Config = aterm_toml::from_str("search_history_lines = 5000000").unwrap();
    assert_eq!(search_index_depth(&c), 5_000_000);
    let c: Config = aterm_toml::from_str("tab_strip_rows = 2").unwrap();
    assert_eq!(c.tab_strip_rows, Some(2));
    assert_eq!(Config::default().tab_strip_rows, None);
    use aterm_types::scheme::parse_hex;
    assert_eq!(
        parse_hex("#FF8800"),
        aterm_core::terminal::Rgb::new(0xFF, 0x88, 0x00).into()
    );
    assert_eq!(
        parse_hex("102030"),
        aterm_core::terminal::Rgb::new(0x10, 0x20, 0x30).into()
    );
    assert!(parse_hex("#xyz").is_none() && parse_hex("#12345").is_none());
    let c: Config = aterm_toml::from_str(
        "foreground = \"#102030\"\nbackground = \"#405060\"\nselection_color = \"#708090\"",
    )
    .unwrap();
    let th = c.theme();
    assert_eq!(
        (th.fg, th.bg, th.selection),
        (0x0010_2030, 0x0040_5060, 0x0070_8090)
    );
    assert_eq!(
        Config::default().theme().selection,
        super::Theme::default().selection
    );
}

#[test]
fn paste_confirm_flags_any_line_end_only_when_unbracketed() {
    assert!(!super::paste_needs_confirm("a\nb\nc", true));
    assert!(!super::paste_needs_confirm("make test\n", true));
    assert!(!super::paste_needs_confirm("ls -la", false));
    assert!(super::paste_needs_confirm("make test\n", false));
    assert!(super::paste_needs_confirm("make test\r\n", false));
    assert!(super::paste_needs_confirm("make test\r", false));
    assert!(super::paste_needs_confirm("ls\nrm -rf ~\n", false));
    assert!(super::paste_needs_confirm("a\rb", false));
    assert!(Config::default().confirm_multiline_paste_or_default());
    assert!(
        !aterm_toml::from_str::<Config>("confirm_multiline_paste = false")
            .unwrap()
            .confirm_multiline_paste_or_default()
    );
}

#[test]
fn controls_prefs_projects_current_native_top_settings_tree() {
    use crate::app_introspect::AuxTarget;
    let mut app = crate::App::headless_for_test();

    let closed = app.read_aux_controls(AuxTarget::Prefs);
    assert_eq!(closed[0], "state open=false");
    assert!(closed.iter().any(|line| line == "prefs fields=0"));
    assert!(
        closed
            .iter()
            .any(|line| line == "native surface=native-tab route=- controls=0 semantics=0")
    );

    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let open = app.read_aux_controls(AuxTarget::Prefs);
    assert!(
        open[0].contains("state open=true")
            && open[0].contains("pane=native-tab")
            && open[0].contains("surface=native-tab")
            && open[0].contains("route=/home")
            && open[0].contains("page=\"Top Settings\"")
            && open[0].contains("category=Top Settings"),
        "native route identity is explicit: {:?}",
        open[0]
    );
    assert!(
        !open[0].contains("pane=sidebar") && !open[0].contains("category=Appearance"),
        "the retired overlay cannot leak into native inspection: {:?}",
        open[0]
    );

    let (wid, instance, view) = app
        .native_settings_view_target()
        .map(|(wid, instance, view, _)| (wid, instance, view))
        .expect("native Settings target");
    let viewport = app
        .native_ui_viewport_for(wid, view)
        .expect("exact native viewport");
    let compiled = app
        .compiled_native_ui_for(wid, instance, view, viewport)
        .expect("native Settings semantic tree compiles");
    let expected_ui = compiled.controls_lines();
    let emitted_ui = open
        .iter()
        .filter(|line| line.starts_with("ui "))
        .cloned()
        .collect::<Vec<_>>();
    assert_eq!(
        emitted_ui, expected_ui,
        "compatibility inspection appends the exact compiled native semantics"
    );
    let actionable = compiled
        .semantics
        .iter()
        .filter(|node| node.action.is_some())
        .count();
    assert!(open.iter().any(|line| {
        line == &format!(
            "native surface=native-tab route=/home controls={actionable} semantics={}",
            compiled.semantics.len()
        )
    }));

    let mut expected_field_keys: Vec<String> = Vec::new();
    for node in &compiled.semantics {
        if let Some(key) = crate::native_settings::edited_setting_key(&node.key)
            && !expected_field_keys.iter().any(|seen| seen == key)
        {
            expected_field_keys.push(key.to_string());
        }
    }
    let emitted_field_keys = open
        .iter()
        .filter_map(|line| {
            line.strip_prefix("field key=")?
                .split_whitespace()
                .next()
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
    assert_eq!(emitted_field_keys, expected_field_keys);
    assert!(
        open.iter()
            .any(|line| line == &format!("prefs fields={}", expected_field_keys.len())),
        "compatibility field count binds the visible native setting controls"
    );
    assert!(expected_field_keys.contains(&crate::prefs::EDIT_THEME.to_string()));
}

#[test]
fn keybindings_table_parses() {
    let kb = crate::keybinding::Keybindings::from_config(Config::default().keybindings.as_ref());
    assert!(kb.is_empty());
    let c: Config = aterm_toml::from_str(
        "[keybindings]\n\"cmd+shift+n\" = \"new_tab\"\n\"ctrl+a\" = \"find\"\n",
    )
    .unwrap();
    let kb = crate::keybinding::Keybindings::from_config(c.keybindings.as_ref());
    assert!(!kb.is_empty());
    use aterm_window::keyboard::{Key as WK, ModifiersState as MS, SmolStr};
    assert_eq!(
        kb.lookup(&WK::Character(SmolStr::new("n")), MS::SUPER | MS::SHIFT),
        Some(crate::keybinding::Action::NewTab)
    );
    assert_eq!(
        kb.lookup(&WK::Character(SmolStr::new("a")), MS::CONTROL),
        Some(crate::keybinding::Action::Find)
    );
    assert_eq!(
        kb.lookup(&WK::Character(SmolStr::new("t")), MS::SUPER),
        None
    );
}

#[test]
fn theme_colors_reach_rendercell() {
    use aterm_core::terminal::Terminal;
    let c: Config =
        aterm_toml::from_str("foreground = \"#FF8800\"\nbackground = \"#102030\"").unwrap();
    let mut t = Terminal::new(4, 10);
    t.apply_config(&c.applied_terminal_config());
    t.process(b"X");
    let cells = t.render_row(0);
    assert_eq!(cells[0].ch, 'X');
    assert_eq!(cells[0].fg, [0xFF, 0x88, 0x00]);
    assert_eq!(cells[0].bg, [0x10, 0x20, 0x30]);

    let c: Config = aterm_toml::from_str("palette = [\"#000000\", \"#AB12CD\"]").unwrap();
    let mut t = Terminal::new(4, 10);
    t.apply_config(&c.applied_terminal_config());
    t.process(b"\x1b[31mR");
    let cells = t.render_row(0);
    assert_eq!(cells[0].ch, 'R');
    assert_eq!(cells[0].fg, [0xAB, 0x12, 0xCD]);

    let c: Config =
        aterm_toml::from_str("theme = \"Dracula\"\npalette = [\"#000000\", \"#00FF00\"]").unwrap();
    let mut t = Terminal::new(2, 4);
    t.apply_config(&c.applied_terminal_config());
    t.process(b"\x1b[31mR");
    let cells = t.render_row(0);
    assert_eq!(
        cells[0].fg,
        [0x00, 0xff, 0x00],
        "palette[1] override beats the theme's ansi[1]"
    );
    assert_ne!(cells[0].fg, [0xff, 0x55, 0x55], "not Dracula's red");
}

#[test]
fn named_theme_reaches_rendercell_and_chrome() {
    use aterm_core::terminal::Terminal;
    let c: Config = aterm_toml::from_str("theme = \"Dracula\"").unwrap();
    let th = c.theme();
    assert_eq!(th.fg, 0x00f8_f8f2, "theme fg reaches the renderer Theme");
    assert_eq!(th.bg, 0x0028_2a36, "theme bg reaches the renderer Theme");
    let mut t = Terminal::new(4, 12);
    t.apply_config(&c.applied_terminal_config());
    t.process(b"A\x1b[31mR\x1b[0m\x1b[1;34mB");
    let cells = t.render_row(0);
    assert_eq!(cells[0].ch, 'A');
    assert_eq!(cells[0].fg, [0xf8, 0xf8, 0xf2], "default fg = Dracula fg");
    assert_eq!(cells[0].bg, [0x28, 0x2a, 0x36], "default bg = Dracula bg");
    assert_eq!(cells[1].fg, [0xff, 0x55, 0x55], "SGR 31 → Dracula ansi[1]");
    assert_eq!(
        cells[2].fg,
        [0xd6, 0xac, 0xff],
        "bold SGR 34 → Dracula ansi[12] (bright)"
    );

    let c2: Config = aterm_toml::from_str("theme = \"Dracula\"\nbackground = \"#102030\"").unwrap();
    assert_eq!(
        c2.theme().bg,
        0x0010_2030,
        "background override beats the theme"
    );
    let mut t2 = Terminal::new(2, 4);
    t2.apply_config(&c2.applied_terminal_config());
    t2.process(b"X");
    assert_eq!(t2.render_row(0)[0].bg, [0x10, 0x20, 0x30]);
}

#[test]
fn unknown_theme_falls_back_to_default() {
    let c: Config = aterm_toml::from_str("theme = \"no-such-theme\"").unwrap();
    assert_eq!(c.theme().bg, super::Theme::default().bg);
    assert_eq!(c.theme().fg, super::Theme::default().fg);
    let resolved = c.resolved_colors(
        aterm_types::Appearance::Dark,
        &crate::app_config::ThemeCatalog::default(),
        None,
    );
    assert!(matches!(
        resolved.source,
        crate::resolved_colors::ThemeSource::DefaultFallback { .. }
    ));
}

#[test]
fn font_px_precedence_flag_beats_config_beats_default() {
    use super::{FONT_PX, FONT_PX_MAX, FONT_PX_MIN};
    use crate::app_config::{font_px_is_explicit_with, resolve_font_px_with};
    assert_eq!(resolve_font_px_with(Some(24.0), Some(18.0)), 24.0);
    assert_eq!(resolve_font_px_with(None, Some(18.0)), 18.0);
    assert_eq!(resolve_font_px_with(None, None), FONT_PX);
    assert_eq!(resolve_font_px_with(Some(9999.0), None), FONT_PX);
    assert_eq!(resolve_font_px_with(None, Some(0.0)), FONT_PX);
    assert_eq!(resolve_font_px_with(Some(9999.0), Some(18.0)), 18.0);
    assert_eq!(resolve_font_px_with(Some(9999.0), Some(9999.0)), FONT_PX);
    assert_eq!(resolve_font_px_with(Some(6.0), None), FONT_PX_MIN);
    assert_eq!(resolve_font_px_with(None, Some(200.0)), FONT_PX_MAX);

    assert!(!font_px_is_explicit_with(None, None));
    assert!(!font_px_is_explicit_with(None, Some(0.0)));
    assert!(!font_px_is_explicit_with(Some(f32::NAN), Some(f32::NAN)));
    assert!(font_px_is_explicit_with(Some(f32::NAN), Some(18.0)));
    assert!(font_px_is_explicit_with(Some(24.0), Some(9999.0)));
}

#[test]
fn reload_retheme_then_revert_live_terminal() {
    use aterm_core::terminal::Terminal;
    let mut t = Terminal::new(4, 10);
    t.process(b"X");
    let themed: Config = aterm_toml::from_str("background = \"#203040\"").unwrap();
    t.apply_config(&themed.applied_terminal_config());
    assert_eq!(t.render_row(0)[0].bg, [0x20, 0x30, 0x40]);
    let empty: Config = aterm_toml::from_str("").unwrap();
    t.apply_config(&empty.applied_terminal_config());
    let theme_bg = {
        let bg = empty.theme().bg;
        [
            ((bg >> 16) & 0xff) as u8,
            ((bg >> 8) & 0xff) as u8,
            (bg & 0xff) as u8,
        ]
    };
    assert_eq!(
        t.render_row(0)[0].bg,
        theme_bg,
        "revert lands on the theme bg"
    );
    assert_ne!(
        t.render_row(0)[0].bg,
        [0, 0, 0],
        "revert never falls back to spec-black"
    );
    assert_ne!(t.render_row(0)[0].bg, [0x20, 0x30, 0x40]);
}

#[test]
fn find_matches_and_highlight() {
    use crate::app_search::map_matches;
    use crate::control::search_full_history;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
    term.lock().unwrap().process(b"Hello hello HELLO");
    let base_y = i64::try_from(term.lock().unwrap().grid().base_y()).unwrap();

    let results = search_full_history(&term, "hello", false, false).unwrap();
    let matches = map_matches(&results.results.matches, base_y);
    assert_eq!(matches, vec![(0, 0, 4), (0, 6, 10), (0, 12, 16)]);
    assert!(
        search_full_history(&term, "xyz", false, false)
            .unwrap()
            .results
            .matches
            .is_empty()
    );

    let (r, c0, c1) = matches[0];
    let mut t = term.lock().unwrap();
    let sel = t.text_selection_mut();
    sel.start_selection(r, c0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r, u16::try_from(c1).unwrap(), SelectionSide::Right);
    assert_eq!(t.selection_to_string().as_deref(), Some("Hello"));
}

#[test]
fn search_wide_char_columns_are_display_cells() {
    use crate::app_search::map_matches;
    use crate::control::search_full_history;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    let term = Arc::new(Mutex::new(Terminal::new(4, 20)));
    term.lock().unwrap().process("世界hit".as_bytes());
    let base_y = i64::try_from(term.lock().unwrap().grid().base_y()).unwrap();
    let results = search_full_history(&term, "hit", false, false).unwrap();
    let matches = map_matches(&results.results.matches, base_y);
    assert_eq!(
        matches,
        vec![(0, 4, 6)],
        "match reported at DISPLAY cols 4..=6"
    );

    let (r, c0, c1) = matches[0];
    let mut t = term.lock().unwrap();
    let sel = t.text_selection_mut();
    sel.start_selection(r, c0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r, u16::try_from(c1).unwrap(), SelectionSide::Right);
    assert_eq!(t.selection_to_string().as_deref(), Some("hit"));
}

#[test]
fn a_hit_that_straddles_a_soft_wrap_is_found_once_at_the_row_it_starts_on() {
    use crate::app_search::map_matches;
    use crate::control::search_full_history;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
    term.lock().unwrap().process(b"aaaaaaaawrapmatchbb");
    let base_y = i64::try_from(term.lock().unwrap().grid().base_y()).unwrap();

    let results = search_full_history(&term, "wrapmatch", false, false).unwrap();
    let matches = map_matches(&results.results.matches, base_y);
    assert_eq!(
        matches,
        vec![(0, 8, 16)],
        "one hit at row 0 col 8, ending 9 columns later — 7 of them on row 1"
    );

    let (row, c0, c1) = matches[0];
    let cols = usize::from(term.lock().unwrap().grid().cols());
    let mut t = term.lock().unwrap();
    let sel = t.text_selection_mut();
    sel.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(
        row + i32::try_from(c1 as usize / cols).unwrap(),
        u16::try_from(c1 as usize % cols).unwrap(),
        SelectionSide::Right,
    );
    assert_eq!(t.selection_to_string().as_deref(), Some("wrapmatch"));
}

#[test]
fn a_vs16_base_left_narrow_in_the_last_column_keeps_the_runs_stride() {
    use crate::app_search::map_matches;
    use crate::control::search_full_history;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    let run = concat!(
        "aaaaaaaaa\u{2714}\u{fe0f}",
        "QbbbbbbbbY",
        "cccccccc\u{26a0}\u{fe0f}",
        "ddZ",
    );
    let mut wrong = Vec::new();
    for (place, trailer) in [
        ("on the screen", ""),
        ("in history", "\r\n\r\n\r\n\r\n\r\n\r\n"),
    ] {
        let term = Arc::new(Mutex::new(Terminal::new(6, 10)));
        term.lock()
            .unwrap()
            .process(format!("{run}{trailer}").as_bytes());
        {
            let t = term.lock().unwrap();
            if place == "on the screen" {
                let g = t.grid();
                assert!(
                    !g.cell(0, 9).unwrap().is_wide() && !g.cell(2, 9).unwrap().is_wide(),
                    "precondition: both bases stay one cell wide"
                );
                assert_eq!(t.cell_grapheme(1, 0).as_deref(), Some("Q"));
                assert_eq!(t.cell_grapheme(1, 9).as_deref(), Some("Y"));
                assert_eq!(t.cell_grapheme(3, 2).as_deref(), Some("Z"));
            } else {
                assert!(
                    t.grid().scrollback_lines() >= 4,
                    "precondition: the run is history"
                );
            }
        }
        let base_y = i64::try_from(term.lock().unwrap().grid().base_y()).unwrap();
        let cols = usize::from(term.lock().unwrap().grid().cols());
        for needle in ["Q", "Y", "Z", "bY", "Ycc"] {
            let results = search_full_history(&term, needle, true, false).unwrap();
            let matches = map_matches(&results.results.matches, base_y);
            let picked: Vec<_> = matches
                .iter()
                .map(|&(row, c0, c1)| {
                    let mut t = term.lock().unwrap();
                    let sel = t.text_selection_mut();
                    sel.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
                    sel.update_selection(
                        row + i32::try_from(c1 as usize / cols).unwrap(),
                        u16::try_from(c1 as usize % cols).unwrap(),
                        SelectionSide::Right,
                    );
                    t.selection_to_string()
                })
                .collect();
            if picked != [Some(needle.to_string())] {
                wrong.push(format!(
                    "{place}: {needle:?} reported at {matches:?}, which selects {picked:?}"
                ));
            }
        }
    }
    assert!(
        wrong.is_empty(),
        "find names cells the hit is not drawn in:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn a_soft_wrap_does_not_splice_a_match_across_the_blanks_at_a_row_end() {
    use crate::control::search_full_history;
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    let term = Arc::new(Mutex::new(Terminal::new(4, 10)));
    term.lock().unwrap().process(b"abcdef   \x20gh");
    assert!(
        search_full_history(&term, "fg", false, false)
            .unwrap()
            .results
            .matches
            .is_empty(),
        "blanks at the row end are real columns, not a seam to match across"
    );
    assert_eq!(
        search_full_history(&term, "gh", false, false)
            .unwrap()
            .results
            .matches
            .len(),
        1,
        "the continuation row itself still matches"
    );
}

#[test]
fn url_detection_in_text() {
    let line = "see (http://example.com/p?q=1). end";
    assert_eq!(url_at(line, 5).as_deref(), Some("http://example.com/p?q=1"));
    assert_eq!(
        url_at(line, 12).as_deref(),
        Some("http://example.com/p?q=1")
    );
    let close_paren = line.find(')').unwrap();
    assert_eq!(url_at(line, close_paren), None);
    assert_eq!(url_at(line, 0), None);
    assert_eq!(url_at(line, 2), None);
    let bare = "https://a.b/c";
    assert_eq!(url_at(bare, 0).as_deref(), Some("https://a.b/c"));
    assert_eq!(
        url_at(bare, bare.len() - 1).as_deref(),
        Some("https://a.b/c")
    );
    assert_eq!(url_at("ftp://x.y not-a-link", 0), None);
}

#[test]
fn a_plain_url_is_as_long_as_its_ink_and_says_when_the_ink_is_faint() {
    let row_of = |bytes: &str| {
        let mut t = aterm_core::terminal::Terminal::new(5, 80);
        t.process(bytes.as_bytes());
        t.render_row(0)
    };
    let at = |row: &[crate::RenderCell], col, floor| {
        crate::plain_url_at(row, col, floor).map(|plain| (plain.url, plain.faint))
    };
    let tail_in = |ink: &str| {
        row_of(&format!(
            "Docs: \x1b[48;2;17;19;24mhttps://google.com{ink}.evil.example/login\x1b[0m \
             (official)\r\n"
        ))
    };
    let hidden = tail_in("\x1b[8m");
    for floor in [1.0, 4.5] {
        let head = Some(("https://google.com".to_string(), false));
        assert_eq!(at(&hidden, 20, floor), head, "floor {floor}");
        assert_eq!(at(&hidden, 30, floor), None, "floor {floor}");
    }
    for (ink, faint) in [
        ("\x1b[38;2;18;19;24m", true),
        ("\x1b[38;2;40;43;50m", true),
        ("\x1b[38;2;88;91;100m", true),
        ("\x1b[38;2;95;98;108m", false),
    ] {
        let tail = tail_in(ink);
        let whole = Some(("https://google.com.evil.example/login".to_string(), faint));
        assert_eq!(at(&tail, 20, 1.0), whole, "{ink:?}");
        assert_eq!(at(&tail, 30, 1.0), whole, "{ink:?}");
    }
    let plain = row_of("Docs: https://example.com/path/to/page\r\n");
    let page = Some(("https://example.com/path/to/page".to_string(), false));
    assert_eq!(at(&plain, 20, 1.0), page);
    let dim = row_of("Docs: \x1b[38;2;33;34;44;48;2;40;42;54mhttps://example.com/path\x1b[0m\r\n");
    let url = "https://example.com/path".to_string();
    assert_eq!(at(&dim, 20, 4.5), Some((url.clone(), false)));
    assert_eq!(at(&dim, 20, 1.0), Some((url, true)));
}

#[test]
fn safe_url_allowlist() {
    for ok in [
        "http://example.com",
        "https://example.com/path?q=1#frag",
        "HTTPS://EXAMPLE.COM",
        "mailto:user@example.com",
        "  https://example.com  ", // surrounding whitespace is trimmed
    ] {
        assert!(is_safe_url(ok), "should allow: {ok:?}");
    }
    for bad in [
        "file:///etc/passwd",
        "x-apple-systempreferences:com.apple.preference",
        "javascript:alert(1)",
        "tel:+15551234",
        "ftp://example.com",
        "http://exa mple.com", // internal whitespace
        "https://e\nvil.com",  // control byte
        "://noscheme",
        "",
        "   ",
    ] {
        assert!(!is_safe_url(bad), "should reject: {bad:?}");
    }
}

use super::{MetricsView, font_px_for_scale};

#[test]
fn metrics_view_for_scale_padded_honors_config_padding() {
    let v = MetricsView::for_scale_padded(super::FONT_PX, 2.0, 20.0, 5.0);
    assert_eq!((v.pad, v.pad_top), (40, 10));
    assert_eq!(v.font_px, font_px_for_scale(super::FONT_PX, 2.0));
    let v = MetricsView::for_scale_padded(super::FONT_PX, 1.0, 4.0, 9.0);
    assert_eq!((v.pad, v.pad_top), (4, 4));
    for &s in &[1.0f64, 1.25, 1.5, 2.0, 3.0] {
        assert_eq!(
            MetricsView::for_scale_padded(
                super::FONT_PX,
                s,
                super::PAD_LOGICAL_PX,
                super::PAD_TOP_LOGICAL_PX,
            ),
            MetricsView::for_scale(s),
            "scale {s}"
        );
    }
}

#[test]
fn two_windows_mixed_dpi_each_resolve_their_own_cell_size() {
    use super::{App, MetricsView, WindowId};
    let mut app = App::headless_for_test();
    let w0 = WindowId(0);
    if let Some(ws) = app.windows.get_mut(&w0) {
        ws.scale = 1.0;
        ws.metrics = MetricsView::for_scale(1.0);
    }
    let w1 = WindowId(1);
    app.install_window_state(w1, super::stub_session(1), 24, 80);
    if let Some(ws) = app.windows.get_mut(&w1) {
        ws.scale = 2.0;
        ws.metrics = MetricsView::for_scale(2.0);
    }

    assert_eq!(app.win_pad(w0), MetricsView::for_scale(1.0).pad);
    assert_eq!(app.win_pad(w1), MetricsView::for_scale(2.0).pad);
    assert_ne!(
        app.win_pad(w0),
        app.win_pad(w1),
        "1× and 2× pad must differ"
    );

    let (c0w, c0h) = app.win_cell_size(w0);
    let (c1w, c1h) = app.win_cell_size(w1);
    assert!(
        c1w > c0w && c1h > c0h,
        "2× cell box must exceed 1×: ({c0w},{c0h}) vs ({c1w},{c1h})"
    );

    app.apply_window_scale(w1);
    assert_eq!(
        app.win_cell_size(w1),
        app.cell_size(),
        "the ACTIVE window's per-window size equals the live backend size"
    );
    assert!(
        (app.font_px - MetricsView::for_scale(2.0).font_px).abs() < 0.5,
        "the active font follows the 2× window"
    );
    assert_eq!(
        app.win_cell_size(w0),
        (c0w, c0h),
        "background 1× window keeps its own cell box while 2× is active"
    );
    assert_eq!(app.win_pad(w0), MetricsView::for_scale(1.0).pad);

    app.apply_window_scale(w0);
    assert_eq!(
        app.win_cell_size(w0),
        app.cell_size(),
        "the 1× window is now the active, live backend size"
    );
    assert_eq!(
        app.win_cell_size(w1),
        (c1w, c1h),
        "background 2× window keeps its own cell box while 1× is active"
    );
}

#[test]
fn reload_refreshes_every_window_metric_to_the_new_font_size() {
    use super::{App, MetricsView, WindowId};
    let mut app = App::headless_for_test();
    let w0 = WindowId(0);
    if let Some(ws) = app.windows.get_mut(&w0) {
        ws.scale = 1.0;
        ws.metrics = MetricsView::for_scale(1.0);
    }
    let w1 = WindowId(1);
    app.install_window_state(w1, super::stub_session(1), 24, 80);
    if let Some(ws) = app.windows.get_mut(&w1) {
        ws.scale = 2.0;
        ws.metrics = MetricsView::for_scale(2.0);
    }

    app.font_size = 15.0;
    app.refresh_all_window_metrics();
    assert_eq!(app.windows[&w0].metrics.font_px, 15.0);
    assert_eq!(app.windows[&w1].metrics.font_px, 30.0);
    assert_eq!(
        app.windows[&w0].metrics.pad,
        MetricsView::for_scale(1.0).pad
    );
    assert_eq!(
        app.windows[&w1].metrics.pad,
        MetricsView::for_scale(2.0).pad
    );
    let (c0w, c0h) = app.win_cell_size(w0);
    let (c1w, c1h) = app.win_cell_size(w1);
    assert!(
        c1w > c0w && c1h > c0h,
        "a named size ⇒ the 2× window's cell box exceeds 1×"
    );

    app.font_size = super::FONT_PX;
    app.refresh_all_window_metrics();
    assert_eq!(app.windows[&w0].metrics, MetricsView::for_scale(1.0));
    assert_eq!(app.windows[&w1].metrics, MetricsView::for_scale(2.0));
}

#[test]
fn live_font_zoom_refreshes_per_window_metrics() {
    use super::{App, MetricsView, WindowId};
    let mut app = App::headless_for_test();
    let w0 = WindowId(0);
    let w1 = WindowId(1);
    app.install_window_state(w1, super::stub_session(1), 24, 80);
    if let Some(ws) = app.windows.get_mut(&w1) {
        ws.scale = 2.0;
        ws.metrics = MetricsView::for_scale(2.0);
    }
    let base = app.font_size;
    let (bw, bh) = app.win_cell_size(w0);

    app.set_font_size(base + 8.0);
    assert_eq!(app.font_size, base + 8.0);
    assert!(
        (app.windows[&w0].metrics.font_px - (base + 8.0)).abs() < 0.5,
        "the per-window MetricsView must track the zoomed size, not stay stale"
    );
    assert_eq!(
        app.windows[&w1].metrics.font_px,
        font_px_for_scale(base + 8.0, 2.0),
        "the 2× window draws the zoom at its own scale"
    );
    let (zw, zh) = app.win_cell_size(w0);
    assert!(
        zw > bw && zh > bh,
        "zooming in must GROW the window's cell box: ({bw},{bh}) -> ({zw},{zh})"
    );

    app.set_font_size(base - 2.0);
    assert!(
        (app.windows[&w0].metrics.font_px - (base - 2.0)).abs() < 0.5,
        "the per-window MetricsView follows a zoom-out too"
    );
    let (sw, sh) = app.win_cell_size(w0);
    assert!(
        sw < bw && sh < bh,
        "zooming out must SHRINK the cell box: ({bw},{bh}) -> ({sw},{sh})"
    );
}

#[test]
fn failed_live_font_zoom_rolls_back_font_authority_and_window_metrics() {
    use super::{App, WindowId};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let (old_size, old_px) = (app.font_size, app.font_px);
    let old_metrics = app.windows[&wid].metrics;
    let old_cell = app.win_cell_size(wid);

    assert!(!app.set_font_size_with(old_size + 8.0, |_| false));
    assert_eq!((app.font_size, app.font_px), (old_size, old_px));
    assert_eq!(app.windows[&wid].metrics, old_metrics);
    assert_eq!(app.win_cell_size(wid), old_cell);
}

#[test]
fn zoom_surviving_a_reload_never_paints_a_frame_wider_than_the_window() {
    use super::{App, MetricsView, WindowId};
    use aterm_window::dpi::PhysicalSize;
    let mut app = App::headless_for_test();
    let w0 = WindowId(0);
    let win = PhysicalSize::new(1808u32, 1452u32);
    if let Some(ws) = app.windows.get_mut(&w0) {
        ws.scale = 2.0;
        ws.metrics = MetricsView::for_scale(2.0);
        ws.win_px = Some(win);
    }
    app.font_px = MetricsView::for_scale(2.0).font_px;
    app.on_resize(w0, win);

    let frame_w = |app: &App| {
        let (cw, _, _) = app.backend.cell_geometry(app.font_px);
        let Some(ws) = app.windows.get(&w0) else {
            panic!("front window must exist")
        };
        ws.cols as usize * cw + 2 * ws.metrics.pad
    };
    assert!(frame_w(&app) <= win.width as usize, "baseline fits");

    app.set_font_size(app.font_size - 2.0 * super::FONT_ZOOM_STEP);
    app.on_resize(w0, win);
    let zoomed_px = app.font_px;
    let zoomed_cols = app.windows[&w0].cols;
    assert_eq!(zoomed_px, font_px_for_scale(app.font_size, 2.0));
    assert!(frame_w(&app) <= win.width as usize, "zoomed-out grid fits");

    app.refresh_all_window_metrics();
    app.apply_window_scale(w0);
    assert_eq!(app.font_px, zoomed_px, "the zoom survives the redraw");
    assert!(frame_w(&app) <= win.width as usize, "frame still fits");

    app.font_size = app.default_font_size;
    app.refresh_all_window_metrics();
    app.apply_window_scale(w0);
    assert!(
        (app.font_px - MetricsView::for_scale(2.0).font_px).abs() < 0.5,
        "the redraw re-tunes to the default at 2x"
    );
    assert!(
        app.windows[&w0].cols < zoomed_cols,
        "the re-tune re-grids: fewer cols at the bigger cell box"
    );
    assert!(
        frame_w(&app) <= win.width as usize,
        "INVARIANT: the composed frame never exceeds the window"
    );
}
