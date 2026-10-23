// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tests for the procedural fallback symbol tier across font-discovery policies.

use aterm_render::procedural::{covers_symbol, symbol_coverage};
use aterm_render::{FaceId, GlyphClass, GlyphImage, MISSING_FONT_CLASS_TEXT, Renderer, Theme};

fn dejavu() -> Vec<u8> {
    std::fs::read(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/assets/DejaVuSansMono.ttf"
    ))
    .expect("bundled DejaVu asset")
}

fn renderer(runtime: bool, sealed: bool) -> Renderer {
    let mut r = Renderer::from_bytes(&dejavu(), 18.0, Theme::default()).expect("fixture parses");
    r.set_runtime_font_discovery(runtime);
    if sealed {
        r.seal_admitted_font_sources();
    }
    r
}

fn all_symbols() -> Vec<char> {
    (0x2300u32..=0x2BFF)
        .filter_map(char::from_u32)
        .filter(|&c| covers_symbol(c))
        .collect()
}

fn mono(img: &GlyphImage) -> (usize, usize, &[u8]) {
    match img {
        GlyphImage::Mono {
            width,
            height,
            bytes,
            ..
        } => (*width, *height, bytes),
        other => panic!("expected a Mono glyph, got {other:?}"),
    }
}

#[test]
fn the_footer_triangle_draws_ink_on_every_policy() {
    for (runtime, sealed) in [(false, false), (true, false), (true, true)] {
        let mut r = renderer(runtime, sealed);
        let (cw, ch) = r.cell_size();
        let baseline = r.baseline();
        let key = r.glyph_key('\u{23F5}');
        let installed_face_may_win = runtime && !sealed;
        assert!(
            key.source == FaceId::Procedural
                || (installed_face_may_win && key.source == FaceId::RuntimeFallback),
            "runtime={runtime} sealed={sealed}: U+23F5 must reach the fontless tier (or, with \
             discovery open, an installed face that draws it), not `.notdef`: {key:?}"
        );
        assert_eq!(key.cell_span, 1, "U+23F5 is a narrow (EAW=N) code point");
        let img = r.glyph_image(key).clone();
        let (w, h, bytes) = mono(&img);
        if key.source == FaceId::Procedural {
            assert_eq!((w, h), (cw, ch), "one cell exactly");
        } else {
            let top = baseline - h as i32 - img.ymin();
            assert!(
                img.xmin() >= 0
                    && img.xmin() as usize + w <= cw
                    && top >= 0
                    && top as usize + h <= ch,
                "the installed face's U+23F5 leaves its cell: {w}x{h} at ({}, {top}) in {cw}x{ch}",
                img.xmin()
            );
        }
        let ink = bytes.iter().filter(|&&b| b > 0).count();
        assert!(
            ink > 0,
            "runtime={runtime} sealed={sealed}: U+23F5 rasterized empty"
        );
        let half = |lo: usize, hi: usize| -> u32 {
            (0..h)
                .flat_map(|y| (lo..hi).map(move |x| (x, y)))
                .map(|(x, y)| u32::from(bytes[y * w + x]))
                .sum()
        };
        assert!(half(0, w / 2) > half(w - w / 2, w), "⏵ must point RIGHT");
    }
}

#[test]
fn no_symbol_in_the_table_is_ever_tofu() {
    let mut r = renderer(false, false);
    for c in all_symbols() {
        let key = r.glyph_key(c);
        let notdef = key.source == FaceId::Primary && key.glyph_class == GlyphClass::Mono;
        assert!(
            !notdef,
            "U+{:04X} {c:?} still resolves to `.notdef`",
            c as u32
        );
        let ink = r
            .glyph_image(key)
            .bytes()
            .iter()
            .filter(|&&b| b > 0)
            .count();
        assert!(
            ink > 0,
            "U+{:04X} {c:?} rasterized empty ({key:?})",
            c as u32
        );
    }
}

#[test]
fn a_font_that_has_the_glyph_always_wins() {
    let mut r = renderer(false, false);
    for c in ['▶', '●', '■', '✓'] {
        let key = r.glyph_key(c);
        assert_eq!(
            key.source,
            FaceId::Primary,
            "{c:?} belongs to the primary face"
        );
        assert_eq!(key.glyph_class, GlyphClass::MonoGid);
    }
}

#[test]
fn a_wide_symbol_spans_both_cells() {
    let mut r = renderer(false, false);
    let (cw, ch) = r.cell_size();
    let key = r.glyph_key('\u{23E9}');
    assert_eq!(key.source, FaceId::Procedural);
    assert_eq!(key.cell_span, 2);
    let (w, h, bytes) = mono(r.glyph_image(key));
    assert_eq!((w, h), (2 * cw, ch));
    let col_ink = |x: usize| (0..h).any(|y| bytes[y * w + x] > 0);
    assert!(
        (cw..2 * cw).any(col_ink),
        "a two-cell symbol must put ink in its second cell"
    );
}

#[test]
fn the_synthesis_still_reports_the_miss_to_a_poll_based_host() {
    let mut r = renderer(false, false);
    r.take_missing_font_classes();
    r.glyph_key('\u{23F5}');
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_TEXT);
}

/// The other entry to the synthesis: a default-text symbol only the colour face
/// covers (`⏺` U+23FA with Noto Color Emoji) draws the fontless disc, not the
/// face's monochrome key-cap silhouette, and reports the text class, so the
/// text face a poll-based host injects in answer outranks the synthesis. Linux
/// only: Linux's Noto Color Emoji is the face whose silhouette is the key cap.
#[cfg(target_os = "linux")]
#[test]
fn the_colour_silhouette_intercept_draws_the_disc_and_reports_the_text_class() {
    let emoji = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf";
    let Ok(bytes) = std::fs::read(emoji) else {
        aterm_spec::host::could_not_run(format!("{emoji} is not installed here"));
    };
    let mut r = renderer(false, false);
    r.set_color_font_bytes(bytes).expect("colour face parses");
    r.take_missing_font_classes();
    let key = r.glyph_key('\u{23FA}');
    assert_eq!(
        key.source,
        FaceId::Procedural,
        "`⏺` must not take the colour face's monochrome key-cap silhouette"
    );
    assert_eq!(
        r.take_missing_font_classes(),
        MISSING_FONT_CLASS_TEXT,
        "the intercept is still a TEXT-face miss, so a poll host injects one"
    );
    let (w, h, bytes) = mono(r.glyph_image(key));
    for &(x, y) in &[(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
        assert_eq!(bytes[y * w + x], 0, "corner ({x},{y}) inked: not a disc");
    }
}

#[test]
fn symbol_bitmaps_are_sized_inked_and_inside_the_box() {
    for &(w, h) in &[
        (4usize, 8usize),
        (7, 15),
        (9, 19),
        (10, 21),
        (12, 26),
        (20, 40),
    ] {
        for c in all_symbols() {
            for span in [1usize, 2] {
                let cov = symbol_coverage(c, w, h, span)
                    .unwrap_or_else(|| panic!("{c:?} claimed but not drawn"));
                assert_eq!(cov.len(), span * w * h, "{c:?} {w}x{h} span {span}");
                assert!(cov.iter().any(|&b| b > 0), "{c:?} {w}x{h}: no ink");
            }
        }
    }
    assert!(symbol_coverage('A', 10, 20, 1).is_none());
    assert!(
        symbol_coverage('─', 10, 20, 1).is_none(),
        "box drawing is not this tier"
    );
    assert!(symbol_coverage('\u{23F5}', 0, 20, 1).is_none());
}

#[test]
fn mirrored_symbols_are_exact_mirrors() {
    let (w, h) = (11usize, 23usize);
    let mirror = |v: &[u8]| -> Vec<u8> {
        (0..h)
            .flat_map(|y| (0..w).rev().map(move |x| (x, y)))
            .map(|(x, y)| v[y * w + x])
            .collect()
    };
    for (a, b) in [('⏵', '⏴'), ('▶', '◀'), ('►', '◄'), ('◐', '◑'), ('▷', '◁')] {
        let ca = symbol_coverage(a, w, h, 1).unwrap();
        let cb = symbol_coverage(b, w, h, 1).unwrap();
        let diff = mirror(&ca)
            .iter()
            .zip(&cb)
            .map(|(&x, &y)| x.abs_diff(y))
            .max()
            .unwrap();
        assert!(
            diff <= 16,
            "{a:?} and {b:?} are not mirror images (max diff {diff})"
        );
    }
}

#[test]
fn the_renderer_draws_the_tree_connector_procedurally_even_when_a_font_covers_it() {
    let check = |r: &mut Renderer, label: &str| {
        let key = r.glyph_key('\u{23BF}');
        assert_eq!(
            key.source,
            FaceId::Procedural,
            "{label}: `⎿` must be drawn by the procedural box-drawing family"
        );
        let (w, h, conn) = mono(r.glyph_image(key));
        let conn = conn.to_vec();
        let bar_key = r.glyph_key('│');
        let (bw, bh, bar) = mono(r.glyph_image(bar_key));
        assert_eq!((w, h), (bw, bh), "{label}: same cell geometry as `│`");
        assert_eq!(&conn[..w], &bar[..w], "{label}: top row must meet `│`");
    };
    for (runtime, sealed) in [(false, false), (true, false), (true, true)] {
        let mut r = renderer(runtime, sealed);
        check(&mut r, &format!("runtime={runtime} sealed={sealed}"));
    }
    let iosevka = include_bytes!("../assets/bundled/IosevkaTerm-Regular.ttf");
    let mut r = renderer(false, false);
    r.add_fallback_bytes(iosevka)
        .expect("Iosevka Term admissible");
    check(&mut r, "with Iosevka Term (covers U+23BF) in the chain");
}
