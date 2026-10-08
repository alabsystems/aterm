// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{ComposedCursorFxClock, *};
use std::time::{Duration, Instant};

fn mixed_fixture(style: &str) -> (App, WindowId, u64) {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some(style.into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    let (session, _) =
        app.split_active_with_stub_terminal(wid, crate::tab_model::SplitAxis::Horizontal);
    assert!(
        app.active_visible_leaf_plan(wid)
            .is_some_and(|plan| plan.leaves.len() == 2),
        "fixture is a canonical terminal/native composition"
    );
    (app, wid, session)
}

fn enable_output_streak(app: &mut App) {
    app.config.output_streak = Some(crate::app_config::OutputStreakConfig {
        enabled: Some(true),
        sound: Some(false),
        ..Default::default()
    });
}

#[test]
fn output_streak_inert_gate_covers_every_zero_work_policy() {
    assert!(!output_streak_inert(true, true, 1.0, false));
    assert!(output_streak_inert(false, true, 1.0, false));
    assert!(output_streak_inert(true, false, 1.0, false));
    assert!(output_streak_inert(true, true, 0.0, false));
    assert!(output_streak_inert(true, true, 1.0, true));
}

#[test]
fn composed_output_streak_inert_policy_builds_no_pane_engines() {
    let t0 = Instant::now();

    let (mut mixed, mixed_wid, mixed_session) = mixed_fixture("fire");
    enable_output_streak(&mut mixed);
    mixed
        .config
        .output_streak
        .as_mut()
        .expect("configured streak")
        .intensity = Some(0.0);
    let mixed_term = mixed
        .pool
        .get(mixed_session)
        .expect("terminal leaf")
        .term
        .clone();
    term_lock(&mixed_term).process(b"mixed output");
    assert!(
        mixed
            .prepare_heterogeneous_input_scratch_with_cursor_fx(
                mixed_wid,
                Some(ComposedCursorFxClock::Advance(t0)),
            )
            .is_some()
    );
    assert!(mixed.windows[&mixed_wid].fx.streak_panes().next().is_none());

    let (mut pure, pure_wid, pure_term) = pure_fixture("fire");
    enable_output_streak(&mut pure);
    pure.config.motion = Some("reduced".into());
    term_lock(&pure_term).process(b"pure output");
    assert!(redraw_pure(&mut pure, pure_wid, t0));
    assert!(pure.windows[&pure_wid].fx.streak_panes().next().is_none());
}

/// The streak takes the cursor trail's focus: `motion.animate(OutputStreak)`
/// resolves on `cursor_fx_focus`, so an unfocused window spawns no streak
/// unless it is typed into (the typed wake) or recorded (the recording pin),
/// and does spawn during either. Each arm is a fresh split driven by real
/// output.
#[test]
fn an_unfocused_window_streaks_only_while_typed_into_or_recorded() {
    #[derive(Clone, Copy, Debug)]
    enum Arm {
        Nothing,
        TypedWake,
        Recorded,
    }
    let streaked = |arm: Arm| -> bool {
        // The recording arm's video root; bound before the App, so it
        // outlives the recording and is removed after it.
        let video_root = aterm_tempfile::Builder::new()
            .prefix("aterm-streak-focus-video-")
            .tempdir()
            .unwrap();
        let (mut app, wid, term) = pure_fixture("fire");
        enable_output_streak(&mut app);
        let t0 = Instant::now();
        app.windows.get_mut(&wid).expect("test window").focused = false;
        match arm {
            Arm::Nothing => {}
            Arm::TypedWake => {
                app.windows.get_mut(&wid).expect("test window").last_key_at = Some(t0);
            }
            Arm::Recorded => {
                let root = video_root.path();
                crate::control_auth::ensure_private_dir(root).unwrap();
                let dir = crate::control_auth::confine_video_dir(root).unwrap();
                let (reply, _rx) = std::sync::mpsc::channel();
                app.video_rec = Some(crate::VideoRec {
                    window: wid,
                    deadline: t0,
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
            }
        }
        term_lock(&term).process(b"first");
        assert!(redraw_pure(&mut app, wid, t0));
        let mut any = !app.windows[&wid].input_scratch.nova_add.is_empty();
        for step in 1..6u64 {
            term_lock(&term).process(b" more output");
            let at = t0 + Duration::from_millis(120 * step);
            assert!(redraw_pure(&mut app, wid, at));
            any |= !app.windows[&wid].input_scratch.nova_add.is_empty();
        }
        any
    };
    assert!(
        !streaked(Arm::Nothing),
        "an unfocused, untyped, unrecorded window spawns no streak"
    );
    assert!(
        streaked(Arm::TypedWake),
        "the typed wake lights the streak (the non-vacuous control)"
    );
    assert!(streaked(Arm::Recorded), "the recording pin lights it too");
}

#[test]
fn output_streak_fingerprint_tracks_placement_while_preserving_idle_zero() {
    let pane = ComposedOutputStreakPane {
        session: 7,
        slot: 0,
        place: PanePlace {
            row_off: 2,
            col_off: 3,
            rows: 10,
            cols: 20,
            win_rows: 24,
            win_cols: 80,
            cell_w: 8,
            cell_h: 16,
        },
        may_speak: false,
        echo: crate::app_input::OutputEchoSample::default(),
    };
    let moved = ComposedOutputStreakPane {
        place: PanePlace {
            col_off: 4,
            cols: 19,
            ..pane.place
        },
        ..pane
    };

    assert_eq!(placed_output_streak_fp(0, pane, 0), 0);
    assert_eq!(placed_output_streak_fp(0, moved, 0), 0);
    assert_ne!(
        placed_output_streak_fp(0x1234_5678, pane, 0),
        placed_output_streak_fp(0x1234_5678, moved, 0),
        "divider motion must repaint an unchanged pane-local streak frame"
    );
}

#[test]
fn mixed_output_streak_uses_content_clock_not_damage_epoch() {
    let (mut app, wid, session) = mixed_fixture("fire");
    enable_output_streak(&mut app);
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    term_lock(&term).process(b"ink");
    let t0 = Instant::now();
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(t0)),
        )
        .is_some()
    );
    assert!(app.windows[&wid].input_scratch.nova_add.is_empty());

    // DECSCNM changes the rendered frame and damage epoch, but it writes no
    // terminal content. The exact content clock must keep this a repaint.
    term_lock(&term).process(b"\x1b[?5h");
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(
                t0 + Duration::from_millis(400),
            )),
        )
        .is_some()
    );
    assert!(
        app.windows[&wid].input_scratch.nova_add.is_empty(),
        "damage-only reverse video cannot license a streak"
    );

    term_lock(&term).process(b"!");
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(
                t0 + Duration::from_millis(800),
            )),
        )
        .is_some()
    );
    assert!(
        !app.windows[&wid].input_scratch.nova_add.is_empty(),
        "a real content edge remains the non-vacuous control"
    );
}

#[test]
fn mixed_output_streak_retain_does_not_consume_the_next_advance_or_its_cue() {
    use aterm_effects::trail_sound::{OutputGesture, SoundGesture};

    let (mut app, wid, session) = mixed_fixture("fire");
    enable_output_streak(&mut app);
    app.config
        .output_streak
        .as_mut()
        .expect("configured streak")
        .sound = Some(true);
    app.config.trail_sounds = Some(true);
    app.trail_audio = crate::trail_audio::TrailAudio::capturing_for_test();
    app.windows.get_mut(&wid).expect("window").focused = true;
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    term_lock(&term).process(b"base");
    let t0 = Instant::now();
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(t0)),
        )
        .is_some()
    );
    assert!(app.trail_audio.take_captured_for_test().is_empty());

    term_lock(&term).process(b" output");
    let plan = app.active_visible_leaf_plan(wid).expect("capture layout");
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan_outcome(
            wid,
            Some(ComposedCursorFxClock::Retain {
                observed_at: t0 + Duration::from_millis(400),
            }),
            &plan,
        )
        .into_option()
        .is_some()
    );
    assert!(
        app.windows[&wid].input_scratch.nova_add.is_empty(),
        "Retain may only replay a successfully presented streak stream"
    );
    assert!(
        app.trail_audio.take_captured_for_test().is_empty(),
        "Retain cannot speak or consume a cue"
    );

    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(
                t0 + Duration::from_millis(401),
            )),
        )
        .is_some()
    );
    assert!(
        !app.windows[&wid].input_scratch.nova_add.is_empty(),
        "the content edge remains pending for the next Advance"
    );
    assert!(
        app.trail_audio
            .take_captured_for_test()
            .iter()
            .any(|event| matches!(event.kind, SoundGesture::Output(OutputGesture::Shimmer))),
        "the next Advance still owns the episode-opening cue"
    );
}

#[test]
fn mixed_output_streak_live_and_capture_emit_identical_pixels() {
    let (mut live, live_wid, live_session) = mixed_fixture("fire");
    let (mut capture, capture_wid, capture_session) = mixed_fixture("fire");
    enable_output_streak(&mut live);
    enable_output_streak(&mut capture);
    let live_term = live
        .pool
        .get(live_session)
        .expect("live terminal leaf")
        .term
        .clone();
    let capture_term = capture
        .pool
        .get(capture_session)
        .expect("capture terminal leaf")
        .term
        .clone();
    term_lock(&live_term).process(b"base");
    term_lock(&capture_term).process(b"base");
    let t0 = Instant::now();
    assert!(
        live.prepare_heterogeneous_input_scratch_with_cursor_fx(
            live_wid,
            Some(ComposedCursorFxClock::Advance(t0)),
        )
        .is_some()
    );
    let capture_plan = capture
        .active_visible_leaf_plan(capture_wid)
        .expect("capture layout");
    assert!(
        capture
            .prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan_outcome(
                capture_wid,
                Some(ComposedCursorFxClock::Advance(t0)),
                &capture_plan,
            )
            .into_option()
            .is_some()
    );

    term_lock(&live_term).process(b" output");
    term_lock(&capture_term).process(b" output");
    let tick = t0 + Duration::from_millis(400);
    assert!(
        live.prepare_heterogeneous_input_scratch_with_cursor_fx(
            live_wid,
            Some(ComposedCursorFxClock::Advance(tick)),
        )
        .is_some()
    );
    assert!(
        capture
            .prepare_heterogeneous_input_scratch_with_cursor_fx_from_plan_outcome(
                capture_wid,
                Some(ComposedCursorFxClock::Advance(tick)),
                &capture_plan,
            )
            .into_option()
            .is_some()
    );
    let live_nova = &live.windows[&live_wid].input_scratch.nova_add;
    let capture_nova = &capture.windows[&capture_wid].input_scratch.nova_add;
    assert!(!live_nova.is_empty(), "parity control owns visible light");
    assert_eq!(live_nova, capture_nova);
}

#[test]
fn composed_cursor_body_shape_reaches_pure_mixed_and_retained_frames() {
    let t0 = Instant::now();

    let (mut pure, pure_wid, _) = pure_fixture("laser");
    assert!(redraw_pure(&mut pure, pure_wid, t0));
    {
        let input = &pure.windows[&pure_wid].input_scratch;
        assert_eq!(
            input.cursor_effect_style_override,
            Some(CursorStyle::Bolt),
            "the pure compositor must carry the laser body's real shape"
        );
        assert!(
            input.cursor_fill_override.is_some(),
            "the laser shape and its live fill must arrive together"
        );
    }

    let (mut mixed, mixed_wid, _) = mixed_fixture("laser");
    assert!(
        mixed
            .prepare_heterogeneous_input_scratch_with_cursor_fx(
                mixed_wid,
                Some(ComposedCursorFxClock::Advance(t0)),
            )
            .is_some()
    );
    let advanced = {
        let input = &mixed.windows[&mixed_wid].input_scratch;
        assert_eq!(
            input.cursor_effect_style_override,
            Some(CursorStyle::Bolt),
            "the heterogeneous compositor must not discard the laser shape"
        );
        (
            input.cursor_style,
            input.cursor_effect_style_override,
            input.cursor_fill_override,
        )
    };
    assert!(
        mixed
            .prepare_heterogeneous_input_scratch_with_cursor_fx(
                mixed_wid,
                Some(ComposedCursorFxClock::Retain {
                    observed_at: t0 + Duration::from_millis(17),
                }),
            )
            .is_some()
    );
    let retained = &mixed.windows[&mixed_wid].input_scratch;
    assert_eq!(
        (
            retained.cursor_style,
            retained.cursor_effect_style_override,
            retained.cursor_fill_override,
        ),
        advanced,
        "Retain projects the prior complete body without a second effect tick"
    );

    let (mut rainbow, rainbow_wid, _) = pure_fixture("rainbow kitty");
    for index in 0..8 {
        rainbow
            .windows
            .get_mut(&rainbow_wid)
            .expect("rainbow window")
            .fx
            .cursor_mut()
            .cadence
            .on_keystroke(t0 + Duration::from_millis(index));
    }
    assert!(redraw_pure(
        &mut rainbow,
        rainbow_wid,
        t0 + Duration::from_millis(16),
    ));
    let input = &rainbow.windows[&rainbow_wid].input_scratch;
    assert_eq!(
        input.cursor_effect_style_override,
        Some(CursorStyle::SteadyBlock),
        "rainbow blink-twinkle pins the composed cursor body steady"
    );
    assert!(
        input.cursor_fill_override.is_some(),
        "rainbow body style cannot arrive without its streaming fill"
    );
}

#[test]
fn clean_composed_capture_strips_effect_shape_and_restores_decscusr() {
    let (mut app, wid, _) = pure_fixture("laser");
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Advance(Instant::now()),
        )
        .is_some()
    );
    let styled = &app.windows[&wid].input_scratch;
    let terminal_style = styled.cursor_style;
    assert_eq!(
        styled.cursor_effect_style_override,
        Some(CursorStyle::Bolt),
        "negative control: the styled capture contains the laser body"
    );
    assert!(styled.cursor_fill_override.is_some());

    let mut clean = styled.clone();
    clean.clear_overlays();
    assert_eq!(clean.cursor_style, terminal_style);
    assert!(clean.cursor_effect_style_override.is_none());
    assert!(clean.cursor_fill_override.is_none());
    assert!(
        clean.cursor_glow_add.is_empty()
            && clean.glow_halo.is_empty()
            && clean.fire_patch.is_empty()
            && clean.glow_under.is_empty()
            && clean.cursor_trail.is_empty()
    );
    assert_eq!(
        aterm_render::effective_cursor_style(&clean, None),
        terminal_style,
        "plain capture resolves the terminal's DECSCUSR, not a naked effect shape"
    );
}

fn arm_synthetic_typed(app: &mut App, wid: WindowId, now: Instant) {
    let window = app.windows.get_mut(&wid).expect("test window");
    window.fx.cursor_mut().glow.note_synthetic_typed(now, 1);
    window.fx.cursor_mut().trail.note_synthetic_typed(now);
}

/// Type one printable character through the same seam the input host
/// uses: the press stamps the typed LICENSE, then the byte reaches the
/// terminal. The composed tick that follows observes the caret advance
/// and — because a human touched the keyboard inside the freshness
/// window — mints its cat pulse.
fn type_one_licensed_echo(
    app: &mut App,
    wid: WindowId,
    term: &std::sync::Arc<std::sync::Mutex<Terminal>>,
    at: Instant,
) {
    let leaf = app
        .active_visible_leaf_plan(wid)
        .expect("composed plan")
        .leaves
        .into_iter()
        .find(|leaf| leaf.focused)
        .expect("focused terminal leaf");
    let row_offset = leaf.rect.origin.y.round().max(0.0) as u16;
    let col_offset = leaf.rect.origin.x.round().max(0.0) as u16;
    let origin = {
        let terminal = term_lock(term);
        let cursor = terminal.cursor();
        assert!(
            cursor.col + 1 < terminal.cols(),
            "fixture needs one free cell"
        );
        (
            cursor.row.saturating_add(row_offset),
            cursor.col.saturating_add(col_offset),
        )
    };
    {
        let window = app.windows.get_mut(&wid).expect("test window");
        assert_eq!(window.fx.cursor_mut().glow.cursor_anchor(), Some(origin));
        assert_eq!(window.fx.cursor_mut().trail.cursor_anchor(), Some(origin));
        window.fx.cursor_mut().glow.note_typed_cells(at, 1);
        window.fx.cursor_mut().trail.note_typed(at);
    }
    term_lock(term).process(b"x");
}

fn bind_extracted_cat_pulse_route(
    label: &str,
    classified: bool,
    pending_after_render: bool,
    delivered: bool,
    replay_changed_cat: bool,
) {
    let model = aterm_spec::derive::cursor_cat_motion_pulse_routing_model();
    // PROVENANCE, classifier-fed: the pulse exists because a licensed spawn
    // classified this move as a typed advance/fold. `licensed` is read off
    // the engine's own admission ring, so a route bound here is bound to a
    // pulse the licence seam let through.
    let licensed_state = model.successors("LicenseMove", &model.init_state())[0].clone();
    let classified_state = model.successors("ClassifyPulse", &licensed_state)[0].clone();
    assert_eq!(
        classified_state[&"classified"],
        i64::from(classified),
        "{label}: a licensed, classified move must mint the pulse"
    );
    assert_eq!(classified_state[&"cold_pulse"], 0);
    let selected = model.successors("SelectComposedExtractedRoute", &classified_state)[0].clone();
    let mut observed = model.successors("RenderComposedExtractedRoute", &selected)[0].clone();
    observed.insert("pending", i64::from(pending_after_render));
    observed.insert("consumes", i64::from(classified && !pending_after_render));
    observed.insert("deliveries", i64::from(delivered));
    let transition_label = format!("{label} composed pulse consume/delivery");
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &selected,
        &observed,
        Some("RenderComposedExtractedRoute"),
        &transition_label,
    );
    assert!(ok, "{label}: runtime pulse route rejected: {why}");

    // A route that ran but left the pulse pending and delivered nothing must
    // not satisfy the healthy transition, even with all other bookkeeping
    // agreeing.
    let mut forged_strand = observed.clone();
    forged_strand.insert("pending", 1);
    forged_strand.insert("consumes", 0);
    forged_strand.insert("deliveries", 0);
    forged_strand.insert("stranded", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &selected,
        &forged_strand,
        Some("RenderComposedExtractedRoute"),
        "forged composed cursor-cat pulse strand",
    );
    assert!(!ok, "{label}: a composed pulse strand must be rejected");

    let switched = model.successors("SwitchToOrdinaryRoute", &observed)[0].clone();
    assert!(
        !replay_changed_cat,
        "{label}: the ordinary forwarding seam replayed a consumed pulse"
    );
    let mut forged_replay = switched.clone();
    forged_replay.insert("consumes", 2);
    forged_replay.insert("deliveries", 2);
    forged_replay.insert("stale_replay", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &switched,
        &forged_replay,
        Some("RenderOrdinaryRoute"),
        "forged cursor-cat pulse replay after route switch",
    );
    assert!(
        !ok,
        "{label}: a consumed pulse cannot replay after switching routes"
    );
}

fn bind_ordinary_cat_pulse_route(
    classified: bool,
    pending_after_render: bool,
    delivered: bool,
    replay_changed_cat: bool,
) {
    let model = aterm_spec::derive::cursor_cat_motion_pulse_routing_model();
    // Same provenance chain as the extracted route: licence, then classify.
    let licensed_state = model.successors("LicenseMove", &model.init_state())[0].clone();
    let classified_state = model.successors("ClassifyPulse", &licensed_state)[0].clone();
    assert_eq!(
        classified_state[&"classified"],
        i64::from(classified),
        "the ordinary licensed, classified move must mint the pulse"
    );
    assert_eq!(classified_state[&"cold_pulse"], 0);
    let selected = model.successors("SelectOrdinaryRoute", &classified_state)[0].clone();
    assert!(
        !model
            .successors("RenderOrdinaryRoute", &selected)
            .is_empty(),
        "the healthy ordinary consume action is enabled"
    );
    let mut observed = model.successors("RenderOrdinaryRoute", &selected)[0].clone();
    observed.insert("pending", i64::from(pending_after_render));
    observed.insert("consumes", i64::from(classified && !pending_after_render));
    observed.insert("deliveries", i64::from(delivered));
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &selected,
        &observed,
        Some("RenderOrdinaryRoute"),
        "ordinary single-pane pulse consume/delivery",
    );
    assert!(ok, "ordinary runtime pulse route rejected: {why}");

    // Field-sensitive/non-vacuous control: leave every real projection
    // intact except the delivery count. An ordinary take without its
    // same-frame cat delivery must not satisfy the healthy action.
    let mut forged_undelivered = observed.clone();
    forged_undelivered.insert("deliveries", 0);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &selected,
        &forged_undelivered,
        Some("RenderOrdinaryRoute"),
        "forged ordinary cursor-cat pulse without delivery",
    );
    assert!(
        !ok,
        "an ordinary consumed-but-undelivered pulse must be rejected"
    );

    let switched = model.successors("SwitchToComposedExtractedRoute", &observed)[0].clone();
    assert!(
        !replay_changed_cat,
        "the composed forwarding seam replayed an ordinary-consumed pulse"
    );
    assert!(
        model
            .successors("RenderComposedExtractedRoute", &switched)
            .is_empty(),
        "the healthy composed action rejects an already-consumed ordinary pulse"
    );
}

#[test]
fn ordinary_single_pane_forwards_and_drains_licensed_cat_pulse() {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    let t0 = Instant::now();

    // Seed both the engine cursor and production generation fence on the
    // exact ordinary/single-pane host path.
    {
        let window = app.windows.get_mut(&wid).expect("ordinary window");
        window.focused = true;
        window.poof_row_buf = vec![' '; 80];
    }
    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 0));
    seed.row_probe = Some((2, 0, aterm_effects::cursor_glow::ProbeTrust::Full));
    app.cursor_frame(wid, seed)
        .expect("seed ordinary cursor route");

    let typed = t0 + Duration::from_millis(1);
    {
        let window = app.windows.get_mut(&wid).expect("ordinary window");
        window.fx.cursor_mut().glow.note_typed_cells(typed, 1);
        window.fx.cursor_mut().trail.note_typed(typed);
        window.poof_row_buf[0] = 'x';
    }

    let tick = t0 + Duration::from_millis(16);
    let mut live = CursorFxInputs::sample_for_test(tick);
    live.cur = Some((2, 1));
    live.row_probe = Some((2, 1, aterm_effects::cursor_glow::ProbeTrust::Full));
    app.cursor_frame(wid, live)
        .expect("advance ordinary cursor route");

    let ordinary_route = {
        let window = app.windows.get_mut(&wid).expect("ordinary window");
        let classified = window.fx.cursor_mut().glow.admission_tally().licensed > 0;
        let before_replay = window.fx.companions_mut().cursor_cat.momentum(tick);
        let delivered = before_replay > 0.0;
        assert!(
            delivered,
            "the licensed ordinary pulse reaches the cat in its own frame"
        );
        let replay_pulse = window.fx.cursor_mut().glow.take_cursor_cat_motion_pulse();
        let pending = replay_pulse.is_some();
        forward_kitty_cursor_motion(
            true,
            false,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            replay_pulse,
            &mut window.fx.companions_mut().cursor_cat,
        );
        let replay_changed = window
            .fx
            .companions_mut()
            .cursor_cat
            .momentum(tick)
            .to_bits()
            != before_replay.to_bits();
        assert!(
            !pending,
            "the ordinary route leaves no pulse for composition"
        );
        (classified, pending, delivered, replay_changed)
    };
    bind_ordinary_cat_pulse_route(
        ordinary_route.0,
        ordinary_route.1,
        ordinary_route.2,
        ordinary_route.3,
    );
}

#[test]
fn mixed_advance_drains_muted_cues_and_retain_does_not_advance() {
    let (mut app, wid, session) = mixed_fixture("fire");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    let t0 = Instant::now();
    assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t0)));

    arm_synthetic_typed(&mut app, wid, t0 + Duration::from_millis(1));
    term_lock(&term).grid_mut().cursor_forward(1);
    assert!(app.composed_cursor_frame(
        wid,
        ComposedCursorFxClock::Advance(t0 + Duration::from_millis(16))
    ));
    let window = app.windows.get_mut(&wid).expect("test window");
    assert!(
        !window.fx.cursor_mut().glow_scratch.is_empty(),
        "the typed cursor move really spawned the fire whose cue was drained"
    );
    assert!(
        window
            .fx
            .cursor_mut()
            .glow
            .drain_sound_cues()
            .next()
            .is_none(),
        "muted/headless Advance discards cues immediately; no later unmute can replay them"
    );
    let retained_glow = window.fx.cursor_mut().glow_scratch.clone();
    let retained_trail = window.fx.cursor_mut().trail_scratch.clone();

    assert!(app.composed_cursor_frame(
        wid,
        ComposedCursorFxClock::Retain {
            observed_at: t0 + Duration::from_millis(17),
        },
    ));
    let window = app.windows.get_mut(&wid).expect("test window");
    assert_eq!(window.fx.cursor_mut().glow_scratch, retained_glow);
    assert_eq!(window.fx.cursor_mut().trail_scratch, retained_trail);
    assert!(
        window
            .fx
            .cursor_mut()
            .glow
            .drain_sound_cues()
            .next()
            .is_none(),
        "Retain neither ticks nor manufactures a cue"
    );
}

/// A history retain arms no hide the next live frame would take.
///
/// `rk::Engine::hide_next_frame` is taken by the tick, and the `Retain` arm
/// never ticks, so a hide armed from
/// [`App::retire_composed_cursor_effects_for_history`] would blank the band
/// on the next live frame. `TornBand::Keep` reads
/// `CursorGlow::v2_owns_frame` instead. Pinned: nothing is armed over
/// history, the band is kept, and the following live frame draws it.
#[test]
fn a_history_retain_keeps_the_band_and_arms_no_hide_the_next_live_frame_would_take() {
    let (mut app, wid, session) = mixed_fixture("rainbow kitty");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    let t0 = Instant::now();
    let advance = |app: &mut App, at: Instant| {
        assert!(
            app.prepare_heterogeneous_input_scratch_with_cursor_fx(
                wid,
                Some(ComposedCursorFxClock::Advance(at)),
            )
            .is_some()
        );
    };
    // The scrollback to scroll INTO is written before the band is laid:
    // forty fresh lines afterwards would scroll the band's own rows off
    // the grid, and the frame it did not draw would be honest.
    {
        let mut terminal = term_lock(&term);
        terminal.process(&b"history\r\n".repeat(40));
        terminal.scroll_to_bottom();
    }
    advance(&mut app, t0);
    for i in 1..=6u64 {
        type_one_licensed_echo(&mut app, wid, &term, t0 + Duration::from_millis(i * 8));
        advance(&mut app, t0 + Duration::from_millis(i * 8 + 2));
    }
    let live_quads = app
        .windows
        .get(&wid)
        .expect("test window")
        .fx
        .cursor()
        .glow
        .under_quads()
        .len();
    assert!(
        live_quads > 0,
        "fixture: a rainbow kitty band is on the live frame"
    );

    // Into history: the Retain arm reaches the Keep law and ticks nothing.
    {
        let mut terminal = term_lock(&term);
        terminal.scroll_display(1);
        assert!(
            terminal.grid().display_offset() > 0,
            "fixture entered history"
        );
    }
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Retain {
                observed_at: t0 + Duration::from_millis(60),
            }),
        )
        .is_some()
    );
    {
        let window = app.windows.get(&wid).expect("test window");
        assert!(
            window.input_scratch.glow_under.is_empty(),
            "the history frame projects none of the band"
        );
        assert!(
            !window.fx.cursor().glow.v2_hide_armed(),
            "the non-ticking history arm left a one-frame hide standing for whatever \
             viewport ticks next"
        );
        assert!(
            window.fx.cursor().glow.v2_owns_frame(),
            "the band was cut instead of kept"
        );
    }

    // Back to the live viewport: the very next tick draws the band.
    {
        let mut terminal = term_lock(&term);
        terminal.scroll_to_bottom();
        assert_eq!(terminal.grid().display_offset(), 0, "back on the live grid");
    }
    advance(&mut app, t0 + Duration::from_millis(68));
    let window = app.windows.get(&wid).expect("test window");
    assert!(
        !window.fx.cursor().glow.under_quads().is_empty(),
        "the first live frame after a history retain drew nothing: the hide armed over \
         history was taken by it"
    );
}

#[test]
fn mixed_retain_cannot_project_a_live_cursor_frame_over_new_history() {
    let (mut app, wid, session) = mixed_fixture("comet");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    let t0 = Instant::now();
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(t0)),
        )
        .is_some()
    );
    arm_synthetic_typed(&mut app, wid, t0 + Duration::from_millis(1));
    term_lock(&term).grid_mut().cursor_forward(1);
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(
                t0 + Duration::from_millis(16)
            )),
        )
        .is_some()
    );
    {
        let window = app.windows.get(&wid).expect("test window");
        assert!(
            !window.fx.cursor().glow_scratch.is_empty()
                && !window.fx.cursor().trail_scratch.is_empty(),
            "negative control retains a genuinely charged composed cursor frame"
        );
        assert!(window.composed_cursor_effect_valid);
    }

    let transcript = b"history\r\n".repeat(40);
    {
        let mut terminal = term_lock(&term);
        terminal.process(&transcript);
        terminal.scroll_display(1);
        assert!(
            terminal.grid().display_offset() > 0,
            "fixture entered history"
        );
    }
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Retain {
                observed_at: t0 + Duration::from_millis(17),
            }),
        )
        .is_some()
    );
    let window = app.windows.get(&wid).expect("test window");
    assert!(
        !window.composed_cursor_effect_valid
            && !window.fx.cursor().glow.is_active()
            && !window.fx.cursor().trail.is_active()
            && window.fx.cursor().glow_scratch.is_empty()
            && window.fx.cursor().trail_scratch.is_empty(),
        "exact history extraction retires both retained engines and their frame vectors"
    );
    assert!(
        window.input_scratch.cursor_glow_add.is_empty()
            && window.input_scratch.glow_halo.is_empty()
            && window.input_scratch.fire_patch.is_empty()
            && window.input_scratch.glow_under.is_empty()
            && window.input_scratch.cursor_trail.is_empty()
            && window.input_scratch.cursor_effect_style_override.is_none()
            && window.input_scratch.cursor_fill_override.is_none(),
        "Retain projects no live cursor-owned pixels over the extracted history cells"
    );

    let model = aterm_spec::derive::cursor_viewport_lifecycle_model();
    let charged = model.successors("ChargeLive", &model.init_state())[0].clone();
    let history = model.successors("EnterHistory", &charged)[0].clone();
    let mut retained = history.clone();
    retained.insert("retained_checked", 1);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &history,
        &retained,
        Some("RetainHistory"),
        "composed Retain history projection",
    );
    assert!(ok, "real retained-history projection rejected: {why}");
    let mut stale = retained;
    stale.insert("trail_visible", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &history,
        &stale,
        Some("RetainHistory"),
        "composed Retain stale-frame negative control",
    );
    assert!(!ok, "Retain may not resurrect a previous live cursor frame");
}

fn run_alt_reanchor(with_repaint_blink: bool) -> (Vec<aterm_render::TrailCell>, Option<Instant>) {
    let (mut app, wid, session) = mixed_fixture("comet");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    let t0 = Instant::now();
    term_lock(&term).process(b"\x1b[?1049h\x1b[6;3H");
    assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t0)));
    // A real press, so the relocation that follows is licensed. What the
    // repaint blink then decides is the MORPHOLOGY: a Claude-Code-style
    // hide-inside-sync redraw is a re-anchor (no comet across cells the
    // caret never swept), while an unblinked alt move — vim's own one-row
    // motions — keeps its comet, exactly as v0.43.0 had it.
    {
        let window = app.windows.get_mut(&wid).expect("test window");
        window
            .fx
            .cursor_mut()
            .glow
            .note_typed(t0 + Duration::from_millis(1));
        window
            .fx
            .cursor_mut()
            .trail
            .note_typed(t0 + Duration::from_millis(1));
    }
    if with_repaint_blink {
        term_lock(&term).process(b"\x1b[?2026h\x1b[?25l\x1b[6;20H\x1b[?25h\x1b[?2026l");
    } else {
        term_lock(&term).process(b"\x1b[6;20H");
    }
    let tick = t0 + Duration::from_millis(16);
    assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(tick)));
    let window = app.windows.get(&wid).expect("test window");
    (
        window.fx.cursor().trail_scratch.clone(),
        window.last_blink_at,
    )
}

#[test]
fn mixed_advance_feeds_alt_repaint_blink_to_both_cursor_engines() {
    let (claude_like, blink_at) = run_alt_reanchor(true);
    assert!(
        claude_like.is_empty(),
        "alt + synchronized repaint blink re-anchors instead of sweeping a fake comet"
    );
    assert!(
        blink_at.is_some(),
        "the host consumed the focused terminal's repaint-blink epoch"
    );

    let (vim_like, blink_at) = run_alt_reanchor(false);
    assert!(
        !vim_like.is_empty(),
        "without the blink the same licensed move keeps its comet — the blink IS \
         the discriminator, which is what makes the arm above non-vacuous"
    );
    assert_eq!(blink_at, None);
}

#[test]
fn mixed_advance_translates_scroll_anchors_and_offsets_the_row_probe() {
    let (mut app, wid, session) = mixed_fixture("comet");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    let plan = app.active_visible_leaf_plan(wid).expect("mixed plan");
    let terminal_leaf = plan
        .leaves
        .iter()
        .find(|leaf| leaf.focused)
        .expect("focused terminal leaf");
    let col_off = terminal_leaf.rect.origin.x.round().max(0.0) as usize;
    assert!(
        col_off > 0,
        "fixture places the terminal after its native sibling"
    );

    let t0 = Instant::now();
    term_lock(&term).process(b"\x1b[3;1HABOVE\x1b[4;1HCURRENT\x1b[5;1HBELOW\x1b[4;11H");
    assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t0)));
    assert!(app.composed_cursor_frame(
        wid,
        ComposedCursorFxClock::Advance(t0 + Duration::from_millis(8))
    ));
    {
        let window = app.windows.get(&wid).expect("test window");
        assert!(
            window.poof_row_buf[..col_off].iter().all(|&ch| ch == ' '),
            "the pane-local row probe is shifted into exact window columns"
        );
        let actual: String = window.poof_row_buf[col_off..col_off + 7].iter().collect();
        assert_eq!(actual, "CURRENT", "the cursor row stays row-coherent");
        assert_eq!(window.poof_row_above_buf.len(), col_off);
        assert_eq!(window.poof_row_below_buf.len(), col_off);
        assert!(window.poof_row_above_buf.iter().all(|&ch| ch == ' '));
        assert!(window.poof_row_below_buf.iter().all(|&ch| ch == ' '));
    }

    {
        let window = app.windows.get_mut(&wid).expect("test window");
        let authored = t0 + Duration::from_millis(15);
        window.fx.cursor_mut().glow.note_return(authored);
        window.fx.cursor_mut().trail.note_return(authored);
    }
    term_lock(&term).process(b"\x1b[999;11H\n");
    assert!(app.composed_cursor_frame(
        wid,
        ComposedCursorFxClock::Advance(t0 + Duration::from_millis(16))
    ));
    let window = app.windows.get(&wid).expect("test window");
    // The subject is the scroll anchor: the host's cumulative scroll clock
    // has advanced to the terminal's, so the next move classifies against
    // where the caret now sits.
    assert_eq!(
        window.fx.scroll_baseline(),
        Some(term_lock(&term).content_scroll_state())
    );
}

#[test]
fn mixed_advance_captures_rainbow_neighbor_rows_after_engagement() {
    let (mut app, wid, session) = mixed_fixture("rainbow kitty");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    let plan = app.active_visible_leaf_plan(wid).expect("mixed plan");
    let terminal_leaf = plan
        .leaves
        .iter()
        .find(|leaf| leaf.focused)
        .expect("focused terminal leaf");
    let col_off = terminal_leaf.rect.origin.x.round().max(0.0) as usize;
    assert!(col_off > 0);
    term_lock(&term).process(b"\x1b[3;1HABOVE\x1b[4;1HCURRENT\x1b[5;1HBELOW\x1b[4;11H");
    let t0 = Instant::now();
    assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t0)));
    assert!(app.windows[&wid].fx.cursor().glow.v2_owns_frame());
    assert!(app.composed_cursor_frame(
        wid,
        ComposedCursorFxClock::Advance(t0 + Duration::from_millis(8))
    ));
    let window = app.windows.get(&wid).expect("test window");
    for (probe, marker) in [
        (&window.poof_row_above_buf, "ABOVE"),
        (&window.poof_row_buf, "CURRENT"),
        (&window.poof_row_below_buf, "BELOW"),
    ] {
        assert!(
            probe[..col_off].iter().all(|&ch| ch == ' '),
            "the pane-local probe has exact window-column padding"
        );
        let actual: String = probe[col_off..col_off + marker.len()].iter().collect();
        assert_eq!(actual, marker, "the rainbow probe stays row-coherent");
    }
}

#[test]
fn exact_composed_sample_distinguishes_grid_edges_from_blank_neighbors() {
    let (mut app, wid, session) = mixed_fixture("rainbow kitty");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(Instant::now())));
    let v2_owns_frame = app.windows[&wid].fx.cursor().glow.v2_owns_frame();
    assert!(
        v2_owns_frame,
        "rainbow kitty is engaged after its first tick"
    );
    let mut term = term_lock(&term);

    term.process(b"\x1b[1;1Htop");
    let top = focused_composed_cursor_fx_sample(
        session,
        &term,
        true,
        v2_owns_frame,
        (0, 0),
        &[],
        None,
        ComposedWitnessRows::default(),
        Vec::new(),
        Vec::new(),
        Vec::new(),
    );
    assert!(!top.row_above_present, "top edge has no row above");
    assert!(top.row_below_present, "a real blank row below is observed");
    assert!(top.row_above_probe.is_empty());
    assert!(
        top.row_below_probe.iter().all(|&ch| ch == ' '),
        "blank is data, not an absent neighbor"
    );

    term.process(b"\x1b[999;1Hbottom");
    let bottom = focused_composed_cursor_fx_sample(
        session,
        &term,
        true,
        v2_owns_frame,
        (0, 0),
        &[],
        None,
        top.witness_rows,
        top.row_probe,
        top.row_above_probe,
        top.row_below_probe,
    );
    assert!(
        bottom.row_above_present,
        "bottom row has a real neighbor above"
    );
    assert!(!bottom.row_below_present, "bottom edge has no row below");
    assert!(bottom.row_below_probe.is_empty());
}

#[test]
fn composed_sample_skips_neighbors_when_rainbow_is_not_engaged() {
    for style in ["comet", "rainbow kitty"] {
        let (mut app, wid, session) = mixed_fixture(style);
        if style == "rainbow kitty" {
            app.config.cursor_trail = Some(false);
        }
        let term = app.pool.get(session).expect("terminal leaf").term.clone();
        term_lock(&term).process(b"\x1b[3;1HABOVE\x1b[4;1HCURRENT\x1b[5;1HBELOW\x1b[4;11H");
        assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(Instant::now())));
        let v2_owns_frame = app.windows[&wid].fx.cursor().glow.v2_owns_frame();
        assert!(
            !v2_owns_frame,
            "{style} must not own rainbow's neighbor gate"
        );
        let sample = focused_composed_cursor_fx_sample(
            session,
            &term_lock(&term),
            true,
            v2_owns_frame,
            (0, 0),
            &[],
            None,
            ComposedWitnessRows::default(),
            Vec::new(),
            vec!['!'; 12],
            vec!['!'; 12],
        );
        assert!(
            sample
                .row_probe
                .iter()
                .take(7)
                .copied()
                .eq("CURRENT".chars()),
            "the cursor row still feeds typing and erase proofs for {style}"
        );
        assert!(!sample.row_above_present && !sample.row_below_present);
        assert!(sample.row_above_probe.is_empty() && sample.row_below_probe.is_empty());
    }
}

// The content witness on a composed frame (`ribbon_rows` →
// `observe_ribbon_row`/`capture_ribbon_row` → `Engine::witness_rows`) runs
// for split panes, zoomed panes and `aterm ctl image`/`video` captures, not
// only the single-pane path. In a split the caret rarely leaves its row, so
// only the witness can retire a stale band.

/// The focused terminal leaf's pane origin in the composed window.
fn focused_pane_origin(app: &mut App, wid: WindowId) -> (usize, usize) {
    let plan = app.active_visible_leaf_plan(wid).expect("composed plan");
    let leaf = plan
        .leaves
        .iter()
        .find(|leaf| leaf.focused)
        .expect("focused terminal leaf");
    (
        leaf.rect.origin.y.round().max(0.0) as usize,
        leaf.rect.origin.x.round().max(0.0) as usize,
    )
}

/// Resident ribbon cells on one window row: `(col, leaving)`.
fn composed_ribbon_row(app: &App, wid: WindowId, row: u16) -> Vec<(u16, bool)> {
    let mut v: Vec<(u16, bool)> = app
        .windows
        .get(&wid)
        .expect("test window")
        .fx
        .cursor()
        .glow
        .v2_ribbon()
        .expect("rainbow kitty owns the composed frame")
        .cells()
        .iter()
        .filter(|c| c.row == row)
        .map(|c| (c.col, c.leaving()))
        .collect();
    v.sort_unstable();
    v
}

fn composed_ribbon_retired(app: &App, wid: WindowId) -> u64 {
    app.windows
        .get(&wid)
        .expect("test window")
        .fx
        .cursor()
        .glow
        .v2_status()
        .map_or(0, |s| s.retired)
}

/// `ribbon_followed=` on the composed window: the cells the follow pass
/// carried to another row with their text.
fn composed_ribbon_followed(app: &App, wid: WindowId) -> u64 {
    app.windows
        .get(&wid)
        .expect("test window")
        .fx
        .cursor()
        .glow
        .v2_status()
        .map_or(0, |s| s.followed)
}

/// Type `text` into the composed fixture one licensed key at a time, at a
/// 90 ms cadence, presenting a composed frame per key. Returns the clock of
/// the last present.
fn composed_type(
    app: &mut App,
    wid: WindowId,
    term: &std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
    mut t: Instant,
    text: &str,
) -> Instant {
    for ch in text.bytes() {
        t += Duration::from_millis(90);
        app.windows
            .get_mut(&wid)
            .expect("test window")
            .fx
            .cursor_mut()
            .glow
            .note_typed_cells(t, 1);
        term_lock(term).process(&[ch]);
        assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t)));
    }
    t
}

/// Idle composed frames at 16 ms until `ms` have passed.
fn composed_idle(app: &mut App, wid: WindowId, mut t: Instant, ms: u64) -> Instant {
    let end = t + Duration::from_millis(ms);
    while t < end {
        t += Duration::from_millis(16);
        assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t)));
    }
    t
}

/// The composed shapes the witness is fed under. All of them route through
/// `composed_cursor_frame_sampled`; what
/// differs is where the focused pane sits in the window.
#[derive(Clone, Copy, Debug)]
enum ComposedShape {
    /// One split beside the native sibling: a column offset (Horizontal)
    /// or a row offset (Vertical).
    Split(crate::tab_model::SplitAxis),
    /// Two splits: the focused leaf carries both offsets at once.
    ThreePane,
    /// A split, then the focused leaf zoomed — one visible leaf that still
    /// routes as composed (`visible_content_route_from_plan`: `composed:
    /// plan.zoomed`), so it must be fed here and not in the single-pane hold.
    Zoomed,
}

/// A ribbon laid on a composed frame, and the window row it sits on.
fn composed_hello(
    shape: ComposedShape,
) -> (
    App,
    WindowId,
    std::sync::Arc<std::sync::Mutex<aterm_core::terminal::Terminal>>,
    Instant,
    u16,
    u16,
) {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    assert!(app.open_settings_tab(crate::native_settings::SettingsRoute::Home));
    use crate::tab_model::SplitAxis;
    let session = match shape {
        ComposedShape::Split(axis) => app.split_active_with_stub_terminal(wid, axis).0,
        ComposedShape::ThreePane => {
            let _ = app.split_active_with_stub_terminal(wid, SplitAxis::Horizontal);
            app.split_active_with_stub_terminal(wid, SplitAxis::Vertical)
                .0
        }
        ComposedShape::Zoomed => {
            let (session, _) = app.split_active_with_stub_terminal(wid, SplitAxis::Horizontal);
            assert!(
                app.windows
                    .get_mut(&wid)
                    .expect("test window")
                    .tab_set
                    .active_mut()
                    .expect("active tab")
                    .toggle_zoom(),
                "the focused terminal leaf zooms"
            );
            app.resize_panes(wid);
            session
        }
    };
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    let (row_off, col_off) = focused_pane_origin(&mut app, wid);
    // The shape is what it claims: a three-pane focused leaf is offset on
    // Both axes, and a zoomed tab plans as zoomed.
    match shape {
        ComposedShape::ThreePane => assert!(
            row_off > 0 && col_off > 0,
            "{shape:?}: the focused leaf carries both offsets ({row_off}, {col_off})"
        ),
        ComposedShape::Zoomed => assert!(
            app.active_visible_leaf_plan(wid)
                .is_some_and(|plan| plan.zoomed),
            "{shape:?}: the plan is zoomed"
        ),
        ComposedShape::Split(_) => {}
    }
    let t0 = Instant::now();
    // Park the caret on the pane's own row 5 and seed the engine.
    term_lock(&term).process(b"\x1b[6;1H");
    assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t0)));
    let t = composed_type(&mut app, wid, &term, t0, "hello world");
    let win_row = u16::try_from(row_off).expect("pane row fits") + 5;
    let win_col = u16::try_from(col_off).expect("pane col fits");
    let laid: Vec<(u16, bool)> = (0..11).map(|k| (win_col + k, false)).collect();
    assert_eq!(
        composed_ribbon_row(&app, wid, win_row),
        laid,
        "the typed band is laid under the text, in the pane's own columns"
    );
    assert_eq!(composed_ribbon_retired(&app, wid), 0);
    (app, wid, term, t, win_row, win_col)
}

/// A program re-lays its input box two rows down with no key behind it: the
/// move is declined, and the band follows its text (`rk::witness`'s follow
/// pass, `Engine::follow_rows`) to the glyphs standing two rows down, with
/// every clock intact: nothing on the old row, nothing retired,
/// `ribbon_followed=11`. That count proves the composed path feeds the
/// witness. Single-pane twin: `abandoned_ribbon.rs`'s
/// `a_relocated_input_box_carries_the_band_with_its_text_and_stamps_nothing`.
#[test]
fn a_relocated_input_box_carries_its_band_with_its_text_on_a_composed_frame() {
    for shape in [
        ComposedShape::Split(crate::tab_model::SplitAxis::Horizontal),
        ComposedShape::ThreePane,
        ComposedShape::Zoomed,
    ] {
        let (mut app, wid, term, t, win_row, win_col) = composed_hello(shape);
        let t = composed_idle(&mut app, wid, t, 400);
        term_lock(&term).process(b"\x1b[2J\x1b[8;1Hhello world");
        let t = composed_idle(&mut app, wid, t, 16);
        assert!(
            composed_ribbon_row(&app, wid, win_row).is_empty(),
            "{shape:?}: the band left the row its text left: {:?}",
            composed_ribbon_row(&app, wid, win_row)
        );
        let carried: Vec<(u16, bool)> = (0..11).map(|k| (win_col + k, false)).collect();
        assert_eq!(
            composed_ribbon_row(&app, wid, win_row + 2),
            carried,
            "{shape:?}: …and stands under its text two rows down, live"
        );
        assert_eq!(composed_ribbon_followed(&app, wid), 11, "{shape:?}");
        assert_eq!(composed_ribbon_retired(&app, wid), 0, "{shape:?}");
        let _ = composed_idle(&mut app, wid, t, 200);
        assert!(
            composed_ribbon_row(&app, wid, win_row).is_empty(),
            "{shape:?}: nothing came back to the row the text left"
        );
        assert_eq!(
            composed_ribbon_row(&app, wid, win_row + 2),
            carried,
            "{shape:?}: the band stays under its text 200 ms on"
        );
    }
}

/// The witness reads the focused pane's own cells, with the pane's column
/// rotation and row offset; otherwise it compares ribbon cells against
/// another pane's glyphs. Covered: both splits (one offset each), a
/// three-pane layout (both offsets) and a zoomed leaf, with the band on a
/// row the caret has left (read into a resident witness slot). Also the
/// shape law: a lone glyph changed strictly inside the band is not
/// evidence, the same change at the band's last cell retires.
#[test]
fn the_composed_witness_reads_the_focused_pane_s_own_rows_and_columns() {
    for axis in [
        ComposedShape::Split(crate::tab_model::SplitAxis::Horizontal),
        ComposedShape::Split(crate::tab_model::SplitAxis::Vertical),
        ComposedShape::ThreePane,
        ComposedShape::Zoomed,
    ] {
        let (mut app, wid, term, t, win_row, win_col) = composed_hello(axis);
        // A redraw that puts the same text back under the caret is the
        // same text — the caret's row rides this frame's own probe.
        term_lock(&term).process(b"\x1b[6;1Hhello world");
        let t = composed_idle(&mut app, wid, t, 32);
        assert_eq!(
            composed_ribbon_retired(&app, wid),
            0,
            "{axis:?}: a redraw of the same text retired the caret's row"
        );

        // Return: the caret LEAVES the band's row on a licensed move, so
        // the band stays where it was typed and row-scoped renewal no
        // longer touches it. From here on row `win_row` is sampled into
        // its pane-local resident witness slot — the read this test is about.
        let t = t + Duration::from_millis(90);
        app.windows
            .get_mut(&wid)
            .expect("test window")
            .fx
            .cursor_mut()
            .glow
            .note_return(t);
        term_lock(&term).process(b"\r\n");
        assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t)));
        let t = composed_idle(&mut app, wid, t, 200);
        let live: Vec<(u16, bool)> = (0..11).map(|k| (win_col + k, false)).collect();
        assert_eq!(
            composed_ribbon_row(&app, wid, win_row),
            live,
            "{axis:?}: the band the owner typed is untouched — its text \
             never moved"
        );
        assert_eq!(
            composed_ribbon_retired(&app, wid),
            0,
            "{axis:?}: the witness read another pane's rows or columns and \
             retired light the owner earned"
        );

        // The shape law through the composed path (`Witness::shape_verdicts`):
        // one glyph changed strictly inside the run (the fifth cell, `o` →
        // `X`) is not evidence; the band stays whole and the counter at zero.
        term_lock(&term).process(b"\x1b7\x1b[6;5HX\x1b8");
        let t = composed_idle(&mut app, wid, t, 32);
        assert_eq!(
            composed_ribbon_row(&app, wid, win_row),
            live,
            "{axis:?}: a lone interior glyph change punched a hole in a \
             standing run"
        );
        assert_eq!(
            composed_ribbon_retired(&app, wid),
            0,
            "{axis:?}: a lone interior glyph change retired a standing run"
        );

        // One changed glyph at the end of the same run, caret undisturbed,
        // is a suffix and retires: the non-vacuity arm (a starved witness
        // passes the interior arm and fails here). Exactly one cell retires,
        // since the interior record already took the glyph standing there;
        // a witness that did not would retire two.
        term_lock(&term).process(b"\x1b7\x1b[6;11HX\x1b8");
        let _ = composed_idle(&mut app, wid, t, 32);
        assert_eq!(
            composed_ribbon_retired(&app, wid),
            1,
            "{axis:?}: the witness never saw the glyph change, or the \
             interior record did not take the glyph standing there"
        );
    }
}

/// A scroll makes projection suppress its row probe. Far witness rows
/// scanned under that same extraction lock would be discarded; the next
/// unchanged scroll snapshot captures them again normally.
#[test]
fn composed_scroll_skips_far_witness_scan_then_recovers() {
    for shape in [ComposedShape::ThreePane, ComposedShape::Zoomed] {
        let (mut app, wid, term, t, win_row, _) = composed_hello(shape);
        let t = t + Duration::from_millis(90);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .glow
            .note_return(t);
        term_lock(&term).process(b"\r\n");
        assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t)));
        term_lock(&term).process(b"\x1b[8;1H");
        let pane_origin = focused_pane_origin(&mut app, wid);
        let session = app.windows[&wid].composed_cursor_effect_session.unwrap();
        let mut requested = [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
        let n = app.windows[&wid]
            .fx
            .cursor()
            .glow
            .ribbon_rows(&mut requested);
        assert!(
            requested[..n].contains(&win_row),
            "fixture asks for a far band"
        );
        let previous_scroll = app.windows[&wid].fx.scroll_baseline();
        let stable = focused_composed_cursor_fx_sample(
            session,
            &term_lock(&term),
            true,
            app.windows[&wid].fx.cursor().glow.v2_owns_frame(),
            pane_origin,
            &requested[..n],
            previous_scroll,
            ComposedWitnessRows::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(
            stable.witness_rows.get(win_row).is_some(),
            "steady frame captures the far row"
        );

        term_lock(&term).process(b"\x1b[999;1H\n");
        let scroll = focused_composed_cursor_fx_sample(
            session,
            &term_lock(&term),
            true,
            app.windows[&wid].fx.cursor().glow.v2_owns_frame(),
            pane_origin,
            &requested[..n],
            previous_scroll,
            stable.witness_rows,
            stable.row_probe,
            stable.row_above_probe,
            stable.row_below_probe,
        );
        let current_scroll = scroll.content_scroll_state;
        assert!(
            !matches!(
                ContentScrollState::delta_since(previous_scroll, current_scroll),
                ContentScrollDelta::Baseline | ContentScrollDelta::Unchanged
            ),
            "the parser produced a real scroll"
        );
        assert_eq!(
            scroll.witness_rows.active, 0,
            "no far row is scanned on a scroll frame"
        );
        assert!(scroll.witness_rows.get(win_row).is_none());
        assert!(
            sync_cursor_effect_scroll(app.windows.get_mut(&wid).unwrap(), current_scroll).changed()
        );

        let next = focused_composed_cursor_fx_sample(
            session,
            &term_lock(&term),
            true,
            app.windows[&wid].fx.cursor().glow.v2_owns_frame(),
            pane_origin,
            &requested[..n],
            app.windows[&wid].fx.scroll_baseline(),
            scroll.witness_rows,
            scroll.row_probe,
            scroll.row_above_probe,
            scroll.row_below_probe,
        );
        assert!(
            next.witness_rows.get(win_row).is_some(),
            "the next steady frame captures it"
        );
    }
}

#[test]
fn composed_far_witness_uses_the_extracted_row_after_later_pty_output() {
    for shape in [ComposedShape::ThreePane, ComposedShape::Zoomed] {
        let (mut app, wid, term, t, win_row, win_col) = composed_hello(shape);
        let t = t + Duration::from_millis(90);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .glow
            .note_return(t);
        term_lock(&term).process(b"\r\n");
        assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t)));
        // The retained band is on local row 5; park the caret on row 7,
        // outside its already-captured caret/neighbor trio.
        term_lock(&term).process(b"\x1b[8;1H");
        let (pane_row, pane_col) = focused_pane_origin(&mut app, wid);
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let pane_rows = plan
            .leaves
            .iter()
            .find(|leaf| leaf.focused)
            .unwrap()
            .rect
            .size
            .height
            .round() as usize;
        let session = app.windows[&wid].composed_cursor_effect_session.unwrap();
        let mut requested = [0u16; aterm_effects::cursor_glow::CURSOR_WITNESS_ROWS];
        let n = app.windows[&wid]
            .fx
            .cursor()
            .glow
            .ribbon_rows(&mut requested);
        assert!(
            requested[..n].contains(&win_row),
            "fixture asks for the far band"
        );
        let sample = focused_composed_cursor_fx_sample(
            session,
            &term_lock(&term),
            true,
            app.windows[&wid].fx.cursor().glow.v2_owns_frame(),
            (pane_row, pane_col),
            &requested[..n],
            None,
            ComposedWitnessRows::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert_eq!(
            sample.witness_rows.get(win_row).unwrap()[usize::from(win_col) + 10],
            'd',
            "far row came from the same extraction hold as the old cells"
        );
        let caret_row = sample.cursor.0 + u16::try_from(pane_row).unwrap();
        assert!(
            !needs_fresh_witness_read(
                &requested[..n],
                caret_row,
                pane_row,
                pane_rows,
                sample
                    .row_above_present
                    .then_some(sample.row_above_probe.as_slice()),
                sample
                    .row_below_present
                    .then_some(sample.row_below_probe.as_slice()),
                &sample.witness_rows,
            ),
            "every in-pane row is already captured; projection owes no second lock"
        );
        // The live PTY changes *after* the extracted frame. Sampling it
        // again here would incorrectly retire the ribbon over old pixels.
        term_lock(&term).process(b"\x1b7\x1b[6;11HX\x1b8");
        assert!(!sample.witness_stamp.matches(&term_lock(&term)));
        let model = aterm_spec::derive::composed_witness_generation_model();
        let mut state = model.init_state();
        for action in ["Capture", "Mutate"] {
            assert!(model.fire(action, &mut state));
        }
        assert!(model.action_enabled("ReadCaptured", &state));
        assert!(!model.action_enabled("ReadFresh", &state));
        assert!(app.composed_cursor_frame_sampled(
            wid,
            ComposedCursorFxClock::Advance(t + Duration::from_millis(16)),
            Some(sample),
            &plan,
        ));
        assert_eq!(composed_ribbon_retired(&app, wid), 0);
        assert_eq!(
            composed_ribbon_row(&app, wid, win_row),
            (0..11).map(|k| (win_col + k, false)).collect::<Vec<_>>()
        );
        composed_idle(&mut app, wid, t + Duration::from_millis(16), 32);
        assert!(
            composed_ribbon_retired(&app, wid) > 0,
            "the next extracted frame sees the real replacement"
        );
    }
}

#[test]
fn composed_witness_defers_rows_changed_after_cell_extraction() {
    for shape in [ComposedShape::ThreePane, ComposedShape::Zoomed] {
        let (mut app, wid, term, t, win_row, win_col) = composed_hello(shape);
        // Leave a live band two rows above the caret. This is outside
        // the exact caret-and-neighbor capture, so only a matching
        // second-lock generation may fill its direct witness slot.
        let t = t + Duration::from_millis(90);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .glow
            .note_return(t);
        term_lock(&term).process(b"\r\n");
        assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t)));
        term_lock(&term).process(b"\x1b[8;1H");
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let session = app.windows[&wid].composed_cursor_effect_session.unwrap();
        let sample = focused_composed_cursor_fx_sample(
            session,
            &term_lock(&term),
            true,
            app.windows[&wid].fx.cursor().glow.v2_owns_frame(),
            (0, 0),
            &[],
            None,
            ComposedWitnessRows::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        // The grid now differs from the extracted cells at the run's last
        // recorded cell, where a lone changed glyph is evidence.
        term_lock(&term).process(b"\x1b7\x1b[6;11HX\x1b8");
        assert!(!sample.witness_stamp.matches(&term_lock(&term)));
        assert!(app.composed_cursor_frame_sampled(
            wid,
            ComposedCursorFxClock::Advance(t + Duration::from_millis(16)),
            Some(sample),
            &plan,
        ));
        assert_eq!(
            composed_ribbon_retired(&app, wid),
            0,
            "{shape:?}: newer glyphs retired the old frame"
        );
        assert_eq!(
            composed_ribbon_row(&app, wid, win_row),
            (0..11).map(|k| (win_col + k, false)).collect::<Vec<_>>()
        );
        // A fresh observation must still retire the real replacement.
        composed_idle(&mut app, wid, t + Duration::from_millis(16), 32);
        assert!(
            composed_ribbon_retired(&app, wid) > 0,
            "{shape:?}: witness was disabled"
        );
    }
}

#[test]
fn composed_neighbor_witness_uses_its_captured_change_after_later_pty_output() {
    for shape in [ComposedShape::ThreePane, ComposedShape::Zoomed] {
        let (mut app, wid, term, t, win_row, _) = composed_hello(shape);
        let t = t + Duration::from_millis(90);
        app.windows
            .get_mut(&wid)
            .unwrap()
            .fx
            .cursor_mut()
            .glow
            .note_return(t);
        term_lock(&term).process(b"\r\n");
        assert!(app.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(t)));
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let session = app.windows[&wid].composed_cursor_effect_session.unwrap();
        // The changed last glyph is IN the exact extracted neighbor row.
        term_lock(&term).process(b"\x1b7\x1b[6;11HX\x1b8");
        let sample = focused_composed_cursor_fx_sample(
            session,
            &term_lock(&term),
            true,
            app.windows[&wid].fx.cursor().glow.v2_owns_frame(),
            (0, 0),
            &[],
            None,
            ComposedWitnessRows::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(sample.row_above_present);
        // A later, unrelated PTY write invalidates any second grid read.
        // It cannot invalidate the neighbor the sample already owns.
        term_lock(&term).process(b"\x1b7\x1b[1;1HZ\x1b8");
        assert!(!sample.witness_stamp.matches(&term_lock(&term)));
        assert!(app.composed_cursor_frame_sampled(
            wid,
            ComposedCursorFxClock::Advance(t + Duration::from_millis(16)),
            Some(sample),
            &plan,
        ));
        assert_eq!(
            composed_ribbon_retired(&app, wid),
            1,
            "{shape:?}: the captured neighbor was starved by a later PTY write"
        );
        assert_eq!(composed_ribbon_row(&app, wid, win_row).len(), 11);
    }
}

#[test]
fn composed_witness_generation_model_matches_real_terminal_mutations() {
    let model = aterm_spec::derive::composed_witness_generation_model();
    for bytes in [
        b"X".as_slice(),
        b"\x1b[?2026h",
        b"\x1b[?2026h\x1b[?2026l",
        b"\x1b[?1049h",
    ] {
        let (app, wid, term, _, _, _) = composed_hello(ComposedShape::Zoomed);
        let session = app.windows[&wid].composed_cursor_effect_session.unwrap();
        let mut state = model.init_state();
        assert!(!model.action_enabled("ReadCaptured", &state));
        let sample = focused_composed_cursor_fx_sample(
            session,
            &term_lock(&term),
            true,
            app.windows[&wid].fx.cursor().glow.v2_owns_frame(),
            (0, 0),
            &[],
            None,
            ComposedWitnessRows::default(),
            Vec::new(),
            Vec::new(),
            Vec::new(),
        );
        assert!(sample.row_above_present);
        assert!(model.fire("Capture", &mut state));
        let stamp = sample.witness_stamp;
        let above_row = sample.cursor.0.checked_sub(1).unwrap();
        let captured = captured_witness_neighbor(
            above_row,
            sample.cursor.0,
            Some(sample.row_above_probe.as_slice()),
            None,
        );
        assert_eq!(
            stamp.matches(&term_lock(&term)),
            model.action_enabled("ReadFresh", &state)
        );
        assert_eq!(
            captured.is_some(),
            model.action_enabled("ReadCaptured", &state)
        );
        term_lock(&term).process(bytes);
        assert!(model.fire("Mutate", &mut state));
        assert_eq!(
            stamp.matches(&term_lock(&term)),
            model.action_enabled("ReadFresh", &state)
        );
        assert_eq!(
            captured.is_some(),
            model.action_enabled("ReadCaptured", &state)
        );
        // The generation fence rejects all of these unrelated generations.
        assert_eq!(stamp.terminal_id, term_lock(&term).render_identity());
        assert_eq!(term_lock(&term).grid().display_offset(), 0);
    }
}

#[derive(Debug, PartialEq)]
struct CoherenceObservation {
    marker: char,
    cursor: (usize, usize, CursorStyle),
    cursor_effect_style: Option<CursorStyle>,
    cursor_color: u32,
    cursor_fill: Option<u32>,
    glow: Vec<aterm_render::GlowQuad>,
    trail: Vec<aterm_render::TrailCell>,
}

fn set_coherence_state(
    term: &std::sync::Mutex<Terminal>,
    marker: char,
    cursor: (u16, u16),
    color: &str,
    style: &str,
) {
    // Deliberately avoid ED2 here: erase-display is a content-coordinate
    // invalidation and correctly resets both cursor engines. This fixture
    // tests coherent sampling across an ordinary authored cursor move, so
    // overwrite its marker in place and preserve the seeded source anchor.
    let sequence = format!(
        "\x1b[H{marker}\x1b[{};{}H\x1b]12;#{color}\x07{style}",
        cursor.0 + 1,
        cursor.1 + 1,
    );
    term_lock(term).process(sequence.as_bytes());
}

fn primed_coherence_fixture(
    t0: Instant,
) -> (App, WindowId, std::sync::Arc<std::sync::Mutex<Terminal>>) {
    let (mut app, wid, session) = mixed_fixture("fire");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    set_coherence_state(&term, 'P', (1, 2), "203040", "\x1b[2 q");
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(t0)),
        )
        .is_some()
    );
    (app, wid, term)
}

fn pure_fixture(style: &str) -> (App, WindowId, std::sync::Arc<std::sync::Mutex<Terminal>>) {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some(style.into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    let session = app.split_active_stub_tab(wid);
    let term = app
        .pool
        .get(session)
        .expect("focused terminal")
        .term
        .clone();
    assert!(
        app.active_visible_leaf_plan(wid)
            .is_some_and(|plan| plan.leaves.len() == 2),
        "fixture is a canonical pure-terminal split"
    );
    (app, wid, term)
}

fn redraw_pure(app: &mut App, wid: WindowId, now: Instant) -> bool {
    let window = app.windows.get(&wid).expect("test window");
    let (rows, cols) = (usize::from(window.rows), usize::from(window.cols));
    app.redraw_compose(wid, rows, cols, false, false, None, 0, now)
        .is_some()
}

fn composed_scratch_identity(
    app: &App,
    wid: WindowId,
) -> ((usize, usize, usize), (usize, usize, usize)) {
    let window = app.windows.get(&wid).expect("test window");
    (
        (
            window.composed_sync_observation_scratch.as_ptr() as usize,
            window.composed_sync_observation_scratch.len(),
            window.composed_sync_observation_scratch.capacity(),
        ),
        (
            window.composed_pane_stage_meta.as_ptr() as usize,
            window.composed_pane_stage_meta.len(),
            window.composed_pane_stage_meta.capacity(),
        ),
    )
}

fn visible_terminal_handles(
    app: &App,
    wid: WindowId,
) -> Vec<std::sync::Arc<std::sync::Mutex<Terminal>>> {
    app.active_visible_leaf_plan(wid)
        .expect("visible plan")
        .leaves
        .iter()
        .filter_map(|leaf| {
            let crate::tab_model::View::Terminal(view) = app.view_store.get(leaf.view).copied()?
            else {
                return None;
            };
            app.pool
                .get(view.session)
                .map(|session| session.term.clone())
        })
        .collect()
}

/// The DEC 2026 close/reopen scripts on every present route, one row per
/// (route, script). Each route writes each step to every visible terminal
/// (a pure split formats the pane index into its bytes) and then presents;
/// the row's `(ms, present?)` sequence is the law.
///
/// * CLEAN close+reopen: the completed bracket keeps exactly one present
///   license, and the newly armed episode holds on the following frame.
/// * DIRTY reopen: writes in the reopened episode replaced the close boundary,
///   so the partial next frame stays off glass until that episode closes.
#[test]
fn close_reopen_presents_once_and_a_dirty_reopen_never_presents_partial() {
    #[derive(Clone, Copy, Debug)]
    enum Route {
        PureSplit,
        Mixed,
        HeadlessSingleCapture,
    }
    // (bytes — `{i}` is the pane index — or "" for none, ms, presents?, why)
    type Step = (&'static str, u64, bool, &'static str);
    const HOLD: &str = "\x1b[?2026hheld";
    let rows: [(Route, [Step; 3]); 5] = [
        (
            Route::PureSplit,
            [
                (HOLD, 0, false, "one held pane holds the complete split"),
                (
                    "frame-{i}\x1b[?2026l\x1b[?2026h",
                    16,
                    true,
                    "unchanged extraction-lock samples preserve the close+rearm present license",
                ),
                (
                    "",
                    32,
                    false,
                    "the newly armed episodes hold again on the following frame",
                ),
            ],
        ),
        (
            Route::PureSplit,
            [
                (HOLD, 0, false, "held"),
                (
                    "complete-{i}\x1b[?2026l\x1b[?2026hpartial-{i}",
                    16,
                    false,
                    "writes in the reopened episodes replaced the close boundary and must stay off glass",
                ),
                (
                    "done\x1b[?2026l",
                    32,
                    true,
                    "closing every dirty reopened episode releases the complete split",
                ),
            ],
        ),
        (
            Route::Mixed,
            [
                (HOLD, 0, false, "held"),
                (
                    "complete\x1b[?2026l\x1b[?2026h",
                    16,
                    true,
                    "the completed bracket keeps its one mixed-frame present license",
                ),
                (
                    "",
                    32,
                    false,
                    "the newly armed mixed episode holds on the next frame",
                ),
            ],
        ),
        (
            Route::Mixed,
            [
                (HOLD, 0, false, "held"),
                (
                    "complete\x1b[?2026l\x1b[?2026hpartial",
                    16,
                    false,
                    "a heterogeneous frame cannot expose dirty reopened terminal cells",
                ),
                (
                    "done\x1b[?2026l",
                    32,
                    true,
                    "the completed reopened episode releases the mixed frame",
                ),
            ],
        ),
        (
            Route::HeadlessSingleCapture,
            [
                (HOLD, 0, false, "held"),
                (
                    "complete\x1b[?2026l\x1b[?2026hpartial",
                    16,
                    false,
                    "capture must not mistake a dirty reopen for the completed close boundary",
                ),
                (
                    "done\x1b[?2026l",
                    32,
                    true,
                    "capture resumes with the fully completed reopened episode",
                ),
            ],
        ),
    ];
    for (route, steps) in rows {
        let (mut app, wid, terms) = match route {
            Route::PureSplit => {
                let (app, wid, _) = pure_fixture("rainbow kitty");
                let terms = visible_terminal_handles(&app, wid);
                assert_eq!(terms.len(), 2, "a two-pane split");
                (app, wid, terms)
            }
            Route::Mixed => {
                let (app, wid, session) = mixed_fixture("rainbow kitty");
                let term = app.pool.get(session).expect("terminal leaf").term.clone();
                (app, wid, vec![term])
            }
            Route::HeadlessSingleCapture => {
                let mut app = App::headless_for_test();
                app.config.cursor_trail = Some(true);
                app.config.cursor_trail_style = Some("rainbow kitty".into());
                app.config.trail_sounds = Some(false);
                let wid = WindowId(0);
                let term = app
                    .front_terminal(wid)
                    .expect("front terminal")
                    .term
                    .clone();
                (app, wid, vec![term])
            }
        };
        let t0 = Instant::now();
        for (bytes, ms, presents, why) in steps {
            for (index, term) in terms.iter().enumerate() {
                if !bytes.is_empty() {
                    term_lock(term).process(bytes.replace("{i}", &index.to_string()).as_bytes());
                }
            }
            let now = t0 + Duration::from_millis(ms);
            let presented = match route {
                Route::PureSplit => redraw_pure(&mut app, wid, now),
                Route::Mixed => app
                    .prepare_heterogeneous_input_scratch_with_cursor_fx(
                        wid,
                        Some(ComposedCursorFxClock::Advance(now)),
                    )
                    .is_some(),
                Route::HeadlessSingleCapture => app
                    .prepare_terminal_capture_grid_with_cursor_fx(
                        wid,
                        ComposedCursorFxClock::Advance(now),
                    )
                    .is_some(),
            };
            assert_eq!(presented, presents, "{route:?} +{ms} ms: {why}");
        }
    }
}

#[test]
fn live_composed_sync_staging_reuses_resident_allocations() {
    let t0 = Instant::now();

    let (mut pure, pure_wid, _) = pure_fixture("rainbow kitty");
    let pure_terms = visible_terminal_handles(&pure, pure_wid);
    assert!(redraw_pure(&mut pure, pure_wid, t0));
    let pure_identity = composed_scratch_identity(&pure, pure_wid);
    assert_eq!(pure_identity.0.1, 2);
    assert_eq!(pure_identity.1.1, 2);
    for frame in 1..=16 {
        term_lock(&pure_terms[frame % pure_terms.len()]).process(b"x");
        assert!(redraw_pure(
            &mut pure,
            pure_wid,
            t0 + Duration::from_millis(frame as u64 * 16),
        ));
        assert_eq!(
            composed_scratch_identity(&pure, pure_wid),
            pure_identity,
            "pure split frame {frame} must refill resident staging in place"
        );
    }
    term_lock(&pure_terms[0]).process(b"\x1b[?2026hpartial");
    assert!(!redraw_pure(
        &mut pure,
        pure_wid,
        t0 + Duration::from_millis(17 * 16),
    ));
    assert_eq!(
        composed_scratch_identity(&pure, pure_wid),
        pure_identity,
        "a held pure frame must restore the resident observation buffer"
    );

    let (mut mixed, mixed_wid, mixed_session) = mixed_fixture("rainbow kitty");
    let mixed_term = mixed
        .pool
        .get(mixed_session)
        .expect("terminal leaf")
        .term
        .clone();
    assert!(
        mixed
            .prepare_heterogeneous_input_scratch_with_cursor_fx(
                mixed_wid,
                Some(ComposedCursorFxClock::Advance(t0)),
            )
            .is_some()
    );
    let mixed_identity = composed_scratch_identity(&mixed, mixed_wid).0;
    assert_eq!(mixed_identity.1, 1);
    for frame in 1..=16 {
        term_lock(&mixed_term).process(b"x");
        assert!(
            mixed
                .prepare_heterogeneous_input_scratch_with_cursor_fx(
                    mixed_wid,
                    Some(ComposedCursorFxClock::Advance(
                        t0 + Duration::from_millis(frame as u64 * 16),
                    )),
                )
                .is_some()
        );
        assert_eq!(
            composed_scratch_identity(&mixed, mixed_wid).0,
            mixed_identity,
            "mixed frame {frame} must refill resident sync staging in place"
        );
    }
    term_lock(&mixed_term).process(b"\x1b[?2026hpartial");
    assert!(
        mixed
            .prepare_heterogeneous_input_scratch_with_cursor_fx(
                mixed_wid,
                Some(ComposedCursorFxClock::Advance(
                    t0 + Duration::from_millis(17 * 16),
                )),
            )
            .is_none()
    );
    assert_eq!(
        composed_scratch_identity(&mixed, mixed_wid).0,
        mixed_identity,
        "a held mixed frame must restore the resident observation buffer"
    );
}

#[test]
fn headless_split_capture_holds_advance_and_retain_without_ticking_effects() {
    let (mut app, wid, _) = pure_fixture("rainbow kitty");
    let terms = visible_terminal_handles(&app, wid);
    let t0 = Instant::now();
    let retained_at = t0 + Duration::from_millis(37);
    let next_trail_tick = t0 + Duration::from_secs(3);
    let last_trail_fire = t0 + Duration::from_millis(2);
    let predictor_deadline = {
        let window = app.windows.get_mut(&wid).expect("test window");
        window.next_trail_tick = Some(next_trail_tick);
        window.last_trail_fire = Some(last_trail_fire);
        window
            .predictor
            .set_mode(crate::predict::PredictMode::Always);
        assert!(window.predictor.predict_char('?', (0, 0), window.cols, t0));
        window
            .predictor
            .next_deadline()
            .expect("non-vacuous predictor deadline")
    };
    let before = app.windows[&wid].fx.cursor().glow.admission_tally();
    for term in &terms {
        term_lock(term).process(b"\x1b[?2026hpartial");
    }

    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Retain {
                observed_at: retained_at,
            },
        )
        .is_none(),
        "a retained recording capture cannot pair old effects with partial new cells"
    );
    assert_eq!(
        app.windows[&wid].sync_hold_until,
        Some(retained_at + SYNC_HOLD_CAP),
        "Retain feeds its caller-injected observation time to the DEC-2026 barrier"
    );
    {
        let window = &app.windows[&wid];
        assert_eq!(window.fx.cursor().glow.admission_tally(), before);
        assert_eq!(window.next_trail_tick, Some(next_trail_tick));
        assert_eq!(window.last_trail_fire, Some(last_trail_fire));
        assert_eq!(window.predictor.next_deadline(), Some(predictor_deadline));
    }
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Advance(retained_at + Duration::from_millis(1)),
        )
        .is_none(),
        "an advancing headless capture cannot expose partial synchronized cells"
    );
    let window = &app.windows[&wid];
    assert_eq!(window.fx.cursor().glow.admission_tally(), before);
    assert_eq!(window.next_trail_tick, Some(next_trail_tick));
    assert_eq!(window.last_trail_fire, Some(last_trail_fire));
    assert_eq!(window.predictor.next_deadline(), Some(predictor_deadline));

    for term in &terms {
        term_lock(term).process(b"\x1b[?2026l");
    }
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Advance(retained_at + Duration::from_millis(16)),
        )
        .is_some(),
        "capture resumes on the complete release frame"
    );
}

#[test]
fn capture_preparation_reports_exact_hold_unavailable_and_clean_reopen() {
    let (mut app, wid, _) = pure_fixture("rainbow kitty");
    let terms = visible_terminal_handles(&app, wid);
    let t0 = Instant::now();
    for term in &terms {
        term_lock(term).process(b"\x1b[?2026hpartial");
    }

    match app.prepare_terminal_capture_grid_with_cursor_fx_outcome(
        wid,
        ComposedCursorFxClock::Advance(t0),
    ) {
        CapturePreparation::Held { retry_at } => assert!(retry_at > t0),
        CapturePreparation::Ready(_) => panic!("partial sync frame was reported ready"),
        CapturePreparation::Unavailable => panic!("a real sync hold was lost"),
    }

    for term in &terms {
        term_lock(term).process(b"complete\x1b[?2026l\x1b[?2026h");
    }
    assert!(matches!(
        app.prepare_terminal_capture_grid_with_cursor_fx_outcome(
            wid,
            ComposedCursorFxClock::Advance(t0 + Duration::from_millis(16)),
        ),
        CapturePreparation::Ready(_)
    ));
    assert!(
        app.windows[&wid].sync_hold_until.is_some(),
        "a clean reopen remains armed, proving readiness was not inferred from deadline absence"
    );

    assert!(matches!(
        app.prepare_terminal_capture_grid_with_cursor_fx_outcome(
            WindowId(u64::MAX),
            ComposedCursorFxClock::Advance(t0),
        ),
        CapturePreparation::Unavailable
    ));

    let (mut mixed, mixed_wid, session) = mixed_fixture("rainbow kitty");
    let mixed_term = mixed
        .pool
        .get(session)
        .expect("mixed terminal")
        .term
        .clone();
    term_lock(&mixed_term).process(b"\x1b[?2026hpartial");
    assert!(matches!(
        mixed.prepare_heterogeneous_input_scratch_with_cursor_fx_outcome(
            mixed_wid,
            Some(ComposedCursorFxClock::Advance(t0)),
        ),
        CapturePreparation::Held { .. }
    ));
    assert!(matches!(
        mixed.prepare_heterogeneous_input_scratch_with_cursor_fx_outcome(
            WindowId(u64::MAX),
            Some(ComposedCursorFxClock::Advance(t0)),
        ),
        CapturePreparation::Unavailable
    ));
}

#[test]
fn headless_single_capture_never_relocks_after_its_authorized_snapshot() {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow kitty".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    let term = app
        .front_terminal(wid)
        .expect("front terminal")
        .term
        .clone();
    let t0 = Instant::now();

    set_coherence_state(&term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    let mutate = term.clone();
    let captured = app
        .prepare_terminal_capture_grid_with_cursor_fx_interleaved(
            wid,
            ComposedCursorFxClock::Advance(t0),
            move || {
                term_lock(&mutate).process(b"\x1b[?2026h");
                set_coherence_state(&mutate, 'B', (9, 15), "CC2244", "\x1b[3 q");
            },
        )
        .expect("the pre-sync A snapshot is complete");
    assert!(!captured.composed);
    let captured_seq = captured.leaves[0].snapshot_seq;
    let captured_observation = coherence_observation(&app, wid);
    assert_eq!(captured_observation.marker, 'A');
    assert_eq!(
        captured_observation.cursor,
        (1, 3, CursorStyle::BlinkingBar),
        "cursor effects and cells must both stay on the authorized A sample"
    );

    let pending_epoch = term_lock(&term).damage_epoch();
    assert!(
        pending_epoch > captured_seq,
        "the post-extraction partial write remains pending instead of being consumed"
    );
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Advance(t0 + Duration::from_millis(16)),
        )
        .is_none(),
        "the next capture holds rather than exposing B's partial synchronized frame"
    );

    term_lock(&term).process(b"\x1b[?2026l");
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            ComposedCursorFxClock::Advance(t0 + Duration::from_millis(32)),
        )
        .is_some(),
        "capture resumes after the bracket closes"
    );
    assert_eq!(coherence_observation(&app, wid).marker, 'B');
}

fn primed_pure_live_fixture(
    t0: Instant,
) -> (App, WindowId, std::sync::Arc<std::sync::Mutex<Terminal>>) {
    let (mut app, wid, term) = pure_fixture("fire");
    set_coherence_state(&term, 'P', (1, 2), "203040", "\x1b[2 q");
    assert!(redraw_pure(&mut app, wid, t0));
    (app, wid, term)
}

fn primed_pure_capture_fixture(
    t0: Instant,
) -> (App, WindowId, std::sync::Arc<std::sync::Mutex<Terminal>>) {
    let (mut app, wid, term) = pure_fixture("fire");
    set_coherence_state(&term, 'P', (1, 2), "203040", "\x1b[2 q");
    assert!(
        app.prepare_terminal_capture_grid_with_cursor_fx(wid, ComposedCursorFxClock::Advance(t0),)
            .is_some()
    );
    (app, wid, term)
}

#[test]
fn extracted_mixed_live_and_split_capture_forward_and_drain_licensed_cat_pulses() {
    let t0 = Instant::now();

    // Heterogeneous composition is the extracted live route: native cells
    // and the focused terminal sample are committed together.
    let (mut mixed, mixed_wid, mixed_session) = mixed_fixture("rainbow kitty");
    let mixed_term = mixed
        .pool
        .get(mixed_session)
        .expect("mixed terminal")
        .term
        .clone();
    assert!(
        mixed
            .prepare_heterogeneous_input_scratch_with_cursor_fx(
                mixed_wid,
                Some(ComposedCursorFxClock::Advance(t0)),
            )
            .is_some()
    );
    type_one_licensed_echo(
        &mut mixed,
        mixed_wid,
        &mixed_term,
        t0 + Duration::from_millis(1),
    );
    let mixed_tick = t0 + Duration::from_millis(16);
    assert!(
        mixed
            .prepare_heterogeneous_input_scratch_with_cursor_fx(
                mixed_wid,
                Some(ComposedCursorFxClock::Advance(mixed_tick)),
            )
            .is_some()
    );
    let mixed_route = {
        let window = mixed.windows.get_mut(&mixed_wid).expect("mixed window");
        let classified = window.fx.cursor_mut().glow.admission_tally().licensed > 0;
        let before_replay = window.fx.companions_mut().cursor_cat.momentum(mixed_tick);
        let delivered = before_replay > 0.0;
        assert!(
            delivered,
            "the licensed mixed-live pulse reaches the cat in its own frame"
        );
        let replay_pulse = window.fx.cursor_mut().glow.take_cursor_cat_motion_pulse();
        let pending = replay_pulse.is_some();
        forward_kitty_cursor_motion(
            true,
            false,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            replay_pulse,
            &mut window.fx.companions_mut().cursor_cat,
        );
        let replay_changed = window
            .fx
            .companions_mut()
            .cursor_cat
            .momentum(mixed_tick)
            .to_bits()
            != before_replay.to_bits();
        assert!(
            !pending,
            "the mixed-live route leaves no pulse for a later route"
        );
        (classified, pending, delivered, replay_changed)
    };
    bind_extracted_cat_pulse_route(
        "mixed live",
        mixed_route.0,
        mixed_route.1,
        mixed_route.2,
        mixed_route.3,
    );

    // Pure terminal splits take the same extracted seam for headless image
    // capture.
    let (mut split, split_wid, split_term) = pure_fixture("rainbow kitty");
    assert!(
        split
            .prepare_terminal_capture_grid_with_cursor_fx(
                split_wid,
                ComposedCursorFxClock::Advance(t0),
            )
            .is_some()
    );
    type_one_licensed_echo(
        &mut split,
        split_wid,
        &split_term,
        t0 + Duration::from_millis(1),
    );
    let split_tick = t0 + Duration::from_millis(16);
    assert!(
        split
            .prepare_terminal_capture_grid_with_cursor_fx(
                split_wid,
                ComposedCursorFxClock::Advance(split_tick),
            )
            .is_some()
    );
    let split_route = {
        let window = split.windows.get_mut(&split_wid).expect("split window");
        let classified = window.fx.cursor_mut().glow.admission_tally().licensed > 0;
        let before_replay = window.fx.companions_mut().cursor_cat.momentum(split_tick);
        let delivered = before_replay > 0.0;
        assert!(
            delivered,
            "the licensed split-capture pulse reaches the cat in its own frame"
        );
        let replay_pulse = window.fx.cursor_mut().glow.take_cursor_cat_motion_pulse();
        let pending = replay_pulse.is_some();
        forward_kitty_cursor_motion(
            true,
            false,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            replay_pulse,
            &mut window.fx.companions_mut().cursor_cat,
        );
        let replay_changed = window
            .fx
            .companions_mut()
            .cursor_cat
            .momentum(split_tick)
            .to_bits()
            != before_replay.to_bits();
        assert!(
            !pending,
            "the split-capture route leaves no pulse for a later route"
        );
        (classified, pending, delivered, replay_changed)
    };
    bind_extracted_cat_pulse_route(
        "split capture",
        split_route.0,
        split_route.1,
        split_route.2,
        split_route.3,
    );
}

#[test]
fn extracted_composed_style_gate_consumes_pulse_before_route_change() {
    let (mut app, wid, session) = mixed_fixture("fire");
    let term = app.pool.get(session).expect("terminal leaf").term.clone();
    let t0 = Instant::now();
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(t0)),
        )
        .is_some()
    );
    type_one_licensed_echo(&mut app, wid, &term, t0 + Duration::from_millis(1));
    let fire_tick = t0 + Duration::from_millis(16);
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(fire_tick)),
        )
        .is_some()
    );
    {
        let window = app.windows.get_mut(&wid).expect("test window");
        assert!(window.fx.cursor_mut().glow.admission_tally().licensed > 0);
        assert_eq!(
            window.fx.companions_mut().cursor_cat.momentum(fire_tick),
            0.0,
            "a non-kitty owner must not forward the licensed pulse"
        );
        assert!(
            window
                .fx
                .cursor_mut()
                .glow
                .take_cursor_cat_motion_pulse()
                .is_none(),
            "gating forwarding off still consumes the one-frame pulse"
        );
    }

    app.config.cursor_trail_style = Some("rainbow kitty".into());
    let switched = fire_tick + Duration::from_millis(16);
    assert!(
        app.prepare_heterogeneous_input_scratch_with_cursor_fx(
            wid,
            Some(ComposedCursorFxClock::Advance(switched)),
        )
        .is_some()
    );
    assert_eq!(
        app.windows
            .get(&wid)
            .expect("test window")
            .fx
            .companions()
            .cursor_cat
            .momentum(switched),
        0.0,
        "switching into the kitty route cannot replay the fire frame's pulse"
    );
}

fn coherence_observation(app: &App, wid: WindowId) -> CoherenceObservation {
    let leaf = app
        .active_visible_leaf_plan(wid)
        .expect("mixed plan")
        .leaves
        .into_iter()
        .find(|leaf| leaf.focused)
        .expect("focused terminal leaf");
    let row = leaf.rect.origin.y.round().max(0.0) as usize;
    let col = leaf.rect.origin.x.round().max(0.0) as usize;
    let window = app.windows.get(&wid).expect("test window");
    CoherenceObservation {
        marker: window.input_scratch.cells[row][col].ch,
        cursor: (
            window.input_scratch.cursor_row,
            window.input_scratch.cursor_col,
            window.input_scratch.cursor_style,
        ),
        cursor_effect_style: window.input_scratch.cursor_effect_style_override,
        cursor_color: window.input_scratch.cursor_color,
        cursor_fill: window.input_scratch.cursor_fill_override,
        glow: window.fx.cursor().glow_scratch.clone(),
        trail: window.fx.cursor().trail_scratch.clone(),
    }
}

#[test]
fn mixed_advance_uses_the_cell_extractions_exact_terminal_sample() {
    let t0 = Instant::now();
    let tick = t0 + Duration::from_millis(16);

    let (mut a_control, wid, a_term) = primed_coherence_fixture(t0);
    set_coherence_state(&a_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    assert!(
        a_control
            .prepare_heterogeneous_input_scratch_with_cursor_fx(
                wid,
                Some(ComposedCursorFxClock::Advance(tick)),
            )
            .is_some()
    );
    let a = coherence_observation(&a_control, wid);
    assert_eq!(a.marker, 'A');
    assert!(
        !a.glow.is_empty() || a.cursor_fill.is_some(),
        "the composed route must preserve the focused pane's complete cursor body"
    );

    let (mut interleaved, wid, interleaved_term) = primed_coherence_fixture(t0);
    set_coherence_state(&interleaved_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    let mutate_to_b = std::sync::Arc::clone(&interleaved_term);
    assert!(
        interleaved
            .prepare_heterogeneous_input_scratch_with_cursor_fx_interleaved(
                wid,
                Some(ComposedCursorFxClock::Advance(tick)),
                move || {
                    set_coherence_state(&mutate_to_b, 'B', (9, 15), "CC2244", "\x1b[3 q");
                },
            )
            .is_some()
    );
    let interleaved_observation = coherence_observation(&interleaved, wid);
    assert_eq!(
        interleaved_observation, a,
        "a PTY write after extraction cannot mix B cursor/effects over A cells"
    );
    assert_eq!(
        {
            let terminal = term_lock(&interleaved_term);
            let cursor = terminal.cursor();
            (cursor.row, cursor.col, terminal.cursor_style())
        },
        (9, 15, CursorStyle::BlinkingUnderline),
        "the deterministic hook really advanced the live terminal to B"
    );

    // The two-lock choreography: extract A, mutate to B, then invoke the
    // public pure-split fallback. The generation fence keeps both unowned
    // effect samples dark.
    let (mut torn_control, wid, torn_term) = primed_coherence_fixture(t0);
    set_coherence_state(&torn_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    assert!(
        torn_control
            .prepare_heterogeneous_input_scratch(wid)
            .is_some()
    );
    set_coherence_state(&torn_term, 'B', (9, 15), "CC2244", "\x1b[3 q");
    assert!(torn_control.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(tick),));
    let torn = coherence_observation(&torn_control, wid);
    assert_eq!(
        (torn.marker, torn.cursor, torn.cursor_color),
        (a.marker, a.cursor, a.cursor_color),
        "the negative control retains A's extracted cell/cursor metadata"
    );
    assert_ne!(
        (torn.cursor_fill, &torn.glow, &torn.trail),
        (a.cursor_fill, &a.glow, &a.trail),
        "negative control: the retired second lock produces a torn cursor plane over A cells"
    );
}

#[test]
fn pure_live_redraw_keeps_effects_and_cells_on_one_focused_sample() {
    let t0 = Instant::now();
    let tick = t0 + Duration::from_millis(16);

    let (mut a_control, wid, a_term) = primed_pure_live_fixture(t0);
    set_coherence_state(&a_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    assert!(redraw_pure(&mut a_control, wid, tick));
    let a = coherence_observation(&a_control, wid);
    assert_eq!(a.marker, 'A');
    assert!(
        !a.glow.is_empty() || a.cursor_fill.is_some(),
        "the pure compositor must preserve the focused pane's complete cursor body"
    );

    let (mut interleaved, wid, interleaved_term) = primed_pure_live_fixture(t0);
    set_coherence_state(&interleaved_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    let mutate_to_b = std::sync::Arc::clone(&interleaved_term);
    let window = interleaved.windows.get(&wid).expect("test window");
    let (rows, cols) = (usize::from(window.rows), usize::from(window.cols));
    assert!(
        interleaved
            .redraw_compose_interleaved(wid, rows, cols, false, false, None, 0, tick, move || {
                set_coherence_state(&mutate_to_b, 'B', (9, 15), "CC2244", "\x1b[3 q");
            },)
            .is_some()
    );
    assert_eq!(
        coherence_observation(&interleaved, wid),
        a,
        "a PTY write after focused extraction cannot mix B cells over A effects"
    );

    // Two-lock negative control: tick the dark unowned A sample, mutate to B,
    // then extract B's cells and retain the dark effect frame.
    let (mut torn_control, wid, torn_term) = primed_pure_live_fixture(t0);
    set_coherence_state(&torn_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    assert!(torn_control.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(tick),));
    set_coherence_state(&torn_term, 'B', (9, 15), "CC2244", "\x1b[3 q");
    assert!(torn_control.prepare_terminal_capture_grid(wid).is_some());
    assert!(
        torn_control
            .composed_cursor_frame(wid, ComposedCursorFxClock::Retain { observed_at: tick },)
    );
    let torn = coherence_observation(&torn_control, wid);
    assert_eq!(
        (torn.glow, torn.trail),
        (a.glow, a.trail),
        "the negative control retains A's effect tick"
    );
    assert_ne!(
        (torn.marker, torn.cursor, torn.cursor_color),
        (a.marker, a.cursor, a.cursor_color),
        "but the later extraction observably contributes B's cells and cursor"
    );
}

#[test]
fn headless_composed_capture_keeps_effects_and_cells_on_one_focused_sample() {
    let t0 = Instant::now();
    let tick = t0 + Duration::from_millis(16);

    let (mut a_control, wid, a_term) = primed_pure_capture_fixture(t0);
    set_coherence_state(&a_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    assert!(
        a_control
            .prepare_terminal_capture_grid_with_cursor_fx(
                wid,
                ComposedCursorFxClock::Advance(tick),
            )
            .is_some()
    );
    let a = coherence_observation(&a_control, wid);
    assert_eq!(a.marker, 'A');
    assert!(
        !a.glow.is_empty() || a.cursor_fill.is_some(),
        "headless composed capture must preserve the focused pane's complete cursor body"
    );

    let (mut interleaved, wid, interleaved_term) = primed_pure_capture_fixture(t0);
    set_coherence_state(&interleaved_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    let mutate_to_b = std::sync::Arc::clone(&interleaved_term);
    assert!(
        interleaved
            .prepare_terminal_capture_grid_with_cursor_fx_interleaved(
                wid,
                ComposedCursorFxClock::Advance(tick),
                move || {
                    set_coherence_state(&mutate_to_b, 'B', (9, 15), "CC2244", "\x1b[3 q");
                },
            )
            .is_some()
    );
    assert_eq!(
        coherence_observation(&interleaved, wid),
        a,
        "a PTY write after capture extraction cannot mix B effects over A cells"
    );

    // Headless two-lock choreography: extract A, mutate to B, then take a
    // second terminal lock to tick effects from B.
    let (mut torn_control, wid, torn_term) = primed_pure_capture_fixture(t0);
    set_coherence_state(&torn_term, 'A', (1, 3), "11AA33", "\x1b[5 q");
    assert!(torn_control.prepare_terminal_capture_grid(wid).is_some());
    set_coherence_state(&torn_term, 'B', (9, 15), "CC2244", "\x1b[3 q");
    assert!(torn_control.composed_cursor_frame(wid, ComposedCursorFxClock::Advance(tick),));
    let torn = coherence_observation(&torn_control, wid);
    assert_eq!(
        (torn.marker, torn.cursor, torn.cursor_color),
        (a.marker, a.cursor, a.cursor_color),
        "the negative control retains A's extracted cells and cursor metadata"
    );
    assert_ne!(
        (torn.cursor_fill, &torn.glow, &torn.trail),
        (a.cursor_fill, &a.glow, &a.trail),
        "negative control: the retired headless second lock tears effects from later cells"
    );
}

/// A pane's snapshot retires with its route. `unfocused_pane_scratch` is
/// pruned only inside the composed route and keyed by pane index, so no
/// teardown can address it; a window that unsplit or switched to a
/// single-pane, native or mixed tab would otherwise keep every background
/// pane's snapshot, including sole ownership of inline image payloads.
#[test]
fn only_the_composed_route_keeps_the_unfocused_pane_snapshots() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let view = app
        .view_store
        .insert_terminal(0)
        .expect("a view identity for the Native arm");
    let park = |app: &mut App| {
        app.windows
            .get_mut(&wid)
            .expect("test window")
            .unfocused_pane_scratch
            .entry(0)
            .or_default();
    };
    // Control: the composed route keeps them (it prunes to the live pane
    // set), so a fix that cleared unconditionally fails here.
    park(&mut app);
    App::retire_unfocused_pane_snapshots(
        app.windows.get_mut(&wid).expect("test window"),
        crate::VisibleContentRoute::Terminal { composed: true },
    );
    assert!(
        !app.windows[&wid].unfocused_pane_scratch.is_empty(),
        "the composed route prunes its own map and must keep it here"
    );
    // Every other route retires them: none shows an unfocused terminal pane
    // this frame and none prunes.
    for route in [
        crate::VisibleContentRoute::Terminal { composed: false },
        crate::VisibleContentRoute::Heterogeneous,
        crate::VisibleContentRoute::Native {
            instance: crate::tab_model::AppInstanceId::from_stored(7),
            view,
        },
    ] {
        park(&mut app);
        assert!(!app.windows[&wid].unfocused_pane_scratch.is_empty());
        App::retire_unfocused_pane_snapshots(
            app.windows.get_mut(&wid).expect("test window"),
            route,
        );
        assert!(
            app.windows[&wid].unfocused_pane_scratch.is_empty(),
            "{route:?} kept a background pane's whole snapshot"
        );
    }
}

#[test]
fn duplicate_session_views_keep_focus_and_snapshot_provenance_by_leaf_slot() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("test window").focused = true;

    let original_view = app.windows[&wid]
        .tab_set
        .active()
        .expect("initial tab")
        .focus;
    let mirror_view = app
        .view_store
        .insert_terminal(0)
        .expect("second view identity");
    app.pool.attach(0);
    {
        let ws = app.windows.get_mut(&wid).expect("test window");
        assert!(
            ws.tab_set
                .active_mut()
                .expect("active tab")
                .split_focused(crate::tab_model::SplitAxis::Horizontal, mirror_view)
        );
        let active = ws.tabs.active;
        assert!(ws.layouts[active].split_focused(crate::pane::SplitDir::Vertical, 0));
    }
    app.sync_window(wid);

    let plan = app.active_visible_leaf_plan(wid).expect("canonical plan");
    assert_eq!(
        plan.leaves
            .iter()
            .map(|leaf| (leaf.view, leaf.focused))
            .collect::<Vec<_>>(),
        [(original_view, false), (mirror_view, true)],
        "canonical traversal keeps the original leaf first and focuses only the mirror"
    );
    let rects = app
        .active_tree(wid)
        .expect("terminal projection")
        .compute_layout(24, 80);
    assert_eq!(
        rects.iter().map(|rect| rect.session).collect::<Vec<_>>(),
        [0, 0],
        "the compatibility projection preserves canonical leaf order even when ids alias"
    );

    let term = app.pool.get(0).expect("shared terminal").term.clone();
    term_lock(&term).process(b"A");
    let before = term_lock(&term).content_seq();
    let advance = std::sync::Arc::clone(&term);
    assert!(
        app.redraw_compose_before_focused_extract(
            wid,
            24,
            80,
            false,
            false,
            None,
            0,
            Instant::now(),
            move || term_lock(&advance).process(b"B"),
        )
        .is_some()
    );
    let after = term_lock(&term).content_seq();
    assert!(after > before, "the interleave advanced terminal content");

    let ws = app.windows.get(&wid).expect("test window");
    assert_eq!(
        ws.composed_retain.focus, 1,
        "only the canonical mirror is focused"
    );
    assert_eq!(
        ws.unfocused_pane_scratch
            .get(&0)
            .expect("original-view snapshot")
            .content_seq,
        before,
        "the background view retains the token coherent with its earlier cells"
    );
    assert_eq!(
        ws.composed_focus_scratch.content_seq, after,
        "the focused mirror retains the token coherent with its later cells"
    );
    assert_eq!(
        ws.capture_leaf_content_seqs,
        [before, after],
        "presented capture metadata preserves both exact per-view snapshots"
    );
}
