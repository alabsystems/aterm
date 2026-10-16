// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{
    App, DEMO_TICK_INTERVAL, NATIVE_CAPTURE_PRESENT_ATTEMPT_LIMIT, WindowId, find_url_span,
    is_safe_url, native_preview_deadline, native_preview_may_tick, run_capture_present_barrier,
    should_dispatch_boot_health_confirmation,
};
use crate::app_config::Config;

/// A headless launch runs the trial machinery but never presents, so it needs its own
/// health checkpoint: the engine loop reached with the socket bound (or none configured).
/// Without it the sentinel stays armed and a healthy build reverts.
#[test]
fn headless_confirms_boot_health_at_the_control_socket_boundary() {
    use std::sync::atomic::Ordering;

    // Exactly the call site in `about_to_wait`, with the retry due.
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

    // No socket configured (Disabled / NoDir / DirUnusable / PathTooLong): the run loop is
    // the only boundary that exists, and it has been reached.
    assert!(app.sock_plan.is_none());
    assert!(
        would_dispatch(&app),
        "a socketless headless build must still confirm"
    );

    // A configured-but-unbound socket is not yet a proof.
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

    // The one-shot latch still governs headless: one confirmation per run.
    app.boot_health_confirmation_dispatched = true;
    assert!(
        !would_dispatch(&app),
        "the same boundary cannot enqueue a second confirmation"
    );
}

/// The `video … trail` tail: a finalize from `service_video_wake` has no recorded
/// present behind it, so [`App::take_video_rec_for_finalize`] harvests the remaining
/// verdicts, logged and not counted as lost.
#[test]
fn a_finalize_logs_the_verdicts_after_the_last_recorded_present() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis;
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    let cfg = app.glow_config();
    let wid = WindowId(0);
    let geom = crate::cursor_glow::Geom {
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
    let root = std::env::temp_dir().join(format!(
        "aterm-video-trail-tail-{}-{}",
        std::process::id(),
        crate::metrics::now_us()
    ));
    let _ = std::fs::remove_dir_all(&root);
    crate::control_auth::ensure_private_dir(&root).expect("private recording root");
    let dir = crate::control_auth::confine_video_dir(&root).expect("confined recording dir");
    let (reply, _rx) = std::sync::mpsc::channel();
    let t0 = Instant::now();
    app.video_rec = Some(super::VideoRec {
        window: wid,
        deadline: t0 + Duration::from_secs(3),
        started_us: 0,
        keys: false,
        key_log: Vec::new(),
        trail: true,
        trail_log: Vec::new(),
        trail_seen: 0,
        trail_lost: 0,
        pace_ticks: Vec::new(),
        mode: super::VideoMode::OffscreenPresentReal,
        next_frame: None,
        presented: None,
        unseamed_at_begin: 0,
        unlogged_other_window: 0,
        dir,
        handoff: None,
        cancel: super::VideoCancellation::new(),
        reply,
    });
    let mut out = Vec::new();
    let mut walk = |app: &mut App, from: u16, at: Instant| {
        let glow = &mut app
            .windows
            .get_mut(&wid)
            .expect("window")
            .fx
            .cursor_mut()
            .glow;
        for i in 0..3u16 {
            glow.tick(
                Some((2, from + 4 * i)),
                at + ms(16 * u64::from(i)),
                &cfg,
                geom,
                &mut out,
            );
        }
    };
    // Verdicts, then a recorded present's harvest.
    walk(&mut app, 2, t0);
    app.harvest_video_trail(wid, 1_000_000);
    let at_present = app.video_rec.as_ref().expect("recording").trail_log.len();
    assert!(at_present > 0, "fixture: the present harvested verdicts");
    // More verdicts, and no present before the take ends.
    walk(&mut app, 40, t0 + ms(100));
    let newest = app.windows[&wid]
        .fx
        .cursor()
        .glow
        .admission_log()
        .last()
        .expect("a verdict")
        .seq;
    let rec = app.take_video_rec_for_finalize().expect("the recording");
    assert!(app.video_rec.is_none(), "finalize took the recording");
    assert!(
        rec.trail_log.len() > at_present,
        "the tail verdicts are logged ({} vs {at_present} at the last present)",
        rec.trail_log.len()
    );
    assert_eq!(
        rec.trail_seen, newest,
        "every verdict up to the newest was taken"
    );
    assert_eq!(rec.trail_lost, 0, "none of them is miscounted as lost");
    drop(rec);
    let _ = std::fs::remove_dir_all(root);
}

/// The `video … trail` ledger: verdicts land on the frame clock (`t_us = now_us - age`),
/// a harvest takes only what is new, and verdicts overwritten in the engine's 32-slot
/// ring or refused by the cap are counted.
#[test]
fn the_trail_ledger_stamps_verdicts_on_the_frame_clock_and_counts_the_lost() {
    use std::time::{Duration, Instant};
    let ms = Duration::from_millis;
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    let cfg = app.glow_config();
    assert!(cfg.enabled, "fixture: the glow engine judges moves");
    let geom = crate::cursor_glow::Geom {
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
    let mut glow = crate::cursor_glow::CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    glow.tick(Some((2, 2)), t0, &cfg, geom, &mut out);
    // Program output walks the caret, nobody typed: one verdict per move.
    for i in 1..=3u16 {
        glow.tick(
            Some((2, 2 + 4 * i)),
            t0 + ms(16 * u64::from(i)),
            &cfg,
            geom,
            &mut out,
        );
    }
    let verdicts = glow.admission_log().count();
    assert!(verdicts >= 3, "fixture: the moves were judged ({verdicts})");
    let newest = glow.admission_log().last().expect("a verdict").at;

    let now = t0 + ms(100);
    let now_us = 10_000_000u64;
    let mut log = Vec::new();
    let (seen, lost) = super::harvest_trail_verdicts(&glow, 0, now, now_us, &mut log, 4096);
    assert_eq!((log.len(), lost), (verdicts, 0));
    assert_eq!(seen, log.last().expect("logged").seq);
    let age = u64::try_from(now.duration_since(newest).as_micros()).unwrap();
    assert_eq!(
        log.last().unwrap().t_us,
        now_us - age,
        "stamped on the frame clock"
    );
    assert!(log.iter().all(|row| row.json().contains("\"phase\":\"")));

    // A second harvest takes nothing new.
    let (again, lost) = super::harvest_trail_verdicts(&glow, seen, now, now_us, &mut log, 4096);
    assert_eq!((again, lost, log.len()), (seen, 0, verdicts));

    // The ring overwrote verdicts nobody harvested: counted, not silent.
    for i in 0..40u16 {
        let col = if i % 2 == 0 { 30 } else { 60 };
        glow.tick(
            Some((5, col)),
            now + ms(16 * u64::from(i)),
            &cfg,
            geom,
            &mut out,
        );
    }
    let mut late = Vec::new();
    let (_, lost) = super::harvest_trail_verdicts(
        &glow,
        seen,
        now + ms(700),
        now_us + 700_000,
        &mut late,
        4096,
    );
    assert!(lost > 0, "a wrapped ring reports its losses");
    assert_eq!(late.len(), crate::cursor_glow::ADMISSION_LOG_CAP);

    // …and the cap refuses what it cannot hold, counting it.
    let mut capped = Vec::new();
    let (_, lost) = super::harvest_trail_verdicts(&glow, 0, now, now_us, &mut capped, 1);
    assert_eq!(capped.len(), 1);
    assert!(lost >= 1);
}

/// Boot health never folds a past deadline: a saturated dispatch moves the retry onto
/// the backoff, and a retry the guard would refuse is not folded. Pins
/// `note_boot_health_dispatch` and `boot_health_retry_deadline`.
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

    // A closed guard (an uncommitted handoff candidate proves no health).
    app.incoming_handoff_pending = true;
    assert_eq!(
        app.boot_health_retry_deadline(now),
        None,
        "a retry the guard would refuse must not be folded"
    );
    app.incoming_handoff_pending = false;

    // A saturated FIFO: the retry moves onto the backoff, which grows.
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

    // Queued closes the latch and resets the backoff.
    app.note_boot_health_dispatch(crate::app_native::NativeUpdateDispatch::Queued, now);
    assert!(app.boot_health_confirmation_dispatched);
    assert_eq!(app.boot_health_retry_deadline(now), None);
    app.finish_native_boot_health_confirmation(false, now);
    assert_eq!(
        app.boot_health_confirmation_retry_at,
        Some(now + Duration::from_secs(1))
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
    app.config.stream_fade = Some(true);
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
    // This synthetic four-cell hop is a generic authored landing, not a
    // one-glyph echo (which the real classifier correctly re-anchors).
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
    ws.fade_shown = true;
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
    ws.input_scratch
        .rain_quads
        .push(aterm_render::SpriteQuad::default());

    // These states are functional presentation, not fun effects. Serious mode
    // must not erase or demote any of them.
    ws.bell_flash.ring(now);
    let bell_deadline = ws.bell_flash.deadline();
    ws.next_blink = Some(blink_deadline);
    ws.scroll_frac_px = 7;
    ws.pill_shown = true;
    // The typed wake is typing evidence: the serious-mode drain runs every turn and the
    // status classifier reads `last_key_at`, so the drain must leave it alone.
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
    assert_eq!(app.config.stream_fade, Some(true));

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
    assert!(!ws.stream_fade.is_active(now));
    assert!(ws.next_trail_tick.is_none());
    assert!(ws.next_rain_tick.is_none());
    assert!(ws.next_native_preview_tick.is_none());
    assert!(!ws.fade_shown);
    assert!(ws.input_scratch.cursor_trail.is_empty());
    assert!(ws.input_scratch.cursor_glow_add.is_empty());
    assert!(ws.input_scratch.glow_halo.is_empty());
    assert!(ws.input_scratch.cursor_effect_style_override.is_none());
    assert!(ws.input_scratch.cursor_fill_override.is_none());
    assert!(ws.input_scratch.rain_quads.is_empty());
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

    // The typed wake survives the drain — and a second drain, which is what
    // every serious-mode turn runs — yet schedules nothing.
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

/// THE RESIDENT UNDER A LOAD SHED keeps its motion and leaves only the
/// frame train: a moving pet is armed at its owner's coarse offer
/// (`RESIDENT_SHED_CADENCE`) through the shed-free `pet_motion` gate, a
/// settled one owes nothing but its own breath, no sooner than that
/// cadence, and the latch clearing — on the envelope's recovery ramp, not
/// after it — hands the train back.
/// Negative control: the pet gate with the latch folded in (the old
/// `cursor_cat_motion` gate) arms nothing, and the moving pet freezes.
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

    // The latch engages mid-run.
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

    // Presented on its offers alone, the resident arrives and settles.
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

    // The latch clears; on the envelope's recovery ramp a caret move is
    // already back on the train.
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

    let model = aterm_spec::derive::effect_presentability_settle_model();
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

// -----------------------------------------------------------------------
// The effect lane's wake census.
// -----------------------------------------------------------------------

use std::time::{Duration, Instant};

/// One gesture of the confirmation capture's schedule.
#[derive(Clone, Copy, Debug)]
enum LaneGesture {
    Key,
    JumpTo(u16),
    Backspace,
    Enter,
}

/// The before capture's schedule, in ms from the first key: 35 keys at
/// 70 ms, a 1.5 s pause, 9 keys at 35 ms, a 1 s pause, four Ctrl-A /
/// Ctrl-E jumps 500 ms apart, four backspaces at 100 ms, Enter — and the
/// tail the lane is judged on. The seam's twin of this list lives in
/// `cursor_glow::tests::wake_census::census_schedule`.
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

/// The host-level terms that can make the lane's predicate true — the
/// owners an arm is charged to, in the planner's own order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum LaneOwner {
    /// The glow (v1 or v2) asks for the frame train.
    GlowTrain,
    /// A chrome decoration mid-motion.
    Deco,
    /// The M2 stream fade.
    Fade,
    /// A non-caret cursor dependent: trail, companion, pet, word décor,
    /// another cursor body.
    Dependents,
    /// The rainbow caret block's `is_active` — the fingerprint law: its
    /// ignition heat, a flare, or a fill that moved on the last frame.
    CaretCool,
    /// The rainbow caret's colour release after its frames stopped moving
    /// frame to frame (`CursorFx::bodies_settling`) — paced by its own
    /// next visible step on the coarse arm.
    CaretSettle,
    /// The glow offers a coarse next change (a tail, an edge, a poll).
    GlowCoarse,
    /// The glow is `is_active` by state alone: no train, no deadline.
    GlowStateOnly,
}

const LANE_OWNERS: [LaneOwner; 8] = [
    LaneOwner::GlowTrain,
    LaneOwner::Deco,
    LaneOwner::Fade,
    LaneOwner::Dependents,
    LaneOwner::CaretCool,
    LaneOwner::CaretSettle,
    LaneOwner::GlowCoarse,
    LaneOwner::GlowStateOnly,
];

/// The census of one scripted session through the real lane planner.
#[derive(Debug, Default)]
struct LaneCensus {
    /// Turns on which the planner armed a `CursorEffect` deadline.
    arms: u32,
    /// Frames the modelled loop rendered (one per wake).
    frames: u32,
    /// Frames whose fingerprint differed from the last (the present gate
    /// lets these through; the rest are wasted renders).
    presents: u32,
    /// Arms on which each owner was live (an arm can be charged to several).
    live: [u32; 8],
    /// Arms on which the owner was the only live term.
    sole: [u32; 8],
    /// Arms after the last gesture.
    tail_arms: u32,
    /// ms after the last gesture at which the planner first parked (and
    /// stayed parked); `None` if it never did inside the tail.
    park_ms: Option<u64>,
    /// The lane's effect-present interval.
    interval: Duration,
}

impl LaneCensus {
    fn arms_per_frame(&self) -> f64 {
        f64::from(self.arms) / f64::from(self.frames.max(1))
    }

    fn renders_per_present(&self) -> f64 {
        f64::from(self.frames) / f64::from(self.presents.max(1))
    }

    fn line(&self, tag: &str) -> String {
        let mut owners = String::new();
        for (k, owner) in LANE_OWNERS.iter().enumerate() {
            if self.live[k] > 0 {
                owners.push_str(&format!(" {owner:?}={}/{}", self.live[k], self.sole[k]));
            }
        }
        format!(
            "lane census {tag}: interval {:?} arms {} frames {} presents {} → {:.2} arms/frame, \
             {:.2} renders/present; tail arms {} park +{:?} ms; live/sole by owner:{owners}",
            self.interval,
            self.arms,
            self.frames,
            self.presents,
            self.arms_per_frame(),
            self.renders_per_present(),
            self.tail_arms,
            self.park_ms
        )
    }
}

/// Drive one window's real lane planner through [`lane_census_schedule`] as
/// `about_to_wait` does: wake at each gesture and armed tick, service the due tick, plan
/// (arm A), render the glow and caret block, plan again (arm B). `caret` ticks the
/// rainbow caret block beside the glow; `false` isolates the glow's lane.
fn lane_wake_census(caret: bool) -> LaneCensus {
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
    let interval = ws.effect_present_interval();
    let mut census = LaneCensus {
        interval,
        ..LaneCensus::default()
    };
    let script = lane_census_schedule();
    let t0 = Instant::now();
    let last_at = t0 + Duration::from_millis(script.last().map_or(0, |s| s.0));
    let end = last_at + Duration::from_secs(6);
    let mut cell = (12u16, 4u16);
    let mut next_ev = 0usize;
    let mut out = Vec::new();
    let mut last_fp = 0u64;
    let fold = |a: u64, b: u64| (a ^ b).wrapping_mul(0x9E37_79B9_7F4A_7C15).rotate_left(29);
    let mut now = t0 - Duration::from_millis(50);
    // Seed: one dark frame so both engines have a source cell.
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some(cell), now, &glow_cfg, geom, &mut out);
    let charge = |ws: &mut super::WindowState, now: Instant, census: &mut LaneCensus| {
        let armed = ws.plan_terminal_effect_lane(now, true, true).is_some();
        if !armed {
            return false;
        }
        census.arms += 1;
        let glow_train = ws.fx.cursor_mut().glow.needs_frame_cadence();
        let glow_coarse = !glow_train
            && ws
                .fx
                .cursor_mut()
                .glow
                .next_change_deadline(now, interval)
                .is_some();
        let caret_cool = ws.fx.cursor_mut().rainbow.is_active();
        let live = [
            glow_train,
            ws.deco_anim_frame_active(now),
            !ws.is_split() && (ws.fade_shown || ws.stream_fade.is_active(now)),
            ws.cursor_dependents_need_frame_cadence(now, true) && !caret_cool,
            caret_cool,
            ws.fx.cursor().bodies_settling(),
            glow_coarse,
            ws.fx.cursor_mut().glow.is_active() && !glow_train && !glow_coarse,
        ];
        let n = live.iter().filter(|l| **l).count();
        for (k, l) in live.iter().enumerate() {
            if *l {
                census.live[k] += 1;
                if n == 1 {
                    census.sole[k] += 1;
                }
            }
        }
        true
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
        // Arm A: the turn that requested this redraw.
        let a = charge(ws, now, &mut census);
        // The frame.
        let glow_fp = ws
            .fx
            .cursor_mut()
            .glow
            .tick(Some(cell), now, &glow_cfg, geom, &mut out);
        ws.fx.cursor_mut().glow.drain_sound_cues().for_each(drop);
        let caret_cfg = RainbowConfig {
            enabled: caret,
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
        let caret_frame = ws.fx.cursor_mut().rainbow.tick_with_family_phase(
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
        let fp = fold(glow_fp, caret_frame.fp);
        census.frames += 1;
        if fp != last_fp {
            census.presents += 1;
            last_fp = fp;
        }
        // Arm B: the turn after the redraw.
        let b = charge(ws, now, &mut census);
        if now >= last_at {
            let since = now.saturating_duration_since(last_at).as_millis() as u64;
            census.tail_arms += u32::from(a) + u32::from(b);
            // The park is the POST-redraw decision: arm A judged the state
            // before this frame; arm B is the level the loop sleeps on.
            if b {
                census.park_ms = None;
            } else if census.park_ms.is_none() {
                census.park_ms = Some(since);
            }
        }
    }
    census
}

/// The lane's wake census, printed with `--nocapture`: the confirmation schedule through
/// the real planner, with and without the caret block, arms by owner. The lane never
/// arms a frame train for a glow that offered no next change (every arm charged to the
/// glow is a cadence it asked for or a deadline it named), and the session renders at
/// most one frame per arm. `cursor_glow::tests::wake_census` keeps the engine-side law.
#[test]
fn the_effect_lane_never_arms_for_a_glow_with_no_next_change() {
    let full = lane_wake_census(true);
    let bare = lane_wake_census(false);
    for (tag, c) in [("v2 + caret", &full), ("v2 glow only", &bare)] {
        println!("{}", c.line(tag));
        assert!(c.arms > 0 && c.frames > 0, "{tag}: the session rendered");
        assert_eq!(
            c.sole[LANE_OWNERS
                .iter()
                .position(|o| *o == LaneOwner::GlowStateOnly)
                .expect("owner")],
            0,
            "{tag}: the lane armed a frame train for a glow that offered no next change"
        );
        assert!(
            c.frames <= c.arms,
            "{tag}: {} frames rendered on {} arms — a frame nobody armed",
            c.frames,
            c.arms
        );
    }
    // The caret block only adds arms; it never takes the glow's away.
    assert!(
        full.arms >= bare.arms,
        "the caret block took arms off the glow: {} < {}",
        full.arms,
        bare.arms
    );
}

/// After the last key the glow's own lane parks within two seconds (the engine idles
/// at ≤ 1.54 s). Measured on the glow alone: the rainbow caret's colour release
/// (`CaretCool`, `CaretSettle`) is charged by name in the census above.
#[test]
fn the_effect_lane_parks_within_two_seconds_of_the_last_key_under_v2() {
    let v2 = lane_wake_census(false);
    println!("{}", v2.line("v2 glow only"));
    let park = v2
        .park_ms
        .expect("the lane never parked after the last key");
    assert!(
        park <= 2_000,
        "the lane parked {park} ms after the last key"
    );
    // And it does not wake again once parked: the tail's arms all precede
    // the park (the census clears `park_ms` on any later arm).
    assert!(v2.tail_arms > 0, "the tail owed at least the swoosh");
}

/// The caret's colour release keeps the lane to its last step: with only the rainbow
/// caret cooling, identical frames release the frame train, but the lane must stay
/// armed until the caret releases, advance on every plan, and cost no more wakes than
/// the frame train. Drives the real planner as the event loop does. The engine's twin
/// test pins the per-step schedule.
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
    // The host spine the caret is fed: the typing-momentum metric on its
    // 2.0 s tau chased by the ribbon's 0.85 s display ease, and the
    // cadence's 220 ms ignition heat beside it.
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
    // A 120 Hz panel: the shipped effect lane.
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

    // Type, then rest, on the frame train until the fingerprint law lets
    // go: the body has settled, the fill has not.
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

    // Raw focus leaves while the typed wake is live. The same synthetic
    // focus pin that lets the hello paint must keep its bounded erase clock
    // running; pausing here leaves the already-painted still frozen forever.
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

    // The recording watcher is the other synthetic focus pin. Exercise the
    // scheduler fold directly so this test stays device- and filesystem-free.
    let ws = app.windows.get(&wid).expect("test window");
    assert!(
        ws.static_cursor_cat_deadline(now, false, true).is_some(),
        "a recorded static hello also owns its bounded erase wake"
    );

    // Synthetic focus is not sufficient to RESUME state paused by a
    // different presentation gate. Focus loss may preserve a live hello,
    // but ownership of re-admission stays with the renderer's full fold.
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
    app.config.stream_fade = Some(true);
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

    // A preference edited while the overlay is active is the value restored.
    // Keeping a stale snapshot here was the subtle destructive-toggle failure.
    assert!(app.set_serious_mode(true));
    app.config.cursor_trail = Some(false);
    app.config.stream_fade = Some(false);
    assert!(!app.set_serious_mode(false));
    assert!(!app.glow_config().enabled);
    assert!(!app.trail_config().enabled);
    assert!(app.session_rain_enabled(0));
    assert_eq!(app.config.cursor_trail, Some(false));
    assert_eq!(app.config.stream_fade, Some(false));
    assert_eq!(app.pool.rain_override(0), Some(true));
}

#[test]
fn serious_mode_bell_never_consumes_the_audible_rate_limit_lane() {
    let mut app = App::headless_for_test();
    assert!(app.set_serious_mode(true));

    // The headless fixture has no active native view to flash.  What this
    // path must establish is that a suppressed BEL never reaches (or even
    // consumes) the audible limiter; visual-bell preservation is not
    // driven here.
    app.on_bell(WindowId(0), 0);

    assert!(!app.set_serious_mode(false));
    assert!(
        app.bell_beep.try_fire(std::time::Instant::now()),
        "a serious-mode BEL must not consume the audible rate-limit lane"
    );
}

/// `bell_sound = false` silences the audible BEL without consuming the
/// [`BELL_BEEP_INTERVAL`] token (the `&&` order in `on_bell`), like the serious-mode
/// twin. Non-vacuous: it first asserts the key ships on, a fresh lane is open, and one
/// fire consumes it. The audible half is not driven (a headless beep would need AppKit);
/// `native_settings`' `sound_menu_keys_are_read_at_their_consumers` pins it
/// structurally.
#[test]
fn a_silenced_bell_never_consumes_the_audible_rate_limit_lane() {
    let mut app = App::headless_for_test();
    assert!(
        app.config.bell_sound_or_default(),
        "the audible bell ships ON; this switch is the opt-OUT",
    );

    // The lane is consumable: exactly the property the assertion below
    // depends on to mean anything.
    let now = std::time::Instant::now();
    let mut lane = crate::BellRateLimiter::new(crate::BELL_BEEP_INTERVAL);
    assert!(lane.try_fire(now), "a fresh lane is open");
    assert!(!lane.try_fire(now), "one fire consumes the lane's token");

    app.config.bell_sound = Some(false);
    app.on_bell(WindowId(0), 0);
    assert!(
        app.bell_beep.try_fire(std::time::Instant::now()),
        "a silenced BEL must not consume the audible rate-limit lane",
    );
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
    // Full config.
    let c: Config =
        aterm_toml::from_str("font_px = 24.0\ngpu = true\nscrollback_lines = 5000").unwrap();
    assert_eq!(c.font_px, Some(24.0));
    assert_eq!(c.gpu, Some(true));
    assert_eq!(c.scrollback_lines, Some(5000));
    // scrollback maps into the engine config: N -> cap, 0 -> unlimited (None).
    assert_eq!(c.applied_terminal_config().scrollback_limit, Some(5000));
    let unlimited: Config = aterm_toml::from_str("scrollback_lines = 0").unwrap();
    assert_eq!(unlimited.applied_terminal_config().scrollback_limit, None);
    // No engine-side keys -> the engine config an empty config has.
    assert_eq!(
        aterm_toml::from_str::<Config>("font_px = 18.0")
            .unwrap()
            .applied_terminal_config(),
        Config::default().applied_terminal_config()
    );
    // Cursor: shape + blink -> CursorStyle variant.
    use aterm_core::terminal::CursorStyle;
    let c: Config = aterm_toml::from_str("cursor_style = \"bar\"\ncursor_blink = false").unwrap();
    assert_eq!(
        c.applied_terminal_config().cursor_style,
        CursorStyle::SteadyBar
    );
    // The underline option is not supported (block and bar only): such a config degrades
    // to the bar with a one-line note. Programs may still request underline via DECSCUSR.
    let c: Config = aterm_toml::from_str("cursor_style = \"underline\"").unwrap();
    assert_eq!(
        c.applied_terminal_config().cursor_style,
        CursorStyle::BlinkingBar
    );
    // Bad shape falls back (doesn't crash).
    let c: Config = aterm_toml::from_str("cursor_style = \"weird\"").unwrap();
    assert_eq!(
        c.applied_terminal_config().cursor_style,
        CursorStyle::BlinkingBlock
    );
    // Partial + UNKNOWN keys/tables ignored (forward-compatible).
    let c: Config =
        aterm_toml::from_str("gpu = false\nfuture_key = \"x\"\n[unknown]\nk = 1").unwrap();
    assert_eq!(c.font_px, None);
    assert_eq!(c.gpu, Some(false));
    // Empty → all defaults (None).
    let c: Config = aterm_toml::from_str("").unwrap();
    assert!(c.font_px.is_none() && c.gpu.is_none());
    // Wrong type → parse error (load_config warns + falls back to defaults).
    assert!(aterm_toml::from_str::<Config>("font_px = \"big\"").is_err());
    // Initial size.
    let c: Config = aterm_toml::from_str("columns = 120\nlines = 40").unwrap();
    assert_eq!((c.columns, c.lines), (Some(120), Some(40)));
    // Search index depth: parses; `search_index_depth` maps None -> the engine
    // default (100 000) and Some(n) -> n lines retained by the search index.
    use crate::app_config::search_index_depth;
    assert_eq!(
        search_index_depth(&Config::default()),
        aterm_core::search::DEFAULT_MAX_CACHED_LINES
    ); // unset → engine default
    let c: Config = aterm_toml::from_str("search_history_lines = 50000").unwrap();
    assert_eq!(
        (c.search_history_lines, search_index_depth(&c)),
        (Some(50_000), 50_000)
    );
    let c: Config = aterm_toml::from_str("search_history_lines = 0").unwrap();
    assert_eq!(search_index_depth(&c), 0); // 0 → clamped to >=1 line by set_search_max_lines
    let c: Config = aterm_toml::from_str("search_history_lines = 5000000").unwrap();
    assert_eq!(search_index_depth(&c), 5_000_000); // deep history, above the engine default
    // Tab strip rows: parses as a u16; unset is None (the default and the clamp
    // are `resolve_tab_strip_rows`'s).
    let c: Config = aterm_toml::from_str("tab_strip_rows = 2").unwrap();
    assert_eq!(c.tab_strip_rows, Some(2));
    assert_eq!(Config::default().tab_strip_rows, None); // unset
    // Theme colours: hex parses; bad hex warns + is skipped (no crash).
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
    // Renderer theme from config: hex → Theme u32 (0x00RRGGBB).
    let c: Config = aterm_toml::from_str(
        "foreground = \"#102030\"\nbackground = \"#405060\"\nselection_color = \"#708090\"",
    )
    .unwrap();
    let th = c.theme();
    assert_eq!(
        (th.fg, th.bg, th.selection),
        (0x0010_2030, 0x0040_5060, 0x0070_8090)
    );
    // Unset → built-in defaults preserved.
    assert_eq!(
        Config::default().theme().selection,
        super::Theme::default().selection
    );
}

/// `option_as_meta` defaults to `true` (the current ESC-prefix behavior) when
/// the key is absent — so no config is byte-identical. An explicit value wins.
#[test]
fn option_as_meta_defaults_true() {
    // Absent → true (Meta behavior).
    assert!(Config::default().option_as_meta_or_default());
    assert!(
        aterm_toml::from_str::<Config>("font_px = 18.0")
            .unwrap()
            .option_as_meta_or_default()
    );
    // Explicit false opts into composed characters; explicit true is honored.
    assert!(
        !aterm_toml::from_str::<Config>("option_as_meta = false")
            .unwrap()
            .option_as_meta_or_default()
    );
    assert!(
        aterm_toml::from_str::<Config>("option_as_meta = true")
            .unwrap()
            .option_as_meta_or_default()
    );
}

#[test]
fn paste_confirm_flags_any_line_end_only_when_unbracketed() {
    // Bracketed paste (modern shell default) is never flagged: the shell waits
    // for the person's own Enter, trailing newline or not.
    assert!(!super::paste_needs_confirm("a\nb\nc", true));
    assert!(!super::paste_needs_confirm("make test\n", true));
    // A single line with no line end does not run on its own: NOT flagged.
    assert!(!super::paste_needs_confirm("ls -la", false));
    // Unbracketed, a trailing newline submits the line the moment it lands —
    // one planted look-alike command is the pastejack, so it IS flagged.
    assert!(super::paste_needs_confirm("make test\n", false));
    assert!(super::paste_needs_confirm("make test\r\n", false)); // CRLF trailing
    assert!(super::paste_needs_confirm("make test\r", false)); // lone CR trailing
    // An EMBEDDED newline with bracketed paste OFF IS flagged (pastejacking risk).
    assert!(super::paste_needs_confirm("ls\nrm -rf ~\n", false));
    assert!(super::paste_needs_confirm("a\rb", false)); // embedded CR
    // Default config: confirmation is ON.
    assert!(Config::default().confirm_multiline_paste_or_default());
    assert!(
        !aterm_toml::from_str::<Config>("confirm_multiline_paste = false")
            .unwrap()
            .confirm_multiline_paste_or_default()
    );
}

/// `copy_on_select` defaults per platform: `true` off Linux, `false` on Linux, where a
/// selection already owns PRIMARY and CLIPBOARD is for explicit copies. An explicit value
/// wins everywhere.
#[test]
fn copy_on_select_defaults_follow_the_platform_convention() {
    let default_on = cfg!(not(target_os = "linux"));
    assert_eq!(Config::default().copy_on_select_or_default(), default_on);
    assert_eq!(
        aterm_toml::from_str::<Config>("font_px = 18.0")
            .unwrap()
            .copy_on_select_or_default(),
        default_on
    );
    assert!(
        aterm_toml::from_str::<Config>("copy_on_select = true")
            .unwrap()
            .copy_on_select_or_default()
    );
    assert!(
        !aterm_toml::from_str::<Config>("copy_on_select = false")
            .unwrap()
            .copy_on_select_or_default()
    );
}

/// The `controls prefs` alias keeps its line prefixes while projecting the live native
/// Settings tree: route identity, visible rows and `ui` records agree with paint, and
/// compatibility-only config never appears as a visual control.
#[test]
fn controls_prefs_projects_current_native_top_settings_tree() {
    use crate::app_introspect::AuxTarget;
    let mut app = crate::App::headless_for_test();

    // A closed native tab has no visible route or controls. Preserve the
    // historical first line for callers that only test open/closed.
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

    let expected_field_keys = compiled
        .semantics
        .iter()
        .filter_map(|node| {
            node.key
                .as_str()
                .strip_prefix("settings/control/")
                .map(str::to_string)
        })
        .collect::<Vec<_>>();
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
    // No theme follows the system: Top Settings' first card leads with its
    // Dark theme slot, a canonical `ui` record (no `field`: it is no row).
    assert!(
        open.iter()
            .any(|line| line.starts_with("ui key=\"settings/theme-slot/dark\" "))
    );
    assert!(!expected_field_keys.contains(&crate::prefs::EDIT_THEME.to_string()));

    for removed in [
        "sparkle_words.orca.enabled",
        "matrix_rain.materialize",
        "matrix_rain.ink_text",
        "matrix_rain.phosphor",
    ] {
        assert!(
            !open.iter().any(|line| line.contains(removed)),
            "removed/inert control {removed} must not be claimed visible: {open:?}"
        );
    }
}

/// A `[keybindings]` table parses into the chord → action map, and an absent
/// table yields an empty (no-op) map — so the no-config path reaches the
/// hardcoded `on_key` matches unchanged.
#[test]
fn keybindings_table_parses() {
    // Absent → empty map (the hardcoded path is reached unchanged).
    let kb = crate::keybinding::Keybindings::from_config(Config::default().keybindings.as_ref());
    assert!(kb.is_empty());
    // A populated table resolves its chords to actions.
    let c: Config = aterm_toml::from_str(
        "[keybindings]\n\"cmd+shift+n\" = \"new_tab\"\n\"ctrl+a\" = \"find\"\n",
    )
    .unwrap();
    let kb = crate::keybinding::Keybindings::from_config(c.keybindings.as_ref());
    assert!(!kb.is_empty());
    use winit::keyboard::{Key as WK, ModifiersState as MS, SmolStr};
    assert_eq!(
        kb.lookup(&WK::Character(SmolStr::new("n")), MS::SUPER | MS::SHIFT),
        Some(crate::keybinding::Action::NewTab)
    );
    assert_eq!(
        kb.lookup(&WK::Character(SmolStr::new("a")), MS::CONTROL),
        Some(crate::keybinding::Action::Find)
    );
    // An unbound chord misses, so `on_key` falls through to the hardcoded match.
    assert_eq!(
        kb.lookup(&WK::Character(SmolStr::new("t")), MS::SUPER),
        None
    );
}

/// The real proof themes work: a config colour flows config → engine → the
/// rendered `RenderCell.fg/bg` (no renderer change involved) — the default
/// fg/bg, a `palette` slot (SGR 31 is ANSI index 1), and a `palette` entry
/// layered OVER a named theme's slot (last-wins: the theme seeds all 16
/// slots, then `palette[i]` overwrites slot `i`, S8).
#[test]
fn theme_colors_reach_rendercell() {
    use aterm_core::terminal::Terminal;
    let c: Config =
        aterm_toml::from_str("foreground = \"#FF8800\"\nbackground = \"#102030\"").unwrap();
    let mut t = Terminal::new(4, 10);
    t.apply_config(&c.applied_terminal_config());
    t.process(b"X"); // a default-styled glyph uses the configured theme colours
    let cells = t.render_row(0);
    assert_eq!(cells[0].ch, 'X');
    assert_eq!(cells[0].fg, [0xFF, 0x88, 0x00]);
    assert_eq!(cells[0].bg, [0x10, 0x20, 0x30]);

    // The palette: SGR fg = ANSI 1 (red) → palette index 1.
    let c: Config = aterm_toml::from_str("palette = [\"#000000\", \"#AB12CD\"]").unwrap();
    let mut t = Terminal::new(4, 10);
    t.apply_config(&c.applied_terminal_config());
    t.process(b"\x1b[31mR");
    let cells = t.render_row(0);
    assert_eq!(cells[0].ch, 'R');
    assert_eq!(cells[0].fg, [0xAB, 0x12, 0xCD]);

    // A palette entry over a named theme: Dracula's ansi[1] is #ff5555; slot 1
    // overridden to pure green wins.
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

/// A named `theme` flows config → engine → rendered cells: it sets the default
/// fg/bg and the full ANSI palette (incl. bold-to-bright), and the renderer Theme
/// chrome. A per-key override still wins over the theme (last-wins precedence).
#[test]
fn named_theme_reaches_rendercell_and_chrome() {
    use aterm_core::terminal::Terminal;
    // Dracula: fg #f8f8f2, bg #282a36; ansi[1]=#ff5555 (red), ansi[12]=#d6acff
    // (bright blue, reached by bold-blue 1;34).
    let c: Config = aterm_toml::from_str("theme = \"Dracula\"").unwrap();
    // Renderer chrome comes from the scheme.
    let th = c.theme();
    assert_eq!(th.fg, 0x00f8_f8f2, "theme fg reaches the renderer Theme");
    assert_eq!(th.bg, 0x0028_2a36, "theme bg reaches the renderer Theme");
    // Engine default + palette reach rendered cells.
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

    // A per-key override beats the theme.
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

/// An unknown `theme` name falls back to Default (never panics), and the
/// resolution says it fell back.
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

/// Config hot-reload keeps startup's font-size precedence (`--font-px > config >
/// default`), with the flag passed explicitly; the launch flag still wins after a reload.
#[test]
fn font_px_precedence_flag_beats_config_beats_default() {
    use super::{FONT_PX, FONT_PX_MAX, FONT_PX_MIN};
    use crate::app_config::{font_px_is_explicit_with, resolve_font_px_with};
    // The flag wins over config.
    assert_eq!(resolve_font_px_with(Some(24.0), Some(18.0)), 24.0);
    // No flag → config wins over the built-in default.
    assert_eq!(resolve_font_px_with(None, Some(18.0)), 18.0);
    // Neither → built-in default.
    assert_eq!(resolve_font_px_with(None, None), FONT_PX);
    // Out-of-range values (flag and config) are filtered, falling to default.
    assert_eq!(resolve_font_px_with(Some(9999.0), None), FONT_PX);
    assert_eq!(resolve_font_px_with(None, Some(0.0)), FONT_PX);
    // An out-of-range flag falls through to a valid config value, not the built-in
    // default.
    assert_eq!(resolve_font_px_with(Some(9999.0), Some(18.0)), 18.0);
    // Out-of-range flag and out-of-range config → default.
    assert_eq!(resolve_font_px_with(Some(9999.0), Some(9999.0)), FONT_PX);
    // In-range bounds are honoured.
    assert_eq!(resolve_font_px_with(Some(6.0), None), FONT_PX_MIN);
    assert_eq!(resolve_font_px_with(None, Some(200.0)), FONT_PX_MAX);

    // Explicitness uses the exact same admission law. Invalid authored
    // values are diagnostics, not a hidden request to disable HiDPI auto-size.
    assert!(!font_px_is_explicit_with(None, None));
    assert!(!font_px_is_explicit_with(None, Some(0.0)));
    assert!(!font_px_is_explicit_with(Some(f32::NAN), Some(f32::NAN)));
    assert!(font_px_is_explicit_with(Some(f32::NAN), Some(18.0)));
    assert!(font_px_is_explicit_with(Some(24.0), Some(9999.0)));
}

/// The reload engine path genuinely re-themes a live terminal: applying a new
/// `applied_terminal_config` recolours already-rendered default cells, and
/// reverting the key reverts the colour — exactly what `App::reload_config` does
/// to every session via `apply_config`. Crucially, the revert lands on the theme
/// background, never spec-black (the black-backed-text fix).
#[test]
fn reload_retheme_then_revert_live_terminal() {
    use aterm_core::terminal::Terminal;
    let mut t = Terminal::new(4, 10);
    t.process(b"X");
    // Apply a themed config (config → engine), then re-render: the default
    // glyph picks up the new background, proving the live re-apply works.
    let themed: Config = aterm_toml::from_str("background = \"#203040\"").unwrap();
    t.apply_config(&themed.applied_terminal_config());
    assert_eq!(t.render_row(0)[0].bg, [0x20, 0x30, 0x40]);
    // Reverting the key: `applied_terminal_config()` pins the engine default bg
    // to the theme bg — so a default-styled cell reverts to the themed window
    // background, not the engine's spec-default black.
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

/// The black-backed-text fix, end to end: with NO colour config, an unstyled
/// cell paints the theme default fg/bg (`#D0D0D0` on `#111318`) — the same colour
/// the window clears its margins/padding to — instead of the engine's spec
/// default (white-on-black) leaking through as a darker text region.
#[test]
fn default_config_cell_uses_theme_colors_not_spec_black() {
    use aterm_core::terminal::Terminal;
    let c = Config::default(); // no foreground/background set
    let mut t = Terminal::new(4, 10);
    t.apply_config(&c.applied_terminal_config());
    t.process(b"X");
    let cells = t.render_row(0);
    assert_eq!(cells[0].ch, 'X');
    assert_eq!(
        cells[0].bg,
        [0x11, 0x13, 0x18],
        "unstyled bg = theme bg, not black"
    );
    assert_eq!(
        cells[0].fg,
        [0xD0, 0xD0, 0xD0],
        "unstyled fg = theme fg, not pure white"
    );
    assert_ne!(cells[0].bg, [0, 0, 0]);
}

/// A theme change retints the App-level tab strip — BUT only once the strip cache
/// is invalidated, which `reload_config` does (`last_strip_fp = None` on a theme
/// change). The cache key is `(fingerprint, cols)`, not the theme, so this proves
/// (a) a stale cache serves the old theme until invalidated, and (b) after the
/// invalidation `reload_config` performs, the active tab repaints in the new theme.
#[test]
fn hot_reload_retints_tab_strip() {
    let mut app = super::App::headless_for_test();
    app.tab_strip_rows = 1;
    let wid = super::WindowId(0);
    let active_bg = |app: &super::App| {
        app.windows[&wid].cached_strip_rows[0][0].bg // active tab's first cell
    };

    // Theme A (Default): paint + cache the strip.
    app.theme = super::Theme::default();
    app.splice_tab_strip_with(wid, 1);
    let bg_a = active_bg(&app);
    assert!(
        app.windows[&wid]
            .tab_segments
            .iter()
            .any(|segment| segment.kind == super::tab_bar::TabHit::Select(0)),
        "a lone tab remains explicit title identity"
    );
    assert!(
        app.windows[&wid]
            .tab_segments
            .iter()
            .any(|segment| segment.kind == super::tab_bar::TabHit::NewTab),
        "single-tab title chrome retains the New Tab action"
    );

    // Switch to a light scheme without invalidating: the unchanged key stale-serves. Light
    // because on Linux the band's base tone follows the theme's appearance, so two dark
    // schemes share cell 0's tone.
    app.theme = super::Theme::from(&aterm_types::scheme::builtin("GitHub Light").unwrap());
    app.splice_tab_strip_with(wid, 1);
    assert_eq!(
        active_bg(&app),
        bg_a,
        "without invalidation the cache serves the old theme"
    );

    // Invalidate exactly as reload_config does on a theme change → strip retints.
    if let Some(ws) = app.windows.get_mut(&wid) {
        ws.last_strip_fp = None;
    }
    app.splice_tab_strip_with(wid, 1);
    assert_ne!(
        active_bg(&app),
        bg_a,
        "after invalidation the active tab repaints in the new theme"
    );
}

/// Cmd-F find core, end to end over the real shared path: `search_full_history`
/// finds every case-insensitive occurrence over a live terminal, `map_matches`
/// maps the engine's absolute rows to selection rows, and highlighting a match
/// via the selection (what `search_apply_current` does) round-trips to the
/// matched text — so the find genuinely works.
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

    // Case-insensitive by default: all three occurrences, in visual order.
    let results = search_full_history(&term, "hello", false, false).unwrap();
    let matches = map_matches(&results.results.matches, base_y);
    assert_eq!(matches, vec![(0, 0, 4), (0, 6, 10), (0, 12, 16)]);
    // A miss is an empty set (not an error).
    assert!(
        search_full_history(&term, "xyz", false, false)
            .unwrap()
            .results
            .matches
            .is_empty()
    );

    // The highlight round-trips: applying the first match as a selection selects it.
    let (r, c0, c1) = matches[0];
    let mut t = term.lock().unwrap();
    let sel = t.text_selection_mut();
    sel.start_selection(r, c0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r, u16::try_from(c1).unwrap(), SelectionSide::Right);
    assert_eq!(t.selection_to_string().as_deref(), Some("Hello"));
}

/// Wide-char (CJK) columns are cell columns, not char indices: a match after a
/// double-width prefix reports the cell column it occupies.
#[test]
fn search_wide_char_columns_are_display_cells() {
    use crate::app_search::map_matches;
    use crate::control::search_full_history;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    // "世界" = two double-width glyphs = 4 display columns; "hit" starts at cell 4,
    // not char index 2.
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

    // The mapped cell columns round-trip to the matched text on the real grid.
    let (r, c0, c1) = matches[0];
    let mut t = term.lock().unwrap();
    let sel = t.text_selection_mut();
    sel.start_selection(r, c0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(r, u16::try_from(c1).unwrap(), SelectionSide::Right);
    assert_eq!(t.selection_to_string().as_deref(), Some("hit"));
}

/// A soft-wrapped line is one line to find: a word spanning the wrap is one match, keyed
/// at its start, whose end column runs past the grid width by the part on the next row,
/// so selection and tint cover both halves.
#[test]
fn a_hit_that_straddles_a_soft_wrap_is_found_once_at_the_row_it_starts_on() {
    use crate::app_search::map_matches;
    use crate::control::search_full_history;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    // 10 columns: "wrapmatch" starts at column 8 and finishes at column 6 of
    // the wrapped row below it.
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

    // Both halves round-trip through the selection the find bar arms: the far
    // anchor is the end column DIVIDED by the width, so it lands on row 1.
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

/// R15 content-4: a VS16 base in the last column of a soft-wrapped row stays
/// one cell wide (the grid has no room to widen it), so the row is exactly
/// the grid's width. Find reports every hit on the run's later rows in the
/// cells the grid drew it in, on the screen and after the run scrolls into
/// history: each reported span selects exactly the needle. On origin/main
/// the joined run measured one column wider than its stride per such row, so
/// `Q` was reported one cell right, `Y` (the last column) at column 0 of the
/// next row, and `Z` two cells right after the second such row.
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

/// The join pads a continued row back out to the grid's width before it
/// appends the next one. Grid rows are read TRIMMED, so without that padding
/// a run whose row ends in blanks would splice its neighbour's first column
/// onto its last non-blank one — inventing a match that is not on the glass.
#[test]
fn a_soft_wrap_does_not_splice_a_match_across_the_blanks_at_a_row_end() {
    use crate::control::search_full_history;
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    // Column 9 of row 0 is blank; "gh" begins row 1. On the glass "fg" are
    // not adjacent, so neither is a match for them.
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

/// A history match maps to a negative selection row; scrolling the viewport `-row` lines
/// brings it into view with the selection round-tripping. This replicates the arithmetic
/// only; `emacs_navigation_conforms_to_derived_transition_model` and `app_render`'s
/// `cancel_*` tests pin `search_apply_current_in`.
#[test]
fn search_scrollback_scroll_to_match() {
    use crate::app_search::map_matches;
    use crate::control::search_full_history;
    use aterm_core::selection::{SelectionSide, SelectionType};
    use aterm_core::terminal::Terminal;
    use std::sync::{Arc, Mutex};

    let term = Arc::new(Mutex::new(Terminal::new(4, 20))); // 4 visible rows
    for i in 0..12 {
        term.lock()
            .unwrap()
            .process(format!("LINE{i:02}\r\n").as_bytes()); // ~8 lines scroll off
    }
    let base_y = {
        let mut t = term.lock().unwrap();
        t.scroll_to_bottom();
        i64::try_from(t.grid().base_y()).unwrap()
    };
    let results = search_full_history(&term, "line03", false, false).unwrap();
    let matches = map_matches(&results.results.matches, base_y);
    assert_eq!(matches.len(), 1);
    let (row, c0, c1) = matches[0];
    assert!(row < 0, "LINE03 must be in scrollback, got row {row}");
    // Apply (mirrors search_apply_current).
    let mut t = term.lock().unwrap();
    t.scroll_to_bottom();
    let sel = t.text_selection_mut();
    sel.start_selection(row, c0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(row, u16::try_from(c1).unwrap(), SelectionSide::Right);
    t.scroll_display(-row);
    assert_eq!(
        t.grid().display_offset() as i32,
        -row,
        "scrolled the match into view"
    );
    assert_eq!(t.selection_to_string().as_deref(), Some("LINE03"));
}

#[test]
fn url_detection_in_text() {
    let line = "see (http://example.com/p?q=1). end";
    // "http" starts at col 5; the URL spans into the query.
    assert_eq!(url_at(line, 5).as_deref(), Some("http://example.com/p?q=1"));
    assert_eq!(
        url_at(line, 12).as_deref(),
        Some("http://example.com/p?q=1")
    );
    // Trailing `).` is trimmed, so the close-paren col is not in the URL.
    let close_paren = line.find(')').unwrap();
    assert_eq!(url_at(line, close_paren), None);
    // Outside any URL.
    assert_eq!(url_at(line, 0), None);
    assert_eq!(url_at(line, 2), None);
    // https + bare URL, whole-span membership.
    let bare = "https://a.b/c";
    assert_eq!(url_at(bare, 0).as_deref(), Some("https://a.b/c"));
    assert_eq!(
        url_at(bare, bare.len() - 1).as_deref(),
        Some("https://a.b/c")
    );
    // Not a URL scheme.
    assert_eq!(url_at("ftp://x.y not-a-link", 0), None);
}

/// A detected URL is as long as what the row PAINTS, and says when any of
/// it is painted faintly. Concealed text (SGR 8) keeps its characters in
/// the grid but paints with its own background, so `google.com` followed
/// by a hidden `.evil.example/login` opens `google.com`, the hidden cells
/// open nothing, and a minimum-contrast floor does not reveal them. A tail
/// a shade off its background IS painted — even one step off it (1.002:1
/// on the default theme's `#111318`) — so it stays in the URL, and below
/// 3:1 it marks the URL faint, the run that earns the destination caption;
/// at 3.05:1 it does not. An ordinary URL is not faint, and with the floor
/// on, a 1.11:1 URL (Dracula's palette black on its own background) is
/// read the way the renderer lifts it: clickable, and legible enough to
/// need no caption.
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
        ("\x1b[38;2;18;19;24m", true),   // 1.002:1
        ("\x1b[38;2;40;43;50m", true),   // 1.31:1
        ("\x1b[38;2;88;91;100m", true),  // 2.74:1
        ("\x1b[38;2;95;98;108m", false), // 3.05:1
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
    // Allowed: http/https/mailto, case-insensitive.
    for ok in [
        "http://example.com",
        "https://example.com/path?q=1#frag",
        "HTTPS://EXAMPLE.COM",
        "mailto:user@example.com",
        "  https://example.com  ", // surrounding whitespace is trimmed
    ] {
        assert!(is_safe_url(ok), "should allow: {ok:?}");
    }
    // Rejected: filesystem, app/custom schemes, injection, empties.
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

// ---- W12: per-window metrics for mixed DPI -----------------------------------

use super::{
    FONT_PX_MAX, FONT_PX_MIN, MetricsView, font_px_for_scale, pad_for_scale, pad_top_for_scale,
};

/// The per-window derivation is a function of one scale: [`MetricsView::for_scale`]
/// is exactly `{font_px_for_scale, pad_for_scale, pad_top_for_scale}`, at the
/// concrete 1× / 1.5× / 2× / 3× DPIs a laptop + external-monitor pair spans.
#[test]
fn metrics_view_is_pure_function_of_own_scale() {
    for &s in &[1.0f64, 1.25, 1.5, 2.0, 3.0] {
        let v = MetricsView::for_scale(s);
        // Composition: the view is its component derivations.
        assert_eq!(v.font_px, font_px_for_scale(s));
        assert_eq!(v.pad, pad_for_scale(s));
        assert_eq!(v.pad_top, pad_top_for_scale(s));
        // The top pad is tighter than (or equal to) the all-edge pad.
        assert!(v.pad_top <= v.pad, "top pad never exceeds the base pad");
    }
    // Concrete values: PAD_LOGICAL_PX=12, PAD_TOP_LOGICAL_PX=2, round(·scale), font
    // clamped; FONT_PX is 12 on macOS, 16 on Windows, 15 on Linux/BSD. Literals are
    // gated per platform rather than derived, so the test does not restate the
    // multiplication under test.
    #[cfg(target_os = "macos")]
    let (f1x, f2x, f3x) = (12.0, 24.0, 36.0);
    #[cfg(windows)]
    let (f1x, f2x, f3x) = (16.0, 32.0, 48.0);
    #[cfg(not(any(target_os = "macos", windows)))]
    let (f1x, f2x, f3x) = (15.0, 30.0, 45.0);
    assert_eq!(
        MetricsView::for_scale(1.0),
        MetricsView {
            font_px: f1x,
            pad: 12,
            pad_top: 2,
            head: 0
        }
    );
    assert_eq!(
        MetricsView::for_scale(2.0),
        MetricsView {
            font_px: f2x,
            pad: 24,
            pad_top: 4,
            head: 0
        }
    );
    assert_eq!(
        MetricsView::for_scale(3.0),
        MetricsView {
            font_px: f3x,
            pad: 36,
            pad_top: 6,
            head: 0
        }
    );
    // font_px stays within the live bounds even at an extreme scale.
    let hi = font_px_for_scale(100.0);
    assert!(
        (FONT_PX_MIN..=FONT_PX_MAX).contains(&hi),
        "auto font must stay clamped"
    );
}

/// `for_scale_padded` derives device px from the supplied logical pads with the same
/// `round(·scale)` law, caps the top at the base, and equals `for_scale` at the built-in
/// constants.
#[test]
fn metrics_view_for_scale_padded_honors_config_padding() {
    let v = MetricsView::for_scale_padded(2.0, 20.0, 5.0);
    assert_eq!((v.pad, v.pad_top), (40, 10));
    assert_eq!(v.font_px, font_px_for_scale(2.0));
    // Top capped at the base pad even when mis-supplied above it.
    let v = MetricsView::for_scale_padded(1.0, 4.0, 9.0);
    assert_eq!((v.pad, v.pad_top), (4, 4));
    // Defaults reproduce the pure derivation exactly, at every test scale.
    for &s in &[1.0f64, 1.25, 1.5, 2.0, 3.0] {
        assert_eq!(
            MetricsView::for_scale_padded(s, super::PAD_LOGICAL_PX, super::PAD_TOP_LOGICAL_PX),
            MetricsView::for_scale(s),
            "scale {s}"
        );
    }
}

/// Two windows on different-scale displays are both pixel-correct through one shared
/// renderer: each resolves its own cell box independent of which size is active, and
/// `apply_window_scale` switches the active size to the drawing window while per-window
/// reads stay pinned.
#[test]
fn two_windows_mixed_dpi_each_resolve_their_own_cell_size() {
    use super::{App, MetricsView, WindowId};
    let mut app = App::headless_for_test(); // window 0
    app.font_px_explicit = false; // auto font ⇒ apply_window_scale is live
    // Window 0 = the 1× laptop.
    let w0 = WindowId(0);
    if let Some(ws) = app.windows.get_mut(&w0) {
        ws.scale = 1.0;
        ws.metrics = MetricsView::for_scale(1.0);
    }
    // Window 1 = the 2× external monitor.
    let w1 = WindowId(1);
    app.install_window_state(w1, super::stub_session(1), 24, 80);
    if let Some(ws) = app.windows.get_mut(&w1) {
        ws.scale = 2.0;
        ws.metrics = MetricsView::for_scale(2.0);
    }

    // Per-window pad is each window's own MetricsView pad (mixed-DPI, non-vacuous).
    assert_eq!(app.win_pad(w0), MetricsView::for_scale(1.0).pad);
    assert_eq!(app.win_pad(w1), MetricsView::for_scale(2.0).pad);
    assert_ne!(
        app.win_pad(w0),
        app.win_pad(w1),
        "1× and 2× pad must differ"
    );

    // Per-window cell box: the 2× window's is strictly larger (bigger font_px).
    let (c0w, c0h) = app.win_cell_size(w0);
    let (c1w, c1h) = app.win_cell_size(w1);
    assert!(
        c1w > c0w && c1h > c0h,
        "2× cell box must exceed 1×: ({c0w},{c0h}) vs ({c1w},{c1h})"
    );

    // Draw the 2× window: it activates its metrics on the shared renderer.
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
    // The CRUX: window 0's geometry is UNPERTURBED while the renderer is at 2×.
    assert_eq!(
        app.win_cell_size(w0),
        (c0w, c0h),
        "background 1× window keeps its own cell box while 2× is active"
    );
    assert_eq!(app.win_pad(w0), MetricsView::for_scale(1.0).pad);

    // Now draw the 1× window; the 2× reads stay pinned in turn.
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

/// A config reload that flips the font-size REGIME re-resolves every
/// window's per-window metric authority, so hit-test / IME / grid-sizing never
/// read a stale size after the backend is rebuilt. An EXPLICIT font pins the px
/// for every window (each keeping its own scale-derived pad); dropping back to
/// auto restores each window's own scale-derived px.
#[test]
fn reload_refreshes_every_window_metric_to_the_new_font_regime() {
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

    // Simulate a reload that added an explicit `font_px = 30` (the regime flip
    // `reload_config` performs before calling `refresh_all_window_metrics`).
    app.font_px_explicit = true;
    app.font_px = 30.0;
    app.refresh_all_window_metrics();
    // Both windows now render at the explicit px (each keeps its own pad).
    assert_eq!(app.windows[&w0].metrics.font_px, 30.0);
    assert_eq!(app.windows[&w1].metrics.font_px, 30.0);
    assert_eq!(
        app.windows[&w0].metrics.pad,
        MetricsView::for_scale(1.0).pad
    );
    assert_eq!(
        app.windows[&w1].metrics.pad,
        MetricsView::for_scale(2.0).pad
    );
    // ... and both windows agree with the pure geometry read at that px.
    assert_eq!(
        app.win_cell_size(w0),
        app.win_cell_size(w1),
        "explicit font ⇒ equal cell boxes"
    );

    // A subsequent reload that REMOVED the explicit font restores auto scaling:
    // each window returns to its own scale-derived px.
    app.font_px_explicit = false;
    app.refresh_all_window_metrics();
    assert_eq!(app.windows[&w0].metrics, MetricsView::for_scale(1.0));
    assert_eq!(app.windows[&w1].metrics, MetricsView::for_scale(2.0));
    let (c0w, c0h) = app.win_cell_size(w0);
    let (c1w, c1h) = app.win_cell_size(w1);
    assert!(
        c1w > c0w && c1h > c0h,
        "auto font ⇒ 2× window's cell box exceeds 1×"
    );
}

/// Live font zoom: `set_font_px` re-resolves and pins every window's [`MetricsView`], so
/// `on_resize` re-grids with the new cell box instead of drawing new glyphs into the old
/// layout.
#[test]
fn live_font_zoom_refreshes_per_window_metrics_and_pins_explicit() {
    use super::{App, WindowId};
    let mut app = App::headless_for_test(); // window 0, AUTO font at FONT_PX
    app.font_px_explicit = false;
    let w0 = WindowId(0);
    let base_px = app.font_px;
    let (bw, bh) = app.win_cell_size(w0);

    // Zoom IN. The per-window record must follow, the size must pin explicit, and the
    // cell box must actually GROW (the stale-metrics bug kept it flat).
    app.set_font_px(base_px + 8.0);
    assert!(
        app.font_px_explicit,
        "a live zoom pins the font size as explicit"
    );
    assert!(
        (app.windows[&w0].metrics.font_px - (base_px + 8.0)).abs() < 0.5,
        "the per-window MetricsView must track the zoomed px, not stay stale"
    );
    let (zw, zh) = app.win_cell_size(w0);
    assert!(
        zw > bw && zh > bh,
        "zooming in must GROW the window's cell box: ({bw},{bh}) -> ({zw},{zh})"
    );

    // Zoom back out, below the base: the per-window cell box shrinks in lockstep.
    app.set_font_px(base_px - 2.0);
    assert!(
        (app.windows[&w0].metrics.font_px - (base_px - 2.0)).abs() < 0.5,
        "the per-window MetricsView follows a zoom-out too"
    );
    let (sw, sh) = app.win_cell_size(w0);
    assert!(
        sw < bw && sh < bh,
        "zooming out must SHRINK the cell box: ({bw},{bh}) -> ({sw},{sh})"
    );
}

/// A renderer rebuild is fallible. The zoom transaction must not publish
/// new App/window geometry while the old renderer remains live, or hit
/// testing, PTY reflow and painted glyph cells disagree after the failure.
#[test]
fn failed_live_font_zoom_rolls_back_font_authority_and_window_metrics() {
    use super::{App, WindowId};
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.font_px_explicit = false;
    let old_px = app.font_px;
    let old_metrics = app.windows[&wid].metrics;
    let old_cell = app.win_cell_size(wid);

    assert!(!app.set_font_px_with(old_px + 8.0, |_| false));
    assert_eq!(app.font_px, old_px);
    assert!(!app.font_px_explicit);
    assert_eq!(app.windows[&wid].metrics, old_metrics);
    assert_eq!(app.win_cell_size(wid), old_cell);
}

/// A config reload without `font_px` keeps a zoomed px pinned (`reload_font_pin`), and
/// even with the pin dropped `apply_window_scale` re-grids the window it re-tunes, so the
/// frame never exceeds the glass.
#[test]
fn zoom_surviving_a_reload_never_paints_a_frame_wider_than_the_window() {
    use super::{App, MetricsView, WindowId};
    use winit::dpi::PhysicalSize;
    let mut app = App::headless_for_test();
    let w0 = WindowId(0);
    let win = PhysicalSize::new(1808u32, 1452u32);
    // A 2×-display window state: per-window scale + metrics + a recorded raw size.
    app.font_px_explicit = false;
    if let Some(ws) = app.windows.get_mut(&w0) {
        ws.scale = 2.0;
        ws.metrics = MetricsView::for_scale(2.0);
        ws.win_px = Some(win);
    }
    app.font_px = MetricsView::for_scale(2.0).font_px;
    app.default_font_px = app.font_px;
    app.on_resize(w0, win);

    // The frame the compose would build: grid cols × the active cell box + pad.
    let frame_w = |app: &App| {
        let (cw, _, _) = app.backend.cell_geometry(app.font_px);
        let Some(ws) = app.windows.get(&w0) else {
            panic!("front window must exist")
        };
        ws.cols as usize * cw + 2 * ws.metrics.pad
    };
    assert!(frame_w(&app) <= win.width as usize, "baseline fits");

    // Cmd-−: zoom out. (Headless: `rebuild_backend`'s re-grid loop skips
    // windows with no OS surface, so re-grid explicitly like the windowed path.)
    app.set_font_px(app.font_px - 2.0 * super::FONT_ZOOM_STEP);
    app.on_resize(w0, win);
    let zoomed_px = app.font_px;
    let zoomed_cols = app.windows[&w0].cols;
    assert!(frame_w(&app) <= win.width as usize, "zoomed-out grid fits");

    // A config reload that preserves the zoom (no font_px; scale default
    // unchanged): the pin law must keep the zoom pinned...
    let pin = crate::app_config::reload_font_pin(false, app.font_px, app.default_font_px);
    assert!(
        pin,
        "a surviving zoom keeps its explicit pin across a reload"
    );
    // ...so the reload's regime flip leaves metrics AT the zoomed px and the
    // redraw-time scale re-tune stays gated: nothing moves.
    app.font_px_explicit = pin;
    app.refresh_all_window_metrics();
    app.apply_window_scale(w0);
    assert_eq!(app.font_px, zoomed_px, "pinned zoom survives the redraw");
    assert!(frame_w(&app) <= win.width as usize, "frame still fits");

    // With the pin dropped and metrics at the scale default, the redraw-time re-tune must
    // re-grid, so the frame never exceeds the glass.
    app.font_px_explicit = false;
    app.refresh_all_window_metrics(); // metrics -> for_scale(2.0) ≠ zoomed px
    app.apply_window_scale(w0); // flips the active px back to the scale default
    assert!(
        (app.font_px - MetricsView::for_scale(2.0).font_px).abs() < 0.5,
        "unpinned redraw re-tunes to the scale default"
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

/// The W1 padding-absorption law holds PER WINDOW — each window
/// derives its own interior pad from its own scale ([`pad_for_scale`]) and the
/// shipping `aterm_render::pad_split` exact-cover / floor / near-even law holds for
/// that pad independently, with no cross-window coupling. This reuses the W1 lattice
/// (`aterm-render/tests/pad_absorption.rs`) parameterized by per-window scale.
#[test]
fn per_window_padding_absorption_holds_for_each_window_scale() {
    // Distinct-DPI windows; each contributes its own device-px pad.
    let scales = [1.0f64, 1.5, 2.0, 3.0];
    // A cell lattice spanning odd/even boxes (the absorption law is proven for any
    // (pad, cell); the per-window angle is that each window uses its own pad).
    let cells = [7usize, 8, 9, 16, 17];
    let mut pads_seen = std::collections::BTreeSet::new();
    let mut odd_remainder_seen = 0usize;
    for &scale in &scales {
        let pad = pad_for_scale(scale);
        pads_seen.insert(pad);
        for &cell in &cells {
            for w in (2 * pad + cell)..=(2 * pad + 13 * cell) {
                let s = aterm_render::pad_split(w, pad, cell);
                // Exact cover: this window's surface is its raw pixels — no scaling.
                assert_eq!(
                    s.pad_lo + s.cells * cell + s.pad_hi,
                    w,
                    "per-window exact cover (scale={scale} pad={pad} cell={cell} w={w})"
                );
                // The configured (per-window) border floor never shrinks.
                assert!(
                    s.pad_lo >= pad && s.pad_hi >= pad,
                    "per-window pad floor (scale={scale} pad={pad} cell={cell} w={w})"
                );
                // Near-even split, leaning trailing.
                assert!(
                    s.pad_lo <= s.pad_hi && s.pad_hi - s.pad_lo <= 1,
                    "per-window near-even split (scale={scale} pad={pad} cell={cell} w={w})"
                );
                if s.pad_lo != s.pad_hi {
                    odd_remainder_seen += 1;
                }
            }
        }
    }
    // NON-VACUITY: distinct per-window pads were genuinely exercised (the mixed-DPI
    // premise), and the odd-remainder branch (the one a lopsided split gets wrong) was
    // reached under those per-window pads.
    assert!(
        pads_seen.len() >= 3,
        "windows must span distinct DPIs ({pads_seen:?})"
    );
    assert!(
        odd_remainder_seen > 0,
        "the per-window lattice must reach an odd remainder"
    );
}
