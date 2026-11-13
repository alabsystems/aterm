// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const CELL: (usize, usize) = (9, 23);

fn m() -> Metrics {
    Metrics::for_cell(9, 23)
}

#[test]
fn the_windows_default_cell_gets_a_three_pixel_thumb_that_widens_to_six() {
    let m = m();
    assert_eq!((m.thin, m.fine, m.wide, m.margin), (3, 2, 6, 2));
    let big = Metrics::for_cell(18, 46);
    assert_eq!(
        (big.thin, big.fine, big.wide, big.margin),
        (6, 4, 12, 4),
        "2x draws 2x"
    );
    assert_eq!(Metrics::for_cell(13, 33).fine, 3, "150%");
}

#[test]
fn the_right_pane_draws_in_the_window_padding_and_an_inner_one_in_the_gap() {
    let edge = lane((1, 0, 30, 111), 111, CELL, 12, m()).expect("edge lane");
    assert_eq!((edge.right, edge.room, edge.edge), (111 * 9 + 12, 12, true));
    let (x, w) = span(edge, m().thin, m());
    assert!(x >= 111 * 9, "the resting thumb never covers a column: {x}");
    assert_eq!(x + w + m().margin, edge.right);
    let (wx, ww) = span(edge, m().wide, m());
    assert!(wx >= 111 * 9 && ww == 6, "and neither does the wide one");

    let inner = lane((1, 0, 30, 55), 111, CELL, 12, m()).expect("inner lane");
    assert_eq!((inner.right, inner.room, inner.edge), (56 * 9, 9, false));
}

/// THE THUMB IN A DIVIDER GAP sits on the hairline with the same air from
/// the text on each side, at every scale, resting or widened, and never
/// covers a column of either pane.
#[test]
fn a_gap_thumb_is_centred_on_the_hairline_clear_of_both_panes() {
    for (cw, ch) in [(9u32, 23u32), (14, 35), (18, 46), (10, 20), (7, 17)] {
        let m = Metrics::for_cell(cw, ch);
        let cell = (cw as usize, ch as usize);
        let inner = lane((0, 0, 30, 40), 81, cell, 12, m).expect("inner lane");
        let gap = (40 * cw, 41 * cw);
        let line = gap.0 + hairline_start(cw, m.stroke);
        for width in [m.thin, m.wide, m.wide + 2] {
            let (x, w) = span(inner, width, m);
            assert!(
                x >= gap.0 && x + w <= gap.1,
                "{cw}x{ch} w{width}: inside the gap: {x}+{w}"
            );
            assert!(
                (2 * x + w).abs_diff(2 * line + m.stroke) <= 1,
                "{cw}x{ch} w{width}: centred on the line at {line}: {x}+{w}"
            );
            assert!(
                x <= line && x + w >= line + m.stroke,
                "{cw}x{ch} w{width}: over the hairline"
            );
        }
        let (x, _) = span(inner, m.thin, m);
        assert!(
            x - gap.0 >= 2,
            "{cw}x{ch}: air from the text: {}",
            x - gap.0
        );
    }
    let m = m();
    let inner = lane((0, 0, 30, 40), 81, CELL, 12, m).unwrap();
    assert_eq!(
        span(inner, m.thin, m),
        (40 * 9 + 3, 3),
        "1x: 3 px each side"
    );
}

/// A FLUENT RESTING THUMB IN A GAP BESIDE THE RING covers the hairline and
/// grows away from the ring, at every scale and on either side: a stroke
/// of ground between it and the ring (which stands one stroke clear of the
/// line on its pane's side), at least two pixels from the far pane's text,
/// inside the gap.
#[test]
fn a_fluent_resting_thumb_beside_the_ring_grows_away_from_it() {
    for (cw, ch) in [
        (9u32, 23u32),
        (13, 33),
        (14, 35),
        (17, 44),
        (18, 46),
        (7, 17),
    ] {
        let m = Metrics::for_cell(cw, ch);
        let t = m.stroke;
        let cell = (cw as usize, ch as usize);
        let inner = lane((0, 0, 30, 40), 81, cell, 12, m).expect("inner lane");
        let gap = (40 * cw, 41 * cw);
        let line = gap.0 + hairline_start(cw, t);
        let ring_left = line - 2 * t;
        let ring_right = line + 2 * t;
        let w = rest_width(m, true);
        assert!(w > t, "{cw}x{ch}: wider than the line it covers");
        let mut crowded = false;
        for (side, ring_x) in [(RingSide::Left, ring_left), (RingSide::Right, ring_right)] {
            let (x, sw) = rest_span(inner, w, m, Some(side));
            assert_eq!(sw, w, "{cw}x{ch} {side:?}");
            assert!(
                x <= line && x + sw >= line + t,
                "{cw}x{ch} {side:?}: over the hairline: {x}+{sw}"
            );
            assert!(
                x >= gap.0 + 2 && x + sw + 2 <= gap.1,
                "{cw}x{ch} {side:?}: clear of both panes' text: {x}+{sw}"
            );
            let ground = |x: u32, sw: u32| {
                if ring_x >= x + sw {
                    ring_x - (x + sw)
                } else {
                    x.saturating_sub(ring_x + t)
                }
            };
            assert!(
                ground(x, sw) >= t,
                "{cw}x{ch} {side:?}: a stroke of ground from the ring: {}",
                ground(x, sw)
            );
            let (cx, csw) = span(inner, w, m);
            crowded |= ground(cx, csw) < t;
        }
        assert!(
            crowded,
            "{cw}x{ch}: CONTROL: centred, the thumb crowds one ring"
        );
        assert_eq!(rest_span(inner, w, m, None), span(inner, w, m));
        let edge = lane((0, 41, 30, 40), 81, cell, 12, m).expect("edge lane");
        assert_eq!(
            rest_span(edge, w, m, Some(RingSide::Left)),
            span(edge, w, m),
            "{cw}x{ch}: the padding"
        );
    }
}

/// THE WIDENED BAR IN A GAP BESIDE THE RING keeps to the ring's far side
/// at every scale and on either side: it covers the hairline, a stroke of
/// ground lies between it and the ring's stroke, it stays a stroke clear
/// of the far pane's text, and it is no wider than the centred track.
#[test]
fn a_widened_gap_bar_beside_the_ring_keeps_off_it() {
    for (cw, ch) in [
        (9u32, 23u32),
        (13, 33),
        (14, 35),
        (17, 44),
        (18, 46),
        (7, 17),
    ] {
        let m = Metrics::for_cell(cw, ch);
        let t = m.stroke;
        let cell = (cw as usize, ch as usize);
        let inner = lane((0, 0, 30, 40), 81, cell, 12, m).expect("inner lane");
        let gap = (40 * cw, 41 * cw);
        let line = gap.0 + hairline_start(cw, t);
        let mut covers_ring = false;
        for (side, ring_x) in [
            (RingSide::Left, line - 2 * t),
            (RingSide::Right, line + 2 * t),
        ] {
            let (x, w) = wide_span(inner, m, Some(side));
            assert!(
                x <= line && x + w >= line + t,
                "{cw}x{ch} {side:?}: over the hairline: {x}+{w}"
            );
            assert!(w <= m.wide + 2, "{cw}x{ch} {side:?}: no wider than centred");
            assert!(
                x >= gap.0 + t && x + w + t <= gap.1,
                "{cw}x{ch} {side:?}: a stroke clear of both panes' text: {x}+{w}"
            );
            let ground = if ring_x >= x + w {
                ring_x - (x + w)
            } else {
                x.saturating_sub(ring_x + t)
            };
            assert!(
                ground >= t,
                "{cw}x{ch} {side:?}: a stroke of ground from the ring: {ground}"
            );
            let (cx, cw_) = span(inner, m.wide + 2, m);
            covers_ring |= cx <= ring_x && cx + cw_ >= ring_x + t;
        }
        assert!(
            covers_ring,
            "{cw}x{ch}: CONTROL: the centred track covers the ring"
        );
        assert_eq!(wide_span(inner, m, None), span(inner, m.wide + 2, m));
        let edge = lane((0, 41, 30, 40), 81, cell, 12, m).expect("edge lane");
        assert_eq!(
            wide_span(edge, m, Some(RingSide::Left)),
            span(edge, m.wide + 2, m)
        );
    }
}

/// ONE TONE OVER THE HAIRLINE: from `solid` up the thumb is opaque in the
/// ink composed over the ground, so the hairline under it cannot show;
/// below it the solid tone fades, and over the ground every step is the
/// plain translucent ink's tone.
#[test]
fn a_thumb_over_the_ground_is_one_tone_over_the_hairline_too() {
    let (ink, ground, line) = ([0xf8, 0xf8, 0xf2], [0x28, 0x2a, 0x36], [0x5a, 0x5c, 0x68]);
    let over = |rgb: [u8; 3], alpha: u8, dst: [u8; 3]| -> [u8; 3] {
        std::array::from_fn(|i| {
            let a = u32::from(alpha);
            ((u32::from(rgb[i]) * a + u32::from(dst[i]) * (255 - a) + 127) / 255) as u8
        })
    };
    let solid = rest_alpha(ALPHA_REST, true);
    for alpha in [solid, 200, 255] {
        let (rgb, a) = over_ground(ink, ground, alpha, solid);
        assert_eq!(a, 255, "{alpha}: opaque");
        assert_eq!(
            over(rgb, a, line),
            over(rgb, a, ground),
            "{alpha}: one tone"
        );
        assert_eq!(over(rgb, a, ground), over(ink, alpha, ground), "{alpha}");
    }
    for alpha in [1u8, 40, 120, solid - 1] {
        let (rgb, a) = over_ground(ink, ground, alpha, solid);
        assert!(a < 255, "{alpha}: fading gives the hairline back");
        let want = over(ink, alpha, ground);
        let got = over(rgb, a, ground);
        assert!(
            (0..3).all(|i| want[i].abs_diff(got[i]) <= 1),
            "{alpha}: over the ground the plain ink's tone: {got:?} vs {want:?}"
        );
    }
    assert_ne!(
        over(ink, solid, line),
        over(ink, solid, ground),
        "CONTROL: translucent, two tones"
    );
}

/// ONE RESTING IDIOM UNDER THE FLUENT CHROME: the thin indicator, at the
/// same width and the same (doubled) opacity whatever lane it rests in;
/// the Classic chrome keeps its thin thumb at its own opacity.
#[test]
fn the_fluent_resting_thumb_is_one_width_and_one_ink() {
    let m = m();
    assert_eq!(rest_width(m, true), m.fine);
    assert_eq!(rest_width(m, false), m.thin);
    assert!(m.fine < m.thin && m.fine < m.wide, "thin, and widens");
    assert_eq!(rest_alpha(ALPHA_REST, true), 2 * ALPHA_REST);
    assert_eq!(rest_alpha(ALPHA_ACTIVE, true), 255, "opaque at most");
    assert_eq!(rest_alpha(ALPHA_REST, false), ALPHA_REST);
}

#[test]
fn the_track_spans_the_pane_rows_inset() {
    let l = lane((2, 0, 10, 80), 80, CELL, 12, m()).unwrap();
    assert_eq!(l.top, 2 * 23 + 2);
    assert_eq!(l.len, 10 * 23 - 4);
}

#[test]
fn no_history_no_thumb_and_the_ends_are_exact() {
    let l = lane((0, 0, 30, 80), 80, CELL, 12, m()).unwrap();
    assert_eq!(thumb(l, 30, 0, 0, m()), None);
    let (live_top, len) = thumb(l, 30, 300, 0, m()).unwrap();
    assert_eq!(
        live_top + len,
        l.top + l.len,
        "live bottom: flush with the track's end"
    );
    let (top, _) = thumb(l, 30, 300, 300, m()).unwrap();
    assert_eq!(top, l.top, "top of history: flush with the track's top");
}

#[test]
fn dragging_the_thumb_to_a_place_reads_back_the_offset_that_put_it_there() {
    let l = lane((0, 0, 30, 80), 80, CELL, 12, m()).unwrap();
    for offset in [0u32, 1, 17, 150, 299, 300] {
        let (top, _) = thumb(l, 30, 300, offset, m()).unwrap();
        let back = offset_for_top(l, 30, 300, f64::from(top), m());
        let (_, len) = thumb(l, 30, 300, 0, m()).unwrap();
        let per_px = 300.0 / f64::from(l.len - len);
        assert!(
            (f64::from(back) - f64::from(offset)).abs() <= per_px.ceil(),
            "offset {offset} -> top {top} -> {back}"
        );
    }
    assert_eq!(
        offset_for_top(l, 30, 300, -50.0, m()),
        300,
        "above the track: the top"
    );
    assert_eq!(offset_for_top(l, 30, 300, 1e6, m()), 0, "below it: live");
}

#[test]
fn the_edge_lane_takes_the_padding_and_the_gap_lane_only_its_thumb() {
    let l = lane((0, 0, 30, 80), 80, CELL, 12, m()).unwrap();
    let t = thumb(l, 30, 300, 150, m()).unwrap();
    let x = f64::from(80 * 9 + 6);
    assert_eq!(
        hit(l, t, x, f64::from(t.0 + 1), m()),
        Some(Hit::Thumb { grab: 1.0 })
    );
    assert_eq!(
        hit(l, t, x, f64::from(l.top + 1), m()),
        Some(Hit::Track { above: true })
    );
    assert_eq!(
        hit(l, t, f64::from(80 * 9 - 1), f64::from(t.0 + 1), m()),
        None,
        "text"
    );

    let inner = lane((0, 0, 30, 40), 80, CELL, 12, m()).unwrap();
    let t = thumb(inner, 30, 300, 150, m()).unwrap();
    let x = f64::from(40 * 9 + 4);
    assert!(matches!(
        hit(inner, t, x, f64::from(t.0 + 1), m()),
        Some(Hit::Thumb { .. })
    ));
    assert_eq!(
        hit(inner, t, x, f64::from(inner.top + 1), m()),
        None,
        "off the thumb, a divider gap stays the divider's"
    );
    assert_eq!(
        hit(inner, t, f64::from(40 * 9), f64::from(t.0 + 1), m()),
        None,
        "the gap's edge beside the text stays the divider's"
    );
}

#[test]
fn a_rect_is_cut_at_every_row_band() {
    let mut out = Vec::new();
    push_rect(&mut out, (40, 10, 1, 60), [80, 250, 123], 255, 23);
    let bands: Vec<(u16, u16, u16)> = out.iter().map(|q| (q.row, q.y, q.h)).collect();
    assert_eq!(
        bands,
        vec![(0, 10, 13), (1, 23, 23), (2, 46, 23), (3, 69, 1)]
    );
    assert!(out.iter().all(|q| q.alpha == 255 && q.w == 1 && q.x == 40));
}

/// Every quad is source-over (alpha > 0), lies in one row band, and the
/// bar's pixels are exactly its rectangle: corners shaved, the middle solid.
#[test]
fn a_bar_is_rounded_source_over_and_banded() {
    let mut out = Vec::new();
    legacy_bar_quads(&mut out, (100, 40, 6, 60), [200, 200, 200], 160, 23);
    assert!(!out.is_empty());
    let mut cover = std::collections::BTreeMap::new();
    for q in &out {
        assert!(q.alpha > 0, "a zero alpha would be additive light");
        assert_eq!(
            u32::from(q.y) / 23,
            u32::from(q.y + q.h - 1) / 23,
            "one band"
        );
        assert_eq!(u32::from(q.row), u32::from(q.y) / 23);
        for y in q.y..q.y + q.h {
            for x in q.x..q.x + q.w {
                assert!(cover.insert((x, y), q.alpha).is_none(), "no pixel twice");
            }
        }
    }
    assert!(
        cover
            .keys()
            .all(|&(x, y)| (100..106).contains(&x) && (40..100).contains(&y))
    );
    assert_eq!(cover.get(&(103, 70)), Some(&160), "the body is solid");
    assert!(
        cover.get(&(100, 40)).is_none_or(|a| *a < 80),
        "a corner pixel is shaved"
    );
    assert!(
        cover.get(&(102, 40)).copied().unwrap_or(0) > 0,
        "the cap's middle shows"
    );
}

#[test]
fn an_outline_is_four_edges_and_no_middle() {
    let mut out = Vec::new();
    push_outline(&mut out, (90, 46, 9, 23), 1, [80, 250, 123], 200, 23);
    assert_eq!(out.len(), 4);
    let inside = |x: u16, y: u16| {
        out.iter()
            .any(|q| x >= q.x && x < q.x + q.w && y >= q.y && y < q.y + q.h)
    };
    assert!(inside(90, 46) && inside(98, 68) && inside(90, 60) && inside(98, 50));
    assert!(!inside(94, 57), "hollow");
    assert!(out.iter().all(|q| q.row == 2));
}
