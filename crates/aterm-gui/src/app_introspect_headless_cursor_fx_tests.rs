// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CaptureCompanionSongSync, image_req_for_test, sync_capture_companion_song};
use crate::{App, WindowId, control_auth, term_lock};
use std::time::{Duration, Instant};

fn pet_app(motion: &str, split: bool) -> (App, WindowId) {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    app.config.motion = Some(motion.into());
    app.recompute_sparkle();
    app.sparkle = None;
    if split {
        app.split_active_stub_tab(wid);
    }
    app.windows.get_mut(&wid).unwrap().focused = true;
    (app, wid)
}

fn render_front_image(app: &mut App) {
    let (tx, rx) = std::sync::mpsc::channel();
    let target = control_auth::ConfinedImage::in_memory();
    app.render_image(image_req_for_test(target, false, true, None, tx));
    rx.recv_timeout(crate::app_introspect::WORKER_PATIENCE)
        .expect("image worker reply")
        .expect("headless image succeeds");
}

#[test]
fn capture_owned_clock_syncs_and_retires_the_visual_singer_without_audio() {
    use aterm_effects::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
    use aterm_effects::kitty_sing::SING_ARM_REPEATS;

    let now = Instant::now();
    let start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
    let mut detector = aterm_effects::kitty_sing::KittySing::default();
    for i in 0..SING_ARM_REPEATS {
        detector.note_char(start + Duration::from_millis(u64::from(i) * 10), 0, 'a');
    }
    let mut cat = crate::kitty_cursor::CursorCat::default();
    let tenure_start = now - Duration::from_secs(7);
    for i in 0..160u64 {
        cat.on_pet_mode_motion_pulse(CursorCatMotionPulse {
            at: tenure_start + Duration::from_millis(i * 40),
            kind: CursorCatMotionKind::Advance,
        });
    }
    assert!(!cat.is_active(), "pet tenure stays hidden before the song");
    let mut glow = crate::cursor_glow::CursorGlow::default();
    let mut riff_bar = None;
    assert_eq!(
        sync_capture_companion_song(CaptureCompanionSongSync {
            style: crate::cursor_glow::GlowStyle::RainbowKitty,
            cursor_trail_enabled: true,
            cursor_companions_allowed: true,
            pet_mode: true,
            pet_flies: false,
            now,
            detector: &mut detector,
            cat: &mut cat,
            glow: &mut glow,
            riff_bar: &mut riff_bar,
        }),
        1.0
    );
    assert!(
        cat.is_active(),
        "the capture-owned song spends hidden tenure"
    );
    assert_eq!(cat.static_frame(now).sing, 1.0);
    assert!(riff_bar.is_some(), "the silent visual bar still advances");

    let drained = now + Duration::from_secs(2);
    assert_eq!(
        sync_capture_companion_song(CaptureCompanionSongSync {
            style: crate::cursor_glow::GlowStyle::RainbowKitty,
            cursor_trail_enabled: true,
            cursor_companions_allowed: true,
            pet_mode: true,
            pet_flies: false,
            now: drained,
            detector: &mut detector,
            cat: &mut cat,
            glow: &mut glow,
            riff_bar: &mut riff_bar,
        }),
        0.0
    );
    assert!(
        !cat.is_active(),
        "capture-owned pet mode grounds a fully drained singer"
    );
    assert_eq!(riff_bar, None);
}

#[test]
fn single_and_split_capture_retain_the_pet_watching_selected_history() {
    use aterm_core::selection::{SelectionSide, SelectionType};

    for split in [false, true] {
        let (mut app, wid) = pet_app("reduced", split);
        let start = Instant::now();
        let sid = app.focused_session_id(wid).unwrap();
        let term = app.pool.get(sid).unwrap().term.clone();
        let plan = app.active_visible_leaf_plan(wid).unwrap();
        let leaf = plan.leaves.iter().find(|leaf| leaf.focused).unwrap();
        let rows = leaf.rect.size.height.round() as usize;
        {
            let mut terminal = term_lock(&term);
            for _ in 0..rows + 20 {
                terminal.process(b"line\r\n");
            }
        }
        app.capture_words(wid, start);
        assert!(app.windows[&wid].fx.companion().brain().is_active());
        term_lock(&term).scroll_display(10);

        for (step, selected) in [true, false].into_iter().enumerate() {
            {
                let mut terminal = term_lock(&term);
                let selection = terminal.text_selection_mut();
                if selected {
                    selection.start_selection(-4, 10, SelectionSide::Left, SelectionType::Simple);
                    selection.update_selection(-3, 16, SelectionSide::Right);
                } else {
                    selection.clear();
                }
            }
            let now = start + Duration::from_millis((step as u64 + 1) * 16);
            let capture = app
                .prepare_terminal_capture_grid_with_cursor_fx(
                    wid,
                    crate::app_render::ComposedCursorFxClock::Advance(now),
                )
                .expect("real terminal capture extraction");
            let focus = capture.focus.unwrap();
            assert_eq!(capture.composed, split);
            assert!(!focus.live_viewport && focus.cursor.is_none());
            app.capture_words_sampled(wid, now, focus);
            assert_eq!(
                app.windows[&wid]
                    .fx
                    .companion()
                    .brain()
                    .has_reading_interest(),
                selected
            );
            let (cw, ch) = app.win_cell_size(wid);
            let sprites =
                &aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers);
            if selected {
                assert_eq!(sprites.len(), 1, "one full reading pet, split={split}");
                assert!(usize::from(sprites[0].w) >= 5 * cw);
                assert!(usize::from(sprites[0].h) >= ch);
                assert_eq!(sprites[0].alpha, 255);
            } else {
                assert!(
                    sprites.is_empty(),
                    "history without reading interest remains nonpresentable"
                );
            }
        }
    }
}

#[test]
fn headless_capture_retains_hidden_caret_home_and_clears_real_reset() {
    use aterm_core::terminal::{ContentScrollDelta, ContentScrollState};

    for split in [false, true] {
        let (mut app, wid) = pet_app("reduced", split);
        let start = Instant::now();
        let sid = app.focused_session_id(wid).unwrap();
        let term = app.pool.get(sid).unwrap().term.clone();
        let rows = {
            let mut terminal = term_lock(&term);
            assert!(terminal.grid().rows() >= 8 && terminal.grid().cols() >= 24);
            terminal.process(b"\x1b[6;12H");
            terminal.grid().rows()
        };
        for ago in (1..=6).rev() {
            app.capture_words(wid, start - Duration::from_millis(ago * 100));
        }
        app.capture_words(wid, start);
        let (cw, ch) = app.win_cell_size(wid);
        let full_body = |app: &App| {
            let ws = &app.windows[&wid];
            let bodies: Vec<_> = aterm_render::sprites::published(&ws.input_scratch.fx_layers)
                .into_iter()
                .filter(|s| usize::from(s.w) >= 5 * cw && usize::from(s.h) >= ch)
                .collect();
            assert_eq!(bodies.len(), 1, "one resident, split={split}");
            let body = &bodies[0];
            assert_eq!(body.alpha, 255, "split={split}");
            let (ox, oy, _, _, _) =
                app.effects_origin_win(wid, usize::from(ws.rows), usize::from(ws.cols), ch);
            let x = body.x + i32::from(ox);
            let y = body.y + i32::from(oy);
            assert_eq!(
                ws.fx.companion().hit_rect(),
                Some((x, x + i32::from(body.w), y, y + i32::from(body.h))),
                "the published body must identify the actual emitted sprite"
            );
            (body.x, body.y, body.w, body.h)
        };
        let home = full_body(&app);
        let feet = |(x, y, w, h): (i32, i32, u16, u16)| (x + i32::from(w) / 2, y + i32::from(h));
        let input_seq = app.windows[&wid].fx.companion().brain().console_input_seq();
        term_lock(&term).process(b"\x1b[?25l");
        for (step, elapsed_ms) in [16, 600, 2000].into_iter().enumerate() {
            if step == 1 {
                let mut terminal = term_lock(&term);
                let before = terminal.content_scroll_state();
                terminal.process(
                    format!("\x1b7\x1b[1;{}r\x1b[{};1H\n\x1b[r\x1b8", rows - 2, rows - 2)
                        .as_bytes(),
                );
                assert!(matches!(
                    ContentScrollState::delta_since(Some(before), terminal.content_scroll_state()),
                    ContentScrollDelta::Bands { .. }
                ));
            }
            app.capture_words(wid, start + Duration::from_millis(elapsed_ms));
            assert_eq!(
                feet(full_body(&app)),
                feet(home),
                "a hidden home remains screen-local"
            );
            assert!(!app.windows[&wid].fx.companion().brain().needs_frames());
            assert_eq!(
                app.windows[&wid].fx.companion().brain().console_input_seq(),
                input_seq
            );
        }
        let captured_at = start + Duration::from_millis(2016);
        let capture = app
            .prepare_terminal_capture_grid_with_cursor_fx_interleaved(
                wid,
                crate::app_render::ComposedCursorFxClock::Advance(captured_at),
                || term_lock(&term).process(b"\x1bc\x1b[?25l"),
            )
            .expect("the already-authorized snapshot survives a later reset");
        let focus = capture.focus.unwrap();
        assert_ne!(
            focus.pet_world.stamp.surface.scroll_invalidation,
            term_lock(&term).content_scroll_state().invalidation_epoch,
            "the control must change the real terminal after extraction"
        );
        app.capture_words_sampled(wid, captured_at, focus);
        assert_eq!(
            feet(full_body(&app)),
            feet(home),
            "capture must consume only its owned snapshot"
        );

        for step in 0..20 {
            app.capture_words(wid, start + Duration::from_millis(2100 + step * 100));
        }
        assert_ne!(
            feet(full_body(&app)),
            feet(home),
            "reset must clear the former home, split={split}"
        );
        assert_eq!(
            app.windows[&wid].fx.companion().brain().console_input_seq(),
            input_seq
        );

        term_lock(&term).process(b"\x1b[?25h\x1b[6;12H");
        for (control, owner_removed) in [false, true].into_iter().enumerate() {
            app.windows.get_mut(&wid).unwrap().focused = true;
            app.config.cursor_trail = Some(true);
            let now = start + Duration::from_millis(5000 + control as u64 * 1000);
            for ago in (0..=6).rev() {
                app.capture_words(wid, now - Duration::from_millis(ago * 100));
            }
            full_body(&app);
            if owner_removed {
                app.config.cursor_trail = Some(false);
            } else {
                app.windows.get_mut(&wid).unwrap().focused = false;
            }
            app.capture_words(wid, now + Duration::from_millis(16));
            assert!(
                aterm_render::sprites::published(&app.windows[&wid].input_scratch.fx_layers)
                    .is_empty()
            );
            assert_eq!(app.windows[&wid].fx.companion().hit_rect(), None);
        }
    }
}

#[test]
fn native_and_capture_keep_the_full_pet_under_load_without_idle_frames() {
    for split in [false, true] {
        let (mut app, wid) = pet_app("auto", split);
        app.config.load_adaptive_motion = Some(true);
        let now = Instant::now();
        for frame in 0..3 {
            app.note_present_cost(
                30_000_000,
                Duration::from_millis(16),
                now - Duration::from_secs(2) + Duration::from_millis(frame * 16),
            );
        }
        assert!(
            app.load_shed_active(),
            "real present-cost samples engage the policy"
        );
        assert_eq!(
            app.effective_shed_envelope(app.config.motion_mode(), now),
            0.0
        );
        if split {
            let ws = &app.windows[&wid];
            let (rows, cols) = (usize::from(ws.rows), usize::from(ws.cols));
            let mut at = now;
            for _ in 0..60 {
                assert!(
                    app.redraw_compose(wid, rows, cols, false, false, None, 0, at)
                        .is_some()
                );
                if app.windows[&wid].fx.companion().alpha() == 255 {
                    break;
                }
                at += aterm_effects::companion::RESIDENT_SHED_CADENCE;
            }
            let ws = &app.windows[&wid];
            assert!(
                ws.fx.companion().hit_rect().is_some() && ws.fx.companion().alpha() == 255,
                "native split retains the full body's hit target"
            );
            assert!(ws.fx.companion().brain().is_active());
            assert!(
                !ws.fx.companion().needs_frames(),
                "the resident under load owes no frame train"
            );
        }
        assert!(
            app.load_shed_active(),
            "capture must exercise the load-suspended branch"
        );
        render_front_image(&mut app);
        assert!(
            app.load_shed_active(),
            "the capture did not escape the pressure fixture"
        );
        let (cw, ch) = app.win_cell_size(wid);
        let ws = &app.windows[&wid];
        let bodies: Vec<_> = aterm_render::sprites::published(&ws.input_scratch.fx_layers)
            .into_iter()
            .filter(|sprite| usize::from(sprite.w) >= 5 * cw && usize::from(sprite.h) >= ch)
            .collect();
        assert_eq!(
            bodies.len(),
            1,
            "capture retains one full pet under load, split={split}"
        );
        assert_eq!(bodies[0].alpha, 255);
        assert_eq!(
            aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
            1,
            "no head or animated attachment under load"
        );
        assert!(
            ws.input_scratch.word_decorations.is_empty()
                && ws.input_scratch.ink.is_empty()
                && ws
                    .input_scratch
                    .nova_add
                    .iter()
                    .all(|q| ws.pane_overlay.painted_quad(q)),
            "decorations remain shed"
        );

        assert!(ws.fx.companion().brain().is_active());
        assert!(
            !ws.fx.companion().needs_frames(),
            "capture shares the shed cadence: no frame train"
        );
    }
}

#[test]
fn production_headless_image_keeps_one_full_pet_during_reduced_motion_singing() {
    use aterm_effects::cursor_glow::{CursorCatMotionKind, CursorCatMotionPulse};
    use aterm_effects::kitty_sing::SING_ARM_REPEATS;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.motion = Some("reduced".into());
    app.recompute_sparkle();
    app.sparkle = None;
    let now = Instant::now() + Duration::from_secs(60);
    {
        let ws = app.windows.get_mut(&wid).expect("headless window 0");
        ws.focused = true;
        let tenure_start = now - Duration::from_secs(7);
        for i in 0..160u64 {
            ws.fx
                .companions_mut()
                .cursor_cat
                .on_pet_mode_motion_pulse(CursorCatMotionPulse {
                    at: tenure_start + Duration::from_millis(i * 40),
                    kind: CursorCatMotionKind::Advance,
                });
        }
        let sing_start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
        for i in 0..SING_ARM_REPEATS {
            ws.fx.companions_mut().kitty_sing.note_char(
                sing_start + Duration::from_millis(u64::from(i) * 10),
                0,
                'a',
            );
        }
    }

    render_front_image(&mut app);

    let (cell_w, cell_h) = app.win_cell_size(wid);
    let ws = app.windows.get_mut(&wid).expect("headless window 0");
    assert!(
        ws.fx.companions_mut().cursor_cat.is_active(),
        "the capture-owned detector sync spends the authenticated hidden tenure"
    );
    assert!(
        ws.fx.companion_mut().brain().is_active(),
        "reduced motion keeps the full resident visible and caret-fed during the song"
    );
    assert_eq!(
        aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
        1,
        "the production still contains one full resident, never a replacement or second head"
    );
    assert!(
        aterm_render::sprites::published(&ws.input_scratch.fx_layers)
            .iter()
            .all(|s| !s.art.is_empty())
    );
    let body = &aterm_render::sprites::published(&ws.input_scratch.fx_layers)[0];
    assert!(
        usize::from(body.w) >= 5 * cell_w && usize::from(body.h) >= cell_h,
        "capture must show the full animal, not a replacement head"
    );
    assert_eq!(
        ws.fx
            .companions_mut()
            .cursor_cat
            .static_frame(Instant::now())
            .sing,
        1.0
    );
    assert!(
        ws.input_scratch.word_decorations.is_empty()
            && ws.input_scratch.ink.is_empty()
            && ws.input_scratch.nova_add.is_empty(),
        "the disabled word feed stays byte-empty while its independent singing companion draws"
    );
}

/// The flier (`cursor_pet = "flying cat"`): its earned flight is drawn, and no walker is.
#[test]
fn production_headless_image_keeps_the_flying_cat_without_sparkle_words() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    app.config.cursor_pet = Some("flying cat".into());
    app.config.motion = Some("full".into());
    app.recompute_sparkle();
    app.sparkle = None;
    let now = Instant::now();
    {
        let ws = app.windows.get_mut(&wid).expect("headless window 0");
        ws.focused = true;
        let start = now - Duration::from_millis(95 * 40);
        for i in 0..96u64 {
            ws.fx
                .companions_mut()
                .cursor_cat
                .on_key(start + Duration::from_millis(i * 40), true);
        }
        assert!(
            ws.fx.companions_mut().cursor_cat.is_active(),
            "the fixture earns the real classic cursor-cat lifecycle"
        );
    }

    render_front_image(&mut app);

    let ws = app.windows.get(&wid).expect("headless window 0");
    assert!(ws.fx.companions().cursor_cat.is_active());
    assert!(!ws.fx.companion().brain().is_active());
    assert_eq!(
        aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
        1,
        "the config-free capture carries exactly the one earned classic kitty"
    );
    assert!(
        aterm_render::sprites::published(&ws.input_scratch.fx_layers)
            .iter()
            .all(|s| !s.art.is_empty())
    );
    assert!(
        ws.input_scratch.word_decorations.is_empty()
            && ws.input_scratch.ink.is_empty()
            && ws.input_scratch.nova_add.is_empty(),
        "no word-owned channel is synthesized to host the independent kitty"
    );
}

#[test]
fn serious_mode_capture_hard_retires_a_song_rearmed_after_transition() {
    use aterm_effects::kitty_sing::SING_ARM_REPEATS;

    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    app.recompute_sparkle();
    app.sparkle = None;
    assert!(app.set_serious_mode(true));

    let now = Instant::now();
    let start = now - Duration::from_millis(u64::from(SING_ARM_REPEATS - 1) * 10);
    {
        let ws = app.windows.get_mut(&wid).expect("headless window 0");
        for i in 0..SING_ARM_REPEATS {
            ws.fx.companions_mut().kitty_sing.note_char(
                start + Duration::from_millis(u64::from(i) * 10),
                0,
                'a',
            );
        }
        assert_eq!(
            ws.fx.companions_mut().kitty_sing.drive(now),
            1.0,
            "negative control: the post-transition input really re-armed the singer"
        );
    }

    app.capture_words(wid, now);
    let ws = app.windows.get(&wid).expect("headless window 0");
    assert_eq!(ws.fx.companions().kitty_sing.drive(now), 0.0);
    assert!(!ws.fx.companions().cursor_cat.is_active());
    assert!(!ws.fx.companion().brain().is_active());
    assert!(!ws.fx.companions().music_notes.is_active());
    assert!(
        aterm_render::sprites::published(&ws.input_scratch.fx_layers).is_empty(),
        "Serious Mode capture must publish no companion pixels"
    );
}

#[test]
fn alt_screen_word_suppression_keeps_the_independent_capture_pet() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    app.config.cursor_trail = Some(true);
    app.config.sparkle_words = Some(crate::app_config::SparkleWordsConfig {
        enabled: Some(true),
        suppress_in_alt_screen: Some(true),
        ..Default::default()
    });
    app.prepared_sparkle = app.config.prepare_sparkle_runtime();
    app.sparkle_dirty = true;
    app.recompute_sparkle();
    assert!(
        app.sparkle.is_some(),
        "negative control: word sparkles resolved on"
    );
    let terminal = app.front_terminal(wid).expect("front terminal");
    term_lock(&terminal.term).process(b"\x1b[?1049h");

    app.capture_words(wid, Instant::now());
    let ws = app.windows.get(&wid).expect("headless window 0");
    assert_eq!(
        aterm_render::sprites::published(&ws.input_scratch.fx_layers).len(),
        1,
        "the word-only alt-screen switch cannot blank the resident cursor pet"
    );
    assert!(
        aterm_render::sprites::published(&ws.input_scratch.fx_layers)
            .iter()
            .all(|s| !s.art.is_empty())
    );
    assert!(
        ws.input_scratch.word_decorations.is_empty()
            && ws.input_scratch.ink.is_empty()
            && ws.input_scratch.nova_add.is_empty(),
        "the suppressed word-owned channels remain empty"
    );
}

#[test]
fn composed_capture_decorations_use_the_extracted_screen_sample() {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    let wid = WindowId(0);
    let focused_session = app.split_active_stub_tab(wid);
    let term = app
        .pool
        .get(focused_session)
        .expect("focused terminal")
        .term
        .clone();
    term_lock(&term).process(b"\x1b[?1049h\x1b[4;7H");
    let terminal_id = term_lock(&term).render_identity();
    let switch_term = term.clone();
    let now = Instant::now();
    let capture = app
        .prepare_terminal_capture_grid_with_cursor_fx_interleaved(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(now),
            move || term_lock(&switch_term).process(b"\x1b[?1049l\x1b[2;2H"),
        )
        .expect("composed capture");
    assert!(
        capture
            .focus
            .is_some_and(|sample| { sample.terminal_id == terminal_id && sample.alternate_screen })
    );
    assert!(
        !term_lock(&term).is_alternate_screen(),
        "negative control: the live terminal moved to a newer screen"
    );

    app.windows
        .get_mut(&wid)
        .expect("test window")
        .fx
        .forget_owner_space();
    app.capture_words_sampled(wid, now, capture.focus.expect("focused capture sample"));
    assert_eq!(
        app.windows[&wid].fx.owner_space(),
        Some((terminal_id, true)),
        "decoration custody must stay on the screen paired with captured cells"
    );
}

/// A captured frame that draws no head reports none, whatever the last one drew: the trail
/// gallery's status is read off captures.
#[test]
fn a_captured_frame_reports_only_the_head_it_drew() {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("fire".into());
    let wid = WindowId(0);
    let now = Instant::now();
    let capture = app
        .prepare_terminal_capture_grid_with_cursor_fx(
            wid,
            crate::app_render::ComposedCursorFxClock::Advance(now),
        )
        .expect("composed capture");
    app.windows
        .get_mut(&wid)
        .expect("test window")
        .fx
        .cursor_mut()
        .glow
        .note_head_body(Some((1, 2, 3, 4)));
    app.capture_words_sampled(wid, now, capture.focus.expect("focused capture sample"));
    assert_eq!(
        app.windows[&wid].fx.cursor().glow.head_body_window_box(),
        None,
        "the last frame's head stayed in this frame's status"
    );
}

#[test]
fn headless_capture_ticks_fire_live_then_decays_to_zero() {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true); // master switch (default platform-split)
    app.config.cursor_trail_style = Some("fire".into());
    let wid = WindowId(0);
    let t0 = Instant::now();
    let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(t0),
    );
    {
        let ws = app.windows.get_mut(&wid).expect("headless window 0");
        ws.fx.cursor_mut().glow.note_synthetic_move(t0);
        ws.fx.cursor_mut().trail.note_synthetic_move(t0);
    }
    {
        let terminal = app.front_terminal(wid).expect("front terminal");
        term_lock(&terminal.term).grid_mut().cursor_forward(1);
    }
    let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(t0 + Duration::from_millis(16)),
    );
    {
        let ws = app.windows.get(&wid).expect("headless window 0");
        assert!(
            !ws.input_scratch.glow_add_laid().is_empty(),
            "a headless capture right after a cursor move carries live fire quads"
        );
    }
    let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(t0 + Duration::from_secs(31)),
    );
    let ws = app.windows.get(&wid).expect("headless window 0");
    assert!(
        ws.input_scratch.glow_add_laid().is_empty(),
        "31 s idle: the fire must decay to exactly zero quads in the capture"
    );
    let ember = ws
        .input_scratch
        .cursor_fill_override
        .expect("31 s idle: the fire cursor rests as a warm ember, not the theme fill");
    let (r, b) = ((ember >> 16) & 0xFF, ember & 0xFF);
    assert!(
        r > b,
        "the rest cursor is warm metal (r > b), got {ember:#08x}"
    );
}

#[test]
fn headless_capture_hands_no_neighbor_rows_before_rainbow_kitty_owns_the_frame() {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    {
        let terminal = app.front_terminal(wid).expect("front terminal");
        term_lock(&terminal.term).process(b"\x1b[5;1HABOVE\x1b[6;1HCURRENT\x1b[7;1HBELOW\x1b[6;8H");
    }
    let t0 = Instant::now();
    assert!(!app.windows[&wid].fx.cursor().glow.v2_owns_frame());
    let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(t0),
    );
    let glow = &app.windows[&wid].fx.cursor().glow;
    assert!(glow.v2_owns_frame(), "the first capture's tick engages v2");
    assert_eq!(
        glow.neighbor_rows_probed(),
        Some((false, false)),
        "the engaging tick's capture skipped both flanking rows"
    );
    let _ = app.prepare_terminal_capture_grid_with_cursor_fx(
        wid,
        crate::app_render::ComposedCursorFxClock::Advance(t0 + Duration::from_millis(16)),
    );
    let ws = &app.windows[&wid];
    assert_eq!(
        ws.fx.cursor().glow.neighbor_rows_probed(),
        Some((true, true)),
        "once v2 owns the frame the capture's rows reach the witnesses"
    );
    for (probe, marker) in [
        (&ws.poof_row_above_buf, "ABOVE"),
        (&ws.poof_row_below_buf, "BELOW"),
    ] {
        let actual: String = probe[..marker.len()].iter().collect();
        assert_eq!(actual, marker, "the witnesses' rows are the real ones");
    }
}
