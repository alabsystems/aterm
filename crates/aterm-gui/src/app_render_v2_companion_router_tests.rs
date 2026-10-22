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

/// A grid whose row 4 is inked from `from` to `to` (exclusive), blank
/// elsewhere — the ink probe's whole input.
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

/// The router's lead is the emitter's own ¾-cell lead, measured off the
/// footprint itself so the two constants cannot drift apart.
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

/// Host side of the occlusion law (L-D): with v2 engaged and the words ahead of the caret
/// inked, the flying head seats behind the caret (a negative lead on the frame copy); blank
/// glass ahead keeps the default seat unchanged, and the pet, which seats itself, is never
/// moved.
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

    // Blank glass ahead: the default seat, untouched.
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

    // Words ahead, blank behind: the mirror seat, `lead_px` clear of the
    // caret cell's left edge — a negative lead for a right-facing head.
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

    // The pet seats itself: the router never touches its frame.
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

/// The offer seam delivers (`EffectsPipeline::offer_pet_v2`): v2 mints the
/// perk edge for a real 40-cell nav meteor, the seam runs where both render
/// arms run it (after the pet's tick, before the router drains the impulse),
/// and the resident ends up holding it; a seam that delivered nothing would
/// leave this latch shut.
#[test]
fn the_offer_seam_hands_the_meteors_arrival_to_the_resident() {
    let mut app = App::headless_for_test();
    // The trail is the one opting in: on by default everywhere but Windows
    // (`DEFAULT_DECORATIVE_EFFECTS`), where this seam serves an owner who turned
    // it on.
    app.config.cursor_trail = Some(true);
    let cfg = app.glow_config();
    let now = Instant::now();
    let ws = app.windows.get_mut(&WindowId(0)).expect("test window");
    let mut out = Vec::new();
    // Seat the caret; the rainbow kitty engages v2 on its first tick.
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
    // A 40-cell licensed nav jump — a meteor by every measure (§6.1).
    let t1 = now + Duration::from_millis(16);
    ws.fx.cursor_mut().glow.note_navigation(t1);
    ws.fx
        .cursor_mut()
        .glow
        .tick(Some((3, 40)), t1, &cfg, glow_geom(), &mut out);
    // The host's order: the pet's own tick, then the offer.
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
    // The offer is a pure read: the router's drain still has the impulse to route.
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

/// THE WORD-CATS YIELD TO THE FLYING HEAD WHERE IT IS DRAWN. With the words
/// ahead of the caret inked and the cells behind it blank, the seat law puts
/// the head behind the caret (the mirror seat). The box the host hands the
/// word pass ([`super::RoutedCompanion::on_glass`]) is routed before the pass
/// and covers the actual animated body, not just its destination. Until
/// 2026-10-21 the box was the rest footprint, built before the seat was
/// chosen: ahead of the caret, clear of the drawn head, so an ambient
/// word-cat peeked out under the head (two cats stacked). Negative control:
/// the rest footprint misses the drawn head.
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
    // A resident ticked, then taken off glass: the head owns the caret.
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

    // Seed ahead of the caret on blank glass, then flip the seat behind it
    // just 16 ms later. The follower is still between those targets: reserving
    // only the new seat misses the body on this second frame.
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

    // A signed off-stage fold can reach the saturating pixel boundary even
    // with valid u16 metrics. Its half-open reservation must not overflow.
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
