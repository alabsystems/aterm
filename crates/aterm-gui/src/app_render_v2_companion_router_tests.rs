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
