// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Fallback harmony: the shipping pure policies behind fallback-face metric normalization,
//! wide-glyph centring, the row and column band clips, weight-matched candidate ranking and
//! the macOS CoreText drawability probe.
//!
//! The ty models `fallback_scale_clamp_model`, `wide_center_model` and
//! `fallback_band_clip_model` (aterm-spec) carry the clamp, balance and in-band invariants.
//! This file sweeps the real `f32`/integer functions over dense lattices plus inputs the
//! models cannot express (NaN, ∞, floor division on negative gaps, garbage font bytes); the
//! ratio arithmetic is outside ty's `Expr` language, so these sweeps are its only binding.
//!
//! ## Proof
//!
//! * **Concrete (this file)** — the real `f32`/integer functions are
//!   swept over dense/exhaustive lattices (plus degenerate and adversarial
//!   inputs: NaN, ∞, floor division on negative gaps, garbage font bytes),
//!   with non-vacuity controls and the
//!   pre-fix behaviour shown to violate each law. Floor division and the
//!   ratio arithmetic are outside ty's Expr language (no `*`/`/`), so — per
//!   the box-drawing rounding-law precedent — the lattice sweeps here are the
//!   proof.
//!
//! ## W8 (h): the column band
//!
//! The row band had no column twin, so a fallback glyph wider than its cell
//! painted straight over its neighbours: STIX Two Math designs U+27F5..U+27FC
//! 1.499 em wide, the x-height normalization scales that UP ~11%, and the
//! result covered ~2.9 cells inside a ONE-cell grid box (the grid itself is
//! right — these are East_Asian_Width Neutral, so `wcwidth` and aterm agree
//! on 1 — so the defect is purely a PAINT overrun). `fallback_fit_scale`
//! shrinks such a glyph into its box; `clamp_to_col_band` is the backstop
//! that trims the rounding fringe the fit leaves.
//!
//! The column clamp is a one-line wrapper over the same `clamp_to_band` core
//! as the row clamp (its arithmetic is axis-free).

use aterm_render::{
    CJK_SCALE_MAX, CJK_SCALE_MIN, XHEIGHT_SCALE_MAX, XHEIGHT_SCALE_MIN, clamp_to_row_band,
    fallback_cell_count, fallback_cjk_scale, fallback_fit_scale, fallback_weight_rank,
    fallback_xheight_scale, wide_center_offset,
};

// Normalization clamps.

/// On a dense lattice of finite positive (target, actual) pairs the scale stays inside the
/// clamp interval, and when the ideal ratio is inside it the scaled advance equals the
/// target (1e-3 relative).
#[test]
fn scale_clamp_bounds_and_exactness_lattice() {
    let vals: Vec<f32> = (1..=400).map(|i| i as f32 * 0.25).collect(); // 0.25..=100
    let mut clamped_lo = 0u32;
    let mut clamped_hi = 0u32;
    let mut exact = 0u32;
    for &target in &vals {
        for &actual in &vals {
            for (scale, lo, hi) in [
                (
                    fallback_cjk_scale(target, actual),
                    CJK_SCALE_MIN,
                    CJK_SCALE_MAX,
                ),
                (
                    fallback_xheight_scale(target, actual),
                    XHEIGHT_SCALE_MIN,
                    XHEIGHT_SCALE_MAX,
                ),
            ] {
                assert!(
                    (lo..=hi).contains(&scale),
                    "scale {scale} out of [{lo}, {hi}] for target={target} actual={actual}"
                );
                let ratio = target / actual;
                if ratio >= lo && ratio <= hi {
                    let err = (scale * actual - target).abs() / target;
                    assert!(
                        err <= 1e-3,
                        "in-range ratio must normalize exactly: target={target} \
                         actual={actual} scale={scale} err={err}"
                    );
                    exact += 1;
                } else if ratio < lo {
                    assert_eq!(scale, lo, "below-range ratio must clamp to lo");
                    clamped_lo += 1;
                } else {
                    assert_eq!(scale, hi, "above-range ratio must clamp to hi");
                    clamped_hi += 1;
                }
            }
        }
    }
    // Non-vacuity: all three regimes are genuinely exercised by the lattice.
    assert!(exact > 0 && clamped_lo > 0 && clamped_hi > 0);
}

/// Zero, negative, NaN and ∞ metrics yield the neutral scale `1.0`, never a panic or NaN.
#[test]
fn scale_clamp_total_on_degenerate_inputs() {
    let bad = [
        0.0f32,
        -1.0,
        -100.0,
        f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
    ];
    for &a in &bad {
        for &b in bad.iter().chain(&[16.0f32]) {
            assert_eq!(fallback_cjk_scale(a, b), 1.0, "cjk({a}, {b})");
            assert_eq!(fallback_cjk_scale(b, a), 1.0, "cjk({b}, {a})");
            assert_eq!(fallback_xheight_scale(a, b), 1.0, "xh({a}, {b})");
            assert_eq!(fallback_xheight_scale(b, a), 1.0, "xh({b}, {a})");
        }
    }
    // Finite operands whose ratio overflows to ∞ also yield the neutral scale.
    assert_eq!(fallback_cjk_scale(f32::MAX, f32::MIN_POSITIVE), 1.0);
}

// Centring law.

/// Exhaustive over every (box_w, adv) in a lattice that includes `adv >
/// box_w` (a clamped face whose scaled advance overflows the box): the
/// offset satisfies the floor characterization `2*off <= gap <= 2*off + 1`,
/// hence `|left - right| <= 1`.
#[test]
fn wide_center_offset_balances_margins_exhaustively() {
    let mut nonzero_offsets = 0u32;
    for box_w in 0..=64i32 {
        for adv in 0..=80i32 {
            let gap = box_w - adv;
            let off = wide_center_offset(box_w, adv);
            // The floor characterization.
            assert!(
                2 * off <= gap && gap <= 2 * off + 1,
                "off={off} is not floor((box={box_w} - adv={adv})/2)"
            );
            let (left, right) = (off, gap - off);
            assert!(
                (left - right).abs() <= 1,
                "margins unbalanced: box={box_w} adv={adv} left={left} right={right}"
            );
            if off != 0 {
                nonzero_offsets += 1;
            }
        }
    }
    // Non-vacuity: the offset genuinely moves glyphs (not identically zero).
    assert!(nonzero_offsets > 0);
}

/// The fit contains the whole bitmap and its advance after centring and never enlarges a
/// glyph that fits; an oversized warning sign (30px mask, 27.5px advance, 10px cell) gets a
/// roughly one-third uniform scale, not a crop.
#[test]
fn fallback_fit_scale_contains_ink_and_preserves_aspect() {
    let warning = fallback_fit_scale(10, 20, 30, 27, -1, 27.501_354);
    assert!(
        (0.30..0.35).contains(&warning),
        "warning must shrink uniformly instead of clipping: scale={warning}"
    );

    let mut shrunk = 0usize;
    for box_w in 1..=32usize {
        for box_h in 1..=40usize {
            for width in 1..=48usize {
                for height in [1usize, 7, 19, 41] {
                    for xmin in [-8i32, -1, 0, 3, 11] {
                        for advance in [1.0f32, 8.5, 20.0, 55.25] {
                            let scale =
                                fallback_fit_scale(box_w, box_h, width, height, xmin, advance);
                            assert!(scale.is_finite() && scale > 0.0 && scale <= 1.0);
                            let centre = advance * 0.5;
                            let radius = (xmin as f32 - centre)
                                .abs()
                                .max((xmin as f32 + width as f32 - centre).abs())
                                .max(centre);
                            assert!(
                                2.0 * radius * scale <= box_w as f32 + 1e-4,
                                "centred horizontal extent escaped: box={box_w} width={width} \
                                 xmin={xmin} advance={advance} scale={scale}"
                            );
                            assert!(height as f32 * scale <= box_h as f32 + 1e-4);
                            shrunk += usize::from(scale < 1.0);
                        }
                    }
                }
            }
        }
    }
    assert!(shrunk > 0, "the oversize arm must be exercised");
}

/// The symbol fit contains the centred ink (not the advance), is never smaller than the
/// text fit and never enlarges; for a wide-advance symbol (Noto Sans Symbols 2's `⏺`: 0.9 em
/// advance, 0.7 em disc) it is materially larger than the text fit.
#[test]
fn fallback_ink_fit_scale_contains_the_ink_only() {
    use aterm_render::fallback_ink_fit_scale;
    let mut larger = 0usize;
    for box_w in 1..=32usize {
        for box_h in 1..=40usize {
            for width in 1..=48usize {
                for height in [1usize, 7, 19, 41] {
                    for xmin in [-8i32, -1, 0, 3, 11] {
                        for advance in [1.0f32, 8.5, 20.0, 55.25] {
                            let ink =
                                fallback_ink_fit_scale(box_w, box_h, width, height, xmin, advance);
                            let text =
                                fallback_fit_scale(box_w, box_h, width, height, xmin, advance);
                            assert!(ink.is_finite() && ink > 0.0 && ink <= 1.0);
                            assert!(ink >= text, "the ink fit shrank more than the text fit");
                            let centre = advance * 0.5;
                            let radius = (xmin as f32 - centre)
                                .abs()
                                .max((xmin as f32 + width as f32 - centre).abs());
                            assert!(
                                2.0 * radius * ink <= box_w as f32 + 1e-4,
                                "centred ink escaped: box={box_w} width={width} \
                                 xmin={xmin} advance={advance} scale={ink}"
                            );
                            assert!(height as f32 * ink <= box_h as f32 + 1e-4);
                            larger += usize::from(ink > text);
                        }
                    }
                }
            }
        }
    }
    assert!(larger > 0, "the side-bearing arm must be exercised");
    // ⏺ at 18 px in an 11 px cell: 13 px advance, a 10 px disc inset 1.5 px.
    let ink = fallback_ink_fit_scale(11, 24, 10, 10, 1, 13.0);
    let text = fallback_fit_scale(11, 24, 10, 10, 1, 13.0);
    assert!(ink >= 0.99, "the disc already fits the cell: {ink}");
    assert!(
        text < 0.85,
        "negative control — the advance fit shrinks it: {text}"
    );
}

/// The fit class is the code point's Unicode class: letters (every script,
/// the Mathematical Alphanumeric block included) and digits are TEXT;
/// pictographs, media controls, shapes, dingbats and operators are SYMBOLS.
#[test]
fn the_fallback_fit_class_follows_the_unicode_class() {
    use aterm_render::{FallbackFit, fallback_fit_class};
    for ch in [
        'A', 'ж', 'ß', '中', 'あ', '𝐖', '𝐌', '𝑊', '𝓜', 'ℵ', '𝟙', '7', '٣', 'Ⅻ',
    ] {
        assert_eq!(fallback_fit_class(ch), FallbackFit::Text, "{ch}");
    }
    for ch in [
        '⏺', '⏸', '●', '⬤', '✔', '★', '⚠', '∑', '→', '⊕', '♥', '☃', '⎿',
    ] {
        assert_eq!(fallback_fit_class(ch), FallbackFit::Symbol, "{ch}");
    }
}

/// Cell allocation follows Unicode width and config, never a fallback font's proportional
/// advance.
#[test]
fn fallback_cell_count_matches_terminal_cjk_policy() {
    use aterm_types::text_shaping::{AmbiguousWidth, TextShapingConfig};

    let single = TextShapingConfig::default();
    let double = TextShapingConfig {
        ambiguous_width: AmbiguousWidth::Double,
        ..TextShapingConfig::default()
    };
    assert_eq!(
        fallback_cell_count('\u{26A0}', &single),
        1,
        "bare ⚠ is narrow"
    );
    assert_eq!(
        fallback_cell_count('\u{26A0}', &double),
        1,
        "⚠ is not EAW ambiguous"
    );
    assert_eq!(fallback_cell_count('中', &single), 2, "CJK stays two cells");
    assert_eq!(fallback_cell_count('中', &double), 2, "CJK stays two cells");
    assert_eq!(
        fallback_cell_count('\u{0301}', &single),
        0,
        "combining stays zero-width"
    );
    assert_eq!(
        fallback_cell_count('\u{00B7}', &single),
        1,
        "middle dot is narrow by default"
    );
    assert_eq!(
        fallback_cell_count('\u{00B7}', &double),
        2,
        "EAW ambiguous widens in CJK mode"
    );
}

/// Engine → `RenderInput` → CPU raster cache for the config-sensitive edge classes: one
/// EAW-A scalar received wide, then again after the policy reloads narrow (no reflow), so
/// both spans coexist in one row and must reach distinct fitted raster keys; a zero-width
/// combining overlay stays span 0.
#[cfg(feature = "embedded-font")]
#[test]
fn materialized_ambiguous_spans_coexist_and_combining_stays_zero_width() {
    use aterm_core::config::TerminalConfig;
    use aterm_core::terminal::Terminal;
    use aterm_render::{
        FaceId, Renderer, Theme, display_face_bytes, embedded_font, materialized_cell_span,
    };

    let primary_bytes = display_face_bytes("pixel").expect("bundled primary");
    let fallback_bytes = embedded_font();
    let primary_face = aterm_font::Face::parse(primary_bytes, 0).expect("primary parses");
    let fallback_face = aterm_font::Face::parse(fallback_bytes, 0).expect("fallback parses");
    let covered_only_by_fallback = |ch: char| {
        primary_face.glyph_index(ch).is_none() && fallback_face.glyph_index(ch).is_some()
    };
    let ambiguous = (0..=0xFFFFu32)
        .filter_map(char::from_u32)
        .find(|&ch| {
            aterm_grapheme::is_ambiguous_width(ch)
                && aterm_grapheme::char_width(ch) == 1
                && aterm_grapheme::char_width_cjk(ch) == 2
                && covered_only_by_fallback(ch)
        })
        .expect("fixture pair must expose an EAW-A fallback glyph");
    let combining = (0x0300..=0x036Fu32)
        .filter_map(char::from_u32)
        .find(|&ch| aterm_grapheme::char_width(ch) == 0 && covered_only_by_fallback(ch))
        .expect("fixture pair must expose a combining fallback glyph");

    let mut r = Renderer::from_bytes(primary_bytes, 16.0, Theme::default()).expect("renderer");
    r.set_fallback_bytes(fallback_bytes)
        .expect("fallback installs");
    let (cell_w, _) = r.cell_size();

    let mut term = Terminal::new(1, 6);
    term.process(b"\x1b[?25l");
    let mut config = TerminalConfig {
        ambiguous_width_double: true,
        ..TerminalConfig::default()
    };
    term.apply_config(&config);
    term.process(ambiguous.to_string().as_bytes());
    config.ambiguous_width_double = false;
    term.apply_config(&config);
    term.process(ambiguous.to_string().as_bytes());
    term.process(combining.to_string().as_bytes());
    let input = term.cell_frame(1, 6);
    let row = &input.cells[0];
    assert_eq!(row[0].ch, ambiguous);
    assert!(
        row[1].wide,
        "the first scalar keeps its stored continuation"
    );
    assert_eq!(row[2].ch, ambiguous);
    assert!(
        row.get(3).is_none_or(|cell| !cell.wide),
        "the post-reload scalar stays one cell"
    );
    assert_eq!(materialized_cell_span(row, 0), 2);
    assert_eq!(materialized_cell_span(row, 2), 1);

    let wide_key = r.resolve_cell_key_for_span(
        input.cluster_at(0, 0),
        &row[0],
        materialized_cell_span(row, 0),
    );
    let narrow_key = r.resolve_cell_key_for_span(
        input.cluster_at(0, 2),
        &row[2],
        materialized_cell_span(row, 2),
    );
    assert_eq!(wide_key.source, FaceId::Fallback);
    assert_eq!(narrow_key.source, FaceId::Fallback);
    assert_eq!(wide_key.cell_span, 2);
    assert_eq!(narrow_key.cell_span, 1);
    assert_ne!(wide_key, narrow_key, "span is load-bearing cache identity");

    let combining_key = r.glyph_key(combining);
    assert_eq!(combining_key.source, FaceId::Fallback);
    assert_eq!(combining_key.cell_span, 0);

    assert!(!r.glyph_cache_contains(wide_key));
    assert!(!r.glyph_cache_contains(narrow_key));
    assert!(!r.glyph_cache_contains(combining_key));
    let _ = r.render_input(&input);
    assert!(
        r.glyph_cache_contains(wide_key),
        "shipping frame path must rasterize the stored two-cell variant"
    );
    assert!(
        r.glyph_cache_contains(narrow_key),
        "shipping frame path must rasterize the stored one-cell variant"
    );
    assert!(
        r.glyph_cache_contains(combining_key),
        "shipping frame path must rasterize the zero-width overlay"
    );

    let wide = r.glyph_image(wide_key).clone();
    assert_eq!(wide.advance(), (2 * cell_w) as f32);
    assert!(wide.xmin() >= 0 && wide.xmin() + wide.width() as i32 <= (2 * cell_w) as i32);
    let narrow = r.glyph_image(narrow_key).clone();
    assert_eq!(narrow.advance(), cell_w as f32);
    assert!(narrow.xmin() >= 0 && narrow.xmin() + narrow.width() as i32 <= cell_w as i32);
    assert_ne!(
        (
            wide.width(),
            wide.xmin(),
            wide.advance().to_bits(),
            wide.bytes()
        ),
        (
            narrow.width(),
            narrow.xmin(),
            narrow.advance().to_bits(),
            narrow.bytes()
        ),
        "the two materialized spans must produce genuinely distinct geometry"
    );
    let combining_img = r.glyph_image(combining_key);
    assert_ne!(
        combining_img.width(),
        0,
        "combining coverage is non-vacuous"
    );
    assert_ne!(combining_img.bytes().iter().copied().max(), Some(0));
}

// Row-band clip.

/// Exhaustive over every (top, height, band) in a lattice spanning negative
/// tops (ascender overshoot), zero-size bitmaps/bands and far-past-the-band
/// placements: the trim only ever drops rows, every kept row lies inside
/// `[0, band)`, and an already-in-band glyph is untouched.
#[test]
fn clamp_to_row_band_keeps_rows_in_band_exhaustively() {
    let mut trimmed = 0u32;
    let mut untouched = 0u32;
    for top in -48..=48i32 {
        for height in 0..=48usize {
            for band in 0..=40usize {
                let (skip, keep) = clamp_to_row_band(top, height, band);
                assert!(
                    skip + keep <= height,
                    "trim grew the bitmap: top={top} h={height} band={band} -> ({skip}, {keep})"
                );
                let new_top = top + skip as i32;
                if keep > 0 {
                    assert!(
                        new_top >= 0 && new_top + keep as i32 <= band as i32,
                        "kept rows escape the band: top={top} h={height} band={band} \
                         -> ({skip}, {keep})"
                    );
                }
                // An in-band glyph is byte-identical (no gratuitous trims).
                if top >= 0 && top + height as i32 <= band as i32 {
                    assert_eq!((skip, keep), (0, height), "in-band glyph must be untouched");
                    untouched += 1;
                } else if height > 0 {
                    trimmed += 1;
                }
            }
        }
    }
    // Non-vacuity: both regimes are exercised.
    assert!(trimmed > 0 && untouched > 0);
}

// Weight and italic ranking.

/// A style (italic) mismatch outranks any weight distance (OS/2 weights are 1..=1000 and
/// the italic penalty is 1000); among style-matched candidates the closest weight wins.
#[test]
fn weight_rank_orders_candidates() {
    // Regular primary: a W3 (300) CJK face beats a W6 (600) one.
    assert!(
        fallback_weight_rank(300, false, 400, false) < fallback_weight_rank(600, false, 400, false)
    );
    // Exact match is rank 0 (best possible).
    assert_eq!(fallback_weight_rank(400, false, 400, false), 0);
    // Italic mismatch dominates even the worst weight gap.
    assert!(
        fallback_weight_rank(1000, false, 1, false) < fallback_weight_rank(1, true, 1, false),
        "style mismatch must outrank any weight distance"
    );
    // An italic primary prefers italic candidates symmetrically.
    assert!(
        fallback_weight_rank(400, true, 400, true) < fallback_weight_rank(400, false, 400, true)
    );
}

// Probe soundness (macOS CoreText).

/// The CoreText drawability probe is total (garbage bytes, truncated fonts, an
/// out-of-range collection index and the empty blob return `false` without panicking) and
/// sound on a real face (accepting means a non-empty raster was obtained). Non-vacuity:
/// Menlo is accepted for 'M'; the colour-emoji face is rejected for an emoji, which belongs
/// to the colour pipeline.
#[test]
#[cfg(target_os = "macos")]
fn ct_probe_is_total_and_sound() {
    use aterm_render::ct_face_can_render;

    // Adversarial corpus: must be `false`, must not panic.
    assert!(!ct_face_can_render(&[], 0, 'M'), "empty blob");
    assert!(!ct_face_can_render(&[0u8; 64], 0, 'M'), "zero blob");
    let junk: Vec<u8> = (0..4096u32)
        .map(|i| (i.wrapping_mul(2_654_435_761) >> 13) as u8)
        .collect();
    assert!(!ct_face_can_render(&junk, 0, 'M'), "pseudo-random blob");
    let menlo = std::fs::read("/System/Library/Fonts/Menlo.ttc").expect("macOS ships Menlo.ttc");
    {
        for cut in [1usize, 12, 256, 4096, menlo.len() / 2] {
            assert!(
                !ct_face_can_render(&menlo[..cut.min(menlo.len())], 0, 'M') || cut >= menlo.len(),
                "truncated real font at {cut} bytes must not panic (and must not \
                 claim a raster it can't obtain)"
            );
        }
        // Out-of-range collection index: total, false.
        assert!(
            !ct_face_can_render(&menlo, 999, 'M'),
            "face index out of range"
        );
        // Non-vacuity: accepted for a covered char, rejected for an uncovered one.
        assert!(ct_face_can_render(&menlo, 0, 'M'), "Menlo must accept 'M'");
        assert!(
            !ct_face_can_render(&menlo, 0, '\u{FFFF}'),
            "a permanent noncharacter has no glyph"
        );
    }
    // Colour-bitmap glyphs are rejected (the mono resolver's contract).
    let emoji = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc")
        .expect("macOS ships Apple Color Emoji");
    assert!(
        !ct_face_can_render(&emoji, 0, '\u{1F600}'),
        "a colour-bitmap glyph must not pass the MONO drawability probe"
    );
}

/// A checked-in face's bytes, for tests of the fallback lane's law rather than this host's
/// faces.
fn asset(rel: &str) -> Vec<u8> {
    let path = format!("{}/assets/{rel}", env!("CARGO_MANIFEST_DIR"));
    std::fs::read(&path).unwrap_or_else(|e| panic!("{path}: {e}"))
}

/// A renderer on the checked-in Cascadia Mono (no long arrows, no `⚠`) with
/// `fallback` injected as its broad tier.
fn cascadia_with_fallback(px: f32, fallback: &str) -> aterm_render::Renderer {
    let mut r = aterm_render::Renderer::from_bytes(
        &asset("bundled/CascadiaMonoPL-Regular.ttf"),
        px,
        aterm_render::Theme::default(),
    )
    .expect("Cascadia Mono builds a renderer");
    r.set_fallback_bytes(&asset(fallback))
        .expect("the fallback face parses");
    r
}

/// Whether some face of a candidate file in `paths` maps `ch`: a host fact read from the
/// files, never from the renderer.
fn a_candidate_file_maps(paths: &[String], ch: char) -> bool {
    paths.iter().any(|p| {
        std::fs::read(p).is_ok_and(|bytes| {
            let faces = aterm_font::fonts_in_collection(&bytes).unwrap_or(1);
            (0..faces).any(|i| {
                aterm_font::Face::parse(&bytes, i)
                    .ok()
                    .and_then(|f| f.glyph_index(ch))
                    .is_some_and(|g| g.0 != 0)
            })
        })
    })
}

// End-to-end pins through the real renderer.

/// A wide CJK glyph from the fallback chain sits inside the row band and owns the full
/// 2-cell box, under both backends. It cannot run on a host whose candidate files map no
/// ideograph.
#[test]
fn cjk_fallback_glyph_is_banded_and_wide() {
    use aterm_render::{FaceId, GlyphClass, Renderer, Theme};
    let candidates = ['\u{4E2D}', '\u{4E00}', '\u{4E01}', '\u{53E3}']; // 中 一 丁 口
    let mut r = Renderer::from_system(16.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    if !candidates
        .iter()
        .any(|&c| a_candidate_file_maps(r.debug_fallback_candidate_paths(), c))
    {
        aterm_spec::host::could_not_run(
            "no fallback candidate file on this host maps an ideograph",
        );
    }
    // Otherwise `glyph_key` answers while the fallback parse is in flight and provisionally
    // routes every candidate to the primary, so none would reach the fallback chain.
    r.debug_block_on_lazy_fallbacks();
    let (_, cell_h) = r.cell_size();
    for force_fontdue in [false, true] {
        if force_fontdue {
            r.debug_force_fontdue();
        }
        // Any wide ideograph the fallback chain serves: a host's primary may cover some
        // of them (中 on some hosts).
        let key = candidates
            .iter()
            .map(|&c| r.glyph_key(c))
            .find(|k| k.source == FaceId::Fallback && k.glyph_class == GlyphClass::Mono)
            .expect("a candidate file maps an ideograph, so the chain serves one");
        assert_eq!(key.cell_span, 2, "a true CJK scalar defaults to two cells");
        let baseline = r.baseline();
        let (cell_w, _) = r.cell_size();
        let img = r.glyph_image(key);
        assert!(
            img.height() > 0 && img.bytes().iter().any(|&c| c > 0),
            "中 rasterized empty"
        );
        let top = baseline - img.height() as i32 - img.ymin();
        assert!(
            top >= 0 && top + img.height() as i32 <= cell_h as i32,
            "fallback coverage escapes the cell row band: top={top} h={} cell_h={cell_h} \
             (force_fontdue={force_fontdue})",
            img.height()
        );
        assert_eq!(
            img.advance(),
            (2 * cell_w) as f32,
            "a wide fallback glyph owns its 2-cell box (force_fontdue={force_fontdue})"
        );
        // A 2-cell glyph is fitted to its 2-cell box, never folded into one cell; the
        // CoreText raster width includes the AA pad, so check both bounds.
        assert!(
            img.xmin() >= 0 && img.xmin() + img.width() as i32 <= 2 * cell_w as i32,
            "a wide fallback glyph escapes its 2-cell box: xmin={} w={} cell_w={cell_w} \
             (force_fontdue={force_fontdue})",
            img.xmin(),
            img.width()
        );
        assert!(
            img.width() > cell_w,
            "a wide ideograph's ink no longer spans past one cell (w={} cell_w={cell_w}) — \
             the 2-cell fit has regressed (force_fontdue={force_fontdue})",
            img.width()
        );
    }
}

/// A bare warning sign is text-presentation and one cell wide; a proportional fallback
/// face must not let its ink spill into the next cell (`⚠ MCP`). Runs over the checked-in
/// Noto Sans Symbols 2, whose `⚠` is wider than a cell.
#[test]
fn narrow_fallback_symbol_is_bounded_to_one_cell() {
    use aterm_render::{FaceId, GlyphClass, StyleBits};
    let mut r = cascadia_with_fallback(16.0, "bundled/NotoSansSymbols2-Regular.ttf");
    let key = r.glyph_key('\u{26A0}');
    assert_eq!(
        (key.source, key.glyph_class),
        (FaceId::Fallback, GlyphClass::Mono),
        "the injected face serves the warning sign"
    );
    assert_eq!(key.cell_span, 1, "bare U+26A0 must stay one cell");
    let (cell_w_usize, cell_h) = r.cell_size();
    let cell_w = cell_w_usize as i32;
    let styles = [
        StyleBits::REGULAR,
        StyleBits::BOLD,
        StyleBits::ITALIC,
        StyleBits(StyleBits::BOLD.0 | StyleBits::ITALIC.0),
    ];
    let mut variants = Vec::new();
    for style in styles {
        let styled_key = r.glyph_key_styled('\u{26A0}', style);
        assert_eq!(styled_key.source, key.source);
        assert_eq!(styled_key.glyph_class, GlyphClass::Mono);
        assert_eq!(styled_key.cell_span, 1);
        assert_eq!(
            styled_key.style, style,
            "fallback style stays in cache identity"
        );
        let styled = r.glyph_image(styled_key).clone();
        assert!(
            styled.xmin() >= 0 && styled.xmin() + styled.width() as i32 <= cell_w,
            "{style:?} one-cell warning ink escaped: xmin={} width={} cell_w={cell_w}",
            styled.xmin(),
            styled.width()
        );
        assert_eq!(
            styled.advance(),
            cell_w as f32,
            "{style:?} warning owns exactly one cell"
        );
        assert!(
            styled.height() <= cell_h,
            "{style:?} warning mask must stay in its row band"
        );
        variants.push(styled);
    }
    // Non-vacuity: synthetic bold/italic apply to the fitted mask (deferred, so they
    // cannot shrink the glyph) and must yield a mask distinct from regular.
    for (name, styled) in [("bold", &variants[1]), ("italic", &variants[2])] {
        assert!(
            styled.bytes() != variants[0].bytes()
                || (
                    styled.width(),
                    styled.height(),
                    styled.xmin(),
                    styled.ymin()
                ) != (
                    variants[0].width(),
                    variants[0].height(),
                    variants[0].xmin(),
                    variants[0].ymin(),
                ),
            "{name} warning must exercise a distinct post-style mask"
        );
    }

    let img = &variants[0];

    // Shape preservation: the fitted triangle stays roughly square with ink on both sides
    // of centre; a horizontal crop would leave a tall central slice.
    let (w, h) = (img.width(), img.height());
    assert!(w > 0 && h > 0 && img.bytes().iter().any(|&a| a > 0));
    let mut min_x = w;
    let mut max_x = 0usize;
    let mut min_y = h;
    let mut max_y = 0usize;
    for y in 0..h {
        for x in 0..w {
            if img.bytes()[y * w + x] > 0 {
                min_x = min_x.min(x);
                max_x = max_x.max(x);
                min_y = min_y.min(y);
                max_y = max_y.max(y);
            }
        }
    }
    let (ink_w, ink_h) = (max_x - min_x + 1, max_y - min_y + 1);
    assert!(min_x < w / 2 && max_x >= w / 2, "triangle lost one side");
    assert!(
        ink_w * 2 >= ink_h && ink_h * 2 >= ink_w,
        "fitted warning must preserve its roughly-square silhouette: ink={ink_w}x{ink_h}"
    );
}

/// With the stock macOS candidate list, 中 comes from the native CJK face (Hiragino Sans
/// GB), not Arial Unicode, and a char Hiragino lacks still reaches the broad face.
#[test]
#[cfg(target_os = "macos")]
fn native_cjk_face_leads_the_macos_chain() {
    use aterm_render::{Renderer, Theme};
    let mut r = Renderer::from_system(16.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    let pick = r.debug_fallback_pick_path('\u{4E2D}');
    assert_eq!(
        pick.as_deref(),
        Some("/System/Library/Fonts/Hiragino Sans GB.ttc"),
        "中 must come from the native CJK chain face"
    );
    // Additive tier: Arabic (which Hiragino lacks) still reaches the broad face.
    let arabic = r.debug_fallback_pick_path('\u{0645}'); // م
    assert_eq!(
        arabic.as_deref(),
        Some("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
        "the broad face must still back the chain"
    );
}

/// U+27F5..U+27FC are East_Asian_Width Neutral (one cell) but fallback faces draw them
/// wider (STIX Two Math: 1.612 em advance, 1.499 em ink); their coverage must stay inside
/// the cell box, extending the `tests/display_faces.rs` law to the fallback lane, also under
/// `debug_force_fontdue`. Runs over this host's chain and over the checked-in Noto Sans
/// Math, which maps all eight, so every host checks the law. Non-vacuity: at least one
/// arrow is condensed to exactly `cell_w`.
#[test]
fn long_arrows_never_leave_their_cell() {
    use aterm_render::{FaceId, GlyphClass, Renderer, Theme};
    let mut checked = 0u32;
    let mut condensed = 0u32;
    for px in [12.0f32, 16.0, 24.0] {
        let mut host = Renderer::from_system(px, Theme::default())
            .expect("no font: build with aterm-render's default embedded-font feature");
        host.debug_block_on_lazy_fallbacks();
        let fixed = cascadia_with_fallback(px, "bundled/NotoSansMath-Regular.ttf");
        for mut r in [host, fixed] {
            for force_fontdue in [false, true] {
                if force_fontdue {
                    r.debug_force_fontdue();
                }
                let (cell_w, _) = r.cell_size();
                for ch in '\u{27F5}'..='\u{27FC}' {
                    let key = r.glyph_key(ch);
                    if !matches!(
                        key.source,
                        FaceId::Fallback | FaceId::SymbolFallback | FaceId::RuntimeFallback
                    ) || key.glyph_class != GlyphClass::Mono
                    {
                        continue;
                    }
                    let img = r.glyph_image(key);
                    if img.width() == 0 || img.height() == 0 {
                        continue; // no glyph on this host / this backend
                    }
                    assert!(
                        img.bytes().iter().any(|&c| c > 0),
                        "U+{:04X} condensed to blank coverage (px={px} force_fontdue={force_fontdue})",
                        ch as u32
                    );
                    assert!(
                        img.xmin() >= 0 && img.xmin() + img.width() as i32 <= cell_w as i32,
                        "U+{:04X} paints outside its cell: xmin={} w={} cell_w={cell_w} \
                     (px={px} force_fontdue={force_fontdue})",
                        ch as u32,
                        img.xmin(),
                        img.width()
                    );
                    checked += 1;
                    if img.width() == cell_w {
                        condensed += 1;
                    }
                }
            }
        }
    }
    assert!(checked > 0, "the checked-in face serves every long arrow");
    assert!(
        condensed > 0,
        "non-vacuity: {checked} arrows checked but none was condensed to the cell width — \
         the test would pass on a pre-fix build"
    );
}

/// A long arrow does not shear a box-drawing row: rendering `│⟹│` and `│ │` leaves both
/// `│` cells and columns 3–4 pixel-identical; the middle cell must differ.
#[test]
fn a_long_arrow_does_not_shear_a_box_drawing_row() {
    use aterm_core::terminal::Terminal;
    use aterm_render::{Renderer, Theme};
    let mut r = Renderer::from_system(18.0, Theme::default())
        .expect("no font: build with aterm-render's default embedded-font feature");
    r.debug_block_on_lazy_fallbacks();
    let (cw, ch) = r.cell_size();
    let (rows, cols) = (1usize, 5usize);
    let mut frame_of = |bytes: &[u8]| {
        let mut t = Terminal::new(rows as u16, cols as u16);
        t.process(bytes);
        r.render_input(&t.cell_frame(rows, cols))
    };
    let inked = frame_of("\x1b[?25l\u{2502}\u{27F9}\u{2502}".as_bytes());
    let bare = frame_of("\x1b[?25l\u{2502} \u{2502}".as_bytes());
    let cell = |f: &aterm_render::Frame, col: usize| -> Vec<u32> {
        let mut out = Vec::with_capacity(cw * ch);
        for y in 0..ch.min(f.height) {
            for x in col * cw..(col * cw + cw).min(f.width) {
                out.push(f.pixels[y * f.width + x]);
            }
        }
        out
    };
    // Non-vacuity first: the arrow really drew something.
    assert_ne!(
        cell(&inked, 1),
        cell(&bare, 1),
        "the arrow cell is identical to a space — nothing was drawn, so the \
         shear law below would be vacuous"
    );
    for rule in [0usize, 2] {
        assert_eq!(
            cell(&inked, rule),
            cell(&bare, rule),
            "the long arrow sheared the box-drawing rule at col {rule}"
        );
    }
    // The two columns an overrunning arrow would cover are untouched too.
    for spill in [3usize, 4] {
        assert_eq!(
            cell(&inked, spill),
            cell(&bare, spill),
            "the long arrow painted into col {spill}"
        );
    }
}

/// Exhaustive over a small geometry lattice: a shrink keeps the ink's vertical centre to
/// within half a pixel, and when the smaller ink fits the row band it is placed inside it
/// rather than left for `clamp_to_row_band` to crop.
#[test]
fn a_shrink_keeps_the_ink_centre_and_lands_in_the_band() {
    use aterm_render::shrink_ymin_about_ink_centre as shrink;
    for cell_h in 8usize..=24 {
        for baseline in 0..=cell_h as i32 {
            for h in 1usize..=40 {
                for dst_h in 1..=h.min(cell_h) {
                    for ymin in -20i32..=20 {
                        let y = shrink(ymin, h, dst_h, baseline, cell_h);
                        let top = baseline - dst_h as i32 - y;
                        assert!(
                            top >= 0 && top + dst_h as i32 <= cell_h as i32,
                            "ymin {ymin} h {h}->{dst_h} baseline {baseline} cell {cell_h}: \
                             top row {top} leaves the band"
                        );
                        // Where the band does not bind, the centre moves at most half a px.
                        let unclamped = (2 * ymin + h as i32 - dst_h as i32 + 1).div_euclid(2);
                        let (lo, hi) = (baseline - cell_h as i32, baseline - dst_h as i32);
                        if (lo..=hi).contains(&unclamped) {
                            let want2 = 2 * ymin + h as i32;
                            let got2 = 2 * y + dst_h as i32;
                            assert!(
                                (want2 - got2).abs() <= 1,
                                "ymin {ymin} h {h}->{dst_h}: centre {got2}/2 vs {want2}/2"
                            );
                        }
                    }
                }
            }
        }
    }
    // A 16-row disc resting on the baseline, shrunk to 11, is lifted to its ink centre.
    assert_eq!(shrink(0, 16, 11, 25, 34), 3);
}
