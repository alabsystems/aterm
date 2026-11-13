// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::cat_baker::EyesFrame;
use crate::kitty_cursor::CatFrame;
use crate::kitty_registry::KittyLook;
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
) -> Vec<HeadFrame> {
    let t0 = Instant::now();
    let mut fx = EffectsPipeline::default();
    let cfg = rainbow_kitty();
    let mut sprites = Vec::new();
    fx.cursor_mut()
        .glow
        .tick(Some(path[0]), t0, &cfg, glow_geom(), &mut sprites);
    let cat = &mut fx.companions_mut().cursor_cat;
    cat.on_collect(t0, KittyLook::for_app("claude"));
    cat.set_collection_presentable(t0, true);
    let mut out = Vec::new();
    let mut at = 0usize;
    let mut fold = None;
    for f in 0..frames {
        let now = t0 + Duration::from_millis(16 * f);
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
    head_walk(cells, &path, key_ms, frames)
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
    let walk = head_walk(cells, &path, 60, 300);
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
/// full page's last row, under a row of letters in every cell, up to and past where its
/// whole seat runs out at the margin, the head is drawn in every frame, and from its ease's
/// end its box lies inside the caret's row and off the caret's cell and every letter, to the
/// last column it has room past the caret. Its fit only falls with the room, so it does not
/// pulse. Negative control: without the fit the head lies on the row above.
#[test]
fn the_head_fits_its_row_where_no_seat_holds_it_whole() {
    const LAST_ROOM: u16 = 116;
    let ch = i32::from(GEOM.cell_h);
    let cw = i32::from(GEOM.cell_w);
    let walk = last_row_walk(60, LAST_ROOM, 50, 1_200);
    let settled = (0.3 * 1000.0 / 16.0) as usize;
    let mut least = 1.0f32;
    for &(row, col, x0, x1, y0, y1, fit) in &walk[settled..] {
        assert!(
            y0 >= i32::from(row) * ch && y1 <= (i32::from(row) + 1) * ch,
            "caret ({row}, {col}): the head's box {x0}..{x1} x {y0}..{y1} leaves its row"
        );
        assert!(
            x0 >= (i32::from(col) + 1) * cw,
            "caret ({row}, {col}): the head's box {x0}..{x1} x {y0}..{y1} meets the caret or \
             the letters behind it"
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
    let whole = last_row_walk(60, LAST_ROOM, 50, 1_200);
    crate::rainbow_kitty::companion::set_head_fit_unread(false);
    let &(row, _, _, _, y0, ..) = whole.last().expect("frames");
    assert!(
        y0 < i32::from(row) * ch,
        "negative control: without the fit the head still kept off the row above"
    );
    // To the last column, where no room is left past the caret: still fitted to its row, at
    // the grid's end, and not back over the row above.
    let to_the_end = last_row_walk(60, 119, 50, 1_400);
    let &(row, col, x0, x1, y0, y1, _) = to_the_end.last().expect("frames");
    assert_eq!(col, 119, "the caret reached the last column");
    assert!(
        y0 >= i32::from(row) * ch && y1 <= (i32::from(row) + 1) * ch,
        "at the last column the head's box {x0}..{x1} x {y0}..{y1} left its row"
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
) -> crate::word_decorations::CatFootprint {
    let mut frame = fx.companions_mut().cursor_cat.frame(now);
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
    let mut drawn = head_frame(&mut fx, from, at);
    for _ in 0..30 {
        at += frame;
        fx.cursor_mut()
            .glow
            .tick(Some(from), at, &cfg, glow_geom(), &mut out);
        drawn = head_frame(&mut fx, from, at);
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
        path.push(head_frame(&mut fx, to, t));
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
    for (what, from, to, licensed) in [
        ("Enter", (3u16, 100u16), (5u16, 10u16), true),
        ("Ctrl+U", (3, 110), (3, 6), true),
        ("a screen cleared", (20, 30), (1, 4), false),
    ] {
        crate::word_decorations::HEAD_FLIGHT_OFF.with(|c| c.set(true));
        let (old, _, _) = fly_far(from, to, licensed);
        crate::word_decorations::HEAD_FLIGHT_OFF.with(|c| c.set(false));
        let old_whole = dist(&old[0], old.last().expect("frames"));
        let old_worst = old
            .windows(2)
            .map(|w| dist(&w[0], &w[1]))
            .fold(0.0f32, f32::max);
        assert!(
            old_worst > 0.8 * old_whole,
            "{what}: negative control: the head placed at the landing moved {old_worst:.0} of \
             {old_whole:.0} px in a frame"
        );

        let (path, jump, booked) = fly_far(from, to, licensed);
        let (drawn, rest) = (path[0], *path.last().expect("frames"));
        let whole = dist(&drawn, &rest);
        assert!(
            whole > 300.0,
            "{what}: precondition: a far jump ({whole} px)"
        );
        let steps: Vec<f32> = path.windows(2).map(|w| dist(&w[0], &w[1])).collect();
        assert!(
            steps[0] <= 0.25 * whole,
            "{what}: on the jump's frame the head moved {:.0} of {whole:.0} px: {path:?}",
            steps[0]
        );
        let worst = steps.iter().copied().fold(0.0f32, f32::max);
        assert!(
            worst <= 0.2 * whole,
            "{what}: a frame moved the head {worst:.0} of {whole:.0} px: {steps:?}"
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
            "{what}: still flying {landed_at} frames after the jump: {path:?}"
        );
        if let Some(booked) = booked.filter(|_| licensed) {
            minted += 1;
            assert_eq!(booked, landed, "{what}: the squash lands with the head");
        }
    }
    assert!(minted > 0, "a licensed jump mints a meteor");
}
