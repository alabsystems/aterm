// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The chain's LAST-RESORT fontless symbol tier (`font_chain::Tier::Synthetic`,
//! `procedural::covers_symbol`), bound to the real renderer.
//!
//! THE BUG THIS PINS, measured on a Linux aarch64 host on 2026-09-24: the
//! Claude Code / Codex footer `⏵⏵ bypass permissions on` drew two `.notdef`
//! boxes, because NO installed face covers U+23F5 (`fc-list ':charset=23f5'`
//! answered nothing), and Claude Code's `⏺` tool bullet drew a solid `■` — the
//! monochrome silhouette of Noto Color Emoji's key-cap button. macOS has STIX
//! Two Math for both, which is why it was never seen there.
//!
//! Machine-independent by construction EXCEPT where a test opens runtime
//! discovery on an unsealed renderer: `Renderer::from_bytes` leaves every
//! system-font path list empty, so the bundled DejaVu Sans Mono is the whole
//! chain until then — but runtime discovery asks the INSTALLED fonts, and an
//! installed face that draws the character wins over the synthesis (it is the
//! last resort). A seal closes that tier again. So the unsealed-discovery
//! policy is the one place a test must accept either answer, and it does so by
//! name: macOS resolves U+23F5 to `RuntimeFallback` there (measured
//! 2026-09-25), the bare Linux host above to `Procedural`.

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

/// Every code point the synthesis claims, from the table itself.
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
        // Only an open (unsealed) runtime tier may reach an installed face,
        // and when one draws U+23F5 it must win: the synthesis is the LAST
        // resort. Every other policy has no face for it and must synthesize.
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
            // A font's triangle is fitted into the cell box, not drawn as it.
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
        // A right-pointing triangle: more ink in the left half than the right.
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
        // `.notdef` is the Primary face addressed BY CHAR; a real primary glyph
        // is addressed by glyph id.
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
    // DejaVu Sans Mono carries ▶ ● ■ ✓: the synthesis must NOT pre-empt them.
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
    // ⏩ is Emoji_Presentation=Yes, so two cells wide; with no colour face in
    // the chain, the synthesis draws it centred across both.
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
    // E1: a web host learns which face class to inject from the miss drain. A
    // synthesized glyph is still a font miss, so it must still be reported —
    // otherwise the host never fetches the real face that would outrank it.
    let mut r = renderer(false, false);
    r.take_missing_font_classes();
    r.glyph_key('\u{23F5}');
    assert_eq!(r.take_missing_font_classes(), MISSING_FONT_CLASS_TEXT);
}

/// The OTHER entry to the synthesis: a default-TEXT symbol that only the
/// colour face covers (`⏺` U+23FA with Noto Color Emoji). The chain used to
/// take that face's monochrome silhouette — a solid key-cap square — and
/// report NOTHING; it now draws the fontless disc and reports the TEXT class,
/// deliberately: the text face a poll-based host injects in answer outranks
/// the synthesis, which is the "a real face always wins" contract. This pins
/// both halves of that behaviour change on the intercept path, which
/// `the_synthesis_still_reports_the_miss_to_a_poll_based_host` (no colour
/// face) does not reach.
#[test]
fn the_colour_silhouette_intercept_draws_the_disc_and_reports_the_text_class() {
    let emoji = "/usr/share/fonts/truetype/noto/NotoColorEmoji.ttf";
    let Ok(bytes) = std::fs::read(emoji) else {
        eprintln!("SKIP: {emoji} is not installed here");
        return;
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
    // A disc, not a filled square: the cell's corners carry no ink.
    for &(x, y) in &[(0, 0), (w - 1, 0), (0, h - 1), (w - 1, h - 1)] {
        assert_eq!(bytes[y * w + x], 0, "corner ({x},{y}) inked: not a disc");
    }
}

#[test]
fn a_genuine_miss_is_still_notdef() {
    let mut r = renderer(true, true);
    let key = r.glyph_key('\u{FFFF}');
    assert_eq!(key.source, FaceId::Primary);
    assert_eq!(
        key.glyph_class,
        GlyphClass::Mono,
        "`.notdef` stays `.notdef`"
    );
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
        // The 4x4 subsample grid is symmetric; allow one step of rounding.
        assert!(
            diff <= 16,
            "{a:?} and {b:?} are not mirror images (max diff {diff})"
        );
    }
}

#[test]
fn the_tree_connector_meets_a_vertical_line_above_it() {
    // `⎿` under `│`: the connector's vertical stroke sits on the SAME columns
    // and reaches the cell's top edge, so the two join with no seam.
    let (w, h) = (10usize, 21usize);
    let conn = aterm_render::procedural::coverage('\u{23BF}', w, h).unwrap();
    let bar = aterm_render::procedural::coverage('│', w, h).unwrap();
    assert_eq!(&conn[..w], &bar[..w], "top rows must match `│`");
    assert!(
        conn.iter().all(|&b| b == 0 || b == 255),
        "hard 0/255 like box drawing"
    );
    // It is a PRE-EMPTIVE procedural glyph, not a last-resort symbol: the
    // last-resort table must not also claim it (the two families are disjoint).
    assert!(!covers_symbol('\u{23BF}'));
    assert!(symbol_coverage('\u{23BF}', w, h, 1).is_none());
}

/// The join is only real if the RENDERER draws the connector procedurally —
/// the previous revision drew it in the last-resort tier, so on any host with
/// a face covering U+23BF (Noto CJK on the Linux host that reported the bug)
/// the font's short bottom-of-cell `⎿` won and left a gap under the `│` above.
/// Checked through `glyph_key` / `glyph_image`, on every policy, and — where a
/// real CJK face is installed — with that face injected into the chain.
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
    let cjk = "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc";
    match std::fs::read(cjk) {
        Ok(bytes) => {
            let mut r = renderer(false, false);
            r.add_fallback_bytes(&bytes).expect("CJK face admissible");
            check(&mut r, "with Noto Sans CJK (covers U+23BF) in the chain");
        }
        Err(_) => eprintln!("SKIP (font-covers half): {cjk} is not installed here"),
    }
}
