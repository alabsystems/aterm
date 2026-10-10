// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow kitty ribbon.

use super::*;

/// Bounded model for the occupied-cell rainbow body budget. `Raise` explores
/// every integer request through and beyond the 56/255 ceiling. `Buggy=1`
/// forwards an over-budget request instead of clamping it and must produce a
/// counterexample, so the proof is not vacuous.
fn rainbow_occupied_coverage_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        RainbowOccupiedCoverage {
            // The position-blind bed ceiling, `RAINBOW_UNDER_COV_CAP`:
            // `RAINBOW_BAND_COV_CAP_MAX` times `RAINBOW_BED_OVER_GAIN` (the bed
            // composites source-over, displacing the ground rather than adding
            // to it). The gain is the largest that lands the product on a whole
            // level, so the beam's ordered dither cannot carry a capped request
            // one level past its cap: `236 = floor(212 x 1.1148)`.
            const Cap = 236;
            const RequestedMax = 270;
            const Buggy = 0;
            var requested = 0;
            var emitted = 0;
            action Raise when (requested <= RequestedMax - 1) {
                requested = requested + 1;
                emitted = if requested + 1 > Cap {
                    if Buggy == 1 { requested + 1 } else { Cap }
                } else {
                    requested + 1
                };
            }
            invariant OccupiedBodyBounded: emitted <= Cap;
        }
    }
}

/// Bounded lifecycle for a rainbow cell reclaimed while retracting. Visibility
/// is quantized for exhaustive exploration; Tier 1 projects the real
/// multiplier at the reversal instant. `Buggy=1` snaps back to full.
fn rainbow_retract_rearm_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        RainbowRetractRearm {
            const Peak = 4;
            const Buggy = 0;
            var mode = 0;
            var visibility = 4;
            var discontinuity = 0;
            action BeginFade when (mode == 0) { mode = 1; }
            action Fade when (mode == 1 && visibility > 0) {
                visibility = visibility - 1;
            }
            action Reclaim when (mode == 1) {
                mode = 2;
                visibility = if Buggy == 1 { Peak } else { visibility };
                discontinuity = if Buggy == 1 { 1 } else { 0 };
            }
            action Rearm when (mode == 2 && visibility <= Peak - 1) {
                visibility = visibility + 1;
            }
            invariant VisibilityBounded: visibility <= Peak;
            invariant ReversalContinuous: discontinuity == 0;
        }
    }
}

// ----- rainbow kitty continuous rainbow ribbon -----

/// "rainbow kitty" is the style's user-facing name; "rainbow" maps to it too,
/// and the laser sweep is "phaser". Rainbow kitty draws no shared comet beam
/// (its body is the streak). Every name the style has shipped under still
/// parses.
#[test]
fn rainbow_kitty_parse_and_beam() {
    assert_eq!(GlowStyle::parse("rainbow kitty"), GlowStyle::RainbowKitty);
    assert_eq!(GlowStyle::parse("nyan rainbow"), GlowStyle::RainbowKitty);
    assert_eq!(GlowStyle::parse("nyan"), GlowStyle::RainbowKitty);
    assert_eq!(GlowStyle::parse("rainbow"), GlowStyle::RainbowKitty);
    assert_eq!(GlowStyle::parse("RAINBOW"), GlowStyle::RainbowKitty);
    for alias in [
        "rainbow kitty underline",
        "rainbow underline",
        "underline rainbow",
        "nyan underline",
    ] {
        assert_eq!(GlowStyle::parse(alias), GlowStyle::RainbowKitty, "{alias}");
    }
    assert_eq!(GlowStyle::parse("phaser"), GlowStyle::Phaser);
    assert!(
        !style_has_beam("nyan"),
        "nyan draws its own continuous body, no beam"
    );
    assert!(style_has_beam("phaser"), "phaser keeps the shared beam");
}

/// Tier 0: exhaust the 0..=64 request lattice. The body clamp proves the
/// occupied-cell budget; the pass-through mutant fails once request 57 is
/// reachable.
#[test]
fn rainbow_occupied_coverage_derived_model_proves_and_catches_regression() {
    let model = rainbow_occupied_coverage_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

/// Tier 0 proves every bounded fade depth can reverse without changing
/// visibility; the buggy snap-to-full twin must yield a counterexample.
#[test]
fn rainbow_retract_rearm_model_proves_and_catches_brightness_snap() {
    let model = rainbow_retract_rearm_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

/// A live frame of the effect through the public `tick` seam, so every stream
/// is present (ribbon bed, starfield, wake, crown, fresh-ink pops). Returns the
/// engine (its `under`/`halo` streams resident), this frame's over-ink stream,
/// and the frame instant.
///
/// The row probe is fed as the host does ([`CursorGlow::observe_row`] before
/// every `tick`, then the flanking rows), so the typed run carries glyphs the
/// engine can see.
fn typed_burst(
    c: &GlowConfig,
    g: Geom,
    row: u16,
    cols: std::ops::Range<u16>,
    gap: Duration,
) -> (CursorGlow, Vec<GlowQuad>, Instant) {
    typed_burst_from_phase(c, g, row, cols, gap, 0.0)
}

/// [`typed_burst`] from a chosen point on the rolling hue. A mark shorter than
/// `1 / RAINBOW_LAID_SWEEP_PER_CELL` cells covers only part of the arc (see
/// [`RAINBOW_KITTY_HUE_STEP`]), so callers can sweep the start.
fn typed_burst_from_phase(
    c: &GlowConfig,
    g: Geom,
    row: u16,
    cols: std::ops::Range<u16>,
    gap: Duration,
    hue0: f32,
) -> (CursorGlow, Vec<GlowQuad>, Instant) {
    let mut glow = CursorGlow {
        hue: hue0,
        ..CursorGlow::default()
    };
    let mut t = Instant::now();
    let mut out = Vec::new();
    let blank = vec![' '; g.cols];
    let mut line = blank.clone();
    let probe = |glow: &mut CursorGlow, line: &[char], caret: u16, t: Instant| {
        glow.observe_row(row, caret, line, t);
        glow.observe_neighbor_rows(Some(&vec![' '; line.len()]), Some(&vec![' '; line.len()]));
    };
    probe(&mut glow, &line, cols.start, t);
    glow.tick(Some((row, cols.start)), t, c, g, &mut out);
    for col in cols {
        t += gap;
        line[usize::from(col)] = 'x';
        glow.note_synthetic_typed(t, 1);
        probe(&mut glow, &line, col, t);
        glow.tick(Some((row, col)), t, c, g, &mut out);
    }
    let _ = blank;
    (glow, out, t)
}

#[test]
fn the_brightest_pixel_in_the_frame_is_under_the_cursor() {
    use crate::cursor_rainbow::{CursorRainbow, RainbowConfig};

    let g = retina_geom();
    let light = |rgb: u32| crate::color_math::relative_luminance(rgb) * 255.0;
    let row = 3u16;
    let col = 29u16;
    let mut checked = 0usize;
    let mut worst_margin = f32::INFINITY;
    let mut worst_at = String::new();
    let mut brightest_field = 0.0f32;
    for raw in ["rainbow kitty", "rainbow kitty underline"] {
        let c = cfg_for_style_name(raw, true);
        // A real frame per trial: the cadence moves the momentum spine, plume,
        // starfield and fresh-ink pops, the light the caret competes with.
        for gap_ms in [18u64, 28, 45, 90] {
            let (glow, over_engine, at) =
                typed_burst(&c, g, row, 5..col + 1, Duration::from_millis(gap_ms));
            for step in 0..=6u32 {
                let field = step as f32 / 6.0;
                for &e in &[0.35f32, 0.6, 1.0] {
                    for &flare in &[false, true] {
                        let mut caret = CursorRainbow::default();
                        let mut over = over_engine.clone();
                        let caret_cfg = RainbowConfig {
                            enabled: true,
                            intensity: 1.0,
                            blinking: flare,
                            base: Some(0x0050_FA7B),
                            head_rgb: None,
                            paint: None,
                            ground: None,
                            flare_at: None,
                        };
                        let mut caret_tick =
                            |blink: bool, when: Instant, out: &mut Vec<GlowQuad>| {
                                caret
                                    .tick_with_family_phase(
                                        Some((row, col)),
                                        when,
                                        e,
                                        glow.rainbow_phase(),
                                        field,
                                        blink,
                                        true,
                                        g,
                                        &caret_cfg,
                                        out,
                                    )
                                    .fill
                                    .expect("enabled block fill")
                            };
                        let mark = over.len();
                        let fill = if flare {
                            // Steady, then a charged flip to arm the flare,
                            // then the frame at its peak (`pop` is a half-sine,
                            // so the arming frame is exactly 0).
                            caret_tick(false, at, &mut over);
                            over.truncate(mark);
                            caret_tick(true, at, &mut over);
                            over.truncate(mark);
                            caret_tick(true, at + Duration::from_millis(70), &mut over)
                        } else {
                            caret_tick(false, at, &mut over)
                        };
                        // Non-vacuous: both ornaments survive (in premultiplied
                        // levels the rim is a visible hem and the peak flare
                        // keeps its white arm).
                        let rim_level = over[mark..]
                            .iter()
                            .map(|q| {
                                ((q.color >> 16) & 0xff)
                                    .max((q.color >> 8) & 0xff)
                                    .max(q.color & 0xff)
                            })
                            .max()
                            .unwrap_or(0);
                        let visible_floor = if flare { 16 } else { 4 };
                        assert!(
                            rim_level >= visible_floor,
                            "NON-VACUOUS: {} vanished (peak level {rim_level})",
                            if flare {
                                "the flare arm"
                            } else {
                                "the ordinary rim"
                            }
                        );

                        // Compose in renderer order, ink-free on purpose: the
                        // clause ranks the effect's own light against the
                        // caret, and the theme foreground (`#C8D3F5`, luminance
                        // 166) is not the effect's. What the effect does to text
                        // is `the_effect_never_clips_and_never_lifts_the_text`.
                        let mut f = Glass::new(g, c.theme_bg);
                        f.add_quads(glow.under_quads());
                        f.add_quads(&over);
                        f.add_halos(glow.halos());
                        f.stamp_caret(row, col, fill);

                        let (cx0, cy0, cx1, cy1) = f.cell_rect(row, col);
                        let in_cell =
                            |y: i32, x: i32| (cy0..cy1).contains(&y) && (cx0..cx1).contains(&x);
                        // Glyph rows only: the comet's vivid rail lives in the
                        // leading below a row (the top `RAIL_REACH_MAX_CH` of
                        // the band under it) with its own ceiling
                        // (`RAIL_LUMA_CEIL`); L5 is the caret's law where
                        // letters are.
                        let rail_px = (crate::rainbow_kitty::ribbon::RAIL_REACH_MAX_CH
                            * g.ch as f32)
                            .ceil() as i32;
                        let in_glyph_rows =
                            |y: i32| (y - i32::from(g.origin_y)).rem_euclid(g.ch as i32) >= rail_px;
                        let mut best = (f32::NEG_INFINITY, 0i32, 0i32, 0u32);
                        let mut best_outside = (f32::NEG_INFINITY, 0i32, 0i32, 0u32);
                        for y in 0..g.win_h as i32 {
                            if !in_glyph_rows(y) {
                                continue;
                            }
                            for x in 0..g.win_w as i32 {
                                let colr = f.at(y, x);
                                let l = light(colr);
                                if l > best.0 {
                                    best = (l, y, x, colr);
                                }
                                if !in_cell(y, x) {
                                    if l > best_outside.0 {
                                        best_outside = (l, y, x, colr);
                                    }
                                    // The sparkle field is the white-ish
                                    // population; one bright star makes this a
                                    // real frame, not a bare ribbon.
                                    let (r, gg, b) =
                                        ((colr >> 16) & 0xff, (colr >> 8) & 0xff, colr & 0xff);
                                    if r.min(gg).min(b) > 40 {
                                        brightest_field = brightest_field.max(l);
                                    }
                                }
                            }
                        }
                        assert!(
                            in_cell(best.1, best.2),
                            "the brightest pixel of the frame is at ({}, {}) — \
                             #{:06X}, luminance {:.0} — while the cursor cell \
                             peaks at {:.0}. {raw}, gap {gap_ms} ms, field \
                             {field}, energy {e}, flare {flare}",
                            best.1,
                            best.2,
                            best.3,
                            best.0,
                            (cy0..cy1)
                                .flat_map(|y| (cx0..cx1).map(move |x| (y, x)))
                                .map(|(y, x)| light(f.at(y, x)))
                                .fold(0.0f32, f32::max)
                        );
                        if best.0 - best_outside.0 < worst_margin {
                            worst_margin = best.0 - best_outside.0;
                            worst_at = format!(
                                "({}, {}) #{:06X} L{:.0} vs caret L{:.0} — {raw} gap                                      {gap_ms} field {field} energy {e} flare {flare}",
                                best_outside.1,
                                best_outside.2,
                                best_outside.3,
                                best_outside.0,
                                best.0
                            );
                        }
                        checked += 1;
                    }
                }
            }
        }
    }
    assert!(checked >= 100, "the walk must be a real sweep: {checked}");
    assert!(
        brightest_field > 40.0,
        "NON-VACUOUS: the frames must carry the white sparkle population \
         this clause is about — the brightest one reached only \
         {brightest_field:.0} of luminance"
    );
    // A margin, not a tie: the promised margin is
    // `RAINBOW_CARET_LIGHT_FLOOR · (1 - RAINBOW_SPARKLE_LIGHT_SHARE)` = 8
    // levels, less one level per composited byte for rounding. A star can sit
    // at the field's ceiling (solved to a fractional level, `145 / 2.35 =
    // 61.7`, cast to 61), and the caret's and the star's composites each round
    // to a byte, which can land up to two levels against the margin.
    let promised = RAINBOW_CARET_LIGHT_FLOOR * (1.0 - RAINBOW_SPARKLE_LIGHT_SHARE) - 2.0;
    assert!(
        worst_margin >= promised,
        "the caret's margin over the brightest thing outside it fell to \
         {worst_margin:.1} levels of luminance, under the {promised:.0} its \
         own floor promises — at {worst_at}"
    );
}

/// Every kitty-named spelling selects the full-body resident pet; the flying
/// head keeps its own explicit spellings and aliases, and the two lists are
/// disjoint (an overlap would make one string mean both animals, and the pet
/// would silently win).
#[test]
fn every_kitty_spelling_draws_the_resident_and_flying_stays_reachable() {
    for s in [
        "rainbow kitty",
        "kitty",
        "rainbow kitty pet",
        "kitty pet",
        "pet kitty",
        "  Rainbow Kitty  ",
        // Geometry is not an animal: the underline spellings are the ribbon
        // drawn under the baseline and must draw the pet. All four, because
        // `prefs::CURSOR_TRAIL_STYLE_ALIASES` canonicalises the other three
        // onto this one.
        "rainbow kitty underline",
        "rainbow underline",
        "underline rainbow",
        "nyan underline",
        "  Rainbow Kitty Underline  ",
        // `… tall` names the default geometry (`ribbon_tall` is true for both),
        // so it must not swap the companion. All four spellings, as above.
        "rainbow kitty tall",
        "rainbow tall",
        "tall rainbow",
        "nyan tall",
    ] {
        assert_eq!(GlowStyle::parse(s), GlowStyle::RainbowKitty, "{s}");
        assert!(
            GlowStyle::style_names_kitty_pet(s),
            "{s:?} names a kitty and must draw the full-body resident"
        );
        assert!(GlowStyle::style_names_any_pet(s), "{s}");
        assert!(!GlowStyle::style_names_flying_kitty(s), "{s}");
    }
    // The flying head: explicit spellings plus every historical alias that
    // has ever selected it. All ride the same ribbon; none is a pet.
    for s in [
        "rainbow kitty flying",
        "flying kitty",
        "kitty flying",
        "  Flying Kitty  ",
        "nyan rainbow",
        "nyan",
        "rainbow",
    ] {
        assert_eq!(GlowStyle::parse(s), GlowStyle::RainbowKitty, "{s}");
        assert!(
            !GlowStyle::style_names_any_pet(s),
            "{s:?} must keep the flying head"
        );
    }
    // Disjointness, stated as the property rather than as a spot check.
    for s in [
        "rainbow kitty",
        "kitty",
        "rainbow kitty pet",
        "kitty pet",
        "pet kitty",
        "rainbow kitty flying",
        "flying kitty",
        "kitty flying",
        "rainbow dog pet",
        "dog pet",
        "pet dog",
        "rainbow puppy pet",
        "rainbow kitty underline",
        "rainbow underline",
        "underline rainbow",
        "nyan underline",
    ] {
        assert!(
            !(GlowStyle::style_names_any_pet(s) && GlowStyle::style_names_flying_kitty(s)),
            "{s:?} claims both animals"
        );
    }
    // The converse of `prefs`'s `every_tall_ribbon_spelling_stays_selectable`:
    // choosing the geometry must not change the animal.
    assert_eq!(
        GlowStyle::style_names_any_pet("rainbow kitty"),
        GlowStyle::style_names_any_pet("rainbow kitty underline"),
        "appending the geometry word must not swap the companion"
    );
    assert!(
        GlowStyle::style_names_underline_ribbon("rainbow kitty underline")
            && !GlowStyle::style_names_underline_ribbon("rainbow kitty"),
        "…and the two really are the geometry fork, or the line above is vacuous"
    );
    // THE DOG IS UNTOUCHED: widening the kitty list must not reach a dog
    // spelling, and the bare kitty word must not become a dog.
    for dog in ["rainbow dog pet", "dog pet", "pet dog", "rainbow puppy pet"] {
        assert!(GlowStyle::style_names_dog_pet(dog), "{dog}");
        assert!(!GlowStyle::style_names_kitty_pet(dog), "{dog}");
    }
    for cat in ["rainbow kitty", "kitty", "rainbow kitty pet"] {
        assert!(!GlowStyle::style_names_dog_pet(cat), "{cat}");
    }
    // No non-kitty style has a companion; kitty-named geometry spellings
    // (`tall`, `underline`, `flat`) draw the resident.
    for s in ["comet", "phaser", "lumen", "off"] {
        assert!(!GlowStyle::style_names_kitty_pet(s), "{s}");
        assert!(!GlowStyle::style_names_flying_kitty(s), "{s}");
    }
}

/// Hot rainbow kitty samples bank up and down as a coherent travelling ribbon:
/// the six stripes stay ordered and contained, and the motion is visible after
/// integer rounding.
#[test]
fn rainbow_hot_ribbon_has_visible_travelling_wave() {
    let g = geom();
    let mut c = cfg(GlowStyle::RainbowKitty, true);
    // On the default tall body. The underline waves only where it has bloomed
    // into the typed streak (`rainbow_underline_blooms_into_the_typed_streak`);
    // at rest it is straight.
    c.ribbon_tall = true;
    c.intensity = 1.0;
    c.radius = 0.0;
    c.ring = false;
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 2)), t, &c, g, &mut out);
    for col in 3..=20u16 {
        t += Duration::from_millis(18);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((3, col)), t, &c, g, &mut out);
    }
    assert!(glow.heat > 0.9);
    out.extend_from_slice(glow.under_quads()); // ribbon body rides the under-ink stream
    assert_invariants(&out, g);

    // The green stripe is the only band with B=0 and G>R. Hot bands emit as
    // device-row beam slabs, so accept any width ≥ 2 (star arms are 1px) and
    // track its Y across cells.
    let green_ys: std::collections::BTreeSet<u16> = out
        .iter()
        .filter(|q| {
            q.row == 3
                && is_ribbon_quad(q.color)
                && ((q.color >> 8) & 0xff) > ((q.color >> 16) & 0xff)
        })
        .map(|q| q.y)
        .collect();
    assert!(
        green_ys.len() >= 2,
        "hot ribbon must visibly bank across its run: {green_ys:?}"
    );
    assert!(out.len() <= CursorGlow::MAX_QUADS);
}

/// Momentum: a sustained FAST run makes the ribbon brighter than a lone slow move
/// (typing heat rides through to the band coverage) — the acceleration tell.
#[test]
fn rainbow_ribbon_brightens_with_typing_momentum() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let band_ink = |glow: &mut CursorGlow, out: &Vec<GlowQuad>| -> u64 {
        let _ = glow;
        // Weight by area so the strip subdivision of hot bands (the wave
        // curve) doesn't inflate "ink" by quad count alone.
        out.iter()
            .map(|q| {
                (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64
                    * (q.w as u64)
                    * (q.h as u64)
            })
            .sum()
    };
    // COLD: one isolated single-cell move from rest.
    let mut cold = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    cold.tick(Some((3, 2)), t0, &c, g, &mut out);
    let t1 = t0 + Duration::from_millis(16);
    cold.note_synthetic_typed(t1, 1);
    cold.tick(Some((3, 3)), t1, &c, g, &mut out);
    out.extend_from_slice(cold.under_quads()); // ribbon rides the under-ink stream
    let cold_ink = band_ink(&mut cold, &out);
    // HOT: a sustained fast run of single-cell advances builds heat.
    let mut hot = CursorGlow::default();
    let mut t = Instant::now();
    hot.tick(Some((3, 2)), t, &c, g, &mut out);
    for col in 3..14u16 {
        t += Duration::from_millis(18); // ~55 cps → heat ramps
        hot.note_synthetic_typed(t, 1);
        hot.tick(Some((3, col)), t, &c, g, &mut out);
    }
    out.extend_from_slice(hot.under_quads()); // ribbon rides the under-ink stream
    let hot_ink = band_ink(&mut hot, &out);
    assert!(
        hot_ink > cold_ink,
        "sustained fast typing must brighten/lengthen the ribbon (hot {hot_ink} > cold {cold_ink})"
    );
}

/// Four-letter guarantee: even at ~2.5 cps the ribbon spans at least the last
/// four typed cells when a key lands; spark life chains to the observed
/// cadence, not the clock.
#[test]
fn rainbow_ribbon_spans_four_letters_at_any_rhythm() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 2)), t, &c, g, &mut out);
    for col in 3..=10u16 {
        t += Duration::from_millis(400); // slow, thoughtful typing
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((3, col)), t, &c, g, &mut out);
    }
    assert!(glow.heat < 0.05, "a 400ms cadence must stay cold");
    out.extend_from_slice(glow.under_quads()); // ribbon rides the under-ink stream
    let cols = ribbon_cols(&out, 3, g);
    assert!(
        cols.len() >= 4,
        "ribbon spans at least four letters at any rhythm: {cols:?}"
    );
}

/// The four-letter guarantee holds at a GLACIAL rhythm too (~0.55 cps): the
/// chain window covers any plausible typing cadence, not just brisk ones.
#[test]
fn rainbow_ribbon_four_letters_even_glacial() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 2)), t, &c, g, &mut out);
    for col in 3..=9u16 {
        t += Duration::from_millis(1800); // hunt-and-peck
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((3, col)), t, &c, g, &mut out);
    }
    out.extend_from_slice(glow.under_quads()); // ribbon rides the under-ink stream
    let cols = ribbon_cols(&out, 3, g);
    assert!(
        cols.len() >= 4,
        "ribbon spans four letters even at hunt-and-peck: {cols:?}"
    );
}

/// Continuity: a typed echo that hops several columns in one observed move (PTY
/// batching, key repeat) sweeps every skipped cell as a typing spark.
#[test]
fn rainbow_coalesced_typing_sweeps_every_skipped_cell() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t = Instant::now();
    glow.tick(Some((3, 2)), t, &c, g, &mut out);
    for i in 0..4u64 {
        glow.note_synthetic_typed(t + Duration::from_millis(5 + i), 1);
    }
    glow.tick(Some((3, 6)), t + Duration::from_millis(20), &c, g, &mut out);
    glow.tick(
        Some((3, 6)),
        t + Duration::from_millis(140),
        &c,
        g,
        &mut out,
    );
    let cols = v2_cols(&glow, 3);
    for col in 2..=5u16 {
        assert!(
            cols.contains(&col),
            "swept glyph cell {col} is lit: {cols:?}"
        );
    }
    assert!(
        !cols.contains(&6),
        "the landing is the caret's cell, not a glyph's: {cols:?}"
    );
}

/// A hand at human speed into an app that repaints late: seven presses at 10
/// keys/s, one repaint ~700 ms later, one observed move of seven columns (and
/// one of ten). A credit is spent by the cells it lays and survives until then,
/// so the presses that earned the cells pay for them however old they are.
#[test]
fn a_late_repaint_lays_every_cell_the_hand_typed() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();

    for (keys, lag_ms) in [(7u16, 700u64), (10, 900)] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((3, 2)), t0, &c, g, &mut out);
        // The hand: one press every 100 ms, nothing echoed yet.
        for i in 0..u64::from(keys) {
            glow.note_typed_cells(t0 + Duration::from_millis(i * 100), 1);
        }
        // The app repaints once, `lag_ms` after the first key: all `keys`
        // glyphs appear at once and the caret is observed `keys` columns on.
        let at = t0 + Duration::from_millis(lag_ms);
        glow.tick(Some((3, 2 + keys)), at, &c, g, &mut out);

        let cols = v2_cols(&glow, 3);
        let dark: Vec<u16> = (2..2 + keys).filter(|col| !cols.contains(col)).collect();
        assert!(
            dark.is_empty(),
            "a {keys}-column batched echo after a {lag_ms} ms repaint left \
             cells {dark:?} unlit; lit: {cols:?}"
        );
        let tally = glow.admission_tally();
        assert_eq!(
            tally.declined, 0,
            "the batch is typing, not a jump (last_decline_reason={:?})",
            tally.last_decline_reason
        );
    }
}

/// A repaint that lands after the hand has paused still owes the keys it
/// swallowed: a debounced app can repaint half a second after the last key.
/// The licence is the ledger (an unpaid press is a real key whose glyph has
/// not landed), not `TYPE_HINT_FRESH`. Program output banks no credits, so it
/// cannot take this path.
#[test]
fn a_repaint_that_lands_after_the_hand_pauses_still_lays_the_keys_it_owes() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    for i in 0..7u64 {
        glow.note_typed_cells(t0 + Duration::from_millis(i * 100), 1);
    }
    // The hand stopped at +600 ms; the box repaints at +1000 ms, 400 ms
    // past every banked stamp's freshness.
    glow.tick(
        Some((3, 9)),
        t0 + Duration::from_millis(1000),
        &c,
        g,
        &mut out,
    );
    let cols = v2_cols(&glow, 3);
    let dark: Vec<u16> = (2..9u16).filter(|col| !cols.contains(col)).collect();
    assert!(
        dark.is_empty(),
        "the repaint owed seven cells and left {dark:?} unlit; lit: {cols:?}"
    );
}

/// Refutation control for the gate above: with every press's glyph already on
/// glass, a caret that advances on its own is program output and is refused.
#[test]
fn a_caret_that_advances_with_no_unpaid_press_still_buys_nothing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    for i in 1..=5u16 {
        let at = t0 + Duration::from_millis(u64::from(i) * 40);
        glow.note_typed_cells(at, 1);
        glow.tick(
            Some((3, 2 + i)),
            at + Duration::from_millis(4),
            &c,
            g,
            &mut out,
        );
    }
    let lit_after_typing = v2_cols(&glow, 3);
    // 400 ms of silence, then the caret walks four columns by itself.
    glow.tick(
        Some((3, 11)),
        t0 + Duration::from_millis(620),
        &c,
        g,
        &mut out,
    );
    let cols = v2_cols(&glow, 3);
    let bought: Vec<u16> = (7..=10u16).filter(|col| cols.contains(col)).collect();
    assert!(
        bought.is_empty(),
        "keyless output lit {bought:?}; before it: {lit_after_typing:?}, after: {cols:?}"
    );
}

/// Refutation control for [`a_late_repaint_lays_every_cell_the_hand_typed`]:
/// vim's `w` is one press that hops a word, and one key must not paint a ribbon
/// over it. Five 1-cell echoes spend their credits as they land, so the lone
/// `w` finds an empty pool and its swept cells stay dark.
#[test]
fn one_press_still_cannot_buy_a_word_wide_hop() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);

    // Five ordinary typed echoes, one cell each, each observed on its own
    // frame: the everyday case, and the one that drains the pool.
    for i in 1..=5u16 {
        let at = t0 + Duration::from_millis(u64::from(i) * 40);
        glow.note_typed_cells(at, 1);
        glow.tick(
            Some((3, 2 + i)),
            at + Duration::from_millis(4),
            &c,
            g,
            &mut out,
        );
    }

    // ONE press, and the caret jumps five columns: vim's `w`.
    let at = t0 + Duration::from_millis(260);
    glow.note_typed_cells(at, 1);
    glow.tick(
        Some((3, 12)),
        at + Duration::from_millis(8),
        &c,
        g,
        &mut out,
    );

    // The skimmed cells stay dark. The landing cell (11, behind the caret at
    // 12) is the re-anchor's single spark; `join_cohort` admits only cells
    // adjacent to or inside a live cohort, so it mints its own mark rather than
    // extending the typed run at 2..=6: unclaimed ground between marks, not a
    // hole in one.
    let cols = v2_cols(&glow, 3);
    let bought: Vec<u16> = (7..=10u16).filter(|col| cols.contains(col)).collect();
    assert!(
        bought.is_empty(),
        "one press bought the skimmed cells {bought:?}; lit: {cols:?}"
    );
    assert!(
        (2..=6u16).all(|col| cols.contains(&col)),
        "the five real echoes keep their own band: {cols:?}"
    );
}

/// A Rainbow Kitty sweep refused by the credit budget is recorded as a refusal
/// naming `no-credits`. Drives the shape
/// `one_press_still_cannot_buy_a_word_wide_hop` keeps dark and checks only the
/// ring; control: ordinary admitted typing reads `licensed`.
#[test]
fn a_refused_rainbow_sweep_names_the_credit_budget_that_refused_it() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);

    // Five ordinary echoes SPEND their five credits as they land.
    for i in 1..=5u16 {
        let at = t0 + Duration::from_millis(u64::from(i) * 40);
        glow.note_typed_cells(at, 1);
        glow.tick(
            Some((3, 2 + i)),
            at + Duration::from_millis(4),
            &c,
            g,
            &mut out,
        );
    }
    let admitted = glow.admission_tally();
    assert_eq!(
        admitted.last_decline_reason, None,
        "THE CONTROL: ordinary typing that painted must still read licensed"
    );
    assert_eq!(
        admitted.declined, 0,
        "nothing was refused yet: {admitted:?}"
    );

    // ONE press, caret jumps five columns: vim's `w`. Refused by the credit
    // budget, and the ring must say which budget.
    let at = t0 + Duration::from_millis(260);
    glow.note_typed_cells(at, 1);
    glow.tick(
        Some((3, 12)),
        at + Duration::from_millis(8),
        &c,
        g,
        &mut out,
    );

    let tally = glow.admission_tally();
    assert_eq!(
        tally.last_decline_reason,
        Some(CursorGlow::DECLINE_NO_CREDITS),
        "a sweep refused by the press budget must NAME it: {tally:?}"
    );
    assert!(
        tally.declined > admitted.declined,
        "the refusal must be counted, not reported as licensed: {tally:?}"
    );
    // And the light is unchanged: this is an instrument, not an admission.
    let cols = v2_cols(&glow, 3);
    let bought: Vec<u16> = (7..=10u16).filter(|col| cols.contains(col)).collect();
    assert!(
        bought.is_empty(),
        "recording the decline must not admit the skim: {bought:?}"
    );
}

/// One batched echo lays the same spatial hue sequence as the same glyphs
/// observed one frame at a time: coalescing may change birth time and
/// thermals, never which colour each typed cell owns.
#[test]
fn rainbow_sequential_and_coalesced_typing_lay_equivalent_hues() {
    let g = geom();
    let c = cfg_for_style_name("rainbow kitty", true);
    let t0 = Instant::now();

    let mut sequential = CursorGlow::default();
    let mut out = Vec::new();
    sequential.tick(Some((3, 2)), t0, &c, g, &mut out);
    for (i, col) in (3..=6u16).enumerate() {
        let at = t0 + Duration::from_millis(5 * (i as u64 + 1));
        sequential.note_synthetic_typed(at, 1);
        sequential.tick(Some((3, col)), at, &c, g, &mut out);
    }

    let mut coalesced = CursorGlow::default();
    let mut out = Vec::new();
    coalesced.tick(Some((3, 2)), t0, &c, g, &mut out);
    coalesced.note_synthetic_typed(t0 + Duration::from_millis(5), 4);
    coalesced.tick(
        Some((3, 6)),
        t0 + Duration::from_millis(20),
        &c,
        g,
        &mut out,
    );

    // C2: the field is a function of position in the cohort, so cells 2..=5
    // take the same stops whether they arrived one key at a time or as one
    // wide echo.
    let laid = |glow: &CursorGlow| {
        (2..=5u16)
            .map(|col| (col, glow.v2.field_at(3, col)))
            .collect::<std::collections::BTreeMap<_, _>>()
    };
    let sequential_laid = laid(&sequential);
    let coalesced_laid = laid(&coalesced);
    for col in 2..=5u16 {
        let a = sequential_laid[&col].expect("sequential typing lays every cell");
        let b = coalesced_laid[&col].expect("the wide echo lays every cell");
        assert!(
            (a - b).abs() <= 2.0 * f32::EPSILON,
            "cell {col}: sequential stop {a} != coalesced stop {b}"
        );
    }
}

/// Width-credit pricing: one press of a wide glyph advances two columns, so its
/// echo coalesces on that press's 2-cell credit and sweeps the continuation
/// cell. The same hop on a 1-cell credit is a skim and is refused, and an
/// admitted coalesce spends its credits so they cannot fund a second echo.
#[test]
fn a_wide_glyphs_single_press_sweeps_its_continuation_cell() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    // The typed wide glyph: one press, priced at two cells.
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t = Instant::now();
    glow.tick(Some((3, 4)), t, &c, g, &mut out);
    glow.note_synthetic_typed(t + Duration::from_millis(5), 2);
    glow.tick(Some((3, 6)), t + Duration::from_millis(20), &c, g, &mut out);
    let live = v2_cols(&glow, 3);
    assert!(
        live.contains(&4) && live.contains(&5),
        "the wide glyph's two cells are ribbon cells: {live:?}"
    );
    // The pool is spent: an immediate second 2-cell hop (hint still fresh) has
    // nothing to pay with and is refused.
    glow.tick(Some((3, 8)), t + Duration::from_millis(40), &c, g, &mut out);
    let after = v2_cols(&glow, 3);
    assert!(
        !after.contains(&6) && !after.contains(&7),
        "spent credits must not fund a second multi-cell echo: {after:?}"
    );
    // Control: the same 2-cell hop on a 1-cell credit is a skim (vim `w` over
    // wide text); the continuation cell is never painted as typing.
    let mut skim = CursorGlow::default();
    let mut out2 = Vec::new();
    let t2 = Instant::now();
    skim.tick(Some((3, 4)), t2, &c, g, &mut out2);
    skim.note_typed(t2 + Duration::from_millis(5));
    skim.tick(
        Some((3, 6)),
        t2 + Duration::from_millis(20),
        &c,
        g,
        &mut out2,
    );
    assert!(
        !v2_cols(&skim, 3).contains(&4),
        "one 1-cell credit must not paint a skimmed continuation cell: {:?}",
        v2_cols(&skim, 3)
    );
}

/// A program that echoes wide glyphs for narrow keys (a terminal-side input
/// method, a TUI in full-width forms): the host banks one cell per press but
/// the caret advances two, so a raw-column share rule (`credits * 4 >=
/// dc_abs * 3`) could never pass. The share is denominated in glyphs: the row
/// probe ([`CursorGlow::observe_row`]) spells a wide glyph's continuation
/// column `'\0'`, and those columns in the swept run are counted as second
/// halves of glyphs the presses laid.
///
/// Controls:
///
/// * the same probe over narrow text is unchanged (the probe neither lights
///   nor darkens anything);
/// * one press still cannot buy a wide hop: the `credits >= 2` floor reads the
///   raw ledger (see `a_wide_glyphs_single_press_sweeps_its_continuation_cell`);
/// * two presses cannot buy seven wide glyphs: each press claims at most one
///   extra column, so over an all-wide run the rule reduces to
///   `presses >= 0.75 * glyphs`, the narrow rule's fraction.
#[test]
fn a_wide_glyph_echo_pays_for_the_cells_its_own_glyphs_occupy() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);

    // A row of `n` wide glyphs from `from`: lead char at its column, `'\0'` at
    // the continuation, as `Terminal::row_cols_into` hands `observe_row`.
    let wide_row = |from: u16, n: u16| {
        let mut cols = [' '; 40];
        for i in 0..n {
            let at = usize::from(from) + usize::from(i) * 2;
            cols[at] = '漢';
            cols[at + 1] = '\0';
        }
        cols
    };

    // Seven keys at 10/s; one repaint 700 ms after the first echoes each as a
    // 2-cell glyph, so the caret moves fourteen columns in one move.
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    for i in 0..7u64 {
        glow.note_typed_cells(t0 + Duration::from_millis(i * 100), 1);
    }
    let at = t0 + Duration::from_millis(700);
    glow.observe_row(3, 16, &wide_row(2, 7), at);
    glow.tick(Some((3, 16)), at, &c, g, &mut out);
    let cols = v2_cols(&glow, 3);
    let dark: Vec<u16> = (2..16u16).filter(|col| !cols.contains(col)).collect();
    assert!(
        dark.is_empty(),
        "the seven wide glyphs the hand typed left black cells {dark:?}; lit: {cols:?}"
    );

    // Control 1: the same probe over narrow text, seven keys, seven columns.
    let mut narrow = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    narrow.tick(Some((3, 2)), t0, &c, g, &mut out);
    for i in 0..7u64 {
        narrow.note_typed_cells(t0 + Duration::from_millis(i * 100), 1);
    }
    let at = t0 + Duration::from_millis(700);
    let mut plain = [' '; 40];
    plain[2..9].fill('x');
    narrow.observe_row(3, 9, &plain, at);
    narrow.tick(Some((3, 9)), at, &c, g, &mut out);
    let cols = v2_cols(&narrow, 3);
    let dark: Vec<u16> = (2..9u16).filter(|col| !cols.contains(col)).collect();
    assert!(
        dark.is_empty(),
        "narrow text under the same probe must be unchanged: dark {dark:?}, lit {cols:?}"
    );

    // Control 2: one press over the same wide row (vim `w`). The floor reads
    // the raw ledger, so one credit is one credit.
    let mut skim = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    skim.tick(Some((3, 2)), t0, &c, g, &mut out);
    skim.note_typed_cells(t0 + Duration::from_millis(10), 1);
    let at = t0 + Duration::from_millis(30);
    skim.observe_row(3, 16, &wide_row(2, 7), at);
    skim.tick(Some((3, 16)), at, &c, g, &mut out);
    let cols = v2_cols(&skim, 3);
    let bought: Vec<u16> = (2..15u16).filter(|col| cols.contains(col)).collect();
    assert!(
        bought.is_empty(),
        "one press bought {bought:?} cells of wide text it only skimmed; lit: {cols:?}"
    );

    // Control 3: two presses over seven wide glyphs claim two extra columns:
    // 4 paid against 14 swept is 16 >= 42, refused.
    let mut short = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    short.tick(Some((3, 2)), t0, &c, g, &mut out);
    short.note_typed_cells(t0 + Duration::from_millis(10), 1);
    short.note_typed_cells(t0 + Duration::from_millis(110), 1);
    let at = t0 + Duration::from_millis(300);
    short.observe_row(3, 16, &wide_row(2, 7), at);
    short.tick(Some((3, 16)), at, &c, g, &mut out);
    let cols = v2_cols(&short, 3);
    let bought: Vec<u16> = (2..15u16).filter(|col| cols.contains(col)).collect();
    assert!(
        bought.is_empty(),
        "two presses bought seven wide glyphs {bought:?}; lit: {cols:?}"
    );
}

/// Continuity law: whatever mix of single advances and batched hops typing
/// arrives as, the live sparks on the row form one contiguous run.
#[test]
fn rainbow_typed_row_has_no_interior_gaps() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let mut t = Instant::now();
    glow.tick(Some((3, 1)), t, &c, g, &mut out);
    let mut col = 1u16;
    for (i, hop) in [1u16, 3, 1, 2, 5, 1, 4, 2].into_iter().enumerate() {
        col += hop;
        t += Duration::from_millis(40 + (i as u64 % 3) * 30);
        // Each hop models `hop` batched echoes — arm one press per glyph
        // (the anti-stray press budget).
        for k in 0..u64::from(hop) {
            glow.note_synthetic_typed(t - Duration::from_millis(2) - Duration::from_millis(k), 1);
        }
        glow.tick(Some((3, col)), t, &c, g, &mut out);
        let mut cols: Vec<u16> = glow
            .sparks
            .iter()
            .filter(|s| s.typing && s.row == 3)
            .map(|s| s.col)
            .collect();
        cols.sort_unstable();
        cols.dedup();
        let contiguous = cols.windows(2).all(|w| w[1] - w[0] == 1);
        assert!(contiguous, "no interior gap after hop {i}: {cols:?}");
    }
}

/// Continuity: a typing wrap folds the ribbon around the line end (the old row
/// finished, the new row from its left edge), never disjoint rows or a diagonal
/// sweep.
#[test]
fn rainbow_typing_wrap_folds_around_the_line_end() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t = Instant::now();
    glow.tick(Some((3, 38)), t, &c, g, &mut out);
    // The wrap SHAPE: from the second-to-last column (a wide-glyph wrap)
    // down one row to column 1, backed by the committed wide glyph.
    let wrapped = t + Duration::from_millis(30);
    glow.note_synthetic_typed(wrapped, 2);
    glow.tick(Some((4, 1)), wrapped, &c, g, &mut out);
    // Observe one beat later: the feathered birth ease keeps the folded
    // cells near-zero on the frame the fold is laid.
    glow.tick(
        Some((4, 1)),
        t + Duration::from_millis(150),
        &c,
        g,
        &mut out,
    );
    let old_row = v2_cols(&glow, 3);
    let new_row = v2_cols(&glow, 4);
    assert!(
        old_row.contains(&39),
        "the old row is finished to its edge: {old_row:?}"
    );
    assert!(
        new_row.contains(&0),
        "the new row starts at its left edge: {new_row:?}"
    );
    // No diagonal smear: nothing swept between the fold columns.
    assert!(
        !old_row.contains(&20) && !new_row.contains(&20),
        "no mid-row cells were invented by the fold"
    );

    // …and a COLD +2 hop after the fold, with no press behind it, sweeps
    // nothing: the license declines it before the classifier runs.
    let program = wrapped + Duration::from_millis(400);
    glow.tick(Some((4, 3)), program, &c, g, &mut out);
    let after = v2_cols(&glow, 4);
    assert!(
        !after.contains(&1) && !after.contains(&2),
        "a cold +2 hop after the fold cannot sweep the cells between: {after:?}"
    );
}

/// Continuity through the typed hide-bridge: a keystroke whose echo hides the
/// cursor and reappears several columns on (ConPTY, batched echoes) still lays
/// its swept wake; with no typed candidate the choreography stays inert.
#[test]
fn rainbow_typed_hide_bridge_spans_the_batch() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t = Instant::now();

    let run = |hint: bool| -> std::collections::BTreeSet<u16> {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((3, 2)), t, &c, g, &mut out);
        glow.tick(None, t + Duration::from_millis(10), &c, g, &mut out); // echo hides
        if hint {
            // The batch is 4 echoed glyphs: 4 presses (anti-stray budget).
            for i in 0..4u64 {
                glow.note_synthetic_typed(t + Duration::from_millis(15 + i), 1);
            }
        }
        glow.tick(Some((3, 6)), t + Duration::from_millis(30), &c, g, &mut out);
        // Observe one beat later: the feathered birth ease keeps a just-bridged
        // cell near zero on its landing frame.
        glow.tick(
            Some((3, 6)),
            t + Duration::from_millis(150),
            &c,
            g,
            &mut out,
        );
        v2_cols(&glow, 3)
    };

    let hinted = run(true);
    for col in 2..=5u16 {
        assert!(
            hinted.contains(&col),
            "typed bridge sweeps the batch: {hinted:?}"
        );
    }
    assert!(
        run(false).is_empty(),
        "the unhinted 4-cell reappear stays suppressed (bridge law)"
    );
}

/// First-key orphan: the first key after a multi-second pause takes a long
/// chained life. When a fast burst buries it, the rhythm-aware retro-clamp voids
/// that life, so the whole ribbon, the first key's cell included, fades as one
/// rather than leaving a stuck cell after a jump.
#[test]
fn rainbow_first_key_orphan_does_not_outlive_the_ribbon() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let cw = g.cw as u16;
    let lit = |glow: &CursorGlow| -> std::collections::BTreeSet<(u16, u16)> {
        glow.under_quads() // ribbon rides the under-ink stream
            .iter()
            .filter(|q| is_ribbon_quad(q.color))
            .map(|q| (q.row, q.x / cw))
            .collect()
    };
    let t0 = Instant::now();
    glow.tick(Some((3, 1)), t0, &c, g, &mut out);
    glow.note_synthetic_typed(t0, 1);
    glow.tick(Some((3, 2)), t0, &c, g, &mut out); // establish the rhythm clock
    // A 3s pause, then ONE lone key (its inter-key gap is a full 3s).
    let t_orphan = t0 + Duration::from_millis(3000);
    glow.note_synthetic_typed(t_orphan, 1);
    glow.tick(Some((3, 3)), t_orphan, &c, g, &mut out);
    // Then a fast 20-key burst establishes a much faster rhythm.
    let mut t = t_orphan;
    for k in 1..=20u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((3, 3 + k)), t, &c, g, &mut out);
    }
    // The finger lifts to navigate elsewhere (a jump resets the tail, so the
    // exit swoosh cannot mask per-cell life); the ribbon must fade within a
    // beat or two.
    t += Duration::from_millis(50);
    glow.note_synthetic_move(t);
    glow.tick(Some((0, 5)), t, &c, g, &mut out);
    let t_last = t;
    let orphan = (3u16, 3u16);

    // Within ~1.5s the first key's cell is gone while the burst still glows —
    // it does NOT outlive the ribbon it started.
    glow.tick(
        Some((0, 5)),
        t_last + Duration::from_millis(1500),
        &c,
        g,
        &mut out,
    );
    let mid = lit(&glow);
    assert!(
        !mid.contains(&orphan),
        "the first-key orphan fades with the burst, not 10s later: {mid:?}"
    );
    // And nothing is left parked anywhere long after — no lone mini-rainbow
    // burning at constant brightness while the rest of the screen is dark.
    glow.tick(
        Some((0, 5)),
        t_last + Duration::from_millis(8000),
        &c,
        g,
        &mut out,
    );
    assert!(
        lit(&glow).is_empty(),
        "no cell parked ~8s after the ribbon faded: {:?}",
        lit(&glow)
    );
}

/// The ribbon decays to EXACTLY empty when the cursor rests — 0% idle, fingerprint 0.
#[test]
fn rainbow_decays_to_empty_when_idle() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 2)), t, &c, g, &mut out);
    glow.note_synthetic_move(t);
    glow.tick(Some((2, 8)), t, &c, g, &mut out);
    assert!(!out.is_empty(), "ribbon present right after a move");
    // Long after the comet duration, no move: everything decays away.
    let fp = glow.tick(Some((2, 8)), t + Duration::from_secs(2), &c, g, &mut out);
    assert!(out.is_empty(), "ribbon fully decayed when idle");
    assert_eq!(
        fp, 0,
        "empty snapshot fingerprints to 0 (event loop returns to idle)"
    );
}
