// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The footer card's frame, read off its strips' texels: the line wears the arc's inks, the
//! rect outside each corner's arc holds the shadow or nothing, the shadow lies whole round the
//! card's outline, no rim pixel is blended twice, a strip past the art limit is tiled, the
//! card's ground fills its body, and a card at rest builds nothing.

use super::*;
use aterm_effects::glass_inks::{Edge, perimeter_at};
use aterm_effects::harness_card::CardPalette;

const GROUND: u32 = 0x0018_1818;
const INK: u32 = 0x00E0_E0E0;
const RULE: u32 = 0x005A_5A5A;

fn arc(palette: CardPalette) -> PlateLine {
    PlateLine::Arc(GlassInks::of(Some(palette), GROUND, INK, RULE))
}

/// A card of `cells` at `scale` device pixels a point, its cells 7 by 16 points.
fn key_at(cells: (usize, usize), scale: f64, line: PlateLine, shadow: bool) -> PlateKey {
    let cell = (
        (7.0 * scale).round() as usize,
        (16.0 * scale).round() as usize,
    );
    PlateKey::new(cells, cell, scale, GROUND, line, shadow)
}

/// The frame's premultiplied texel at (`x`, `y`) from the card's top left corner, clear where
/// no sprite lies; no two sprites share a pixel.
fn pixel(frame: &PlateFrame, x: i32, y: i32) -> [u8; 4] {
    let mut found = None;
    for item in frame.items() {
        let FxItem::Sprite(s) = item else {
            panic!("a rastered frame is sprites: {item:?}");
        };
        let (dx, dy) = (x - s.x, y - s.y);
        if (0..i32::from(s.dw)).contains(&dx) && (0..i32::from(s.dh)).contains(&dy) {
            let at = (dy as usize * usize::from(s.art.w()) + dx as usize) * 4;
            let t = &s.art.data()[at..at + 4];
            assert!(found.is_none(), "two sprites at ({x}, {y})");
            found = Some([t[0], t[1], t[2], t[3]]);
        }
    }
    found.unwrap_or([0; 4])
}

fn holds(b: PxRect, x: i32, y: i32) -> bool {
    (b.x0..b.x1).contains(&x) && (b.y0..b.y1).contains(&y)
}

fn opaque(ink: u32) -> [u8; 4] {
    let [r, g, b] = unpack(ink);
    [r, g, b, 255]
}

/// THE LINE WEARS THE ARC AT EIGHT POINTS ROUND THE CARD: the middle of each side and a slice
/// beside each corner's arc are the arc's ink at that slice's own middle, opaque, clockwise
/// from the top left as the launch card's border runs; inside the line the card's ground is
/// opaque. Negative control: the same card under a flat line is one ink at all eight.
#[test]
fn the_line_wears_the_arc_at_eight_perimeter_points() {
    for palette in [CardPalette::Cruise, CardPalette::Land] {
        let (cols, rows) = (36usize, 10usize);
        let inks = GlassInks::of(Some(palette), GROUND, INK, RULE);
        let key = key_at((cols, rows), 1.0, PlateLine::Arc(inks), true);
        let frame = plate_frame(&key);
        let (w, h) = key.size();
        let (cw, ch) = (7.0f32, 16.0f32);
        let (wide, tall) = (cols as f32, rows as f32 * CELL_ASPECT);
        // The arc's fraction of a point of an edge: the one mapping the cell arc takes
        // (`glass_inks::perimeter`, pinned to the launch card's in aterm-effects).
        let s = |edge: Edge, at: f32| perimeter_at(edge, at, wide, tall);
        // The middle of the slice a pixel lies in, in pixels.
        let mid = |p: i32, cell: usize| {
            let k = (0..)
                .find(|k| ((k + 1) * cell / ARC_STEPS) as i32 > p)
                .unwrap();
            ((k * cell / ARC_STEPS) + ((k + 1) * cell / ARC_STEPS)) as f32 / 2.0
        };
        let top = |x: i32| (x, 0, s(Edge::Top, mid(x, 7) / cw));
        let bottom = |x: i32| (x, h - 1, s(Edge::Bottom, mid(x, 7) / cw));
        let right = |y: i32| (w - 1, y, s(Edge::Right, mid(y, 16) / ch * CELL_ASPECT));
        let left = |y: i32| (0, y, s(Edge::Left, mid(y, 16) / ch * CELL_ASPECT));
        let points = [
            top(w / 2),
            top(w - 20),
            right(h / 2),
            right(h - 20),
            bottom(w / 2),
            bottom(20),
            left(h / 2),
            left(20),
        ];
        let mut seen = std::collections::BTreeSet::new();
        for (x, y, along) in points {
            let want = opaque(inks.border_at(along));
            assert_eq!(pixel(&frame, x, y), want, "{palette:?} ({x}, {y})");
            seen.insert(want);
        }
        assert!(
            seen.len() > 4,
            "{palette:?}: the arc, not one ink: {seen:?}"
        );
        // A side's slice at a cell's centre row wears the ink the cell arc gives that cell.
        let cell_ink = |r: usize, c: usize| {
            opaque(inks.border_at(aterm_effects::glass_inks::perimeter(r, c, rows, cols)))
        };
        let near = |a: [u8; 4], b: [u8; 4]| (0..3).all(|k| a[k].abs_diff(b[k]) <= 6);
        for r in [2usize, rows / 2, rows - 3] {
            let y = (r * 16 + 8) as i32;
            assert!(
                near(pixel(&frame, w - 1, y), cell_ink(r, cols - 1)),
                "{palette:?}: the right side at row {r}"
            );
            assert!(
                near(pixel(&frame, 0, y), cell_ink(r, 0)),
                "{palette:?}: the left side at row {r}"
            );
        }
        assert_eq!(
            pixel(&frame, w / 2, 1),
            opaque(GROUND),
            "ground in the line"
        );
        assert_eq!(pixel(&frame, w / 2, h - 2), opaque(GROUND));

        let flat = plate_frame(&key_at((cols, rows), 1.0, PlateLine::Flat(RULE), true));
        for (x, y, _) in points {
            assert_eq!(pixel(&flat, x, y), opaque(RULE), "CONTROL: a flat line");
        }
    }
}

/// THE NEUTRAL FRAMES: a flat line is its one ink round the whole card, rounded as the arc's
/// is, and with no shadow (a forced palette) the frame writes nothing outside the card's rect
/// and reaches no margin. Negative control: the same card with its shadow writes below it.
#[test]
fn a_flat_line_is_one_ink_and_no_shadow_writes_nothing_outside_the_rect() {
    let bare = key_at((30, 6), 1.0, PlateLine::Flat(RULE), false);
    assert_eq!(bare.margin(), (0, 0));
    let (w, h) = bare.size();
    assert_eq!(bare.extent(), PxRect::new(0, 0, w, h));
    let frame = plate_frame(&bare);
    for x in 12..w - 12 {
        assert_eq!(pixel(&frame, x, 0), opaque(RULE), "top at {x}");
        assert_eq!(pixel(&frame, x, h - 1), opaque(RULE), "bottom at {x}");
    }
    for y in 12..h - 12 {
        assert_eq!(pixel(&frame, 0, y), opaque(RULE), "left at {y}");
        assert_eq!(pixel(&frame, w - 1, y), opaque(RULE), "right at {y}");
    }
    for item in frame.items() {
        let b = item.bounds();
        assert!(
            b.x0 >= 0 && b.y0 >= 0 && b.x1 <= w && b.y1 <= h,
            "outside the rect: {b:?}"
        );
    }
    let cast = plate_frame(&key_at((30, 6), 1.0, PlateLine::Flat(RULE), true));
    assert!(
        pixel(&cast, w / 2, h + 2)[3] > 0,
        "CONTROL: no shadow below"
    );
}

/// OUTSIDE EACH CORNER'S ARC THE RECT HOLDS NO CARD, at every scale: the rect's two top corner
/// pixels draw nothing, so those corner cells show their own ground, and its two bottom corner
/// pixels hold the shadow alone (translucent black), as a list overlay's corners do, never the
/// card's ground or line; the line's own pixels a radius along each edge are opaque. Negative
/// controls: a frame of square corners (the straight line) inks its corner pixels, and a card
/// with no shadow leaves all four clear.
#[test]
fn outside_each_arc_the_rect_holds_the_shadow_or_nothing() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let key = key_at((30, 6), scale, arc(CardPalette::Cruise), true);
        let frame = plate_frame(&key);
        let (w, h) = key.size();
        for (x, y) in [(0, 0), (w - 1, 0)] {
            assert_eq!(
                pixel(&frame, x, y),
                [0; 4],
                "{scale}: the top corner ({x}, {y})"
            );
        }
        let bare = plate_frame(&key_at((30, 6), scale, arc(CardPalette::Cruise), false));
        for (x, y) in [(0, h - 1), (w - 1, h - 1)] {
            let [r, g, b, a] = pixel(&frame, x, y);
            assert!(
                (r, g, b) == (0, 0, 0) && a > 0 && a < 255,
                "{scale}: the bottom corner ({x}, {y}) is the shadow: {:?}",
                [r, g, b, a]
            );
            assert_eq!(
                pixel(&bare, x, y),
                [0; 4],
                "CONTROL {scale}: no shadow, a clear corner"
            );
        }
        let r = key.radius().ceil() as i32 + 1;
        for (x, y) in [(r, 0), (w - 1 - r, 0), (0, r), (w - 1, h - 1 - r)] {
            assert_eq!(
                pixel(&frame, x, y)[3],
                255,
                "{scale}: the line at ({x}, {y})"
            );
        }
        let square = straight_frame(&key);
        assert!(
            square.items().iter().any(|item| holds(item.bounds(), 0, 0)),
            "CONTROL: a square frame leaves its corner clear"
        );
    }
}

/// THE SHADOW LIES WHOLE ROUND THE CARD'S OUTLINE, at 1x and at 2x: below the card it reaches
/// 8 points and no farther, beside it 3 points, and the strips hold it to its last pixel (the
/// margin's outermost row and column are clear); inside the rect every texel that draws is the
/// opaque ground or line, but at a corner's arc, where the line's antialiased edge and, outside
/// it, the shadow lie; along the straight edges the shadow's translucent black never shows
/// inside the rect. Negative control: a margin of the 12 device pixels a list overlay's raster
/// cuts would cut the 2x shadow.
#[test]
fn the_shadow_lies_whole_round_the_cards_outline() {
    for scale in [1.0, 2.0] {
        let key = key_at((30, 6), scale, arc(CardPalette::Cruise), true);
        let frame = plate_frame(&key);
        let ((w, h), (side, below)) = (key.size(), key.margin());
        let pt = scale as i32;
        assert!(pixel(&frame, w / 2, h)[3] > 0, "{scale}: a shadow below");
        assert!(
            pixel(&frame, w / 2, h + 8 * pt - 1)[3] > 0,
            "{scale}: to 8 pt"
        );
        assert_eq!(
            pixel(&frame, w / 2, h + 8 * pt)[3],
            0,
            "{scale}: no farther"
        );
        assert!(pixel(&frame, -1, h / 2)[3] > 0, "{scale}: beside it");
        assert!(pixel(&frame, -3 * pt, h / 2)[3] > 0, "{scale}: to 3 pt");
        assert_eq!(pixel(&frame, -3 * pt - 1, h / 2)[3], 0, "{scale}");
        assert!(pixel(&frame, w + 3 * pt - 1, h / 2)[3] > 0, "{scale}");
        for x in -side..w + side {
            assert_eq!(pixel(&frame, x, h + below - 1)[3], 0, "{scale}: cut below");
        }
        for y in 0..h + below {
            assert_eq!(pixel(&frame, -side, y)[3], 0, "{scale}: cut at the left");
            assert_eq!(
                pixel(&frame, w + side - 1, y)[3],
                0,
                "{scale}: cut at the right"
            );
        }
        let near_arc = key.radius().ceil() as i32 + 1;
        for y in 0..h {
            let edge = |x: &i32| *x < key.line_px() || *x >= w - key.line_px();
            for x in (0..w).filter(|x| y < key.band() || y >= h - key.band() || edge(x)) {
                let [r, g, b, a] = pixel(&frame, x, y);
                let corner =
                    (x < near_arc || x >= w - near_arc) && (y < near_arc || y >= h - near_arc);
                assert!(
                    a == 255 || corner,
                    "{scale}: ({x}, {y}) inside the rect is {a} deep"
                );
                assert!(
                    a == 0 || corner || r.max(g).max(b) > 0,
                    "{scale}: the shadow's black at ({x}, {y}) inside the rect"
                );
            }
        }
        if scale == 2.0 {
            assert!(below > 12, "CONTROL: 12 px hold the 2x shadow");
        }
    }
}

/// NO RIM PIXEL IS BLENDED TWICE, at scales 1, 1.25, 1.5 and 2: the arc's slices meet on whole
/// device pixels, so along each straight edge every pixel of the line is opaque and exactly
/// one slice's ink, never a blend of two neighbours. Negative control: a blend of two
/// neighbouring slices' inks is neither.
#[test]
fn no_rim_pixel_is_blended_twice() {
    for scale in [1.0, 1.25, 1.5, 2.0] {
        let inks = GlassInks::of(Some(CardPalette::Cruise), GROUND, INK, RULE);
        let key = key_at((44, 9), scale, PlateLine::Arc(inks), true);
        let frame = plate_frame(&key);
        let (w, h) = key.size();
        let slices: std::collections::BTreeSet<[u8; 4]> = raster_slices(&key, &inks)
            .into_iter()
            .map(|(_, ink)| opaque(ink))
            .collect();
        assert!(slices.len() > 16, "{scale}: {} inks", slices.len());
        let r = key.radius().ceil() as i32 + 1;
        let line = key.line_px();
        for x in r..w - r {
            for d in 0..line {
                let (top, bottom) = (pixel(&frame, x, d), pixel(&frame, x, h - 1 - d));
                assert!(slices.contains(&top), "{scale}: top ({x}, {d}) {top:?}");
                assert!(slices.contains(&bottom), "{scale}: bottom {x} {bottom:?}");
            }
        }
        for y in r..h - r {
            for d in 0..line {
                let (left, right) = (pixel(&frame, d, y), pixel(&frame, w - 1 - d, y));
                assert!(slices.contains(&left), "{scale}: left {y} {left:?}");
                assert!(slices.contains(&right), "{scale}: right {y} {right:?}");
            }
        }
        let mut two = slices.iter();
        let (a, b) = (two.next().unwrap(), two.nth(8).unwrap());
        let blend = [
            a[0] / 2 + b[0] / 2,
            a[1] / 2 + b[1] / 2,
            a[2] / 2 + b[2] / 2,
            255,
        ];
        assert!(
            !slices.contains(&blend),
            "CONTROL: a blend is a slice's ink"
        );
    }
}

/// A STRIP PAST THE ART LIMIT IS TILED: a card wider than [`FX_ART_MAX_SIDE`] pixels draws its
/// top row as sprites of at most that side that abut, the line unbroken across the seam, and
/// its frame stays inside the frame's art budget. Negative control: a card inside the limit
/// draws its top row as one sprite.
#[test]
fn a_strip_past_the_art_limit_is_tiled() {
    let max = i32::from(FX_ART_MAX_SIDE);
    let key = key_at((160, 4), 2.0, PlateLine::Flat(RULE), true);
    let (w, h) = key.size();
    assert!(w > max);
    let frame = plate_frame(&key);
    let top: Vec<PxRect> = frame
        .items()
        .iter()
        .map(FxItem::bounds)
        .filter(|b| b.y0 == 0 && b.y1 == key.band())
        .collect();
    assert!(top.len() >= 2, "the top row in {} sprites", top.len());
    let mut texels = 0u64;
    for item in frame.items() {
        let b = item.bounds();
        assert!(b.x1 - b.x0 <= max && b.y1 - b.y0 <= max, "{b:?}");
        texels += u64::from((b.x1 - b.x0).unsigned_abs()) * u64::from((b.y1 - b.y0).unsigned_abs());
    }
    assert!(
        texels < aterm_core::render::FX_FRAME_ART_TEXELS / 4,
        "{texels}"
    );
    let seam = top.iter().map(|b| b.x1).min().unwrap();
    for x in seam - 3..seam + 3 {
        assert_eq!(
            pixel(&frame, x, 0),
            opaque(RULE),
            "the line across the seam"
        );
    }
    assert_eq!(pixel(&frame, w / 2, h - 1), opaque(RULE));
    let small = plate_frame(&key_at((30, 4), 2.0, PlateLine::Flat(RULE), true));
    let one = small
        .items()
        .iter()
        .filter(|item| item.bounds().y0 == 0 && item.bounds().y1 == key.band())
        .count();
    assert_eq!(one, 1, "CONTROL: a small card's top row is tiled");
}

/// A CARD AT REST BUILDS NOTHING: asked again with an equal key, ink and rules, the kept
/// plate hands on the same content and builds no art and seals no content; a moved rule
/// seals a content and builds no art; a changed key rasters afresh. The rules are rects one
/// line tall at the middle of their rows.
#[test]
fn equal_keys_build_no_art() {
    let key = key_at((30, 8), 2.0, arc(CardPalette::Land), true);
    let rules = [PlateRule {
        row: 3,
        from: 2,
        to: 28,
    }];
    let mut plate = CardPlate::default();
    let first = plate
        .content(key, RULE, |lay| lay.rules.extend(rules))
        .expect("a frame");
    // The card's ground is the layer's first rect; the rule's is the one a line tall.
    let hairline = first
        .items()
        .iter()
        .find_map(|item| match item {
            FxItem::Rect(r) if r.h == 2 => Some(*r),
            _ => None,
        })
        .expect("the rule's rect");
    let (cw, ch) = (i32::from(key.cell.0), i32::from(key.cell.1));
    assert_eq!(
        (hairline.x, hairline.y, hairline.w, hairline.h),
        (2 * cw, 3 * ch + (ch - 2) / 2, (26 * cw) as u16, 2),
        "one point tall at the middle of row 3"
    );

    let _ = aterm_render::test_counters::take_art_builds();
    let sealed = aterm_core::render::fx_counts_here();
    let again = plate
        .content(key, RULE, |lay| lay.rules.extend(rules))
        .expect("a frame");
    assert!(Arc::ptr_eq(&first, &again), "the same content");
    assert_eq!(aterm_render::test_counters::take_art_builds(), 0);
    assert_eq!(
        aterm_core::render::fx_counts_here(),
        sealed,
        "nothing sealed"
    );

    let moved = plate
        .content(key, RULE, |lay| {
            lay.rules.push(PlateRule {
                row: 4,
                from: 2,
                to: 28,
            });
        })
        .expect("a frame");
    assert!(
        !Arc::ptr_eq(&first, &moved),
        "a moved rule is a new content"
    );
    assert_eq!(
        aterm_render::test_counters::take_art_builds(),
        0,
        "and no new art"
    );

    let wider = key_at((31, 8), 2.0, arc(CardPalette::Land), true);
    let _ = plate.content(wider, RULE, |_| {});
    assert!(
        aterm_render::test_counters::take_art_builds() > 0,
        "CONTROL: a new geometry builds no art"
    );
}

/// A PILL IS A ROUNDED BAR ON ITS ROW, at 1x and at 2x: a wash pill runs between its half
/// cells on whole pixels, the row's height less a point above and below; a track pill is two
/// points tall at the row's middle; a pill of no width or opacity is none. In the frame's
/// content and in the wash's own ([`CardWash`]) an equal pill hands on the same content and
/// seals nothing, a pill that moved to another row seals a content and builds no art, and no
/// pill is no content. A pill wider than an art may be is a square bar of its tone. Negative
/// control: the first pill of a size builds its cap art.
#[test]
fn a_pill_is_a_rounded_bar_on_its_row_and_a_moved_pill_builds_no_art() {
    let pill = |row: u16, shape: PillShape| PlatePill {
        row,
        from: 1,
        to: 59,
        shape,
        rgb: [0xE0; 3],
        alpha: 56,
    };
    for (scale, wash, track) in [(1.0, (33, 14), (39, 2)), (2.0, (66, 28), (78, 4))] {
        let key = key_at((30, 6), scale, PlateLine::Flat(INK), false);
        let cw = u32::from(key.cell.0);
        let bar = key.pill_bar(&pill(2, PillShape::Wash)).expect("a bar");
        assert_eq!(
            (bar.x, bar.w, bar.y, bar.h),
            (cw / 2, 59 * cw / 2 - cw / 2, wash.0, wash.1),
            "{scale}x: the wash"
        );
        assert_eq!((bar.rgb, bar.alpha), ([0xE0; 3], 56));
        let bar = key.pill_bar(&pill(2, PillShape::Track)).expect("a bar");
        assert_eq!((bar.y, bar.h), track, "{scale}x: the track");
        let none = PlatePill {
            to: 1,
            ..pill(2, PillShape::Wash)
        };
        assert_eq!(key.pill_bar(&none), None, "no width");
        let clear = PlatePill {
            alpha: 0,
            ..pill(2, PillShape::Wash)
        };
        assert_eq!(key.pill_bar(&clear), None, "no opacity");
    }

    let key = key_at((30, 6), 2.0, PlateLine::Flat(INK), false);
    let mut wash = CardWash::default();
    assert!(wash.content(&key, |_| {}).is_none(), "no pill, no content");
    let _ = aterm_render::test_counters::take_art_builds();
    let first = wash
        .content(&key, |out| out.push(pill(2, PillShape::Wash)))
        .expect("a wash");
    assert_eq!(
        aterm_render::test_counters::take_art_builds(),
        1,
        "CONTROL: the first pill of a size builds its cap art"
    );
    assert!(
        first
            .items()
            .iter()
            .any(|item| matches!(item, FxItem::Sprite(_))),
        "rounded ends"
    );
    let sealed = aterm_core::render::fx_counts_here();
    let again = wash
        .content(&key, |out| out.push(pill(2, PillShape::Wash)))
        .expect("a wash");
    assert!(Arc::ptr_eq(&first, &again), "the same content");
    assert_eq!(
        aterm_core::render::fx_counts_here(),
        sealed,
        "nothing sealed"
    );
    let moved = wash
        .content(&key, |out| out.push(pill(3, PillShape::Wash)))
        .expect("a wash");
    assert!(
        !Arc::ptr_eq(&first, &moved),
        "a moved pill is a new content"
    );
    assert_eq!(first.palette(), moved.palette(), "of the same tone");
    assert_eq!(
        aterm_render::test_counters::take_art_builds(),
        0,
        "and no new art"
    );

    // The frame's content carries its pills after its rules, and follows them.
    let mut plate = CardPlate::default();
    let bare = plate.content(key, RULE, |_| {}).expect("a frame");
    let tracked = plate
        .content(key, RULE, |lay| lay.pills.push(pill(2, PillShape::Track)))
        .expect("a frame");
    assert!(
        tracked.items().len() > bare.items().len(),
        "the pill's items"
    );
    let same = plate
        .content(key, RULE, |lay| lay.pills.push(pill(2, PillShape::Track)))
        .expect("a frame");
    assert!(
        Arc::ptr_eq(&tracked, &same),
        "an equal pill keeps the content"
    );
    let gone = plate.content(key, RULE, |_| {}).expect("a frame");
    assert_eq!(gone.items().len(), bare.items().len(), "a pill that left");

    // A pill wider than an art: a square bar.
    let wide = PlateKey::new((400, 3), (7, 16), 1.0, GROUND, PlateLine::Flat(INK), false);
    let all = PlatePill {
        to: 800,
        ..pill(1, PillShape::Wash)
    };
    assert!(wide.pill_bar(&all).expect("a bar").w > u32::from(FX_ART_MAX_SIDE));
    let square = CardWash::default()
        .content(&wide, |out| out.push(all))
        .expect("a wash");
    assert!(
        matches!(square.items(), [FxItem::Rect(r)] if (r.x, r.y, r.h) == (3, 17, 14)),
        "{:?}",
        square.items().len()
    );
}

/// WITH NO ART THE FRAME IS A STRAIGHT LINE IN THE SAME INKS: where a strip's art cannot be
/// built the frame degrades, in the same call, to rects: a line one point wide inside the
/// rect's edge, closed all round with square corners and no shadow, each slice in the arc's
/// ink the rastered line wears there, and the card's ground on its first and last row inside
/// the line. Under a flat line it is the same closed line in the one ink. Negative control:
/// the rastered frame is sprites.
#[test]
fn with_no_art_the_frame_is_a_straight_line_in_the_arcs_inks() {
    let inks = GlassInks::of(Some(CardPalette::Cruise), GROUND, INK, RULE);
    let key = key_at((30, 6), 2.0, PlateLine::Arc(inks), true);
    let ((w, h), band) = (key.size(), key.band());
    let rastered = plate_frame(&key);
    assert!(
        rastered
            .items()
            .iter()
            .all(|item| matches!(item, FxItem::Sprite(_))),
        "CONTROL: a rastered frame holds a rect"
    );
    for line in [PlateLine::Arc(inks), PlateLine::Flat(RULE)] {
        let key = key_at((30, 6), 2.0, line, true);
        let frame = plate_frame_with(&key, |_, _, _| None);
        assert!(!frame.items.is_empty());
        // The ink of the one rect at a pixel; no two rects share one.
        let ink_at = |x: i32, y: i32| {
            let mut at = frame.items.iter().filter_map(|item| match item {
                FxItem::Rect(r) if holds(item.bounds(), x, y) => {
                    Some(frame.palette[r.ink as usize].color)
                }
                _ => None,
            });
            let ink = at.next();
            assert_eq!(at.next(), None, "two rects at ({x}, {y})");
            ink
        };
        for item in &frame.items {
            let (b, FxItem::Rect(r)) = (item.bounds(), item) else {
                panic!("{item:?}");
            };
            assert!(b.x0 >= 0 && b.y0 >= 0 && b.x1 <= w && b.y1 <= h, "{b:?}");
            assert!(
                frame.palette[r.ink as usize].color == GROUND
                    || b.x1 - b.x0 == 2
                    || b.y1 - b.y0 == 2,
                "the line is one point wide: {b:?}"
            );
        }
        // Closed all round, the sides inside the first and the last row included, two pixels
        // deep and no deeper.
        for x in 0..w {
            for y in [0, 1, h - 2, h - 1] {
                assert_ne!(
                    ink_at(x, y).unwrap_or(GROUND),
                    GROUND,
                    "{line:?} ({x}, {y})"
                );
            }
        }
        for y in 0..h {
            for x in [0, 1, w - 2, w - 1] {
                assert_ne!(
                    ink_at(x, y).unwrap_or(GROUND),
                    GROUND,
                    "{line:?} ({x}, {y})"
                );
            }
        }
        for (x, y) in [(0, band / 2), (w - 1, h - band / 2)] {
            assert!(
                ink_at(x, y).is_some(),
                "{line:?}: a side in a band, ({x}, {y})"
            );
        }
        assert_eq!(ink_at(w / 2, 4), Some(GROUND), "the first row's ground");
        assert_eq!(ink_at(w / 2, h - 5), Some(GROUND), "the last row's ground");
        assert_eq!(ink_at(w / 2, h / 2), None, "the body is the layer's");
        if line == PlateLine::Arc(inks) {
            for (x, y) in [(w / 2, 0), (w - 1, h / 2), (w / 2, h - 1), (0, h / 2)] {
                let [r, g, b, _] = pixel(&rastered, x, y);
                assert_eq!(
                    ink_at(x, y),
                    Some(u32::from_be_bytes([0, r, g, b])),
                    "({x}, {y})"
                );
            }
        }
        let content = plate_content(
            &key,
            &frame,
            &PlateLay::default(),
            RULE,
            &mut CapArts::default(),
        )
        .expect("the straight frame draws");
        assert_eq!(content.items().len(), frame.items.len() + 1, "and the body");
    }
}

/// THE CARD'S GROUND IS OPAQUE BETWEEN ITS FIRST AND LAST ROW, CUT ROUND THE CELLS WITH A GROUND
/// OF THEIR OWN: the layer's rects of the card's ground cover every pixel between the card's
/// first and its last row inside the line exactly once, but the spans named as the cells' own,
/// which no rect touches; whole rows share one rect, so a card with no such cell adds one.
/// Negative control: with no span named the rects cover those cells too.
#[test]
fn the_cards_ground_fills_its_body_round_the_cells_that_have_their_own() {
    let key = key_at((30, 8), 2.0, PlateLine::Flat(RULE), false);
    let ((w, h), (cw, ch), line) = (key.size(), (14, 32), key.line_px());
    let own = [
        PlateRule {
            row: 3,
            from: 4,
            to: 6,
        },
        PlateRule {
            row: 3,
            from: 10,
            to: 11,
        },
        PlateRule {
            row: 6,
            from: 28,
            to: 30,
        },
    ];
    let body = |own: &[PlateRule]| {
        let (mut items, mut palette) = (Vec::new(), Vec::new());
        push_body(&key, own, &mut items, &mut palette);
        assert!(palette.iter().all(|ink| *ink == FxInk::over(GROUND, 255)));
        items
    };
    let whole = body(&[]);
    assert_eq!(whole.len(), 1, "one rect with no cell of its own");
    assert_eq!(whole[0].bounds(), PxRect::new(line, ch, w - line, h - ch));
    let cut = body(&own);
    let hole = |x: i32, y: i32| {
        own.iter().any(|span| {
            y / ch == i32::from(span.row)
                && (i32::from(span.from) * cw..i32::from(span.to) * cw).contains(&x)
        })
    };
    for y in 0..h {
        for x in 0..w {
            let inside = (ch..h - ch).contains(&y) && (line..w - line).contains(&x);
            let covers = |items: &[FxItem]| {
                items
                    .iter()
                    .filter(|item| holds(item.bounds(), x, y))
                    .count()
            };
            let want = usize::from(inside && !hole(x, y));
            assert_eq!(covers(&cut), want, "({x}, {y})");
            assert_eq!(covers(&whole), usize::from(inside), "CONTROL ({x}, {y})");
        }
    }
    assert!(cut.len() > whole.len());
}

/// THE FRAME'S LAYER LIES UNDER THE CARD'S GLYPHS ON THE CARD'S OWN CHROME: an exact layer in
/// the under slot and grid space at the card's corner, clipped as asked, naming the card's
/// occluder as its chrome so that occluder, which cuts every other layer, does not cut it; its
/// extent is the frame's ([`PlateKey::extent`]) moved to the card.
#[test]
fn the_frames_layer_lies_under_the_glyphs_on_the_cards_own_chrome() {
    let key = key_at((30, 6), 2.0, arc(CardPalette::Cruise), true);
    let content = plate_content(
        &key,
        &plate_frame(&key),
        &PlateLay::default(),
        RULE,
        &mut CapArts::default(),
    )
    .expect("a frame");
    let chrome = PxRect::new(24, 80, 444, 272);
    let layer = plate_layer(7, content, (140, 64), PxRect::EVERYWHERE, Some(chrome));
    assert_eq!(
        (layer.id, layer.slot, layer.space, layer.blend),
        (7, FxSlot::Under, FxSpace::Grid, FxBlend::Exact)
    );
    assert_eq!(layer.chrome, Some(chrome));
    assert!(!layer.bloom && !layer.knock);
    let (want, got) = (key.extent(), layer.extent());
    assert!(
        got.x0 >= want.x0 + 140
            && got.x1 <= want.x1 + 140
            && got.y0 >= want.y0 + 64
            && got.y1 <= want.y1 + 64,
        "{got:?} inside {want:?} at the card"
    );
    assert!(!got.is_empty());
}

/// A card on a page of text, as the window lays it: the card's cells, its occluder and its
/// frame's layer.
struct Scene {
    /// The page with the card's cells and occluder, and no frame layer.
    bare: aterm_core::render::RenderInput,
    /// The frame's layer, standing on the card's occluder.
    layer: FxLayer,
    /// The card's occluder, in window pixels.
    card: PxRect,
    key: PlateKey,
    inks: crate::autopilot_menu::CardInks,
    cell: (usize, usize),
    /// The card's rect, in frame cells.
    rect: crate::tab_menu::MenuRect,
}

impl Scene {
    fn with(&self, layer: FxLayer) -> aterm_core::render::RenderInput {
        let mut input = self.bare.clone();
        input.fx_layers.push(layer);
        input
    }

    /// The window pixel `(dx, dy)` from the card's top left corner, as a pixel index of a
    /// frame `width` wide.
    fn at(&self, width: usize, dx: i32, dy: i32) -> usize {
        let (x, y) = (self.card.x0 + dx, self.card.y0 + dy);
        usize::try_from(y).expect("on the frame") * width + usize::try_from(x).expect("on it")
    }
}

/// The scene on `cpu`'s cells under `palette`: a scrolled card of six rows (so its top row
/// holds the `↑` cue) with a rule in view, 30 cells wide at row 2, column 6 of a page of text
/// on a grey ground;
/// an occluder already on the page (`under`, chrome the card covers) is trimmed off the card
/// as the window trims it ([`crate::app_render::scrub_overlay_card`]).
fn scene(
    cpu: &aterm_render::Renderer,
    palette: Option<CardPalette>,
    under: Option<PxRect>,
) -> Scene {
    use crate::autopilot_menu::{CardInks, MenuRow, paint, plate_key, plate_rules};
    let theme = aterm_render::Theme::default();
    let cell = cpu.cell_size();
    let (rows, cols) = (14usize, 60usize);
    let mut term = aterm_core::terminal::Terminal::new(rows as u16, cols as u16);
    let text = "the frame crosses text \u{2588}\u{2588} MMMM ".repeat(3);
    let page: String = (1..=rows)
        .map(|r| {
            format!(
                "\x1b[{r};1H{}",
                &text[..text.char_indices().nth(cols).unwrap().0]
            )
        })
        .collect();
    // A lit page ground: the shadow lies under the glyphs and darkens the ground alone, so on
    // a black ground it draws nothing.
    term.process(format!("\x1b[?25l\x1b[48;2;96;96;96m{page}").as_bytes());
    let mut bare = term.cell_frame(rows, cols);
    let rect = crate::tab_menu::MenuRect {
        row: 2,
        col: 6,
        w: 30,
        h: 6,
    };
    let menu = vec![
        MenuRow::Header("Account".to_string()),
        MenuRow::note("first"),
        MenuRow::note("second"),
        MenuRow::Rule,
        MenuRow::note("third"),
        MenuRow::note("fourth"),
    ];
    let mut viewport = crate::menu_viewport::MenuViewport::default();
    assert!(viewport.scroll(menu.len(), 4, 2));
    let painted = paint(rect, &menu, viewport, None, theme, palette);
    assert_eq!(painted[0][rect.w - 2].ch, '\u{2191}', "the scroll cue");
    for (r, line) in painted.iter().enumerate() {
        bare.cells[rect.row + r][rect.col..rect.col + rect.w].copy_from_slice(line);
    }
    bare.fx_occluders.extend(under);
    let card =
        crate::app_render::scrub_overlay_card(&mut bare, rect, cell, (cpu.pad(), cpu.grid_top()))
            .expect("a card");
    let inks = CardInks::of(theme, palette);
    let key = plate_key(rect, &inks, cell, 1.0, false);
    let mut rules = Vec::new();
    plate_rules(rect, &menu, viewport, &mut rules);
    assert_eq!(rules.len(), 1, "the rule in view");
    // The layers the window pushes ([`crate::autopilot_menu::card_layers`]).
    let [layer, wash] = crate::autopilot_menu::card_layers(
        (rect, &menu, viewport),
        (&inks, key),
        Some(card),
        (9, 10),
        (&mut CardPlate::default(), &mut CardWash::default()),
        [],
    );
    assert!(wash.is_none(), "nothing washed");
    let layer = layer.expect("a frame");
    Scene {
        bare,
        layer,
        card,
        key,
        inks,
        cell,
        rect,
    }
}

/// A CPU renderer of the bundled font, which a GPU renderer is made of too, so both lay the
/// same cells.
pub(crate) fn bundled_cpu() -> aterm_render::Renderer {
    bundled_cpu_in(aterm_render::Theme::default())
}

/// [`bundled_cpu`] in `theme`.
pub(crate) fn bundled_cpu_in(theme: aterm_render::Theme) -> aterm_render::Renderer {
    let mut cpu = aterm_render::Renderer::from_bytes(aterm_render::embedded_font(), 18.0, theme)
        .expect("the bundled font");
    cpu.set_runtime_font_discovery(false);
    cpu
}
/// THE CPU DRAWS THE FRAME UNDER THE CARD'S GLYPHS: on a rendered frame the card's top line is
/// the arc's ink at its slice, its rule a hairline of the card's rule ink across the text's
/// span, and the pixels below the card darker for its shadow; the `↑` cue on the card's top
/// row, over the frame's opaque ground, keeps every pixel it has without the frame below the
/// line, and nothing inside the card but the line and the hairline moves. Negative controls:
/// the same layer in the over slot erases the cue; with no layer the top line is the ground.
#[test]
fn the_cpu_draws_the_frame_under_the_cards_glyphs() {
    let mut cpu = bundled_cpu();
    let s = scene(&cpu, Some(CardPalette::Cruise), None);
    let (w, h) = s.key.size();
    let bare = cpu.render_input(&s.bare);
    let (width, bare) = (bare.width, bare.pixels);
    let with = cpu.render_input(&s.with(s.layer.clone())).pixels;
    let rgb = |p: u32| p & 0x00FF_FFFF;
    let PlateLine::Arc(inks) = s.key.line else {
        panic!("Cruise wears the arc");
    };
    let top = raster_slices(&s.key, &inks)
        .into_iter()
        .find(|((x, y, sw, _), _)| *y == 0 && (*x..*x + *sw).contains(&(w / 2)))
        .expect("the slice at the top's middle")
        .1;
    assert_eq!(rgb(with[s.at(width, w / 2, 0)]), top, "the top line");
    assert_eq!(
        rgb(bare[s.at(width, w / 2, 0)]),
        s.key.ground,
        "CONTROL: with no layer the line's pixel is the ground"
    );
    let (cw, ch) = (s.cell.0 as i32, s.cell.1 as i32);
    let rule_ink = u32::from_be_bytes([0, s.inks.rule[0], s.inks.rule[1], s.inks.rule[2]]);
    let rule_y = 2 * ch + (ch - 1) / 2;
    assert_eq!(
        rgb(with[s.at(width, w / 2, rule_y)]),
        rule_ink,
        "the hairline"
    );
    assert_eq!(rgb(with[s.at(width, w / 2, rule_y - 1)]), s.key.ground);
    assert_eq!(rgb(with[s.at(width, w / 2, rule_y + 1)]), s.key.ground);
    assert_eq!(
        rgb(with[s.at(width, cw / 2, rule_y)]),
        s.key.ground,
        "inside the pad"
    );
    let below = s.at(width, w / 2, h + 1);
    assert_ne!(with[below], bare[below], "the shadow below the card");
    let luma = |p: u32| (p >> 16 & 0xFF) + (p >> 8 & 0xFF) + (p & 0xFF);
    assert!(luma(with[below]) <= luma(bare[below]), "darker");

    // The cue's cell: column w - 2 of the card's top row.
    let cue = |dx: i32, dy: i32| s.at(width, (s.rect.w as i32 - 2) * cw + dx, dy);
    let line = s.key.line_px();
    let mut inked = 0;
    for dy in line..ch {
        // The corner's arc, wider than a cell, crosses the cue's cell at its far edge.
        for dx in 0..cw.min(cw * 2 - s.key.radius().ceil() as i32 - line) {
            assert_eq!(
                with[cue(dx, dy)],
                bare[cue(dx, dy)],
                "the cue at ({dx}, {dy})"
            );
            inked += usize::from(rgb(bare[cue(dx, dy)]) != s.key.ground);
        }
    }
    assert!(inked > 0, "the cue draws");
    let over = FxLayer {
        slot: FxSlot::Over,
        ..s.layer.clone()
    };
    let erased = cpu.render_input(&s.with(over)).pixels;
    assert!(
        (line..ch).any(|dy| (0..cw).any(|dx| erased[cue(dx, dy)] != bare[cue(dx, dy)])),
        "CONTROL: a frame over the glyphs keeps the cue"
    );
    // Inside the card's middle rows nothing moves but the line and the hairline.
    for dy in ch..h - ch {
        for dx in line..w - line {
            if dy == rule_y {
                continue;
            }
            assert_eq!(
                with[s.at(width, dx, dy)],
                bare[s.at(width, dx, dy)],
                "({dx}, {dy})"
            );
        }
    }
}

/// ONLY THE CARD'S OWN OCCLUDER SPARES ITS FRAME, on the CPU's pixels: chrome the card covers
/// (an occluder pushed before it, trimmed off the card) does not cut the frame; chrome laid
/// over the card afterwards cuts it where it covers it and nowhere else; and the same layer
/// standing on no chrome is cut away inside the card by the card's own occluder while its
/// shadow, outside the card, stays.
#[test]
fn only_the_cards_own_occluder_spares_its_frame() {
    let mut cpu = bundled_cpu();
    let plain = scene(&cpu, Some(CardPalette::Land), None);
    let frame = cpu.render_input(&plain.bare);
    let (width, bare) = (frame.width, frame.pixels);
    let whole = cpu.render_input(&plain.with(plain.layer.clone())).pixels;
    let inside = |i: usize| {
        let (x, y) = ((i % width) as i32, (i / width) as i32);
        (plain.card.x0..plain.card.x1).contains(&x) && (plain.card.y0..plain.card.y1).contains(&y)
    };
    assert!((0..whole.len()).any(|i| inside(i) && whole[i] != bare[i]));
    assert!((0..whole.len()).any(|i| !inside(i) && whole[i] != bare[i]));

    // Chrome under the card: a band across its second row, pushed before the card.
    let ch = plain.cell.1 as i32;
    let band = PxRect::new(
        i32::MIN,
        plain.card.y0 + ch,
        i32::MAX,
        plain.card.y0 + 2 * ch,
    );
    let covered = scene(&cpu, Some(CardPalette::Land), Some(band));
    assert!(
        !covered
            .bare
            .fx_occluders
            .iter()
            .any(|o| *o != covered.card && o.overlaps(covered.card)),
        "the band is trimmed off the card: {:?}",
        covered.bare.fx_occluders
    );
    let under = cpu
        .render_input(&covered.with(covered.layer.clone()))
        .pixels;
    let side = plain.at(width, 0, ch + ch / 2);
    assert_eq!(
        under[side], whole[side],
        "chrome under the card leaves the line"
    );
    assert_ne!(under[side], bare[side]);

    // Chrome over the card: the same band, pushed after it.
    let mut over = plain.with(plain.layer.clone());
    over.fx_occluders.push(band);
    let cut = cpu.render_input(&over).pixels;
    assert_eq!(cut[side], bare[side], "chrome over the card cuts the line");
    let lower = plain.at(width, 0, 3 * ch + ch / 2);
    assert_eq!(cut[lower], whole[lower], "and only where it lies");

    // On no chrome the card's own occluder cuts the frame inside the card.
    let orphan = FxLayer {
        chrome: None,
        ..plain.layer.clone()
    };
    let cut = cpu.render_input(&plain.with(orphan)).pixels;
    for i in 0..cut.len() {
        let want = if inside(i) { bare[i] } else { whole[i] };
        assert_eq!(cut[i], want, "pixel ({}, {})", i % width, i / width);
    }
}

/// THE GPU DRAWS THE FRAME THE CPU DRAWS: on every backend the frame moves the card's pixels,
/// and on Metal, which is held to the CPU's bytes, every pixel the frame moves inside the card
/// on a ground both backends share is the CPU's byte for byte, under the arc and under a flat
/// line. The shadow, translucent under the page's glyphs, is held to one step a channel: a
/// glyph's soft edge over a shaded ground rounds apart on the two (13 of 4276 pixels measured
/// on Apple silicon, each one step).
#[test]
fn the_gpu_draws_the_frame_the_cpu_draws() {
    let theme = aterm_render::Theme::default();
    let gpu = aterm_gpu::GpuContext::new()
        .and_then(|ctx| aterm_gpu::GpuRenderer::from_parts(ctx, bundled_cpu(), None, theme));
    let mut gpu = match gpu {
        Ok(gpu) => gpu,
        Err(error) => {
            aterm_spec::host::could_not_run(format_args!("no GPU renderer on this host: {error}"))
        }
    };
    let mut cpu = bundled_cpu();
    assert_eq!(cpu.cell_size(), gpu.cell_size(), "both lay the same cells");
    let mut win = aterm_gpu::WindowGpu::new();
    for palette in [Some(CardPalette::Cruise), None] {
        let s = scene(&cpu, palette, None);
        let layered = s.with(s.layer.clone());
        let gpu_bare = gpu.render_input(&mut win, &s.bare, None).pixels;
        let got = gpu.render_input(&mut win, &layered, None).pixels;
        assert_ne!(got, gpu_bare, "{palette:?}: the GPU draws the frame");
        if !cfg!(target_os = "macos") {
            continue;
        }
        let cpu_bare = cpu.render_input(&s.bare).pixels.clone();
        let cpu_got = cpu.render_input(&layered).pixels.clone();
        let moved: Vec<usize> = (0..got.len())
            .filter(|&i| cpu_bare[i] == gpu_bare[i])
            .filter(|&i| got[i] != gpu_bare[i] || cpu_got[i] != cpu_bare[i])
            .collect();
        assert!(!moved.is_empty());
        let width = cpu.render_input(&layered).width;
        let step = |a: u32, b: u32| {
            (0..3).all(|c| (a >> (8 * c) & 0xFF).abs_diff(b >> (8 * c) & 0xFF) <= 1)
        };
        for i in moved {
            let (x, y) = ((i % width) as i32, (i / width) as i32);
            if (s.card.x0..s.card.x1).contains(&x) && (s.card.y0..s.card.y1).contains(&y) {
                assert_eq!(got[i], cpu_got[i], "{palette:?}: the frame at ({x}, {y})");
            } else {
                assert!(
                    step(got[i], cpu_got[i]),
                    "{palette:?}: the shadow at ({x}, {y}): {:06x} for {:06x}",
                    got[i],
                    cpu_got[i]
                );
            }
        }
    }
}
