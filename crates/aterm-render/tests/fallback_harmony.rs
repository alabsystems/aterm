// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tier-1 conformance for W8 (fallback harmony): the SHIPPING pure policies
//! behind fallback-face metric normalization, wide-glyph centring, the
//! row-band clip, weight-matched candidate ranking, and the macOS CoreText
//! drawability probe.
//!
//! aterm used to rasterize every fallback face via fontdue at the PRIMARY's
//! raw px (audit sin 6): CJK from Arial Unicode instead of a native design,
//! wide glyphs left-biased in their 2-cell box, no weight/italic matching,
//! and fallback overdraw never clipped to the cell row.
//!
//! ## Two-tier proof
//!
//! * **Tier-0 (abstract, model-checked by the Trust `ty` compiler)** — the
//!   derived models `FallbackScaleClamp`, `WideCentre` and `FallbackBandClip`
//!   (`aterm_spec::derive::{fallback_scale_clamp_model, wide_center_model,
//!   fallback_band_clip_model}`) carry the clamp/balance/in-band invariants;
//!   `cargo test -p aterm-spec --test derived_ring_ty` runs the REAL `ty`
//!   binary over each bounded state space, PROVING every invariant at
//!   `Buggy=0` and CATCHING the pre-W8 defect at `Buggy=1` (counterexample).
//! * **Tier-1 (concrete, this file)** — the real `f32`/integer functions are
//!   swept over dense/exhaustive lattices (plus degenerate and adversarial
//!   inputs the abstract models cannot express: NaN, ∞, floor division on
//!   negative gaps, garbage font bytes), with non-vacuity controls and the
//!   pre-fix behaviour shown to violate each law. Floor division and the
//!   ratio arithmetic are outside ty's Expr language (no `*`/`/`), so — per
//!   the box-drawing rounding-law precedent — the lattice sweeps here are the
//!   binding layer for the arithmetic half.
//!
//! ## W8 (g)/(h): the horizontal pair
//!
//! The row band had no column twin, so a fallback glyph wider than its cell
//! painted straight over its neighbours: STIX Two Math designs U+27F5..U+27FC
//! 1.499 em wide, the x-height normalization scales that UP ~11%, and the
//! result covered ~2.9 cells inside a ONE-cell grid box (the grid itself is
//! right — these are East_Asian_Width Neutral, so `wcwidth` and aterm agree
//! on 1 — so the defect is purely a PAINT overrun). (g) `condense_ink_w` +
//! `condense_coverage` area-resample such a glyph along x until it fits and
//! centre it; (h) `clamp_to_col_band` is the backstop under it.
//!
//! The condense law is integer-only, so it has no `ty` model — `div_ceil` is
//! division, the documented WAIVER class `area_overlap` already carries — and
//! the exhaustive lattice below is the binding layer. The column clamp needs
//! no new model at all: it is a one-line wrapper over the same
//! `clamp_to_band` core as the row clamp, so the EXISTING `FallbackBandClip`
//! model twins both (its arithmetic is axis-free). The genuinely new proof
//! obligation is the ANTI-FIGHT law — two mechanisms on one axis must not be
//! able to disagree — pinned by `condense_then_clamp_never_fight`.

use aterm_render::{
    CJK_SCALE_MAX, CJK_SCALE_MIN, CONDENSE_MAX_RATIO, XHEIGHT_SCALE_MAX, XHEIGHT_SCALE_MIN,
    clamp_to_col_band, clamp_to_row_band, condense_coverage, condense_ink_w, fallback_cell_count,
    fallback_cjk_scale, fallback_fit_scale, fallback_weight_rank,
    fallback_xheight_scale, materialized_cell_span, wide_center_offset,
};

// ---- (1) normalization clamps ----

/// Every finite positive (target, actual) pair on a dense lattice: the scale
/// stays inside the clamp interval, and whenever the ideal ratio is already
/// inside it the scaled advance equals the target EXACTLY (f32 division is
/// exact enough here: `scale * actual == target` to within one ulp-scale
/// epsilon, asserted at 1e-3 relative).
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

/// Totality on degenerate/adversarial inputs: zero, negative, NaN and ∞
/// metrics yield the neutral scale `1.0` (the pre-W8 unscaled raster), never
/// a panic or a NaN that would poison the raster px.
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
    // Finite operands whose RATIO overflows to ∞ are caught by the
    // finite-ratio guard: junk metrics yield the neutral scale, never a
    // non-finite raster px.
    assert_eq!(fallback_cjk_scale(f32::MAX, f32::MIN_POSITIVE), 1.0);
}

// ---- (2) centring law ----

/// Exhaustive over every (box_w, adv) in a lattice that includes `adv >
/// box_w` (a clamped face whose scaled advance overflows the box): the
/// offset satisfies the floor characterization `2*off <= gap <= 2*off + 1`
/// (the EXACT hypothesis the ty-proven `WideCentre` model shows implies the
/// balance law), hence `|left - right| <= 1`; and the pre-W8 left-bias
/// (`off = 0`) violates the balance for every gap >= 2 (negative control).
#[test]
fn wide_center_offset_balances_margins_exhaustively() {
    let mut nonzero_offsets = 0u32;
    for box_w in 0..=64i32 {
        for adv in 0..=80i32 {
            let gap = box_w - adv;
            let off = wide_center_offset(box_w, adv);
            // The floor characterization (the Tier-0 model's Fire guard).
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
            // Negative control: the pre-W8 placement (off = 0, whole gap on
            // the right) breaks the law exactly where centring matters.
            if gap.abs() >= 2 {
                let (l0, r0) = (0, gap);
                assert!((l0 - r0).abs() > 1, "the pre-fix left-bias must violate");
            }
        }
    }
    // Non-vacuity: the offset genuinely moves glyphs (not identically zero).
    assert!(nonzero_offsets > 0);
}

/// The fit policy contains the complete proportional bitmap AND its advance
/// after centring, while never enlarging an already-fitting glyph. The real
/// warning-sign metrics that exposed the bug (30px mask / 27.5px advance in a
/// 10px cell) therefore request a roughly one-third UNIFORM scale, not a crop.
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

/// Cell allocation is Unicode-width/config driven, never inferred from a
/// fallback font's proportional advance. This is the CJK/combining guard for
/// the warning-sign fix.
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

/// Non-vacuous engine -> `RenderInput` -> CPU raster-cache bind for the two
/// config-sensitive edge classes. The same EAW-A scalar is first received wide,
/// then the policy reloads narrow without reflowing the old cell, so both spans
/// coexist in one materialized row and must reach distinct fitted raster keys.
/// A zero-width combining overlay on the narrow instance stays span 0.
#[test]
fn materialized_ambiguous_spans_coexist_and_combining_stays_zero_width() {
    use aterm_core::config::TerminalConfig;
    use aterm_core::terminal::Terminal;
    use aterm_render::{FaceId, Renderer, Theme, display_face_bytes, embedded_font};

    let primary_bytes = display_face_bytes("pixel").expect("bundled primary");
    let fallback_bytes = embedded_font();
    let primary_face = ttf_parser::Face::parse(primary_bytes, 0).expect("primary parses");
    let fallback_face = ttf_parser::Face::parse(fallback_bytes, 0).expect("fallback parses");
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

// ---- (4) row-band clip ----

/// Exhaustive over every (top, height, band) in a lattice spanning negative
/// tops (ascender overshoot), zero-size bitmaps/bands and far-past-the-band
/// placements: the trim only ever drops rows, every kept row lies inside
/// `[0, band)`, and an already-in-band glyph is untouched. The pre-W8
/// behaviour (no trim) is shown to violate on every out-of-band shape
/// (negative control).
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
                    // Out-of-band ink: the pre-W8 unclipped blit (skip=0,
                    // keep=height) violates the in-band law here.
                    let would_escape = top < 0 || top + height as i32 > band as i32;
                    assert!(would_escape, "negative control classification");
                    trimmed += 1;
                }
            }
        }
    }
    // Non-vacuity: both regimes are exercised.
    assert!(trimmed > 0 && untouched > 0);
}

// ---- (g) condense-to-cell + (h) column-band clip ----

/// T1. Exhaustive over the whole `(ink_w, box_w)` lattice: every clause of
/// the `condense_ink_w` totality law at once. The pre-fix behaviour (the
/// identity — width-1 fallback glyphs got no horizontal treatment at all) is
/// shown to violate the fit law on exactly the shapes that overran.
#[test]
fn condense_ink_w_never_widens_and_fits_exhaustively() {
    let (mut identity, mut exact_fit, mut floored) = (0u32, 0u32, 0u32);
    for ink_w in 0..=64usize {
        for box_w in 0..=32usize {
            let out = condense_ink_w(ink_w, box_w);
            // NEVER WIDENS.
            assert!(
                out <= ink_w,
                "condense widened: {ink_w} -> {out} (box {box_w})"
            );
            // BOUNDED DISTORTION: never squeezed past CONDENSE_MAX_RATIO:1.
            assert!(
                out >= ink_w.div_ceil(CONDENSE_MAX_RATIO),
                "condense past the {CONDENSE_MAX_RATIO}:1 floor: {ink_w} -> {out} (box {box_w})"
            );
            // NON-VANISHING.
            if ink_w >= 1 {
                assert!(
                    out >= 1,
                    "condense erased the ink: {ink_w} -> 0 (box {box_w})"
                );
            }
            // IDENTITY IFF ALREADY FITTING (or a degenerate cell box).
            let fits = box_w == 0 || ink_w <= box_w;
            assert_eq!(
                out == ink_w,
                fits,
                "identity law: ink={ink_w} box={box_w} -> {out}"
            );
            if fits {
                identity += 1;
                continue;
            }
            // FITS WHEN ACHIEVABLE, and then EXACTLY.
            if ink_w <= CONDENSE_MAX_RATIO * box_w {
                assert_eq!(
                    out, box_w,
                    "fit is not exact: ink={ink_w} box={box_w} -> {out}"
                );
                exact_fit += 1;
                // Negative control: the pre-fix identity (`out == ink_w`)
                // overruns the box on precisely these shapes.
                assert!(ink_w > box_w, "negative control classification");
            } else {
                assert_eq!(out, ink_w.div_ceil(CONDENSE_MAX_RATIO));
                assert!(
                    out > box_w,
                    "the floor regime must still overrun (that is why (h) exists)"
                );
                floored += 1;
            }
        }
    }
    // Non-vacuity: all three arms are actually reached.
    assert!(
        identity > 0 && exact_fit > 0 && floored > 0,
        "lattice missed an arm: identity={identity} exact={exact_fit} floored={floored}"
    );
}

/// T2. The two band clamps really are ONE proven core: over the same lattice
/// the row test sweeps, the column wrapper agrees with the row wrapper
/// pointwise — which is what lets `fallback_band_clip_model` legitimately
/// twin both, instead of the column axis needing a cloned Tier-0 model.
#[test]
fn col_band_clamp_agrees_with_row_band_clamp() {
    let mut trimmed = 0u32;
    for pos in -48..=48i32 {
        for len in 0..=48usize {
            for band in 0..=40usize {
                let col = clamp_to_col_band(pos, len, band);
                assert_eq!(
                    col,
                    clamp_to_row_band(pos, len, band),
                    "the two band clamps diverged at pos={pos} len={len} band={band}"
                );
                let (skip, keep) = col;
                assert!(skip + keep <= len);
                if keep > 0 {
                    let left = pos + skip as i32;
                    assert!(left >= 0 && left + keep as i32 <= band as i32);
                }
                if pos >= 0 && pos + len as i32 <= band as i32 {
                    assert_eq!((skip, keep), (0, len), "in-band bitmap must be untouched");
                } else if len > 0 {
                    trimmed += 1;
                }
            }
        }
    }
    assert!(trimmed > 0);
}

/// T3. THE ANTI-FIGHT OBLIGATION — the reason belt-and-braces is safe here.
/// Whenever the condense fitted the glyph (regimes "already fitting" and
/// "exact fit"), the backstop clamp is the EXACT identity: it can never trim
/// ink the condense already placed. And the complement: the two mechanisms
/// both act only when `ink_w > CONDENSE_MAX_RATIO * box_w`, i.e. precisely
/// when no legible fit exists.
#[test]
fn condense_then_clamp_never_fight() {
    let (mut inert, mut both_act) = (0u32, 0u32);
    for ink_w in 0..=64usize {
        for box_w in 1..=32usize {
            let new_w = condense_ink_w(ink_w, box_w);
            for bleed in 1..=16usize {
                let band_w = box_w + 2 * bleed;
                if new_w <= box_w {
                    let left = wide_center_offset(box_w as i32, new_w as i32);
                    assert_eq!(
                        clamp_to_col_band(left + bleed as i32, new_w, band_w),
                        (0, new_w),
                        "the backstop trimmed a fitted glyph: ink={ink_w} box={box_w} \
                         bleed={bleed} -> new_w={new_w} left={left}"
                    );
                    inert += 1;
                } else {
                    // The only regime where both mechanisms act.
                    assert!(
                        ink_w > CONDENSE_MAX_RATIO * box_w,
                        "the condense left an overrun outside the floor regime: \
                         ink={ink_w} box={box_w} -> {new_w}"
                    );
                    both_act += 1;
                }
            }
        }
    }
    assert!(
        inert > 0 && both_act > 0,
        "inert={inert} both_act={both_act}"
    );
}

/// T4. The x-only area filter's own laws over several coverage patterns:
/// shape, identity, the no-seam law (a solid row stays solid), mass
/// preservation to rounding, non-annihilation, and totality on every zero
/// dimension.
#[test]
fn condense_coverage_preserves_mass_and_solid_rows() {
    // Totality on degenerate dimensions.
    assert!(condense_coverage(&[], 0, 3, 2).is_empty());
    assert!(condense_coverage(&[1, 2, 3], 3, 0, 2).is_empty());
    assert!(condense_coverage(&[1, 2, 3], 3, 1, 0).is_empty());

    let mut condensed = 0u32;
    for w in 1..=24usize {
        for h in 1..=3usize {
            // Four patterns: solid, one lit column, a ramp, alternating.
            let patterns: Vec<Vec<u8>> = vec![
                vec![255u8; w * h],
                (0..w * h)
                    .map(|i| if i % w == w / 2 { 255 } else { 0 })
                    .collect(),
                (0..w * h).map(|i| ((i % w) * 255 / w) as u8).collect(),
                (0..w * h)
                    .map(|i| if i % 2 == 0 { 200 } else { 20 })
                    .collect(),
            ];
            for (pi, src) in patterns.iter().enumerate() {
                for new_w in 1..=w {
                    let out = condense_coverage(src, w, h, new_w);
                    assert_eq!(out.len(), new_w * h, "shape law (w={w} h={h} new={new_w})");
                    if new_w == w {
                        assert_eq!(&out, src, "identity at new_w == w");
                        continue;
                    }
                    condensed += 1;
                    for y in 0..h {
                        let irow = &src[y * w..y * w + w];
                        let orow = &out[y * new_w..y * new_w + new_w];
                        // NO SEAM: an all-255 row stays all-255.
                        if irow.iter().all(|&v| v == 255) {
                            assert!(
                                orow.iter().all(|&v| v == 255),
                                "solid row seamed (p{pi} w={w} new={new_w}): {orow:?}"
                            );
                        }
                        // MASS: sum(out)*w == sum(in)*new_w, to the per-output
                        // round-to-nearest bound of w/2 each.
                        let (si, so) = (
                            irow.iter().map(|&v| u64::from(v)).sum::<u64>(),
                            orow.iter().map(|&v| u64::from(v)).sum::<u64>(),
                        );
                        let (a, b) = (so * w as u64, si * new_w as u64);
                        assert!(
                            a.abs_diff(b) <= (new_w * w).div_ceil(2) as u64,
                            "mass drifted (p{pi} w={w} new={new_w} y={y}): {a} vs {b}"
                        );
                        // NON-ANNIHILATION: ink in, ink out (within the floor).
                        if si > 0 && new_w * CONDENSE_MAX_RATIO >= w {
                            assert!(
                                so > 0,
                                "condense annihilated a lit row (p{pi} w={w} new={new_w})"
                            );
                        }
                    }
                }
            }
        }
    }
    assert!(condensed > 0);
}

// ---- (e) weight/italic ranking ----

/// The ranking law: a style (italic) mismatch outranks ANY weight distance
/// (OS/2 weights are 1..=1000, so the italic penalty of 1000 dominates), and
/// among style-matched candidates the closest weight wins.
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

// ---- (3) probe soundness (macOS CoreText) ----

/// The CT drawability probe is TOTAL over adversarial inputs — garbage bytes,
/// truncated real fonts, an out-of-range collection index, and the empty blob
/// all return `false` without panicking — and sound-by-construction on a real
/// face (accepting == literally obtaining a non-empty raster, which the
/// renderer then reproduces at raster time). Non-vacuity: the system
/// monospace face IS accepted for 'M'; the colour-emoji face is REJECTED for
/// an emoji (colour bitmaps are the colour pipeline's job, not the mono
/// fallback's).
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
    if let Ok(menlo) = std::fs::read("/System/Library/Fonts/Menlo.ttc") {
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
        // Non-vacuity: the real face IS accepted for a covered char and
        // rejected for an uncovered one (cmap duty via ttf-parser).
        assert!(ct_face_can_render(&menlo, 0, 'M'), "Menlo must accept 'M'");
        assert!(
            !ct_face_can_render(&menlo, 0, '\u{FDD0}'),
            "a permanent noncharacter has no glyph"
        );
    } else {
        eprintln!("SKIP: Menlo.ttc not readable");
    }
    // Colour-bitmap glyphs are rejected (the mono resolver's contract).
    if let Ok(emoji) = std::fs::read("/System/Library/Fonts/Apple Color Emoji.ttc") {
        assert!(
            !ct_face_can_render(&emoji, 0, '\u{1F600}'),
            "a colour-bitmap glyph must not pass the MONO drawability probe"
        );
    }
}

// ---- end-to-end regression pins (real renderer) ----

/// A wide CJK glyph resolved through the fallback chain: (i) its coverage
/// rows sit inside the cell row band (the W8 clip, pinned on the REAL raster
/// under both backends), and (ii) its advance is the full 2-cell box (the
/// centring recipe). Skips gracefully on a host with no system font.
#[test]
fn cjk_fallback_glyph_is_banded_and_wide() {
    use aterm_render::{FaceId, GlyphClass, Renderer, Theme};
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system mono font found");
        return;
    };
    // Without this, `glyph_key` answers while the fallback parse is still in
    // flight and PROVISIONALLY routes every candidate to the primary — which
    // made the skip guard below fire on every host, i.e. the test always
    // passed by testing nothing. The two W8 tests further down have always
    // blocked here for exactly this reason.
    r.debug_block_on_lazy_fallbacks();
    let (_, cell_h) = r.cell_size();
    for force_fontdue in [false, true] {
        if force_fontdue {
            r.debug_force_fontdue();
        }
        // Any wide ideograph the FALLBACK chain serves will do — the law under
        // test is the tier's, not one codepoint's. Hard-coding 中 made this
        // test vacuous on hosts whose primary collection covers it (this very
        // machine: 中 resolves to the primary, 一 to Arial Unicode), and a
        // guard that skips on the developer's own machine guards nothing.
        let candidates = ['\u{4E2D}', '\u{4E00}', '\u{4E01}', '\u{53E3}']; // 中 一 丁 口
        let Some(key) = candidates.iter().map(|&c| r.glyph_key(c)).find(|k| {
            k.source == FaceId::Fallback && k.glyph_class == GlyphClass::Mono
        }) else {
            eprintln!("SKIP: no wide ideograph served by the fallback chain on this host");
            return;
        };
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
        // W8 (g)/(h) non-regression: a 2-cell glyph NEVER enters the condense
        // — the stage is gated `cw == 1`, structurally, because the CoreText
        // raster width includes the antialiasing pad, so an ideograph whose
        // ink fits its box perfectly can still report `w > 2 * cell_w` and be
        // squashed by a fit test alone. The width assertion below is the
        // teeth: a condense that reached this glyph would fold its ink into
        // one cell.
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
             the cw == 2 gate on the condense has regressed (force_fontdue={force_fontdue})",
            img.width()
        );
    }
}

/// A bare warning sign is text-presentation and occupies one terminal cell.
/// Proportional fallback faces must not let its square symbol ink spill into
/// the following cell (the visible `⚠ MCP` kerning regression).
#[test]
fn narrow_fallback_symbol_is_bounded_to_one_cell() {
    use aterm_render::{FaceId, GlyphClass, Renderer, StyleBits, Theme};
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system mono font found");
        return;
    };
    r.debug_block_on_lazy_fallbacks();
    let key = r.glyph_key('\u{26A0}');
    if !matches!(
        key.source,
        FaceId::Fallback | FaceId::SymbolFallback | FaceId::RuntimeFallback
    ) || key.glyph_class != GlyphClass::Mono
    {
        eprintln!("SKIP: warning sign is not served by a mono fallback face: {key:?}");
        return;
    }
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
    // Non-vacuity: the synthetic style ran BEFORE the final fit; the fitted
    // masks are contained but not silently collapsed back to regular.
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

    // Shape-preservation pin: a fitted warning triangle remains roughly square
    // and carries ink on BOTH sides of centre. A horizontal crop of the old
    // 30px mask to 10px would leave a tall central slice instead.
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

/// macOS native-CJK routing (W8): with the stock candidate list, the chain
/// face that covers 中 is the native CJK design (Hiragino Sans GB), not Arial
/// Unicode — while a char Hiragino lacks still lands on the broad face
/// (additive tier, no coverage lost). Skips when the host lacks the faces.
#[test]
#[cfg(target_os = "macos")]
fn native_cjk_face_leads_the_macos_chain() {
    use aterm_render::{Renderer, Theme};
    if !std::path::Path::new("/System/Library/Fonts/Hiragino Sans GB.ttc").exists() {
        eprintln!("SKIP: Hiragino Sans GB not installed");
        return;
    }
    if std::env::var_os("ATERM_FALLBACK_FONT").is_some() {
        eprintln!("SKIP: ATERM_FALLBACK_FONT overrides the builtin chain");
        return;
    }
    let Some(mut r) = Renderer::from_system(16.0, Theme::default()) else {
        eprintln!("SKIP: no system mono font found");
        return;
    };
    let pick = r.debug_fallback_pick_path('\u{4E2D}');
    assert_eq!(
        pick.as_deref(),
        Some("/System/Library/Fonts/Hiragino Sans GB.ttc"),
        "中 must come from the native CJK chain face"
    );
    // Additive tier: Arabic (which Hiragino lacks) still reaches the broad face.
    if std::path::Path::new("/System/Library/Fonts/Supplemental/Arial Unicode.ttf").exists() {
        let arabic = r.debug_fallback_pick_path('\u{0645}'); // م
        assert_eq!(
            arabic.as_deref(),
            Some("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
            "the broad face must still back the chain"
        );
    }
}

/// T5. THE REGRESSION PIN for the reported defect. U+27F5..U+27FC are ONE
/// STIX Two Math design — advance 1.612 em, ink 1.499 em — that neither SF
/// Mono nor Arial Unicode carries, so they land on the symbol tier and used
/// to rasterize ~2.9 CELLS wide while occupying exactly ONE cell in the grid,
/// burying the two columns to their right and shearing any box-drawing table
/// they appeared in. The grid was never wrong (these are East_Asian_Width
/// Neutral, `wcwidth` 1); the PAINT was.
///
/// Asserts the display-face law `tests/display_faces.rs` already enforces for
/// bundled faces, now extended to the fallback lane: the coverage lies wholly
/// inside the cell box. Run under `debug_force_fontdue` too, so the guarantee
/// is not a CoreText accident. NON-VACUITY: at least one of the eight must
/// actually have been condensed (its width pinned to exactly `cell_w`), or
/// the test would pass silently on a host whose raster was already narrow.
#[test]
fn long_arrows_never_leave_their_cell() {
    use aterm_render::{FaceId, GlyphClass, Renderer, Theme};
    let mut checked = 0u32;
    let mut condensed = 0u32;
    for px in [12.0f32, 16.0, 24.0] {
        let Some(mut r) = Renderer::from_system(px, Theme::default()) else {
            eprintln!("SKIP: no system mono font found");
            return;
        };
        r.debug_block_on_lazy_fallbacks();
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
    if checked == 0 {
        eprintln!("SKIP: no long arrow is served by a fallback tier on this host");
        return;
    }
    assert!(
        condensed > 0,
        "non-vacuity: {checked} arrows checked but none was condensed to the cell width — \
         the test would pass on a pre-fix build"
    );
}

/// T5b. The USER-VISIBLE law, at frame level: a long arrow in a box-drawing
/// table does not SHEAR it. This is the shape the defect was actually
/// reported in — the arrow buried the two columns to its right, so every
/// rule to the right of it moved. Rendering `│⟹│` and `│ │` must leave both
/// `│` cells pixel-identical; only the middle cell may differ, and it must.
#[test]
fn a_long_arrow_does_not_shear_a_box_drawing_row() {
    use aterm_core::terminal::Terminal;
    use aterm_render::{Renderer, Theme};
    let Some(mut r) = Renderer::from_system(18.0, Theme::default()) else {
        eprintln!("SKIP: no system mono font found");
        return;
    };
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
    // And the two columns the pre-fix raster buried are untouched too.
    for spill in [3usize, 4] {
        assert_eq!(
            cell(&inked, spill),
            cell(&bare, spill),
            "the long arrow painted into col {spill}"
        );
    }
}
