// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The autohinter at the edges of its input: outlines whose points sit at the
//! ends of the 16-bit range, on faces with the fewest units per em the `head`
//! table allows, at sizes up to the scale the hinter fits at.
//!
//! The Trust lane refutes hundreds of the hinter's `i32` additions and
//! subtractions (TRUST-QUIET, `docs/measured/trust-quiet-2026-10-20.md`). Each
//! function is verified alone, so the bound on a 26.6 coordinate (at most
//! 32768 font units times the scale) is no fact the solver has. These tests
//! hold that bound where it matters: past 512 pixels per font unit the 16.16
//! scale wrapped and the fitting overflowed (a panic with overflow checks on,
//! a wrong outline without), so `HintingInstance::new` and `draw` refuse a
//! scale past 128 pixels per unit, and up to it every outline fits.

use aterm_font::OutlineSink;
use aterm_font::autohint::{HintingInstance, Target};

#[derive(Default)]
struct Count(u64);

impl OutlineSink for Count {
    fn move_to(&mut self, _: f32, _: f32) {
        self.0 += 1;
    }
    fn line_to(&mut self, _: f32, _: f32) {
        self.0 += 1;
    }
    fn quad_to(&mut self, _: f32, _: f32, _: f32, _: f32) {
        self.0 += 1;
    }
    fn curve_to(&mut self, _: f32, _: f32, _: f32, _: f32, _: f32, _: f32) {
        self.0 += 1;
    }
    fn close(&mut self) {
        self.0 += 1;
    }
}

fn be16(v: i32) -> [u8; 2] {
    (v as u16).to_be_bytes()
}

/// A simple `glyf` glyph from absolute on-curve contours, every delta a long
/// one (each must fit an `i16`).
fn simple_glyph(contours: &[Vec<(i32, i32)>]) -> Vec<u8> {
    let pts: Vec<(i32, i32)> = contours.iter().flatten().copied().collect();
    let (mut x0, mut y0, mut x1, mut y1) = (i32::MAX, i32::MAX, i32::MIN, i32::MIN);
    for &(x, y) in &pts {
        (x0, y0, x1, y1) = (x0.min(x), y0.min(y), x1.max(x), y1.max(y));
    }
    let mut g = Vec::new();
    g.extend(be16(contours.len() as i32));
    for v in [x0, y0, x1, y1] {
        g.extend(be16(v));
    }
    let mut end = -1;
    for c in contours {
        end += c.len() as i32;
        g.extend(be16(end));
    }
    g.extend(be16(0));
    g.extend(std::iter::repeat_n(0x01u8, pts.len()));
    let (mut px, mut py) = (0, 0);
    let mut ys = Vec::new();
    for &(x, y) in &pts {
        let (dx, dy) = (x - px, y - py);
        assert!(
            i16::try_from(dx).is_ok() && i16::try_from(dy).is_ok(),
            "delta out of range"
        );
        g.extend(be16(dx));
        ys.extend(be16(dy));
        (px, py) = (x, y);
    }
    g.extend(ys);
    if g.len() % 2 == 1 {
        g.push(0);
    }
    g
}

/// `cmap` format 4 (3, 1) mapping `ch` to glyph 1.
fn cmap_for(ch: char) -> Vec<u8> {
    let c = ch as i32;
    let mut sub = Vec::new();
    // Format, length (set below), language, segCountX2, then the search fields.
    for v in [4, 0, 0, 4, 4, 1, 0] {
        sub.extend(be16(v));
    }
    for v in [c, 0xFFFF, 0, c, 0xFFFF, 1 - c, 1, 0, 0] {
        sub.extend(be16(v)); // ends, pad, starts, deltas, range offsets
    }
    let len = sub.len() as i32;
    sub[2..4].copy_from_slice(&be16(len));
    let mut t = Vec::new();
    for v in [0, 1, 3, 1] {
        t.extend(be16(v));
    }
    t.extend(12u32.to_be_bytes());
    t.extend(sub);
    t
}

/// A two-glyph TrueType face: glyph 0 empty, glyph 1 `contours`, mapped from
/// `ch` when there is one (Latin `H` and CJK each pick their writing system;
/// an unmapped glyph takes the default style).
fn face(upem: i32, contours: &[Vec<(i32, i32)>], ch: Option<char>) -> Vec<u8> {
    let glyf = simple_glyph(contours);
    let mut loca = Vec::new();
    for v in [0, 0, glyf.len() as i32 / 2] {
        loca.extend(be16(v));
    }
    let mut head = vec![0u8; 54];
    head[18..20].copy_from_slice(&be16(upem));
    let mut hhea = vec![0u8; 36];
    hhea[34..36].copy_from_slice(&be16(2));
    let mut hmtx = Vec::new();
    for _ in 0..2 {
        hmtx.extend(be16(upem));
        hmtx.extend(be16(0));
    }
    let mut maxp = vec![0, 0, 0x50, 0];
    maxp.extend(be16(2));
    let mut tables: Vec<(&[u8; 4], Vec<u8>)> = vec![
        (b"glyf", glyf),
        (b"head", head),
        (b"hhea", hhea),
        (b"hmtx", hmtx),
        (b"loca", loca),
        (b"maxp", maxp),
    ];
    if let Some(ch) = ch {
        tables.push((b"cmap", cmap_for(ch)));
    }
    tables.sort_by_key(|(t, _)| **t);
    let mut font = vec![0, 1, 0, 0];
    font.extend(be16(tables.len() as i32));
    font.extend([0u8; 6]);
    let mut offset = 12 + 16 * tables.len();
    for (tag, data) in &tables {
        font.extend(*tag);
        font.extend([0u8; 4]);
        font.extend((offset as u32).to_be_bytes());
        font.extend((data.len() as u32).to_be_bytes());
        offset += data.len().next_multiple_of(4);
    }
    for (_, data) in &tables {
        font.extend_from_slice(data);
        font.resize(font.len().next_multiple_of(4), 0);
    }
    font
}

const STYLES: [Option<char>; 3] = [Some('H'), None, Some('\u{4E00}')];
const TARGETS: [Target; 3] = [Target::Normal, Target::Light, Target::Lcd];

/// Fits glyph 1 of `bytes`; `None` when the instance or the draw is refused.
fn fit(bytes: &[u8], px: f32, target: Target) -> Option<u64> {
    let hint = HintingInstance::new(bytes, 0, px, &[], target)?;
    let mut sink = Count::default();
    hint.draw(bytes, 0, 1, &mut sink)?;
    Some(sink.0)
}

/// A stem of a rectilinear contour from the left edge of the 16-bit range to
/// the right one, which overflowed the fitting at 9000 pixels per em on a
/// 16-unit face (562.5 pixels per unit, a wrapped 16.16 scale).
fn wide_stem() -> Vec<Vec<(i32, i32)>> {
    vec![vec![
        (4, -32740),
        (4, -8),
        (4, 32723),
        (-16353, 32723),
        (-32710, 32723),
        (-32710, 23248),
        (-32710, 13773),
        (-16426, 13773),
        (-143, 13773),
        (-143, 6830),
        (-143, -112),
        (13783, -112),
        (27710, -112),
    ]]
}

/// Past 128 pixels per font unit the hinter refuses the size: at 9000 px on a
/// 16-unit face, where the fitting overflowed, and at 10^9 px on a 1000-unit
/// one. One pixel under the bound it fits the same outline.
#[test]
fn a_scale_past_the_bound_is_refused() {
    let stem = wide_stem();
    for ch in STYLES {
        let small = face(16, &stem, ch);
        let large = face(1000, &stem, ch);
        for target in TARGETS {
            assert_eq!(fit(&small, 9000.0, target), None, "{ch:?} {target:?}");
            assert_eq!(fit(&small, 2049.0, target), None, "{ch:?} {target:?}");
            assert_eq!(fit(&large, 1.0e9, target), None, "{ch:?} {target:?}");
            assert!(
                fit(&small, 2048.0, target).is_some_and(|n| n > 0),
                "{ch:?} {target:?}: the bound itself fits"
            );
        }
    }
}

/// An instance built for a face with many units per em, drawn with the data of
/// one with few (the caller's mistake, and the one way the size reaches the
/// fitting unchecked by `new`), refuses the draw rather than wrapping.
#[test]
fn a_draw_holds_the_bound_for_the_face_it_is_given() {
    let stem = wide_stem();
    let large = face(2048, &stem, Some('H'));
    let small = face(16, &stem, Some('H'));
    let hint = HintingInstance::new(&large, 0, 9000.0, &[], Target::Normal)
        .expect("9000 px on a 2048-unit face is in range");
    let mut sink = Count::default();
    assert!(hint.draw(&large, 0, 1, &mut sink).is_some());
    assert_eq!(hint.draw(&small, 0, 1, &mut sink), None);
}

struct Lcg(u64);

impl Lcg {
    fn next(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn below(&mut self, n: usize) -> usize {
        self.next() as usize % n
    }

    /// A coordinate at either end of the 16-bit range, anywhere in it, or near
    /// the origin.
    fn coord(&mut self) -> i32 {
        match self.next() % 4 {
            0 => 32767 - (self.next() % 64) as i32,
            1 => -32768 + (self.next() % 64) as i32,
            2 => (self.next() % 65536) as i32 - 32768,
            _ => (self.next() % 512) as i32 - 256,
        }
    }
}

/// One to four contours, most of them rectilinear (alternating horizontal and
/// vertical runs, so the hinter finds segments, edges, stems and serifs); a
/// run too long for one `glyf` delta goes through its midpoint.
fn contours(rng: &mut Lcg) -> Vec<Vec<(i32, i32)>> {
    (0..1 + rng.below(4))
        .map(|_| {
            let rect = rng.below(4) != 0;
            let mut pts: Vec<(i32, i32)> = Vec::new();
            let mut cur = (rng.coord(), rng.coord());
            for i in 0..3 + rng.below(12) {
                let p = match (rect, i % 2) {
                    (true, 0) => (rng.coord(), cur.1),
                    (true, _) => (cur.0, rng.coord()),
                    (false, _) => (rng.coord(), rng.coord()),
                };
                cur = p;
                if let Some(&(lx, ly)) = pts.last() {
                    pts.push(((lx + p.0) / 2, (ly + p.1) / 2));
                }
                pts.push(p);
            }
            pts
        })
        .collect()
}

fn deltas_fit(contours: &[Vec<(i32, i32)>]) -> bool {
    let mut prev = (0, 0);
    contours.iter().flatten().all(|&(x, y)| {
        let ok = i16::try_from(x - prev.0).is_ok() && i16::try_from(y - prev.1).is_ok();
        prev = (x, y);
        ok
    })
}

/// Outlines at the ends of the 16-bit range, at the bound (128 pixels per
/// unit) and at half of it, on faces of 16 to 2048 units, in every target and
/// writing system: the hinter fits them all (the test profile keeps overflow
/// checks on, so an overflow here is a panic). Before the bound, 300000 such
/// outlines at sizes from 300 px to 10^9 px overflowed the fitting 983 times:
/// 977 at 9000 and 12000 px on 16-unit faces, 6 at 10^9 px on 1000-unit ones.
///
/// It drives sizes at or under the bound only, so it has no negative control
/// of its own: with the bound removed it still passes. The refusal tests
/// above are the control; they fail when `scale_fits` lets a size past 128
/// pixels per unit through. The bound sits 4x under the wrap at 512.
#[test]
fn outlines_at_the_edge_of_the_range_fit_up_to_the_bound() {
    let mut rng = Lcg(7);
    let (mut fitted, mut drawn) = (0u32, 0u64);
    let mut tried = 0u32;
    while tried < 20_000 {
        let c = contours(&mut rng);
        if !deltas_fit(&c) {
            continue;
        }
        tried += 1;
        let upem = [16, 16, 64, 1000, 2048][rng.below(5)];
        let px = upem as f32 * [128.0, 64.0][rng.below(2)];
        let bytes = face(upem, &c, STYLES[rng.below(3)]);
        if let Some(n) = fit(&bytes, px, TARGETS[rng.below(3)]) {
            fitted += 1;
            drawn += n;
        }
    }
    // Not vacuous: most outlines are fitted and drawn.
    assert!(fitted > tried / 2, "{fitted} of {tried} fitted");
    assert!(drawn > u64::from(fitted), "{drawn} path elements");
}
