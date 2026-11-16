// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cat_baker::EyesFrame;
use crate::kitty_cursor::{CatFrame, CursorBody};
use crate::kitty_registry::KittyLook;
use crate::pet_glyphs_gen::PetGlyphId;
use crate::rainbow_kitty::CompanionImpulse;
use crate::word_decorations::KittyCursorLayout;
use std::time::Duration;

const GEOM: EffectGeom = EffectGeom {
    cell_w: 8,
    cell_h: 16,
    rows: 24,
    cols: 120,
};
const FROM: (u16, u16) = (3, 0);
const LANDING: (u16, u16) = (3, 40);

const fn glow_geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 24,
        cols: 120,
        origin_x: 0,
        origin_y: 0,
        win_w: 960,
        win_h: 384,
        head: 0,
    }
}

fn rainbow_kitty() -> GlowConfig {
    GlowConfig {
        accent: 0x007A_A2F7,
        length: 18,
        audible: true,
        radius: 0.6,
        ring: true,
        ..Default::default()
    }
}

/// A window with the flying head on glass at `FROM` (a collection hello
/// presents it), its placement follower at rest there, and v2 engaged;
/// then a licensed jump to `to`, the meteor, observed at `t1`.
fn fly(t0: Instant, t1: Instant, to: (u16, u16)) -> EffectsPipeline {
    let mut fx = EffectsPipeline::for_window();
    let cfg = rainbow_kitty();
    let mut out = Vec::new();
    fx.cursor_mut()
        .glow
        .tick(Some(FROM), t0, &cfg, glow_geom(), &mut out);
    let cat = &mut fx.companions_mut().cursor_cat;
    cat.on_collect(t0, KittyLook::for_app("claude"));
    cat.set_collection_presentable(t0, true);
    let frame = fx.companions_mut().cursor_cat.frame(t0);
    let placement_frame = fx.companions().cursor_cat.placement_frame(t0);
    fx.decos_mut()
        .resolve_kitty_cursor_placement(layout(FROM, &frame), placement_frame)
        .expect("the head is placed at its launch");
    fx.cursor_mut().glow.note_navigation(t1);
    fx.cursor_mut()
        .glow
        .tick(Some(to), t1, &cfg, glow_geom(), &mut out);
    fx
}

const fn layout(cursor: (u16, u16), frame: &CatFrame) -> KittyCursorLayout {
    KittyCursorLayout {
        geom: GEOM,
        cursor,
        look: frame.render_look(),
        body: frame.body,
        bob: frame.bob,
        lift: 0.0,
    }
}

fn blank() -> Vec<Vec<RenderCell>> {
    vec![vec![RenderCell::default(); 120]; 24]
}

/// A frame of [`head_walk`]: its caret, its drawn box `(x0, x1, y0, y1)`, if any, and its fit.
type HeadFrame = ((u16, u16), Option<(i32, i32, i32, i32)>, f32);

/// The caret taken along `path` on the page `cells`, one key each `key_ms`, each cell it leaves
/// lettered, for `frames`, 16 ms apart, the head routed and placed each frame as the host does:
/// each frame's caret, drawn box `(x0, x1, y0, y1)`, if any, and fit.
fn head_walk(
    mut cells: Vec<Vec<RenderCell>>,
    path: &[(u16, u16)],
    key_ms: u64,
    frames: u64,
    body: CursorBody,
) -> Vec<HeadFrame> {
    let t0 = Instant::now();
    let mut fx = EffectsPipeline::default();
    let cfg = rainbow_kitty();
    let mut sprites = Vec::new();
    fx.cursor_mut()
        .glow
        .tick(Some(path[0]), t0, &cfg, glow_geom(), &mut sprites);
    let cat = &mut fx.companions_mut().cursor_cat;
    // The head is a collection hello, on glass at once. The glide is the pet setting's flier,
    // summoned by a run of keys and kept by the walk's own keys, so its pose is the glide's
    // (`CursorCat::glides`), not a hello's.
    let flier = match body {
        CursorBody::Glide(face) => crate::kitty_pet::glide_species(face),
        CursorBody::Head => None,
    };
    let t0 = match flier {
        Some(species) => {
            cat.set_body(Some(species));
            let mut at = t0;
            while !cat.is_active() {
                cat.on_key(at, true);
                at += Duration::from_millis(25);
                assert!(
                    at < t0 + Duration::from_secs(10),
                    "a 25 ms run summons the flier"
                );
            }
            at
        }
        None => {
            cat.on_collect(t0, KittyLook::for_app("claude"));
            cat.set_collection_presentable(t0, true);
            t0
        }
    };
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut fold = None;
    for f in 0..frames {
        let now = t0 + Duration::from_millis(16 * f);
        // The flier's keys go on at the walk's pace past its end, holding it on glass.
        if flier.is_some() && (f * 16) % key_ms < 16 {
            fx.companions_mut().cursor_cat.on_key(now, true);
        }
        if f * 16 >= (at as u64 + 1) * key_ms && at + 1 < path.len() {
            let (r, c) = path[at];
            cells[usize::from(r)][usize::from(c)].ch = 'x';
            at += 1;
            if path[at].0 != r {
                fold = Some(now);
            }
        }
        let cell = path[at];
        let mut frame = fx.companions_mut().cursor_cat.frame(now);
        frame.body = body;
        let seat = fx.route_flying_head(
            CompanionDuty::FlyingHead { cell },
            &cells,
            now,
            GEOM,
            &mut frame,
            true,
        );
        let mut motion = fx.companions().cursor_cat.placement_frame(now);
        // The line fold as the glow classifies a wrap and the cat clocks it.
        motion.fold = fold.and_then(|started| {
            let progress = now.saturating_duration_since(started).as_secs_f32()
                / crate::kitty_cursor::CURSOR_FOLD_DURATION.as_secs_f32();
            (progress < 1.0).then_some(crate::kitty_cursor::CatFoldFrame {
                started,
                direction: crate::kitty_cursor::CatFoldDirection::Forward,
                progress,
            })
        });
        let placed = fx.prepare_flying_head(
            KittyCursorLayout {
                lift: seat.lift,
                ..layout(cell, &frame)
            },
            motion,
            frame.pose,
            &cells,
        );
        let drawn = placed
            .and_then(|p| p.drawn_footprint())
            .map(|r| (r.x, r.x + i32::from(r.w), r.y, r.y + i32::from(r.h)));
        // Drawn as the host draws it: the placement's fold reads the last frame it emitted.
        if let Some(placed) = placed {
            let _ = fx.draw_flying_head(
                CanvasInk::Window,
                crate::word_decorations::KittyCursorFrame {
                    geom: GEOM,
                    cursor: cell,
                    look: frame.render_look(),
                    body: frame.body,
                    colors: crate::cat_baker::CatColorKey::default(),
                    bob: frame.bob,
                    lift: seat.lift,
                    alpha: 255,
                    pose: frame.pose,
                    sing: 0.0,
                    notes: [None; crate::kitty_sing::MAX_NOTES],
                },
                placed,
            );
        }
        out.push((cell, drawn, fx.flying_fit.at(now)));
    }
    out
}

/// The caret walked along the grid's last row under a page whose every other row holds a
/// letter in every cell, the one over the caret among them: each frame's caret, drawn box and
/// fit, `(row, col, x0, x1, y0, y1, fit)`.
fn last_row_walk(
    from: u16,
    to: u16,
    key_ms: u64,
    frames: u64,
    body: CursorBody,
) -> Vec<(u16, u16, i32, i32, i32, i32, f32)> {
    let mut cells = blank();
    for row in (0..=22).step_by(2) {
        for c in &mut cells[row] {
            c.ch = 'x';
        }
    }
    for c in &mut cells[23][..usize::from(from)] {
        c.ch = 'x';
    }
    let path: Vec<(u16, u16)> = (from..=to).map(|col| (23, col)).collect();
    head_walk(cells, &path, key_ms, frames, body)
        .into_iter()
        .map(|((row, col), drawn, fit)| {
            let (x0, x1, y0, y1) = drawn.expect("the head is drawn in every frame");
            (row, col, x0, x1, y0, y1, fit)
        })
        .collect()
}

/// A HEAD ON ITS WAY ACROSS A WRAP IS NOT FITTED FOR IT. The fold takes it off the right edge
/// and in from the left, over the letters of the row it comes to, to a whole seat clear over
/// the line: fitted from a frame of that way, it stayed fitted, and ran on along the new row's
/// letters (the trail gallery's `focus-wrap`, tail06-e, on every page).
#[test]
fn a_head_crossing_a_wrap_is_not_fitted_for_its_way_across() {
    let mut cells = blank();
    for c in &mut cells[8] {
        c.ch = 'x';
    }
    for c in &mut cells[9][..112] {
        c.ch = 'x';
    }
    for c in &mut cells[11][..8] {
        c.ch = 'x';
    }
    let mut path: Vec<(u16, u16)> = (112..120).map(|col| (9, col)).collect();
    path.extend((0..8).map(|col| (10, col)));
    let walk = head_walk(cells, &path, 60, 300, CursorBody::Head);
    assert!(
        walk.iter().any(|f| f.0.0 == 10),
        "fixture: the caret reached the row the wrap brings it to"
    );
    let fitted: Vec<_> = walk.iter().filter(|f| f.2 < 1.0).collect();
    assert!(
        fitted.is_empty(),
        "{} frames fitted for a way with a whole seat at its end, the first {:?}",
        fitted.len(),
        fitted.first()
    );
}

/// THE HEAD FITS ITS ROW WHERE NO SEAT HOLDS IT WHOLE ([`companion::HeadFit`]): typed along a
/// full page's last row, under a row of letters in every cell, the head is drawn in every frame,
/// and from its ease's end its box lies inside the caret's row and off the caret's cell and every
/// letter, to the last column it has room past the caret, and is never fitted under
/// [`companion::HEAD_FIT_FLOOR`] of itself. Its fit only falls with the room, so it does not
/// pulse; at the last column it is still fitted to its row, at the grid's end. Negative control:
/// without the fit the head lies on the row above.
#[test]
fn the_head_fits_its_row_where_no_seat_holds_it_whole() {
    let ch = i32::from(GEOM.cell_h);
    let cw = i32::from(GEOM.cell_w);
    let body = CursorBody::Head;
    let walk = last_row_walk(60, 116, 50, 1_200, body);
    let settled = (0.3 * 1000.0 / 16.0) as usize;
    let mut least = 1.0f32;
    for &(row, col, x0, x1, y0, y1, fit) in &walk[settled..] {
        assert!(
            y0 >= i32::from(row) * ch && y1 <= (i32::from(row) + 1) * ch,
            "caret ({row}, {col}): the head's box {x0}..{x1} x {y0}..{y1} leaves its row"
        );
        assert!(
            x0 >= (i32::from(col) + 1) * cw,
            "caret ({row}, {col}): the head's box {x0}..{x1} x {y0}..{y1} meets the caret or the \
             letters behind it"
        );
        assert!(
            fit <= least,
            "caret ({row}, {col}): the head's fit grew back from {least} to {fit}"
        );
        least = fit;
    }
    assert!(
        least >= crate::rainbow_kitty::companion::HEAD_FIT_FLOOR,
        "the head stayed big enough to read: fit {least}"
    );
    crate::rainbow_kitty::companion::set_head_fit_unread(true);
    let whole = last_row_walk(60, 116, 50, 1_200, body);
    crate::rainbow_kitty::companion::set_head_fit_unread(false);
    let &(row, _, _, _, y0, ..) = whole.last().expect("frames");
    assert!(
        y0 < i32::from(row) * ch,
        "negative control: without the fit the head still kept off the row above"
    );
    let to_the_end = last_row_walk(60, 119, 50, 1_400, body);
    let &(row, col, x0, x1, y0, y1, _) = to_the_end.last().expect("frames");
    assert_eq!(col, 119, "the caret reached the last column");
    assert!(
        y0 >= i32::from(row) * ch && y1 <= (i32::from(row) + 1) * ch,
        "at the last column the head's box {x0}..{x1} x {y0}..{y1} left its row"
    );
}

/// THE GLIDE FITTED BY THE WALL RISES, NEVER SHRINKS ([`companion::head_fit_seat`]): typed along
/// the same last row to the grid's last column, the flier's glide is drawn in every frame, and
/// from its ease's end its box lies inside one row and off the caret's cell and every letter: on
/// the caret's row at its row's size while there is room past the caret, then, where the grid's
/// end leaves none, on the blank row above the full one, at the same size, gone out and come in
/// there across the full row ([`crate::word_decorations::HEAD_FLIGHT`]'s fade), never over it.
/// Its size never changes, and it changes rows once. At 0a12236dc7b it shrank with the room to a
/// quarter of itself (`kitty_cursor_fit_floor`), at full alpha a key at a time, to keep off the
/// caret. Negative control (`FIT_RISES_OFF`, the glide shrinking with the room as the head does):
/// at the last column its box, no smaller than the floor, lies on the caret.
#[test]
fn the_glide_fitted_by_the_wall_rises_and_never_shrinks() {
    let ch = i32::from(GEOM.cell_h);
    let cw = i32::from(GEOM.cell_w);
    let body = CursorBody::Glide(PetGlyphId::PetFly);
    let walk = last_row_walk(60, 119, 50, 1_400, body);
    let settled = (0.3 * 1000.0 / 16.0) as usize;
    let mut seat: Option<(i32, f32)> = None;
    let mut rose = false;
    for &(row, col, x0, x1, y0, y1, fit) in &walk[settled..] {
        let seat_row = (y0 + y1) / 2 / ch;
        assert!(
            y0 >= seat_row * ch && y1 <= (seat_row + 1) * ch,
            "caret ({row}, {col}): the glide's box {x0}..{x1} x {y0}..{y1} lies across rows"
        );
        // The page: every even row above the last lettered, the last row up to the caret.
        let lettered = |r: i32, c: i32| {
            (r < i32::from(row) && r % 2 == 0) || (r == i32::from(row) && c < i32::from(col))
        };
        for r in y0 / ch..=(y1 - 1) / ch {
            for c in x0 / cw..=(x1 - 1) / cw {
                assert!(
                    !lettered(r, c) && (r, c) != (i32::from(row), i32::from(col)),
                    "caret ({row}, {col}): the glide's box {x0}..{x1} x {y0}..{y1} meets cell \
                     ({r}, {c})"
                );
            }
        }
        assert!(
            fit >= crate::rainbow_kitty::companion::HEAD_FIT_FLOOR,
            "caret ({row}, {col}): fitted to {fit}, under the floor"
        );
        if let Some((r, f)) = seat {
            assert!(
                (fit - f).abs() < 1e-4,
                "caret ({row}, {col}): the glide's fit went from {f} to {fit}"
            );
            if r != seat_row {
                assert!(!rose, "the glide changed rows twice");
                rose = true;
            }
        }
        seat = Some((seat_row, fit));
    }
    assert!(
        rose,
        "the grid's end left no room past the caret, and the glide took the blank row above"
    );
    crate::rainbow_kitty::companion::FIT_RISES_OFF.with(|c| c.set(true));
    let shrunk = last_row_walk(60, 119, 50, 1_400, body);
    crate::rainbow_kitty::companion::FIT_RISES_OFF.with(|c| c.set(false));
    let &(row, col, x0, x1, ..) = shrunk.last().expect("frames");
    assert!(
        x0 < (i32::from(col) + 1) * cw && x1 > i32::from(col) * cw,
        "negative control: shrinking with the room, the glide at ({row}, {col}) still kept off the \
         caret: {x0}..{x1}"
    );
}

/// §7.2(a): a head not yet drawn anywhere (nothing to fly from,
/// [`a_far_caret_is_flown_to_from_where_the_head_was_drawn`] flies one that was) is placed at
/// the landing on the jump's frame, the spine is floored at `disp ≥ 0.97` from a cooled head,
/// the whip reads `lead == −0.30` at `t₀`, the eyes squint through the flight and 200 ms past
/// arrival, and the squash lands on `t₀ + T`.
#[test]
fn the_flying_head_is_at_the_landing_on_frame_zero_and_squashes_on_arrival() {
    let t0 = Instant::now();
    let t1 = t0 + Duration::from_millis(1_500);
    let Some(CompanionImpulse::Meteor {
        t0: spawn,
        t_flight,
        ..
    }) = fly(t0, t1, LANDING)
        .cursor_mut()
        .glow
        .take_companion_impulse()
    else {
        panic!("a 40-cell licensed jump mints a meteor");
    };
    let mut fx = fly(t0, t1, LANDING);
    let mut frame = fx.companions_mut().cursor_cat.frame(t1);
    let before = frame.pose;
    let cooled = fx.companions().cursor_cat.spine();
    assert!(cooled < 0.97, "precondition: the spine cooled to {cooled}");
    fx.route_flying_head(
        CompanionDuty::FlyingHead { cell: LANDING },
        &blank(),
        t1,
        GEOM,
        &mut frame,
        true,
    );
    let rest = fx
        .decos()
        .kitty_cursor_footprint(layout(LANDING, &frame))
        .expect("a rest at the landing");
    let placement_frame = fx.companions().cursor_cat.placement_frame(t1);
    let placed = fx
        .decos_mut()
        .resolve_kitty_cursor_placement(layout(LANDING, &frame), placement_frame)
        .expect("the head is placed");
    assert_eq!(
        (placed.sample.x, placed.sample.y),
        (rest.x, rest.y),
        "frame zero is at the landing, not gliding from the launch"
    );
    let cat = &fx.companions().cursor_cat;
    assert!(
        cat.spine() >= 0.97,
        "spine floored: {} (was {cooled})",
        cat.spine()
    );
    assert_eq!(spawn, t1, "the meteor is minted on the echo tick");
    assert!(
        (frame.pose.lead - (-0.30)).abs() < 1e-4,
        "the whip at t0: lead {} (before the meteor: {})",
        frame.pose.lead,
        before.lead
    );
    assert_eq!(frame.pose.eyes, EyesFrame::Happy, "the squint at t0");
    assert_eq!(cat.land_at(), Some(spawn + t_flight));
    let land = spawn + t_flight;
    let early = fx
        .companions_mut()
        .cursor_cat
        .frame(land - Duration::from_millis(1));
    let late = fx
        .companions_mut()
        .cursor_cat
        .frame(land + Duration::from_millis(16));
    assert!(
        late.pose.scale_y < early.pose.scale_y - 0.1,
        "the squash lands on the arrival edge: {} then {}",
        early.pose.scale_y,
        late.pose.scale_y
    );
    assert_eq!(late.pose.eyes, EyesFrame::Happy, "the squint past landing");
    let after = fx
        .companions_mut()
        .cursor_cat
        .frame(land + Duration::from_millis(210));
    assert_ne!(
        after.pose.eyes,
        EyesFrame::Happy,
        "the squint ends 200 ms past the arrival edge (spine {})",
        fx.companions().cursor_cat.spine()
    );

    let mut still = fly(t0, t1, LANDING);
    let mut frame = still.companions_mut().cursor_cat.frame(t1);
    let unwhipped = frame.pose;
    still.route_flying_head(
        CompanionDuty::FlyingHead { cell: LANDING },
        &blank(),
        t1,
        GEOM,
        &mut frame,
        false,
    );
    assert_eq!(frame.pose, unwhipped, "no beat for a still head");
    assert!(frame.pose.lead > 0.0, "no whip: {}", frame.pose.lead);
    assert_ne!(
        still.companions().cursor_cat.land_at(),
        Some(land),
        "no squash booked"
    );

    let mut reduced = EffectsPipeline::for_window();
    let cfg = rainbow_kitty();
    let mut out = Vec::new();
    reduced.cursor_mut().glow.set_reduced_motion(true);
    reduced
        .cursor_mut()
        .glow
        .tick(Some(FROM), t0, &cfg, glow_geom(), &mut out);
    reduced
        .companions_mut()
        .cursor_cat
        .on_collect(t0, KittyLook::for_app("claude"));
    reduced
        .companions_mut()
        .cursor_cat
        .set_collection_presentable(t0, true);
    let launch_frame = reduced.companions_mut().cursor_cat.frame(t0);
    let launch_pf = reduced.companions().cursor_cat.placement_frame(t0);
    reduced
        .decos_mut()
        .resolve_kitty_cursor_placement(layout(FROM, &launch_frame), launch_pf)
        .expect("the head is placed at its launch under reduced motion");
    reduced.flying_sky_row = Some(FROM.0);
    reduced.cursor_mut().glow.note_navigation(t1);
    reduced
        .cursor_mut()
        .glow
        .tick(Some(LANDING), t1, &cfg, glow_geom(), &mut out);
    let mut land_frame = reduced.companions_mut().cursor_cat.frame(t1);
    reduced.route_flying_head(
        CompanionDuty::FlyingHead { cell: LANDING },
        &blank(),
        t1,
        GEOM,
        &mut land_frame,
        false,
    );
    assert_eq!(
        reduced.flying_sky_row, None,
        "reduced-motion Land impulse must clear sky hysteresis"
    );
    let land_pf = reduced.companions().cursor_cat.placement_frame(t1);
    let placed_reduced = reduced
        .decos_mut()
        .resolve_kitty_cursor_placement(layout(LANDING, &land_frame), land_pf)
        .expect("the head is placed at its landing under reduced motion");
    assert_eq!(
        (placed_reduced.sample.x, placed_reduced.sample.y),
        (rest.x, rest.y),
        "reduced-motion Land impulse must teleport to the landing on frame zero"
    );

    let down = (FROM.0 + 12, FROM.1);
    let Some(CompanionImpulse::Meteor { dir, .. }) =
        fly(t0, t1, down).cursor_mut().glow.take_companion_impulse()
    else {
        panic!("a 12-row licensed jump mints a meteor");
    };
    assert!(dir.is_vertical(), "precondition: {dir:?}");
    let mut vertical = fly(t0, t1, down);
    let mut frame = vertical.companions_mut().cursor_cat.frame(t1);
    let unmoved = frame.pose;
    vertical.route_flying_head(
        CompanionDuty::FlyingHead { cell: down },
        &blank(),
        t1,
        GEOM,
        &mut frame,
        true,
    );
    assert_eq!(frame.pose, unmoved, "no beat for a vertical flight");
    assert_eq!(vertical.companions().cursor_cat.land_at(), None);

    let mut sky_fx = fly(t0, t1, (5, 78));
    sky_fx.flying_sky_row = Some(5);
    let mut frame = sky_fx.companions_mut().cursor_cat.frame(t1);
    sky_fx.route_flying_head(
        CompanionDuty::FlyingHead { cell: (5, 20) },
        &blank(),
        t1,
        GEOM,
        &mut frame,
        true,
    );
    assert_eq!(
        sky_fx.flying_sky_row, None,
        "a horizontal meteor teleport must clear stale same-row sky hysteresis"
    );
    sky_fx.flying_sky_row = Some(5);
    sky_fx.front_replaced(t1);
    assert_eq!(sky_fx.flying_sky_row, None);

    let t_post_hold = t0 + Duration::from_millis(2_900);
    sky_fx
        .companions_mut()
        .cursor_cat
        .on_key(t_post_hold - Duration::from_millis(20), true);
    let mut delight_frame = sky_fx.companions_mut().cursor_cat.frame(t_post_hold);
    assert_eq!(
        delight_frame.reaction,
        crate::kitty_cursor::CatReaction::Cruise
    );
    sky_fx
        .cursor_mut()
        .glow
        .earn_hero(t_post_hold, FROM.0, FROM.1);
    sky_fx.route_flying_head(
        CompanionDuty::FlyingHead { cell: FROM },
        &blank(),
        t_post_hold,
        GEOM,
        &mut delight_frame,
        true,
    );
    assert_eq!(
        delight_frame.reaction,
        crate::kitty_cursor::CatReaction::Delight,
        "route_flying_head must refresh cat_frame.reaction on frame 0 of Delight"
    );
    assert_eq!(
        delight_frame.pose.eyes,
        EyesFrame::Happy,
        "route_flying_head must refresh cat_frame.pose.eyes on frame 0 of Delight"
    );
}

/// One frame of the host's flying head at `cell`, as `aterm-gui` runs it (route, prepare,
/// draw): the rectangle drawn.
fn head_frame(
    fx: &mut EffectsPipeline,
    cell: (u16, u16),
    now: Instant,
    body: CursorBody,
) -> crate::word_decorations::CatFootprint {
    let mut frame = fx.companions_mut().cursor_cat.frame(now);
    frame.body = body;
    let duty = CompanionDuty::FlyingHead { cell };
    let seat = fx.route_flying_head(duty, &blank(), now, GEOM, &mut frame, true);
    let layout = KittyCursorLayout {
        lift: seat.lift,
        ..layout(cell, &frame)
    };
    let motion = fx.companions().cursor_cat.placement_frame(now);
    let placement = fx
        .prepare_flying_head(layout, motion, frame.pose, &blank())
        .expect("the head is placed");
    fx.draw_flying_head(
        CanvasInk::Window,
        crate::word_decorations::KittyCursorFrame {
            geom: GEOM,
            cursor: cell,
            look: layout.look,
            body: layout.body,
            colors: crate::cat_baker::CatColorKey::default(),
            bob: frame.bob,
            lift: layout.lift,
            alpha: 255,
            pose: frame.pose,
            sing: 0.0,
            notes: [None; crate::kitty_sing::MAX_NOTES],
        },
        placement,
    )
    .expect("the head is drawn");
    placement.drawn_footprint().expect("a drawn head")
}

/// The host's flying head drawn at `from` for half a second, then taken to `to` (a meteor's
/// jump when `licensed`, else a rest that moved): the rectangles drawn from the last frame at
/// `from` on, 16 ms apart, the instant of the jump, and the landing a meteor booked then.
fn fly_far(
    from: (u16, u16),
    to: (u16, u16),
    licensed: bool,
    body: CursorBody,
) -> (
    Vec<crate::word_decorations::CatFootprint>,
    Instant,
    Option<Instant>,
) {
    let t0 = Instant::now();
    let cfg = rainbow_kitty();
    let frame = Duration::from_millis(16);
    let mut fx = EffectsPipeline::for_window();
    let mut out = Vec::new();
    fx.cursor_mut()
        .glow
        .tick(Some(from), t0, &cfg, glow_geom(), &mut out);
    let cat = &mut fx.companions_mut().cursor_cat;
    cat.on_collect(t0, KittyLook::for_app("claude"));
    cat.set_collection_presentable(t0, true);
    let mut at = t0;
    let mut drawn = head_frame(&mut fx, from, at, body);
    for _ in 0..30 {
        at += frame;
        fx.cursor_mut()
            .glow
            .tick(Some(from), at, &cfg, glow_geom(), &mut out);
        drawn = head_frame(&mut fx, from, at, body);
    }
    let jump = at + frame;
    if licensed {
        fx.cursor_mut().glow.note_navigation(jump);
    }
    let mut path = vec![drawn];
    let mut booked = None;
    let mut t = jump;
    while t <= jump + crate::word_decorations::HEAD_FLIGHT + Duration::from_millis(200) {
        fx.cursor_mut()
            .glow
            .tick(Some(to), t, &cfg, glow_geom(), &mut out);
        path.push(head_frame(&mut fx, to, t, body));
        if t == jump {
            booked = fx.companions().cursor_cat.land_at();
        }
        t += frame;
    }
    (path, jump, booked)
}

/// A HEAD ON GLASS FLIES TO A FAR CARET FROM WHERE IT WAS DRAWN (r29 K10-3). Enter and Ctrl+U
/// are meteors, and a screen cleared puts the caret more than the two rows the follower
/// glides. The head was drawn at the landing on the jump's own frame, so it blinked across
/// the screen: 49 to 58 cells in one frame at Enter and Ctrl+U, the old ribbon still fading on
/// the line above. Now on the jump's frame it is where it was, each frame after moves it no
/// more than a fifth of the way (a 60 Hz frame of the flight's ease), it comes down where it
/// rests within the flight's time, and a meteor's squash is booked for that landing.
///
/// NEGATIVE CONTROL (`HEAD_FLIGHT_OFF`, the head placed at the landing as before): the whole
/// way on one frame, every case.
#[test]
fn a_far_caret_is_flown_to_from_where_the_head_was_drawn() {
    let frame_ms = 16u64;
    let mut minted = 0;
    let dist = |a: &crate::word_decorations::CatFootprint,
                b: &crate::word_decorations::CatFootprint| {
        (((a.x - b.x).pow(2) + (a.y - b.y).pow(2)) as f32).sqrt()
    };
    for (what, from, to, licensed, body) in [
        ("Enter", (3u16, 100u16), (5u16, 10u16), true),
        ("Ctrl+U", (3, 110), (3, 6), true),
        ("a screen cleared", (20, 30), (1, 4), false),
    ]
    .into_iter()
    .flat_map(|(what, from, to, licensed)| {
        // Held for the head and for the flier's glide.
        [CursorBody::Head, CursorBody::Glide(PetGlyphId::PetFly)]
            .map(|body| (what, from, to, licensed, body))
    }) {
        crate::word_decorations::HEAD_FLIGHT_OFF.with(|c| c.set(true));
        let (old, _, _) = fly_far(from, to, licensed, body);
        crate::word_decorations::HEAD_FLIGHT_OFF.with(|c| c.set(false));
        let old_whole = dist(&old[0], old.last().expect("frames"));
        let old_worst = old
            .windows(2)
            .map(|w| dist(&w[0], &w[1]))
            .fold(0.0f32, f32::max);
        assert!(
            old_worst > 0.8 * old_whole,
            "{what} {body:?}: negative control: the head placed at the landing moved {old_worst:.0} of \
             {old_whole:.0} px in a frame"
        );

        let (path, jump, booked) = fly_far(from, to, licensed, body);
        let (drawn, rest) = (path[0], *path.last().expect("frames"));
        let whole = dist(&drawn, &rest);
        assert!(
            whole > 300.0,
            "{what} {body:?}: precondition: a far jump ({whole} px)"
        );
        let steps: Vec<f32> = path.windows(2).map(|w| dist(&w[0], &w[1])).collect();
        assert!(
            steps[0] <= 0.25 * whole,
            "{what} {body:?}: on the jump's frame the head moved {:.0} of {whole:.0} px: {path:?}",
            steps[0]
        );
        let worst = steps.iter().copied().fold(0.0f32, f32::max);
        assert!(
            worst <= 0.2 * whole,
            "{what} {body:?}: a frame moved the head {worst:.0} of {whole:.0} px: {steps:?}"
        );
        // Down where it rests within the flight's time: the landing's squash and the whip's
        // spring settle it a few pixels more after.
        let landed = jump + crate::word_decorations::HEAD_FLIGHT;
        let landed_at = path
            .iter()
            .enumerate()
            .position(|(i, r)| i > 0 && dist(r, &rest) <= 0.03 * whole)
            .expect("it comes to rest");
        assert!(
            jump + Duration::from_millis(frame_ms * landed_at as u64)
                <= landed + Duration::from_millis(frame_ms),
            "{what} {body:?}: still flying {landed_at} frames after the jump: {path:?}"
        );
        if let Some(booked) = booked.filter(|_| licensed) {
            minted += 1;
            assert_eq!(
                booked, landed,
                "{what} {body:?}: the squash lands with the head"
            );
        }
    }
    assert!(minted > 0, "a licensed jump mints a meteor");
}

/// THE HOST DRAWS THE GLIDE'S BREATH IN WHOLE PIXELS. The flier's breath (`glide_lift`) is a
/// fraction of the cell; [`EffectsPipeline::route_flying_head`], which every host path calls,
/// rounds it to 0 or +1 device pixel (`glide_bob_cells`) before any seat or draw reads it, so no
/// pixel under the glide's outline shimmers through a sub-pixel breath. Control: the cat's own
/// frames, before routing, hold breaths that are not whole pixels, so a route that passed them
/// on would fail.
#[test]
fn the_host_draws_the_glides_breath_in_whole_pixels() {
    let t0 = Instant::now();
    let mut fx = EffectsPipeline::default();
    let cfg = rainbow_kitty();
    let mut sprites = Vec::new();
    let cell = (3, 10);
    fx.cursor_mut()
        .glow
        .tick(Some(cell), t0, &cfg, glow_geom(), &mut sprites);
    let cat = &mut fx.companions_mut().cursor_cat;
    cat.set_body(Some(crate::kitty_pet::PetSpecies::Cat));
    let mut at = t0;
    while !cat.is_active() {
        cat.on_key(at, true);
        at += Duration::from_millis(25);
        assert!(at < t0 + Duration::from_secs(10), "a run summons the flier");
    }
    let ch = f32::from(GEOM.cell_h);
    let (mut raw_fractional, mut levels) = (0, std::collections::BTreeSet::new());
    for f in 0..150_u64 {
        let now = at + Duration::from_millis(16 * f);
        let mut frame = fx.companions_mut().cursor_cat.frame(now);
        assert!(
            matches!(frame.body, CursorBody::Glide(_)),
            "frame {f} flies the glide"
        );
        let raw = frame.bob * ch;
        raw_fractional += usize::from((raw - raw.round()).abs() > 1e-3);
        fx.route_flying_head(
            CompanionDuty::FlyingHead { cell },
            &blank(),
            now,
            GEOM,
            &mut frame,
            true,
        );
        let px = frame.bob * ch;
        assert!(
            (px - px.round()).abs() < 1e-3,
            "frame {f}: the routed breath is {px} px, not a whole pixel"
        );
        levels.insert(px.round() as i32);
    }
    assert_eq!(
        levels.into_iter().collect::<Vec<_>>(),
        vec![0, 1],
        "the glide rests and rises a pixel"
    );
    assert!(
        raw_fractional > 10,
        "control: the cat's own breath is a fraction of a pixel in {raw_fractional} frames"
    );
}
