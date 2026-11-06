// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The strip box's own laws (`docs/DESIGN-stats-deck-units-2026-11-04.md` §3, §12 "Strip box
//! (R3)"), on [`skyline_box`] alone. The deck's and the lens's laws, which read the painted
//! frame, are in `stats_deck_paint_tests.rs` and `stats_lens_tests.rs`.

use super::*;
use crate::stats_deck::tests::dark_inks;
use aterm_vitals::ink::heat_step;

const AT: Rect = (10.0, 20.0, 12.0, 24.0);

fn mean(v: f32) -> BoxValue {
    BoxValue {
        mean: Some(v),
        ..BoxValue::default()
    }
}

/// A read zero draws a column one device px tall on the box's foot, in the coldest heat, never
/// nothing (`a_read_zero_draws_a_one_pixel_foot`); an unread box draws no column at all.
#[test]
fn a_read_zero_draws_a_one_pixel_foot() {
    let inks = dark_inks();
    for (w, h) in [(1.0, 1.0), (2.0, 7.0), (12.0, 24.0), (40.0, 13.0)] {
        let at = (3.0, 5.0, w, h);
        for v in [0.0, -0.2, 1e-4] {
            let sky = skyline_box(at, mean(v), 2.0, &inks, false);
            let ((x, y, cw, ch), ink) = sky.column.expect("a read box draws its column");
            assert_eq!(
                (x, y, cw, ch),
                (3.0, 5.0 + h - 1.0, w, 1.0),
                "{w}×{h} px at {v}"
            );
            assert_eq!(ink, inks.heat[0], "{w}×{h} px at {v}");
            assert_eq!(sky.plate, Some(at), "{w}×{h} px at {v}: its plate");
        }
        let unread = skyline_box(at, BoxValue::default(), 2.0, &inks, false);
        assert!(
            unread.column.is_none() && unread.plate.is_none() && unread.hollow.is_some(),
            "CONTROL {w}×{h} px: an unread box is hollow, not a foot: {unread:?}"
        );
    }
}

/// A peak tick draws only where it clears its column by at least two device px, so it always
/// stands apart from the column's top with a pixel of plate between; one device px tall, inset a
/// px each side on a box 5 px or wider, in the heat of the largest reading.
#[test]
fn a_tick_draws_only_two_pixels_or_more_above_its_column() {
    let inks = dark_inks();
    let mut drawn = 0;
    for h in [4.0_f32, 7.0, 13.0, 24.0, 48.0] {
        for w in [1.0_f32, 4.0, 5.0, 12.0] {
            for m in 0..=20 {
                for k in m..=20 {
                    let (mean_v, max_v) = (m as f32 / 20.0, k as f32 / 20.0);
                    let sky = skyline_box(
                        (0.0, 0.0, w, h),
                        BoxValue {
                            mean: Some(mean_v),
                            max: Some(max_v),
                            over: 0.0,
                        },
                        2.0,
                        &inks,
                        false,
                    );
                    let ((_, col_y, _, col_h), _) = sky.column.expect("a column");
                    let th = (max_v * h).round().clamp(1.0, h);
                    let at = format!("{w}×{h} px, mean {mean_v}, max {max_v}");
                    match sky.tick {
                        Some(((tx, ty, tw, tick_h), ink)) => {
                            drawn += 1;
                            assert!(th >= col_h + 2.0, "{at}: a tick within a px of its column");
                            assert_eq!(ty, h - th, "{at}: the tick stands at the max");
                            assert!(ty + 1.0 < col_y, "{at}: plate between tick and column");
                            assert_eq!(tick_h, 1.0, "{at}");
                            let inset = if w >= 5.0 { 1.0 } else { 0.0 };
                            assert_eq!((tx, tw), (inset, w - 2.0 * inset), "{at}");
                            assert_eq!(ink, inks.heat[usize::from(heat_step(max_v))], "{at}");
                        }
                        None => assert!(th < col_h + 2.0, "{at}: a tick that clears is missing"),
                    }
                }
            }
        }
    }
    assert!(drawn > 100, "CONTROL: ticks were drawn ({drawn})");
}

/// An over cap is as long as its box's over share of the box's width, whole px, at least one,
/// centred over the box, and none where nothing was over; it stands outside the plate, 1 device
/// px clear and 2 px tall at 2×, flush and 1 px tall at 1× (A1 §3), the rule's own px and not
/// [`cap_px`]'s.
#[test]
fn an_over_cap_is_as_long_as_its_boxs_over_share() {
    let inks = dark_inks();
    let mut lengths = std::collections::BTreeSet::new();
    for (scale, clear, cap_h) in [(1.0, 0.0, 1.0), (2.0, 1.0, 2.0)] {
        assert_eq!(cap_px(scale), (clear, cap_h), "the cap's px at {scale}×");
        for w in [1.0_f32, 3.0, 8.0, 17.0, 40.0] {
            let at = (5.0, 30.0, w, 16.0);
            for k in 0..=40 {
                let share = k as f32 / 40.0;
                let sky = skyline_box(
                    at,
                    BoxValue {
                        mean: Some(0.5),
                        max: None,
                        over: share,
                    },
                    scale,
                    &inks,
                    false,
                );
                let tag = format!("{w} px at {scale}×, over {share}");
                if share == 0.0 {
                    assert_eq!(sky.cap, None, "{tag}");
                    continue;
                }
                let (cx, cy, len, ch) = sky.cap.expect("an over box has its cap");
                let want = (share * w).round().clamp(1.0, w);
                assert_eq!(len, want, "{tag}");
                assert_eq!(ch, cap_h, "{tag}");
                assert_eq!(cy, 30.0 - clear - cap_h, "{tag}");
                let (left, right) = (cx - 5.0, 5.0 + w - (cx + len));
                assert!(
                    (left - right).abs() <= 1.0,
                    "{tag}: centred ({left}, {right})"
                );
                lengths.insert((w as u32, len as u32));
            }
        }
    }
    assert!(
        lengths.iter().filter(|&&(w, _)| w == 40).count() > 20,
        "CONTROL: a wide box's cap takes many lengths, not over or not ({lengths:?})"
    );
}

/// Under Increase Contrast every part keeps its geometry and takes `heat_ic`.
#[test]
fn increase_contrast_moves_only_the_inks() {
    let inks = dark_inks();
    let v = BoxValue {
        mean: Some(0.3),
        max: Some(0.9),
        over: 0.5,
    };
    let (plain, ic) = (
        skyline_box(AT, v, 2.0, &inks, false),
        skyline_box(AT, v, 2.0, &inks, true),
    );
    let geometry = |s: &Skyline| (s.plate, s.column.map(|c| c.0), s.tick.map(|t| t.0), s.cap);
    assert_eq!(geometry(&plain), geometry(&ic));
    assert_eq!(
        ic.column.map(|c| c.1),
        Some(inks.heat_ic[usize::from(heat_step(0.3))])
    );
    assert_eq!(
        ic.tick.map(|t| t.1),
        Some(inks.heat_ic[usize::from(heat_step(0.9))])
    );
    assert_ne!(
        plain.column.map(|c| c.1),
        ic.column.map(|c| c.1),
        "CONTROL: the ramps differ"
    );
}

/// A lens trace's box stands at its axis height and keeps the heat its lens names
/// ([`skyline_box_in`]); the deck's takes the heat of its height.
#[test]
fn a_box_on_its_own_axis_keeps_its_lens_heat() {
    let inks = dark_inks();
    let v = BoxValue {
        mean: Some(0.9),
        max: Some(1.0),
        over: 0.0,
    };
    let heat = BoxHeat {
        mean: Some(0.1),
        max: Some(0.15),
    };
    let own = skyline_box_in(AT, v, heat, 2.0, &inks, false);
    let deck = skyline_box(AT, v, 2.0, &inks, false);
    assert_eq!(own.column.map(|c| c.0), deck.column.map(|c| c.0));
    assert_eq!(own.tick.map(|t| t.0), deck.tick.map(|t| t.0));
    assert_eq!(
        own.column.map(|c| c.1),
        Some(inks.heat[usize::from(heat_step(0.1))])
    );
    assert_eq!(
        own.tick.map(|t| t.1),
        Some(inks.heat[usize::from(heat_step(0.15))])
    );
    assert_ne!(own.column.map(|c| c.1), deck.column.map(|c| c.1), "CONTROL");
}

/// An unread box's outline inks the box's own edge px, the px its plate covers once read, at 1×
/// (no radius) and 2× (radius 1), on boxes the renderer rounds and on boxes too small to round
/// ([`outline`]): rasterized, the middle of each side is inked, the inside is not, and nothing
/// outside the box is. CONTROL: the rect half a px in at no radius, as R3 first drew every outline,
/// leaves the box's left column and top row bare, so at 1× an unread box stood a px inside its
/// read neighbours' plates (the land stage's 1× strain frame, 2026-11-05).
#[test]
fn an_unread_boxs_outline_inks_its_own_edge_pixels() {
    let inks = dark_inks();
    let ground = [0, 0, 0, 0xFF];
    let (cw, ch) = (40_u32, 40_u32);
    let inked = |prims: &[DrawPrim]| -> Vec<(u32, u32)> {
        let rgba = crate::tray_raster::rasterize_tray_pixels(prims, cw, ch, 1.0, ground);
        (0..ch)
            .flat_map(|y| (0..cw).map(move |x| (x, y)))
            .filter(|&(x, y)| {
                let i = ((y * cw + x) * 4) as usize;
                rgba[i..i + 4] != ground
            })
            .collect()
    };
    let mut checked = 0;
    for radius in [0.0_f32, 1.0] {
        for (w, h) in [
            (4.0_f32, 4.0_f32),
            (4.0, 9.0),
            (5.0, 5.0),
            (7.0, 9.0),
            (14.0, 16.0),
            (23.0, 18.0),
        ] {
            let sky = skyline_box((3.0, 2.0, w, h), BoxValue::default(), 1.0, &inks, false);
            assert!(matches!(sky.hollow, Some(Hollow::Outline(_))), "{w}×{h} px");
            let px = inked(&sky.prims(&inks, radius, (0.0, 0.0)));
            let (x0, y0) = (3_u32, 2_u32);
            let (x1, y1) = (x0 + w as u32 - 1, y0 + h as u32 - 1);
            let (mx, my) = ((x0 + x1) / 2, (y0 + y1) / 2);
            let at = format!("{w}×{h} px, radius {radius}");
            for side in [(x0, my), (x1, my), (mx, y0), (mx, y1)] {
                assert!(px.contains(&side), "{at}: the edge px {side:?} is inked");
            }
            assert!(!px.contains(&(mx, my)), "{at}: the inside stays ground");
            assert!(
                px.iter()
                    .all(|&(x, y)| (x0..=x1).contains(&x) && (y0..=y1).contains(&y)),
                "{at}: nothing outside the box is inked: {px:?}"
            );
            checked += 1;
        }
    }
    assert_eq!(checked, 12);
    let half_in = DrawPrim::Stroke {
        x: 3.5,
        y: 2.5,
        w: 6.0,
        h: 8.0,
        radius: 0.0,
        width: 1.0,
        color: rgba(inks.hollow, 0xFF),
    };
    let px = inked(&[half_in]);
    assert!(
        !px.contains(&(3, 6)) && !px.contains(&(6, 2)),
        "CONTROL: half a px in, an unrounded outline misses the box's left column and top row: \
         {px:?}"
    );
}
