// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CursorFxInputs, forward_kitty_cursor_motion, retire_kitty_cursor_without_owner};
use crate::{App, WindowId};
use aterm_effects::cursor_glow::{
    BlockFill, BlockFillOwner, CursorCatMotionKind, CursorCatMotionPulse,
};
use aterm_effects::kitty_pet::PetSense;
use std::time::{Duration, Instant};

/// The grid the hit-box cases below resolve against — the same 10x20 cell on
/// an 80x30 surface their `PetSense` uses, so the rect and the brain agree.
fn hit_geom() -> aterm_effects::word_decorations::EffectGeom {
    aterm_effects::word_decorations::EffectGeom {
        cell_w: 10,
        cell_h: 20,
        rows: 30,
        cols: 80,
    }
}

#[test]
fn reduced_motion_first_resident_pet_frame_is_opaque_without_a_frame_train() {
    let mut app = App::headless_for_test();
    let wid = WindowId(0);
    let now = Instant::now();
    let frame = {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.focused = true;
        crate::app_render::pet_tick_for_test(
            ws,
            aterm_effects::host::CaptureMode::Present,
            PetSense {
                caret_drawn: true,
                now,
                caret: Some((4, 12)),
                rows: 24,
                cols: 80,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: true,
                output_burst: false,
                pointer: None,
                wrapped: false,
            },
        )
    };
    assert_eq!(frame.alpha, 255, "the first reduced frame has real pixels");

    let ws = app.windows.get_mut(&wid).expect("window");
    assert!(
        !ws.fx.companion_mut().brain().needs_frames(),
        "the static placement itself has no unfinished motion"
    );
    assert!(
        !ws.cursor_dependents_need_frame_cadence(now, false),
        "the reduced-motion scheduler excludes that ordinary motion clock"
    );
    assert_eq!(
        ws.plan_terminal_effect_lane(now, false, true),
        None,
        "an opaque reduced still owes no follow-up frame train"
    );
    // The same brain must report actual motion again when it is enabled.
    let _ = crate::app_render::pet_tick_for_test(
        ws,
        aterm_effects::host::CaptureMode::Present,
        PetSense {
            caret_drawn: true,
            now: now + Duration::from_millis(16),
            caret: Some((4, 50)),
            rows: 24,
            cols: 80,
            cell_w: 10,
            cell_h: 20,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        },
    );
    assert!(
        ws.fx.companion_mut().brain().needs_frames(),
        "real resumed motion is owed"
    );
}

/// Real pet ticks and the shipping sprite emitter, with a moving caret through
/// the held song and its tail: the full body stays emitted and hit-testable,
/// and a stale head cannot claim a second companion.
#[test]
fn singing_pet_keeps_its_body_hit_target_and_cursor_home() {
    let geom = hit_geom();
    for reduced in [false, true] {
        let mut app = App::headless_for_test();
        let ws = app.windows.get_mut(&WindowId(0)).expect("window");
        let mut now = Instant::now();
        let mut col = 12;
        for drive in [0.0, 1.0, 0.49, 0.33, 0.1, 0.0] {
            for tick in 0..45 {
                now += Duration::from_millis(16);
                if tick == 20 {
                    col += 2;
                }
                let caret = (4, col);
                let pet = crate::app_render::pet_owner_tick_for_test(
                    ws,
                    aterm_effects::host::CaptureMode::Present,
                    PetSense {
                        caret_drawn: true,
                        now,
                        caret: Some(caret),
                        rows: geom.rows,
                        cols: geom.cols,
                        cell_w: geom.cell_w,
                        cell_h: geom.cell_h,
                        reduced_motion: reduced,
                        output_burst: false,
                        pointer: None,
                        wrapped: false,
                    },
                );
                let frame = pet.pet;
                if drive == 0.0 && col == 12 && tick < 20 {
                    continue; // only the initial appearance ramp
                }
                let body = frame
                    .body_px(geom.cell_w, geom.cell_h, geom.cols, geom.rows)
                    .expect("the full pet persists through every song phase");
                assert!(frame.alpha > 0);
                assert!(body.1 - body.0 >= 5 * i32::from(geom.cell_w));
                assert!(body.3 - body.2 >= i32::from(geom.cell_h));
                let cursor_x = i32::from(col) * i32::from(geom.cell_w);
                let horizontal_gap = (body.0 - cursor_x).max(cursor_x - body.1).max(0);
                assert!(horizontal_gap <= 10 * i32::from(geom.cell_w));
                assert!((body.3 - 5 * i32::from(geom.cell_h)).abs() <= 5 * i32::from(geom.cell_h));
                let mut head = ws.fx.companions_mut().cursor_cat.static_frame(now);
                head.sing = drive;
                head.alpha = 211; // a stale head must lose the final custody tie
                ws.fx.open_words();
                assert!(pet.on_glass);
                let fp = super::emit_single_cursor_companion(
                    ws,
                    geom,
                    Some(caret),
                    &pet,
                    false,
                    head,
                    !reduced,
                    211,
                    now,
                    reduced,
                    0x0010_1010,
                    0x00FF_FFFF,
                    0x0050_FA7B,
                );
                assert_ne!(fp, 0, "the production emitter drew the full resident");
                let sprite = ws
                    .fx
                    .free_sprites()
                    .last()
                    .expect("the resident body is emitted last");
                assert_eq!(
                    (
                        sprite.x,
                        sprite.x + i32::from(sprite.w),
                        sprite.y,
                        sprite.y + i32::from(sprite.h)
                    ),
                    body
                );
                assert!(sprite.w >= 5 * geom.cell_w && sprite.h >= geom.cell_h);
                let attachments = frame.motes.iter().flatten().count()
                    + frame.departures.iter().flatten().count();
                assert!(
                    ws.fx.free_sprites().len() <= 1 + attachments,
                    "only the full resident and its declared attachments may be emitted"
                );
                let owned = ws.fx.free_sprites().to_vec();
                let rejected_head = super::emit_single_cursor_companion(
                    ws,
                    geom,
                    Some(caret),
                    &aterm_effects::companion::CompanionFrame {
                        on_glass: false,
                        duty: super::CompanionDuty::Idle,
                        body_px: None,
                        companion: None,
                        ..pet
                    },
                    false,
                    head,
                    !reduced,
                    211,
                    now,
                    reduced,
                    0x0010_1010,
                    0x00FF_FFFF,
                    0x0050_FA7B,
                );
                assert_eq!(
                    rejected_head, 0,
                    "the production emitter refuses a second companion claim"
                );
                assert_eq!(
                    ws.fx.free_sprites(),
                    owned,
                    "a stale head adds no pixels after the full body"
                );
            }
        }
    }
}

#[test]
fn pet_mode_builds_real_song_tenure_without_an_invisible_ordinary_flight() {
    let t0 = Instant::now();
    let mut cat = crate::kitty_cursor::CursorCat::default();
    for i in 0..160u64 {
        forward_kitty_cursor_motion(
            true,
            true,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            Some(CursorCatMotionPulse {
                at: t0 + Duration::from_millis(i * 40),
                kind: CursorCatMotionKind::Advance,
            }),
            &mut cat,
        );
        assert!(
            !cat.is_active(),
            "pet tenure must never start an invisible ordinary flight"
        );
    }

    // The idle pet-mode render path calls this every drained frame. Hidden
    // tenure is intentionally retained, so the song can still spend it.
    cat.retire_unowned_cursor_motion();
    let song_at = t0 + Duration::from_millis(6_500);
    cat.set_singing(
        song_at,
        crate::kitty_cursor::SingSync {
            drive: 1.0,
            beat: 0.0,
        },
    );
    assert!(cat.is_active(), "the earned singing face must be reachable");

    cat.set_singing(
        song_at + Duration::from_secs(1),
        crate::kitty_cursor::SingSync {
            drive: 0.0,
            beat: 0.0,
        },
    );
    cat.retire_unowned_cursor_motion();
    assert!(
        !cat.is_active(),
        "a fully drained song cannot own invisible frame cadence"
    );
}

#[test]
fn hot_owner_or_style_change_grounds_an_invisible_ordinary_flight() {
    let t0 = Instant::now();
    let mut earned = crate::kitty_cursor::CursorCat::default();
    for i in 0..160u64 {
        forward_kitty_cursor_motion(
            true,
            false,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            Some(CursorCatMotionPulse {
                at: t0 + Duration::from_millis(i * 40),
                kind: CursorCatMotionKind::Advance,
            }),
            &mut earned,
        );
    }
    assert!(
        earned.is_active(),
        "the control must earn an ordinary flight"
    );

    retire_kitty_cursor_without_owner(
        false,
        false,
        crate::cursor_glow::GlowStyle::RainbowKitty,
        0.0,
        &mut earned,
    );
    assert!(
        !earned.is_active(),
        "disabling the trail master must not retain invisible cadence"
    );

    for i in 0..160u64 {
        forward_kitty_cursor_motion(
            true,
            false,
            crate::cursor_glow::GlowStyle::RainbowKitty,
            Some(CursorCatMotionPulse {
                at: t0 + Duration::from_secs(10) + Duration::from_millis(i * 40),
                kind: CursorCatMotionKind::Advance,
            }),
            &mut earned,
        );
    }
    assert!(earned.is_active(), "the second control must earn a flight");
    retire_kitty_cursor_without_owner(
        true,
        false,
        crate::cursor_glow::GlowStyle::Lumen,
        0.0,
        &mut earned,
    );
    assert!(
        !earned.is_active(),
        "switching away from rainbow kitty must ground the old flight"
    );
}

/// The cursor-viewport lifecycle: charged Glow/Trail and cursor-body state
/// retire on the first history snapshot. The resident pet's half is
/// `aterm_effects::companion`'s `history_lets_the_fading_pet_brain_settle`.
#[test]
fn history_retires_cursor_pixels_and_cursor_bodies() {
    let mut app = App::headless_for_test();
    app.config.motion = Some("full".into());
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("comet".into());
    app.config.trail_sounds = Some(false);
    let wid = WindowId(0);
    let t0 = Instant::now();

    let mut seed = CursorFxInputs::sample_for_test(t0);
    seed.cur = Some((2, 2));
    app.cursor_frame(wid, seed).expect("seed cursor engines");
    let authored = t0 + Duration::from_millis(1);
    {
        let ws = app.windows.get_mut(&wid).expect("window");
        ws.fx.cursor_mut().cadence.on_keystroke(authored);
        ws.fx.cursor_mut().glow.note_synthetic_move(authored);
        ws.fx.cursor_mut().trail.note_synthetic_move(authored);
    }
    let mut live = CursorFxInputs::sample_for_test(t0 + Duration::from_millis(2));
    live.cur = Some((2, 5));
    let live_tick = app.cursor_frame(wid, live).expect("live cursor tick");
    let ws = app.windows.get(&wid).expect("window");
    assert!(
        !ws.fx.cursor().trail_scratch.is_empty(),
        "negative control owns a real classic trail"
    );
    assert!(
        !ws.fx.cursor().glow_scratch.is_empty() || ws.fx.cursor().glow.is_active(),
        "negative control owns real glow geometry"
    );
    assert!(
        matches!(
            live_tick.block_fill,
            Some(BlockFill {
                owner: BlockFillOwner::Comet,
                ..
            })
        ),
        "negative control owns a cursor-body overlay"
    );

    let mut history = CursorFxInputs::sample_for_test(t0 + Duration::from_millis(3));
    history.live_viewport = false;
    history.cur = None;
    // Even a stale DECTCEM-visible bit cannot restore an active-grid cursor
    // body over history.
    history.cursor_visible = true;
    let history_tick = app.cursor_frame(wid, history).expect("history tick");
    let ws = app.windows.get(&wid).expect("window");
    assert!(ws.fx.cursor().trail_scratch.is_empty() && !ws.fx.cursor().trail.is_active());
    assert!(
        ws.fx.cursor().glow_scratch.is_empty()
            && !ws.fx.cursor().glow.is_active()
            && ws.fx.cursor().glow.halos().is_empty()
            && ws.fx.cursor().glow.under_quads().is_empty(),
        "history clears resident Glow geometry, not merely its cursor sensor"
    );
    assert!(
        history_tick.block_fill.is_none()
            && !history_tick.bolt_cursor
            && !history_tick.twinkle_cursor,
        "every cursor-body family is dark in history"
    );

    let model = aterm_spec::derive::cursor_viewport_lifecycle_model();
    let charged = model.successors("ChargeLive", &model.init_state())[0].clone();
    let mut projected = charged.clone();
    projected.insert("live_viewport", 0);
    projected.insert("glow_visible", 0);
    projected.insert("trail_visible", 0);
    projected.insert("cursor_body_visible", 0);
    projected.insert("pet_visible", 0);
    projected.insert("base_cursor_visible", 0);
    projected.insert("pet_brain_ticked", 1);
    projected.insert("scheduler_stuck", 0);
    let (ok, why) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &charged,
        &projected,
        Some("EnterHistory"),
        "App cursor viewport history transition",
    );
    assert!(ok, "real history transition rejected: {why}");

    let mut retained_body = projected.clone();
    retained_body.insert("cursor_body_visible", 1);
    let (ok, _) = aterm_spec::verify::validate_transition_tiered(
        &model,
        &[],
        &charged,
        &retained_body,
        Some("EnterHistory"),
        "App history Forge/body negative control",
    );
    assert!(!ok, "history may not retain a cursor fill/body");
}
