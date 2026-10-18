// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A glyph with more than 32 `gvar` tuples draws (TF.3 of
//! docs/AUDIT-third-party-2026-10-15.md).
//!
//! ttf-parser 0.25.1 keeps a glyph's tuples in a 32-slot stack array unless
//! its `gvar-alloc` feature is on; without it, a glyph with more has no
//! outline at any instance, the default included. aterm's Linux build shipped
//! the feature while it had ttf-parser (winit's `wayland-csd-adwaita` ->
//! sctk-adwaita -> ab_glyph turned it on, until linux-windowing LW.5) and its
//! macOS, Windows and wasm builds did not. aterm-font draws such glyphs on
//! every platform. The synthetic face below runs on every host;
//! `reader_oracle.rs` holds SF Pro (`SFNS.ttf`) and `ADTNumeric.ttc` to what
//! ttf-parser with `gvar-alloc` recorded, on a Mac.

use aterm_font::{Face, GlyphId, Rect, Tag};

use crate::never_panic::sfnt;

/// The triangle every simple glyph here is: three on-curve points.
const TRIANGLE: [(i16, i16); 3] = [(0, 0), (100, 0), (0, 100)];

/// A simple glyph's `glyf` record: the triangle, padded to an even length
/// for short `loca` offsets.
pub(crate) fn triangle() -> Vec<u8> {
    let mut g = Vec::new();
    g.extend(1i16.to_be_bytes()); // one contour
    for v in [0i16, 0, 100, 100] {
        g.extend(v.to_be_bytes());
    }
    g.extend(2u16.to_be_bytes()); // last point of contour 0
    g.extend(0u16.to_be_bytes()); // no instructions
    g.extend([1u8; 3]); // on curve, coordinates as i16 deltas
    let mut prev = (0i16, 0i16);
    let (mut xs, mut ys) = (Vec::new(), Vec::new());
    for (x, y) in TRIANGLE {
        xs.extend((x - prev.0).to_be_bytes());
        ys.extend((y - prev.1).to_be_bytes());
        prev = (x, y);
    }
    g.extend(xs);
    g.extend(ys);
    g.resize(g.len().next_multiple_of(2), 0);
    g
}

/// A composite `glyf` record: glyph `component` moved by `(dx, dy)`.
fn composite(component: u16, dx: i16, dy: i16) -> Vec<u8> {
    let mut g = Vec::new();
    g.extend((-1i16).to_be_bytes());
    g.extend([0u8; 8]);
    g.extend(0x0003u16.to_be_bytes()); // ARG_1_AND_2_ARE_WORDS | ARGS_ARE_XY_VALUES
    g.extend(component.to_be_bytes());
    g.extend(dx.to_be_bytes());
    g.extend(dy.to_be_bytes());
    g
}

/// One glyph's variation data: `tuples` tuples, each peaking at `wght` 1.0
/// (an embedded peak) and moving every point (no point numbers) by the byte
/// deltas `x` and `y`, phantom points included.
fn variations(tuples: usize, x: &[i8], y: &[i8]) -> Vec<u8> {
    let points = x.len();
    let headers = 4 + tuples * 6;
    let serialized = 2 * (1 + points);
    let mut v = Vec::new();
    v.extend(u16::try_from(tuples).expect("12-bit count").to_be_bytes());
    v.extend(u16::try_from(headers).expect("headers fit").to_be_bytes());
    for _ in 0..tuples {
        v.extend(u16::try_from(serialized).expect("small").to_be_bytes());
        v.extend(0x8000u16.to_be_bytes()); // EMBEDDED_PEAK_TUPLE
        v.extend(0x4000u16.to_be_bytes()); // peak wght = 1.0 (F2DOT14)
    }
    for _ in 0..tuples {
        for deltas in [x, y] {
            v.push(u8::try_from(points - 1).expect("one run")); // byte deltas
            v.extend(deltas.iter().map(|&d| d.to_be_bytes()[0]));
        }
    }
    v
}

/// A one-axis (`wght` 100..=400..=900) glyf+gvar face of `glyphs`, each a
/// `(glyf record, variation data)` pair.
pub(crate) fn face_bytes(glyphs: &[(Vec<u8>, Vec<u8>)]) -> Vec<u8> {
    let mut fvar = Vec::new();
    for v in [1u16, 0, 16, 2, 1, 20, 0, 8] {
        fvar.extend(v.to_be_bytes());
    }
    fvar.extend(*b"wght");
    for user in [100i32, 400, 900] {
        fvar.extend((user << 16).to_be_bytes());
    }
    fvar.extend([0, 0, 1, 0]); // flags, name id 256

    let (mut glyf, mut loca) = (Vec::new(), Vec::new());
    for (record, _) in glyphs {
        loca.extend(
            u16::try_from(glyf.len() / 2)
                .expect("short loca")
                .to_be_bytes(),
        );
        glyf.extend(record);
    }
    loca.extend(
        u16::try_from(glyf.len() / 2)
            .expect("short loca")
            .to_be_bytes(),
    );

    let count = glyphs.len();
    let array = 20 + (count + 1) * 4;
    let mut gvar = Vec::new();
    gvar.extend(0x0001_0000u32.to_be_bytes());
    gvar.extend(1u16.to_be_bytes()); // one axis
    gvar.extend(0u16.to_be_bytes()); // no shared tuples
    gvar.extend(u32::try_from(array).expect("small").to_be_bytes());
    gvar.extend(u16::try_from(count).expect("small").to_be_bytes());
    gvar.extend(1u16.to_be_bytes()); // long offsets
    gvar.extend(u32::try_from(array).expect("small").to_be_bytes());
    let mut data: Vec<u8> = Vec::new();
    for (_, variation) in glyphs {
        gvar.extend(u32::try_from(data.len()).expect("small").to_be_bytes());
        data.extend(variation);
    }
    gvar.extend(u32::try_from(data.len()).expect("small").to_be_bytes());
    gvar.extend(data);

    sfnt(
        count,
        &[
            (b"fvar", &fvar),
            (b"glyf", &glyf),
            (b"gvar", &gvar),
            (b"loca", &loca),
        ],
    )
}

fn rect(x_min: i16, y_min: i16, x_max: i16, y_max: i16) -> Option<Rect> {
    Some(Rect {
        x_min,
        y_min,
        x_max,
        y_max,
    })
}

/// The triangle moved by `(n, 2n)`: `n` tuples of `(+1, +2)` at full weight.
fn moved(n: i16) -> Option<Rect> {
    rect(n, 2 * n, 100 + n, 100 + 2 * n)
}

/// The synthetic face: a triangle under 32 tuples of `(+1, +2)` at full
/// weight (the most ttf-parser's stack holds), the same under 33 and under
/// the 12-bit maximum 4095, and a composite with 33 tuples of its own
/// (moving its component by `+1` each) over the 33-tuple glyph, offset 1000.
/// `never_panic.rs` mutates it too.
pub(crate) fn past_32_face() -> Vec<u8> {
    let (x, y) = ([1i8, 1, 1, 0, 0, 0, 0], [2i8, 2, 2, 0, 0, 0, 0]);
    face_bytes(&[
        (triangle(), variations(32, &x, &y)),
        (triangle(), variations(33, &x, &y)),
        (triangle(), variations(4095, &x, &y)),
        (
            composite(1, 1000, 0),
            variations(33, &[1, 0, 0, 0, 0], &[0; 5]),
        ),
    ])
}

/// 32 tuples (the most ttf-parser's stack holds), 33, and the 12-bit maximum
/// 4095 each draw, at the default instance and at full weight, and so does a
/// composite with 33 tuples of its own over the 33-tuple glyph. Put the
/// 32-slot cap back and every glyph past it answers `None`.
#[test]
fn a_glyph_past_32_tuples_draws() {
    let bytes = past_32_face();
    let mut face = Face::parse(&bytes, 0).expect("the synthetic face parses");
    assert!(face.tables().gvar.is_some(), "gvar parses");

    // Default instance: every scalar is 0, so no tuple applies, but the
    // glyph still goes through `gvar` (and ttf-parser's cap).
    for g in 0..3 {
        assert_eq!(face.glyph_bounding_box(GlyphId(g)), moved(0), "glyph {g}");
    }
    assert_eq!(
        face.glyph_bounding_box(GlyphId(3)),
        rect(1000, 0, 1100, 100)
    );

    face.set_variation(Tag::from_bytes(b"wght"), 900.0)
        .expect("wght");
    assert_eq!(face.glyph_bounding_box(GlyphId(0)), moved(32));
    assert_eq!(face.glyph_bounding_box(GlyphId(1)), moved(33));
    assert_eq!(face.glyph_bounding_box(GlyphId(2)), moved(4095));
    // The composite's own 33 tuples move its component by 33 more units.
    assert_eq!(
        face.glyph_bounding_box(GlyphId(3)),
        rect(1066, 66, 1166, 166)
    );
}
