// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::{CompanionDuty, CompanionInk, route_v2_companion};
use crate::{App, WindowId};
use aterm_core::terminal::RenderCell;
use aterm_effects::word_decorations::{EffectGeom, KittyCursorLayout};
use std::time::{Duration, Instant};

const GEOM: EffectGeom = EffectGeom {
    cell_w: 8,
    cell_h: 16,
    rows: 24,
    cols: 80,
};

use crate::geom_24x80_for_test as glow_geom;

fn inked_row(from: usize, to: usize) -> Vec<Vec<RenderCell>> {
    let mut cells = vec![vec![RenderCell::default(); 80]; 24];
    for cell in &mut cells[4][from..to] {
        *cell = RenderCell {
            ch: 'x',
            ..RenderCell::default()
        };
    }
    cells
}

#[test]
fn the_router_lead_is_the_footprints_own_lead() {
    let mut app = App::headless_for_test();
    let now = Instant::now();
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    let look = ws
        .fx
        .companions_mut()
        .cursor_cat
        .static_frame(now)
        .render_look();
    let rest = ws
        .fx
        .decos_mut()
        .kitty_cursor_footprint(KittyCursorLayout {
            geom: GEOM,
            cursor: (4, 10),
            look,
            bob: 0.0,
            lift: 0.0,
        })
        .expect("a mid-line footprint");
    let cursor_right = 11 * i32::from(GEOM.cell_w);
    let lead_px = aterm_effects::word_decorations::WordDecorations::kitty_cursor_lead_px;
    assert_eq!(rest.x - cursor_right, lead_px(GEOM.cell_w));
}

#[test]
fn a_v2_flying_head_yields_behind_inked_text_and_the_pet_is_never_moved() {
    let mut app = App::headless_for_test();
    let mut cfg = app.glow_config();
    cfg.enabled = true;
    cfg.style = crate::cursor_glow::GlowStyle::RainbowKitty;
    cfg.intensity = 1.0;
    let now = Instant::now();
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    let mut out = Vec::new();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((4, 10)), now, &cfg, glow_geom(), &mut out);
    assert!(
        ws.fx.cursor_mut().glow.v2_status().is_some(),
        "the rainbow kitty engages v2 on its first tick, untold"
    );
    let before = ws.fx.companions_mut().cursor_cat.static_frame(now);

    let mut frame = before;
    ws.input_scratch.cells = inked_row(0, 0);
    route_v2_companion(
        ws,
        CompanionInk::Single,
        CompanionDuty::FlyingHead { cell: (4, 10) },
        now,
        GEOM,
        &mut frame,
        true,
    );
    assert_eq!(frame.pose.lead, before.pose.lead);
    assert_eq!(frame.bob, before.bob);

    let mut frame = before;
    ws.input_scratch.cells = inked_row(11, 30);
    route_v2_companion(
        ws,
        CompanionInk::Single,
        CompanionDuty::FlyingHead { cell: (4, 10) },
        now,
        GEOM,
        &mut frame,
        true,
    );
    assert!(
        frame.pose.lead < before.pose.lead,
        "the head must yield backwards off the words ahead, got {} vs {}",
        frame.pose.lead,
        before.pose.lead
    );
    assert_eq!(
        frame.bob, before.bob,
        "no sky seat while the row behind is blank"
    );

    let mut frame = before;
    ws.input_scratch.cells = inked_row(0, 80);
    route_v2_companion(
        ws,
        CompanionInk::Single,
        CompanionDuty::Pet,
        now,
        GEOM,
        &mut frame,
        true,
    );
    assert_eq!(frame.pose.lead, before.pose.lead);
    assert_eq!(frame.bob, before.bob);
}

#[test]
fn the_offer_seam_hands_the_meteors_arrival_to_the_resident() {
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    let cfg = app.glow_config();
    let now = Instant::now();
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    let mut out = Vec::new();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((3, 0)), now, &cfg, glow_geom(), &mut out);
    assert!(
        ws.fx.cursor_mut().glow.v2_status().is_some(),
        "fixture: v2 owns the frame"
    );
    assert_eq!(
        ws.fx.companion_mut().brain().pending_v2_offer(),
        (false, false),
        "fixture: the resident has been offered nothing"
    );
    let t1 = now + Duration::from_millis(16);
    ws.fx.cursor_mut().glow.note_navigation(t1);
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((3, 40)), t1, &cfg, glow_geom(), &mut out);
    let pet_frame = crate::app_render::pet_tick_for_test(
        ws,
        aterm_effects::host::CaptureMode::Present,
        aterm_effects::kitty_pet::PetSense {
            caret_drawn: true,
            now: t1,
            caret: Some((3, 40)),
            rows: GEOM.rows,
            cols: GEOM.cols,
            cell_w: GEOM.cell_w,
            cell_h: GEOM.cell_h,
            reduced_motion: false,
            output_burst: false,
            pointer: None,
            wrapped: false,
        },
    );
    ws.fx.offer_pet_v2(&pet_frame, glow_geom(), (0, 0), t1);
    assert!(
        ws.fx.companion_mut().brain().pending_v2_offer().0,
        "the meteor's arrival edge never reached the resident"
    );
    let mut cat = ws.fx.companions_mut().cursor_cat.static_frame(t1);
    route_v2_companion(
        ws,
        CompanionInk::Single,
        CompanionDuty::FlyingHead { cell: (3, 40) },
        t1,
        GEOM,
        &mut cat,
        true,
    );
    assert!(
        ws.fx.cursor_mut().glow.take_companion_impulse().is_none(),
        "the router drains the slot; nothing may be left for a second reader"
    );
}

#[test]
fn the_word_cats_yield_to_the_flying_head_at_its_seat() {
    let mut app = App::headless_for_test();
    let mut cfg = app.glow_config();
    cfg.enabled = true;
    cfg.style = crate::cursor_glow::GlowStyle::RainbowKitty;
    cfg.intensity = 1.0;
    let now = Instant::now();
    let caret = (4u16, 10u16);
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    let mut out = Vec::new();
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some(caret), now, &cfg, glow_geom(), &mut out);
    assert!(ws.fx.cursor_mut().glow.v2_status().is_some(), "fixture: v2");
    let resident = super::pet_owner_tick_for_test(
        ws,
        aterm_effects::host::CaptureMode::Present,
        aterm_effects::kitty_pet::PetSense {
            caret_drawn: true,
            now,
            caret: Some(caret),
            rows: GEOM.rows,
            cols: GEOM.cols,
            cell_w: GEOM.cell_w,
            cell_h: GEOM.cell_h,
            reduced_motion: true,
            output_burst: false,
            pointer: None,
            wrapped: false,
        },
    );
    let pet = aterm_effects::companion::CompanionFrame {
        on_glass: false,
        duty: CompanionDuty::Idle,
        body_px: None,
        companion: None,
        ..resident
    };
    ws.input_scratch.cells = inked_row(11, 30);
    ws.fx.open_words();
    let head = ws.fx.companions_mut().cursor_cat.static_frame(now);
    let rest = ws
        .fx
        .decos()
        .kitty_cursor_footprint(KittyCursorLayout {
            geom: GEOM,
            cursor: caret,
            look: head.render_look(),
            bob: head.bob,
            lift: 0.0,
        })
        .expect("a mid-line footprint");
    let rest = (
        rest.x,
        rest.x + i32::from(rest.w),
        rest.y,
        rest.y + i32::from(rest.h),
    );
    let routed =
        super::route_single_cursor_companion(ws, GEOM, Some(caret), &pet, head, true, 255, now);
    let seat = routed.seat.px.expect("the head is seated");
    assert!(
        seat.1 <= rest.0 && seat.0 < i32::from(caret.1) * i32::from(GEOM.cell_w),
        "fixture: the mirror seat, behind the caret: {seat:?} against the rest {rest:?}"
    );
    let yield_box = routed
        .on_glass(Some(caret), pet.body_px)
        .and_then(|c| c.body_px)
        .expect("the head's yield box");
    assert_eq!(yield_box, seat, "the word pass yields to the seat");

    let fp = super::emit_routed_cursor_companion(
        ws,
        GEOM,
        &pet,
        false,
        routed,
        255,
        now,
        true,
        0x0010_1010,
        0x00FF_FFFF,
        0x0050_FA7B,
    );
    assert_ne!(fp, 0, "the head is drawn");
    let drawn = ws
        .fx
        .free_sprites()
        .iter()
        .max_by_key(|s| u32::from(s.w) * u32::from(s.h))
        .map(|s| (s.x, s.x + i32::from(s.w), s.y, s.y + i32::from(s.h)))
        .expect("the head's sprite");
    let overlap = |a: (i32, i32, i32, i32), b: (i32, i32, i32, i32)| {
        (a.1.min(b.1) - a.0.max(b.0)).max(0) * (a.3.min(b.3) - a.2.max(b.2)).max(0)
    };
    let area = (drawn.1 - drawn.0) * (drawn.3 - drawn.2);
    assert!(
        overlap(yield_box, drawn) * 10 >= area * 9,
        "the yield box covers the head as drawn: box {yield_box:?}, head {drawn:?}"
    );
    assert_eq!(
        overlap(rest, drawn),
        0,
        "negative control: the rest footprint misses the head at its seat: \
         rest {rest:?}, head {drawn:?}"
    );

    ws.fx.decos_mut().rebase_kitty_cursor_placement();
    let t0 = now + Duration::from_secs(1);
    ws.fx.open_words();
    ws.input_scratch.cells = inked_row(0, 0);
    let head = ws.fx.companions_mut().cursor_cat.static_frame(t0);
    let ahead =
        super::route_single_cursor_companion(ws, GEOM, Some(caret), &pet, head, true, 255, t0);
    assert_ne!(
        super::emit_routed_cursor_companion(
            ws,
            GEOM,
            &pet,
            false,
            ahead,
            255,
            t0,
            true,
            0x0010_1010,
            0x00FF_FFFF,
            0x0050_FA7B,
        ),
        0
    );

    let t1 = t0 + Duration::from_millis(16);
    ws.fx.open_words();
    ws.input_scratch.cells = inked_row(11, 30);
    let head = ws.fx.companions_mut().cursor_cat.static_frame(t1);
    let gliding =
        super::route_single_cursor_companion(ws, GEOM, Some(caret), &pet, head, true, 255, t1);
    let target = gliding.seat.px.expect("the new mirror target");
    let reserved = gliding
        .on_glass(Some(caret), None)
        .and_then(|c| c.body_px)
        .expect("the prepared gliding body");
    assert_ne!(
        super::emit_routed_cursor_companion(
            ws,
            GEOM,
            &pet,
            false,
            gliding,
            255,
            t1,
            true,
            0x0010_1010,
            0x00FF_FFFF,
            0x0050_FA7B,
        ),
        0
    );
    let body = ws.fx.free_sprites().last().expect("the emitted head");
    let actual = (
        body.x,
        body.x + i32::from(body.w),
        body.y,
        body.y + i32::from(body.h),
    );
    assert_eq!(
        reserved, actual,
        "reservation and emission consume one placement"
    );
    assert!(
        overlap(target, actual) * 10 < (actual.1 - actual.0) * (actual.3 - actual.2) * 9,
        "negative control: the target alone misses the moving body: {target:?} vs {actual:?}"
    );

    use aterm_effects::kitty_cursor::{
        CatFoldDirection, CatFoldFrame, CatPose, CursorCatPlacementFrame,
    };
    use aterm_effects::word_decorations::{KittyCursorFrame, WordDecorations};
    let mut extreme = WordDecorations::default();
    let layout = KittyCursorLayout {
        geom: EffectGeom {
            cell_w: u16::MAX,
            cols: u16::MAX,
            cell_h: 20,
            ..GEOM
        },
        cursor: caret,
        look: head.render_look(),
        bob: 0.0,
        lift: 0.0,
    };
    let seed = extreme
        .prepare_kitty_cursor_placement(
            layout,
            CursorCatPlacementFrame {
                sampled_at: t0,
                fold: None,
                facing_left: false,
            },
            CatPose::STILL,
        )
        .expect("bounded natural art on a wide logical grid");
    let mut free = Vec::new();
    extreme
        .kitty_cursor_at_placement(
            KittyCursorFrame {
                geom: layout.geom,
                cursor: caret,
                look: layout.look,
                colors: Default::default(),
                bob: 0.0,
                lift: 0.0,
                alpha: 255,
                pose: CatPose::STILL,
                sing: 0.0,
                notes: [None; aterm_effects::kitty_sing::MAX_NOTES],
            },
            seed,
            &mut free,
        )
        .expect("the original placement was emitted");
    let entering = extreme
        .prepare_kitty_cursor_placement(
            layout,
            CursorCatPlacementFrame {
                sampled_at: t1,
                facing_left: true,
                fold: Some(CatFoldFrame {
                    started: t1,
                    direction: CatFoldDirection::Reverse,
                    progress: 0.5,
                }),
            },
            CatPose::STILL,
        )
        .expect("signed fold placement");
    assert_eq!(
        entering.drawn_footprint().expect("prepared body").x,
        i32::MAX
    );
    let extreme_route = super::RoutedCompanion {
        placement: Some(entering),
        ..gliding
    };
    let reserved = extreme_route
        .on_glass(Some(caret), None)
        .and_then(|c| c.body_px)
        .expect("saturated reservation");
    assert_eq!((reserved.0, reserved.1), (i32::MAX, i32::MAX));
}

/// A headless capture is a frame like a present: it offers the resident's
/// drawn body to the sky, so the stars it draws next stay off the fur. The
/// twin ticks the same pet onto the glass without an offer, the control: its
/// body is drawn and its sky shades nothing.
#[test]
fn a_capture_shades_the_sky_where_the_pet_is_drawn() {
    fn pet_app() -> App {
        let mut app = App::headless_for_test();
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty pet".into());
        app.config.motion = Some("full".into());
        app.windows.get_mut(&WindowId(0)).expect("window").focused = true;
        app.recompute_sparkle();
        assert!(app.trail_is_kitty_pet(), "fixture: the pet is the trail");
        app
    }
    let wid = WindowId(0);
    let t0 = Instant::now();
    let mut app = pet_app();
    let drawn = (0..600u64)
        .find_map(|k| {
            app.capture_words(wid, t0 + Duration::from_millis(16 * k));
            app.windows[&wid].fx.companion().hit_rect()
        })
        .expect("fixture: the pet reached the glass in 10 s of captures");
    let ws = &app.windows[&wid];
    assert!(
        ws.fx.cursor().glow.v2_status().is_some(),
        "fixture: the v2 sky owns the frame"
    );
    assert_eq!(
        ws.fx.cursor().glow.pet_shade(),
        Some(drawn),
        "the capture's sky shades the body the capture drew"
    );

    let mut twin = pet_app();
    let cfg = twin.glow_config();
    let tw = twin.windows.get_mut(&wid).expect("window");
    tw.fx
        .cursor_mut()
        .glow
        .tick(Some((5, 10)), t0, &cfg, glow_geom(), &mut Vec::new());
    let on_glass = (0..100u64).any(|k| {
        crate::app_render::pet_tick_for_test(
            tw,
            aterm_effects::host::CaptureMode::Present,
            aterm_effects::kitty_pet::PetSense {
                caret_drawn: true,
                now: t0 + Duration::from_millis(100 * k),
                caret: Some((5, 10)),
                rows: 24,
                cols: 80,
                cell_w: 10,
                cell_h: 20,
                reduced_motion: false,
                output_burst: false,
                pointer: None,
                wrapped: false,
            },
        )
        .alpha
            > 0
    });
    assert!(
        on_glass,
        "control fixture: the twin's pet reached the glass"
    );
    assert!(
        tw.fx.cursor().glow.v2_status().is_some(),
        "control fixture: the twin's v2 sky owns the frame"
    );
    assert_eq!(
        tw.fx.cursor().glow.pet_shade(),
        None,
        "control: a pet ticked onto the glass with no offer leaves the sky unshaded"
    );
}

/// The word pass hands its icons to the next frame's sky (looks6 N3: the
/// gallery's sky drew stars on the `dog`'s face, because a primed word tick
/// hid its icons from the keep-out): every capture's keep-out holds exactly
/// the icons the capture before it drew. The control is the same fixture
/// once the icon has gone: its keep-out lets go, so the boxes follow the
/// icons and are not a sticky leftover that would pass the first check.
#[test]
fn a_capture_s_word_icons_reach_the_sky_s_keep_out() {
    use aterm_render::sprites::{FreeZ, published};
    let mut app = App::headless_for_test();
    app.config.cursor_trail = Some(true);
    app.config.cursor_trail_style = Some("rainbow dog".into());
    app.config.motion = Some("full".into());
    let wid = WindowId(0);
    app.windows.get_mut(&wid).expect("window").focused = true;
    app.recompute_sparkle();
    let term = app.pool.get(0).expect("session 0").term.clone();
    crate::term_lock(&term).process(b"the lazy dog ");
    let t0 = Instant::now();
    let sizes = |boxes: &[(i32, i32, i32, i32)]| {
        let mut v: Vec<_> = boxes.iter().map(|b| (b.1 - b.0, b.3 - b.2)).collect();
        v.sort_unstable();
        v
    };
    let mut drawn: Vec<(i32, i32, i32, i32)> = Vec::new();
    let (mut held, mut released) = (0usize, 0usize);
    for k in 0..400u64 {
        app.capture_words(wid, t0 + Duration::from_millis(16 * k));
        let ws = &app.windows[&wid];
        let kept = ws.fx.cursor().glow.keep_out().to_vec();
        assert_eq!(
            sizes(&kept),
            sizes(&drawn),
            "capture {k}: the keep-out {kept:?} holds the icons {drawn:?} the capture before drew"
        );
        if drawn.is_empty() {
            released += usize::from(held > 0);
        } else {
            held += 1;
        }
        drawn = published(&ws.input_scratch.fx_layers)
            .iter()
            .filter(|s| s.z == FreeZ::UnderText && !s.is_blank())
            .map(|s| {
                let v = s.visible();
                (v.x0, v.x1, v.y0, v.y1)
            })
            .collect();
    }
    assert!(
        held > 10,
        "fixture: the word pass drew the `dog` icon ({held} captures)"
    );
    assert!(
        released > 10,
        "control: the icon left and the keep-out let go ({released} captures)"
    );
}
