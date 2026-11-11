// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

/// The Singularity darkening mask is a soft radial shadow: full at the centre,
/// monotone falloff, centre-symmetric, and empty corners so neighbouring stamps
/// fuse without box seams.
#[test]
fn ring_arc_is_a_soft_radial_shadow() {
    let (w, h) = (16usize, 18usize);
    let cov = deco_coverage(crate::DecoGlyph::RingArc, w, h);
    let at = |x: usize, y: usize| cov[y * w + x];
    let (cx, cy) = (w / 2, h / 2);
    assert!(at(cx, cy) > 220, "the shadow peaks at the cell centre");
    assert!(
        at(cx, cy) > at(cx - 4, cy) && at(cx - 4, cy) > at(cx - 6, cy),
        "coverage falls monotonically from the centre outward"
    );
    assert_eq!(
        at(cx - 7, cy),
        at(cx + 6, cy),
        "the falloff is centre-symmetric"
    );
    assert!(
        at(cx - 6, cy) > 0,
        "the skirt still contributes (blobs must fuse)"
    );
    assert_eq!(at(0, 0), 0, "the corners lie outside the radius");
    assert_eq!(at(w - 1, h - 1), 0, "all corners are empty");
}

/// The super nova veil is a full-cell soft square: interior texels saturate and
/// the one-texel border ring is a partial, non-zero fringe.
#[test]
fn shade_is_a_full_cell_soft_square() {
    let (w, h) = (16usize, 18usize);
    let cov = deco_coverage(crate::DecoGlyph::Shade, w, h);
    let at = |x: usize, y: usize| cov[y * w + x];
    for y in 1..h - 1 {
        for x in 1..w - 1 {
            assert_eq!(at(x, y), 255, "interior texel ({x},{y}) must saturate");
        }
    }
    let mut edges: Vec<(usize, usize)> = Vec::new();
    edges.extend((0..w).flat_map(|x| [(x, 0), (x, h - 1)]));
    edges.extend((0..h).flat_map(|y| [(0, y), (w - 1, y)]));
    for (x, y) in edges {
        let c = at(x, y);
        assert!(
            c > 0 && c < 255,
            "border texel ({x},{y}) must be a soft partial fringe, got {c}"
        );
    }
}

/// Cell sizes exercised by the invariants below: odd/even mixes, squat,
/// tall, and degenerate-tiny.
const SIZES: &[(usize, usize)] = &[
    (1, 1),
    (2, 2),
    (3, 7),
    (7, 15),
    (8, 16),
    (9, 19),
    (10, 20),
    (11, 21),
    (12, 22),
    (20, 8),
];

fn all_procedural_chars() -> impl Iterator<Item = char> {
    (0x2500u32..=0x259F)
        .chain(0x2800..=0x28FF)
        .chain(0x1FB00..=0x1FB8B)
        .chain(0x1CD00..=0x1CDE5)
        .chain(0xE0B0..=0xE0BF)
        .map(|cp| char::from_u32(cp).unwrap())
}

/// Every glyph at every size is cell-sized; orthogonal families are hard 0/255
/// throughout, [`antialiased`] ones on their cell-edge texels
/// (`tests/procedural_aa_edges.rs` covers the whole size lattice).
#[test]
fn every_glyph_is_cell_sized_and_hard_where_required() {
    for &(w, h) in SIZES {
        for ch in all_procedural_chars() {
            let cov = coverage(ch, w, h)
                .unwrap_or_else(|| panic!("{ch:?} must be procedural at {w}x{h}"));
            assert_eq!(cov.len(), w * h, "{ch:?} at {w}x{h}: wrong size");
            if antialiased(ch) {
                let hard_edge = (0..w)
                    .flat_map(|x| [x, (h - 1) * w + x])
                    .chain((0..h).flat_map(|y| [y * w, y * w + w - 1]))
                    .all(|i| cov[i] == 0 || cov[i] == 255);
                assert!(hard_edge, "{ch:?} at {w}x{h}: soft cell-edge texel");
            } else {
                assert!(
                    cov.iter().all(|&b| b == 0 || b == 255),
                    "{ch:?} at {w}x{h}: non-hard coverage byte"
                );
            }
        }
    }
    let cov = coverage('╱', 11, 21).unwrap();
    assert!(
        (1..20).any(|y| (1..10).any(|x| !matches!(cov[y * 11 + x], 0 | 255))),
        "╱ at 11x21 must carry interior anti-aliasing"
    );
}

/// Chars outside the covered blocks are not intercepted.
#[test]
fn non_procedural_chars_are_not_covered() {
    for ch in [
        'A',
        ' ',
        '日',
        '\u{24FF}',
        '\u{25A0}',
        '\u{27FF}',
        '\u{2900}',
        '\u{1FB8C}',
        '\u{E0C0}',
    ] {
        assert!(!covers(ch), "{ch:?} must stay font-rendered");
        assert!(coverage(ch, 8, 16).is_none());
    }
}

/// Solid lines reach both cell edges (the seam guarantee in one cell).
#[test]
fn solid_lines_touch_their_edges() {
    for &(w, h) in SIZES {
        for ch in ['─', '━'] {
            let cov = coverage(ch, w, h).unwrap();
            let lit_col = |x: usize| (0..h).any(|y| cov[y * w + x] != 0);
            assert!(
                lit_col(0) && lit_col(w - 1),
                "{ch:?} at {w}x{h} must span the width"
            );
        }
        for ch in ['│', '┃', '║'] {
            let cov = coverage(ch, w, h).unwrap();
            let lit_row = |y: usize| (0..w).any(|x| cov[y * w + x] != 0);
            assert!(
                lit_row(0) && lit_row(h - 1),
                "{ch:?} at {w}x{h} must span the height"
            );
        }
    }
}

/// The heavy stroke span exactly contains the light span on both axes —
/// the parity property the module's rounding rule promises.
#[test]
fn heavy_span_contains_light_span_centred() {
    for &(w, h) in SIZES {
        let m = Metrics::new(w, h);
        assert!(
            m.vh0 <= m.vl0 && m.vl1 <= m.vh1,
            "{w}x{h}: vertical containment"
        );
        assert!(
            m.hh0 <= m.hl0 && m.hl1 <= m.hh1,
            "{w}x{h}: horizontal containment"
        );
        if m.heavy == 3 * m.light {
            assert_eq!(m.vl0 - m.vh0, m.vh1 - m.vl1, "{w}x{h}: vertical centring");
            assert_eq!(m.hl0 - m.hh0, m.hh1 - m.hl1, "{w}x{h}: horizontal centring");
        }
    }
}

/// ▀/▄ and ▌/▐ cover the whole cell between them (overlap allowed on odd
/// extents, gaps never) — the half-block tiling rule.
#[test]
fn complementary_halves_leave_no_gap() {
    for &(w, h) in SIZES {
        let top = coverage('▀', w, h).unwrap();
        let bottom = coverage('▄', w, h).unwrap();
        assert!(
            top.iter().zip(&bottom).all(|(&a, &b)| a == 255 || b == 255),
            "{w}x{h}: ▀+▄ must tile the cell"
        );
        let left = coverage('▌', w, h).unwrap();
        let right = coverage('▐', w, h).unwrap();
        assert!(
            left.iter().zip(&right).all(|(&a, &b)| a == 255 || b == 255),
            "{w}x{h}: ▌+▐ must tile the cell"
        );
    }
}

/// Braille dots land in their compartments: dot 1 is top-left, dot 8 is
/// bottom-right, and they never bleed across the column midline.
#[test]
fn braille_dot_positions() {
    let (w, h): (usize, usize) = (10, 20);
    let mid_x = w.div_ceil(2);
    let d1 = coverage('\u{2801}', w, h).unwrap();
    let d8 = coverage('\u{2880}', w, h).unwrap();
    let lit = |cov: &[u8]| {
        (0..h)
            .flat_map(|y| (0..w).map(move |x| (x, y)))
            .filter(|&(x, y)| cov[y * w + x] != 0)
            .collect::<Vec<_>>()
    };
    let l1 = lit(&d1);
    let l8 = lit(&d8);
    assert!(!l1.is_empty() && !l8.is_empty());
    assert!(
        l1.iter().all(|&(x, y)| x < mid_x && y < h / 4 + 1),
        "dot 1 confined to top-left"
    );
    assert!(
        l8.iter().all(|&(x, y)| x >= mid_x && y >= 3 * h / 4 - 1),
        "dot 8 confined to bottom-right"
    );
}

/// The shades dither at their nominal densities (exact for even dims).
#[test]
fn shades_have_correct_density() {
    let (w, h) = (8, 16);
    let count = |ch: char| {
        coverage(ch, w, h)
            .unwrap()
            .iter()
            .filter(|&&b| b == 255)
            .count()
    };
    assert_eq!(count('░'), w * h / 4);
    assert_eq!(count('▒'), w * h / 2);
    assert_eq!(count('▓'), 3 * w * h / 4);
}

/// Sextants are covered, hard 0/255, inked and distinct; U+1FB00 fills only the
/// upper-left sub-cell.
#[test]
fn sextants_fill_the_2x3_grid() {
    let (w, h) = (12, 24);
    let mut seen = std::collections::HashSet::new();
    for cp in 0x1FB00u32..=0x1FB3B {
        let ch = char::from_u32(cp).unwrap();
        assert!(covers(ch), "U+{cp:04X} covered");
        let cov = coverage(ch, w, h).unwrap();
        assert_eq!(cov.len(), w * h);
        assert!(cov.iter().all(|&b| b == 0 || b == 255), "hard coverage");
        assert!(cov.contains(&255), "U+{cp:04X} draws ink");
        assert!(
            seen.insert(cov.clone()),
            "U+{cp:04X} duplicates another sextant"
        );
    }
    let at = |cov: &[u8], x: usize, y: usize| cov[y * w + x] == 255;
    let ul = coverage('\u{1FB00}', w, h).unwrap();
    assert!(at(&ul, 0, 0), "U+1FB00 fills upper-left");
    assert!(
        !at(&ul, w - 1, 0) && !at(&ul, 0, h - 1),
        "U+1FB00 only upper-left"
    );
}

/// Octants: all 230 are covered, hard 0/255, distinct, and exactly their
/// mask: at every lattice size the eight sub-rects tile the cell with no
/// gap or overlap, and each glyph is the union of its mask's sub-rects.
#[test]
fn octants_fill_the_2x4_grid_exactly() {
    assert_eq!(OCTANT_MASKS.len(), 0x1CDE5 - 0x1CD00 + 1);
    let distinct: std::collections::HashSet<u8> = OCTANT_MASKS.iter().copied().collect();
    assert_eq!(distinct.len(), 230, "every octant is a distinct pattern");
    assert!(!distinct.contains(&0) && !distinct.contains(&0xFF));
    for &(w, h) in SIZES {
        if w < 2 || h < 4 {
            continue;
        }
        let mut hits = vec![0u8; w * h];
        for bit in 0..8 {
            let mut c = Canvas::new(w, h);
            draw_octant(&mut c, 1 << bit);
            assert!(
                c.buf.contains(&255),
                "octant {} is empty at {w}x{h}",
                bit + 1
            );
            for (hit, &b) in hits.iter_mut().zip(&c.buf) {
                *hit += u8::from(b == 255);
            }
        }
        assert!(
            hits.iter().all(|&n| n == 1),
            "the 2x4 grid tiles at {w}x{h}"
        );
        for (i, &mask) in OCTANT_MASKS.iter().enumerate() {
            let ch = char::from_u32(0x1CD00 + i as u32).unwrap();
            assert!(covers(ch));
            let cov = coverage(ch, w, h).unwrap();
            let mut want = Canvas::new(w, h);
            draw_octant(&mut want, mask);
            assert_eq!(cov, want.buf, "U+{:04X} at {w}x{h}", 0x1CD00 + i);
        }
    }
    assert_eq!(OCTANT_MASKS[0], 0b0000_0100);
    assert_eq!(OCTANT_MASKS[229], 0b1111_1110);
}

/// Octants seam with the blocks that share their splits: the octant
/// unions Unicode encodes elsewhere draw the very pixels of ▀, ▌, ▘ and
/// the upper quarter / three-quarter blocks.
#[test]
fn octant_splits_match_the_blocks_they_extend() {
    for &(w, h) in SIZES {
        for (mask, ch) in [
            (0b0000_1111u8, '\u{2580}'),
            (0b0101_0101, '\u{258C}'),
            (0b0000_0101, '\u{2598}'),
            (0b0000_0011, '\u{1FB82}'),
            (0b0011_1111, '\u{1FB85}'),
            (0xFF, '\u{2588}'),
        ] {
            let mut c = Canvas::new(w, h);
            draw_octant(&mut c, mask);
            assert_eq!(
                c.buf,
                coverage(ch, w, h).unwrap(),
                "mask {mask:08b} vs {ch:?} at {w}x{h}"
            );
        }
    }
}

/// Powerline separators are covered, anti-aliased with hard cell edges,
/// and shaped: a SOLID right triangle (E0B0) is widest at the vertical
/// middle and empty at the edges, fills the apex column at mid-height, and
/// is the EXACT byte mirror of the solid left triangle (E0B2).
#[test]
fn powerline_separators_are_shaped() {
    let (w, h) = (12, 24);
    for cp in 0xE0B0u32..=0xE0BF {
        let ch = char::from_u32(cp).unwrap();
        assert!(covers(ch), "U+{cp:04X} must be covered");
        assert!(antialiased(ch), "U+{cp:04X} is an AA-family glyph");
        let cov = coverage(ch, w, h).unwrap();
        assert_eq!(cov.len(), w * h);
        assert!(cov.contains(&255), "U+{cp:04X} draws ink");
    }
    let at = |cov: &[u8], x: usize, y: usize| cov[y * w + x] == 255;
    let ll = coverage('\u{E0B8}', w, h).unwrap();
    assert!(
        at(&ll, 0, h - 1) && !at(&ll, w - 1, 0),
        "E0B8 fills lower-left"
    );
    let ur = coverage('\u{E0BE}', w, h).unwrap();
    assert!(
        at(&ur, w - 1, 0) && !at(&ur, 0, h - 1),
        "E0BE fills upper-right"
    );
    let right = coverage('\u{E0B0}', w, h).unwrap();
    let left = coverage('\u{E0B2}', w, h).unwrap();
    assert!(at(&right, w - 1, h / 2), "E0B0 apex at right-middle");
    assert!(!at(&right, w - 1, 0), "E0B0 empty at top-right");
    assert!(at(&left, 0, h / 2), "E0B2 apex at left-middle");
    for y in 0..h {
        for x in 0..w {
            assert_eq!(
                right[y * w + x],
                left[y * w + (w - 1 - x)],
                "E0B0/E0B2 mirror at ({x},{y})"
            );
        }
    }
    let ink = |cov: &[u8]| cov.iter().map(|&b| usize::from(b)).sum::<usize>();
    assert!(
        ink(&coverage('\u{E0B1}', w, h).unwrap()) < ink(&right),
        "outline < solid"
    );
}

/// The chevron outline's stroke width is perpendicular, not horizontal: on squat
/// cells every interior row carries a full stroke of ink and consecutive rows
/// overlap (no beading).
#[test]
fn chevron_outline_width_is_perpendicular_not_horizontal() {
    for (w, h) in [(20usize, 8usize), (16, 10), (12, 24), (9, 19)] {
        let t = ((w.min(h) + 4) / 8).max(1);
        for ch in ['\u{E0B1}', '\u{E0B3}'] {
            let cov = coverage(ch, w, h).unwrap();
            let slope = w as f32 / (h as f32 / 2.0);
            let want = (t as f32 * (1.0 + slope * slope).sqrt() * 255.0 * 0.55) as usize;
            let mut prev: Option<(usize, usize)> = None;
            for y in 1..h - 1 {
                let row = &cov[y * w..(y + 1) * w];
                let ink: usize = row.iter().map(|&b| usize::from(b)).sum();
                assert!(
                    ink >= want,
                    "{ch:?} at {w}x{h} row {y}: ink {ink} < {want} — stroke thinned \
                     (horizontal-width beading)"
                );
                let lit: Vec<usize> = (0..w).filter(|&x| row[x] != 0).collect();
                let span = (lit[0], lit[lit.len() - 1]);
                if let Some((p0, p1)) = prev {
                    assert!(
                        span.0 <= p1 + 1 && p0 <= span.1 + 1,
                        "{ch:?} at {w}x{h} rows {}/{y}: stroke disconnected (beaded)",
                        y - 1
                    );
                }
                prev = Some(span);
            }
        }
    }
}

/// Each wedge and its complement (same diagonal, opposite corner) sum to full
/// coverage (≥ 254: one rounding LSB); also spot-checks orientation and the quarter
/// triangles.
#[test]
fn wedge_pairs_tile_and_orient() {
    let pairs: Vec<(u32, u32)> = (0x1FB3C..=0x1FB51).zip(0x1FB52..=0x1FB67).collect();
    for &(w, h) in SIZES {
        for &(a, b) in &pairs {
            let ca = coverage(char::from_u32(a).unwrap(), w, h).unwrap();
            let cb = coverage(char::from_u32(b).unwrap(), w, h).unwrap();
            for i in 0..w * h {
                let sum = usize::from(ca[i]) + usize::from(cb[i]);
                assert!(
                    sum >= 254,
                    "U+{a:04X}+U+{b:04X} at {w}x{h} px {i}: gap (sum {sum})"
                );
            }
        }
    }
    let (w, h) = (12, 24);
    let at = |cov: &[u8], x: usize, y: usize| cov[y * w + x] == 255;
    let c3c = coverage('\u{1FB3C}', w, h).unwrap();
    assert!(at(&c3c, 0, h - 1), "1FB3C fills the lower-left corner");
    assert!(
        !at(&c3c, w - 1, 0) && !at(&c3c, w - 1, h - 1),
        "1FB3C stays lower-left"
    );
    let c6c = coverage('\u{1FB6C}', w, h).unwrap();
    assert!(at(&c6c, 0, h / 2), "1FB6C fills the left edge middle");
    assert!(!at(&c6c, w - 1, h / 2), "1FB6C leaves the right edge");
    let c68 = coverage('\u{1FB68}', w, h).unwrap();
    assert!(at(&c68, w - 1, h / 2), "1FB68 fills the right edge middle");
    assert!(!at(&c68, 0, h / 2), "1FB68 leaves the left edge middle");
    for i in 0..w * h {
        let sum = usize::from(c6c[i]) + usize::from(c68[i]);
        assert!(sum >= 254, "1FB6C+1FB68 px {i}: gap (sum {sum})");
    }
}

/// Legacy eighth blocks (U+1FB70–1FB8B) are hard 0/255 on the same `eighth`
/// boundaries as U+2580–259F.
#[test]
fn legacy_eighth_blocks_are_hard_and_aligned() {
    let (w, h) = (16, 32);
    for cp in 0x1FB70u32..=0x1FB8B {
        let ch = char::from_u32(cp).unwrap();
        assert!(covers(ch) && !antialiased(ch), "U+{cp:04X} orthogonal");
        let cov = coverage(ch, w, h).unwrap();
        assert!(cov.iter().all(|&b| b == 0 || b == 255), "U+{cp:04X} hard");
        assert!(cov.contains(&255), "U+{cp:04X} draws ink");
    }
    let lit = |cov: &[u8], x: usize, y: usize| cov[y * w + x] == 255;
    let v2 = coverage('\u{1FB70}', w, h).unwrap();
    for y in 0..h {
        for x in 0..w {
            let want = (eighth(1, w)..eighth(2, w)).contains(&x);
            assert_eq!(lit(&v2, x, y), want, "1FB70 at ({x},{y})");
        }
    }
    let scan = coverage('\u{1FB81}', w, h).unwrap();
    for y in 0..h {
        let want = [1u32, 3, 5, 8]
            .iter()
            .any(|&k| (eighth(k - 1, h)..eighth(k, h)).contains(&y));
        assert_eq!(lit(&scan, 0, y), want, "1FB81 row {y}");
    }
    let uq = coverage('\u{1FB82}', w, h).unwrap();
    for y in 0..h {
        assert_eq!(lit(&uq, 0, y), y < eighth(2, h), "1FB82 row {y}");
    }
    let rq = coverage('\u{1FB87}', w, h).unwrap();
    for x in 0..w {
        assert_eq!(lit(&rq, x, 0), x >= w - eighth(2, w), "1FB87 col {x}");
    }
}

/// The one-eighth bars keep one stroke: at every cell height from 4 to 48 px each of
/// U+1FB76–1FB7B is as many rows thick as `▁` and `▔` (U+2581, U+2594), starting at its
/// eighth's boundary, and the vertical ones U+1FB70–1FB75 as many columns as `▏` and `▕`;
/// levels stay ordered top to bottom. NEGATIVE CONTROL: at a 14-px cell the bars as their
/// own eighths (`eighth(k − 1)..eighth(k)`) are 1 px at U+1FB77 and U+1FB7B (the quota
/// row's levels 6 and 2) and 2 px at the rest.
#[test]
fn the_one_eighth_bars_keep_one_stroke() {
    let rows = |cov: &[u8], w: usize| {
        cov.chunks(w)
            .enumerate()
            .filter(|(_, row)| row.iter().all(|&b| b == 255))
            .map(|(y, _)| y)
            .collect::<Vec<_>>()
    };
    for h in 4..=48 {
        let w = 8;
        let t = rows(&coverage('\u{2581}', w, h).unwrap(), w).len();
        assert_eq!(t, rows(&coverage('\u{2594}', w, h).unwrap(), w).len());
        let mut last = None;
        for cp in (0x1FB76u32..=0x1FB7B).rev() {
            let k = cp - 0x1FB76 + 2;
            let lit = rows(&coverage(char::from_u32(cp).unwrap(), w, h).unwrap(), w);
            assert_eq!(lit.len(), t, "U+{cp:04X} at {h} px: {lit:?}");
            assert_eq!(lit[0], eighth(k - 1, h).min(h - t), "U+{cp:04X} at {h} px");
            assert!(last.is_none_or(|y| lit[0] <= y), "U+{cp:04X} at {h} px");
            last = Some(lit[0]);
        }
        let (w, cols) = (h, |cov: &[u8], w: usize, x: usize| {
            cov.chunks(w).all(|row| row[x] == 255)
        });
        let t = (0..w)
            .filter(|&x| cols(&coverage('\u{258F}', w, 8).unwrap(), w, x))
            .count();
        for cp in 0x1FB70u32..=0x1FB75 {
            let cov = coverage(char::from_u32(cp).unwrap(), w, 8).unwrap();
            let lit = (0..w).filter(|&x| cols(&cov, w, x)).count();
            assert_eq!(lit, t, "U+{cp:04X} at {w} px wide");
        }
    }
    let own = |k: u32| eighth(k, 14) - eighth(k - 1, 14);
    assert_eq!(
        (2..=7).map(own).collect::<Vec<_>>(),
        [2, 1, 2, 2, 2, 1],
        "NEGATIVE CONTROL: the bars as their own eighths change stroke"
    );
}
