// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow kitty ribbon.

use super::*;

fn rainbow_occupied_coverage_model() -> aterm_spec::derive::Model {
    use aterm_spec::ty_model;
    ty_model! {
        RainbowOccupiedCoverage {
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

#[test]
fn rainbow_occupied_coverage_derived_model_proves_and_catches_regression() {
    assert_eq!(
        crate::rainbow_kitty::ribbon::UNDER_COV_CAP as i64,
        236,
        "RainbowOccupiedCoverage::Cap must equal ribbon::UNDER_COV_CAP"
    );
    let model = rainbow_occupied_coverage_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

#[test]
fn rainbow_retract_rearm_model_proves_and_catches_brightness_snap() {
    let model = rainbow_retract_rearm_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);
}

fn typed_burst(
    c: &GlowConfig,
    g: Geom,
    row: u16,
    cols: std::ops::Range<u16>,
    gap: Duration,
) -> (CursorGlow, Vec<GlowQuad>, Instant) {
    typed_burst_from_phase(c, g, row, cols, gap, 0.0)
}

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
                            caret_tick(false, at, &mut over);
                            over.truncate(mark);
                            caret_tick(true, at, &mut over);
                            over.truncate(mark);
                            caret_tick(true, at + Duration::from_millis(70), &mut over)
                        } else {
                            caret_tick(false, at, &mut over)
                        };
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

                        let mut f = Glass::new(g, c.theme_bg);
                        f.add_quads(glow.under_quads());
                        f.add_quads(&over);
                        f.add_halos(glow.halos());
                        f.stamp_caret(row, col, fill);

                        let (cx0, cy0, cx1, cy1) = f.cell_rect(row, col);
                        let in_cell =
                            |y: i32, x: i32| (cy0..cy1).contains(&y) && (cx0..cx1).contains(&x);
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
    let promised = RAINBOW_CARET_LIGHT_FLOOR * (1.0 - RAINBOW_SPARKLE_LIGHT_SHARE) - 2.0;
    assert!(
        worst_margin >= promised,
        "the caret's margin over the brightest thing outside it fell to \
         {worst_margin:.1} levels of luminance, under the {promised:.0} its \
         own floor promises — at {worst_at}"
    );
}

#[test]
fn every_kitty_spelling_draws_the_resident_and_flying_stays_reachable() {
    for s in [
        "rainbow kitty",
        "kitty",
        "rainbow kitty pet",
        "kitty pet",
        "pet kitty",
        "  Rainbow Kitty  ",
        "rainbow kitty underline",
        "rainbow underline",
        "underline rainbow",
        "nyan underline",
        "  Rainbow Kitty Underline  ",
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
    for dog in ["rainbow dog pet", "dog pet", "pet dog", "rainbow puppy pet"] {
        assert!(GlowStyle::style_names_dog_pet(dog), "{dog}");
        assert!(!GlowStyle::style_names_kitty_pet(dog), "{dog}");
    }
    for cat in ["rainbow kitty", "kitty", "rainbow kitty pet"] {
        assert!(!GlowStyle::style_names_dog_pet(cat), "{cat}");
    }
    for s in ["comet", "phaser", "lumen", "off"] {
        assert!(!GlowStyle::style_names_kitty_pet(s), "{s}");
        assert!(!GlowStyle::style_names_flying_kitty(s), "{s}");
    }
}

#[test]
fn rainbow_ribbon_brightens_with_typing_momentum() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let band_ink = |glow: &mut CursorGlow, out: &Vec<GlowQuad>| -> u64 {
        let _ = glow;
        out.iter()
            .map(|q| {
                (((q.color >> 16) & 0xff) + ((q.color >> 8) & 0xff) + (q.color & 0xff)) as u64
                    * (q.w as u64)
                    * (q.h as u64)
            })
            .sum()
    };
    let mut cold = CursorGlow::default();
    let mut out = Vec::new();
    let t0 = Instant::now();
    cold.tick(Some((3, 2)), t0, &c, g, &mut out);
    let t1 = t0 + Duration::from_millis(16);
    cold.note_synthetic_typed(t1, 1);
    cold.tick(Some((3, 3)), t1, &c, g, &mut out);
    out.extend_from_slice(cold.under_quads());
    let cold_ink = band_ink(&mut cold, &out);
    let mut hot = CursorGlow::default();
    let mut t = Instant::now();
    hot.tick(Some((3, 2)), t, &c, g, &mut out);
    for col in 3..14u16 {
        t += Duration::from_millis(18);
        hot.note_synthetic_typed(t, 1);
        hot.tick(Some((3, col)), t, &c, g, &mut out);
    }
    out.extend_from_slice(hot.under_quads());
    let hot_ink = band_ink(&mut hot, &out);
    assert!(
        hot_ink > cold_ink,
        "sustained fast typing must brighten/lengthen the ribbon (hot {hot_ink} > cold {cold_ink})"
    );
}

#[test]
fn rainbow_ribbon_spans_four_letters_at_any_rhythm() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    for (gap_ms, last_col) in [(400u64, 10u16), (1800, 9)] {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        glow.tick(Some((3, 2)), t, &c, g, &mut out);
        for col in 3..=last_col {
            t += Duration::from_millis(gap_ms);
            glow.note_synthetic_typed(t, 1);
            glow.tick(Some((3, col)), t, &c, g, &mut out);
        }
        assert!(
            glow.heat < 0.05,
            "{gap_ms} ms: a slow cadence must stay cold"
        );
        out.extend_from_slice(glow.under_quads());
        let cols = ribbon_cols(&out, 3, g);
        assert!(
            cols.len() >= 4,
            "{gap_ms} ms: ribbon spans at least four letters at any rhythm: {cols:?}"
        );
    }
}

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

#[test]
fn a_late_repaint_lays_every_cell_the_hand_typed() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut out = Vec::new();

    for (keys, lag_ms) in [(7u16, 700u64), (10, 900)] {
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        glow.tick(Some((3, 2)), t0, &c, g, &mut out);
        for i in 0..u64::from(keys) {
            glow.note_typed_cells(t0 + Duration::from_millis(i * 100), 1);
        }
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

#[test]
fn a_refused_rainbow_sweep_names_the_credit_budget_that_refused_it() {
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
    let admitted = glow.admission_tally();
    assert_eq!(
        admitted.last_decline_reason, None,
        "THE CONTROL: ordinary typing that painted must still read licensed"
    );
    assert_eq!(
        admitted.declined, 0,
        "nothing was refused yet: {admitted:?}"
    );

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
    let cols = v2_cols(&glow, 3);
    let bought: Vec<u16> = (7..=10u16).filter(|col| cols.contains(col)).collect();
    assert!(
        bought.is_empty(),
        "recording the decline must not admit the skim: {bought:?}"
    );
    assert!(
        (2..=6u16).all(|col| cols.contains(&col)),
        "the five real echoes keep their own band: {cols:?}"
    );
}

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

#[test]
fn a_wide_glyphs_single_press_sweeps_its_continuation_cell() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
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
    glow.tick(Some((3, 8)), t + Duration::from_millis(40), &c, g, &mut out);
    let after = v2_cols(&glow, 3);
    assert!(
        !after.contains(&6) && !after.contains(&7),
        "spent credits must not fund a second multi-cell echo: {after:?}"
    );
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

#[test]
fn a_wide_glyph_echo_pays_for_the_cells_its_own_glyphs_occupy() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);

    let wide_row = |from: u16, n: u16| {
        let mut cols = [' '; 40];
        for i in 0..n {
            let at = usize::from(from) + usize::from(i) * 2;
            cols[at] = '漢';
            cols[at + 1] = '\0';
        }
        cols
    };

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

    for (presses, glyphs) in [(2u64, 3u16), (1, 1)] {
        let mut few = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        few.tick(Some((3, 2)), t0, &c, g, &mut out);
        for i in 0..presses {
            few.note_typed_cells(t0 + Duration::from_millis(10 + i * 100), 1);
        }
        let to = 2 + 2 * glyphs;
        let at = t0 + Duration::from_millis(60 + (presses - 1) * 100);
        few.observe_row(3, to, &wide_row(2, glyphs), at);
        few.tick(Some((3, to)), at, &c, g, &mut out);
        let cols = v2_cols(&few, 3);
        let bought: Vec<u16> = (2..to - 1).filter(|col| cols.contains(col)).collect();
        assert!(
            bought.is_empty(),
            "{presses} presses bought {glyphs} wide glyphs {bought:?}; lit: {cols:?}"
        );
    }
}

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

#[test]
fn rainbow_typing_wrap_folds_around_the_line_end() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let t = Instant::now();
    glow.tick(Some((3, 38)), t, &c, g, &mut out);
    let wrapped = t + Duration::from_millis(30);
    glow.note_synthetic_typed(wrapped, 2);
    glow.tick(Some((4, 1)), wrapped, &c, g, &mut out);
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
    assert!(
        !old_row.contains(&20) && !new_row.contains(&20),
        "no mid-row cells were invented by the fold"
    );

    let program = wrapped + Duration::from_millis(400);
    glow.tick(Some((4, 3)), program, &c, g, &mut out);
    let after = v2_cols(&glow, 4);
    assert!(
        !after.contains(&1) && !after.contains(&2),
        "a cold +2 hop after the fold cannot sweep the cells between: {after:?}"
    );
}

#[test]
fn rainbow_typed_hide_bridge_spans_the_batch() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t = Instant::now();

    let run = |hint: bool| -> std::collections::BTreeSet<u16> {
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((3, 2)), t, &c, g, &mut out);
        glow.tick(None, t + Duration::from_millis(10), &c, g, &mut out);
        if hint {
            for i in 0..4u64 {
                glow.note_synthetic_typed(t + Duration::from_millis(15 + i), 1);
            }
        }
        glow.tick(Some((3, 6)), t + Duration::from_millis(30), &c, g, &mut out);
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

#[test]
fn rainbow_first_key_orphan_does_not_outlive_the_ribbon() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let cw = g.cw as u16;
    let lit = |glow: &CursorGlow| -> std::collections::BTreeSet<(u16, u16)> {
        glow.under_quads()
            .iter()
            .filter(|q| is_ribbon_quad(q.color))
            .map(|q| (q.row, q.x / cw))
            .collect()
    };
    let t0 = Instant::now();
    glow.tick(Some((3, 1)), t0, &c, g, &mut out);
    glow.note_synthetic_typed(t0, 1);
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    let t_orphan = t0 + Duration::from_millis(3000);
    glow.note_synthetic_typed(t_orphan, 1);
    glow.tick(Some((3, 3)), t_orphan, &c, g, &mut out);
    let mut t = t_orphan;
    for k in 1..=20u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((3, 3 + k)), t, &c, g, &mut out);
    }
    t += Duration::from_millis(50);
    glow.note_synthetic_move(t);
    glow.tick(Some((0, 5)), t, &c, g, &mut out);
    let t_last = t;
    let orphan = (3u16, 3u16);

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

#[test]
fn rainbow_decays_to_empty_when_idle() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let mut glow = CursorGlow::default();
    let t = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 2)), t, &c, g, &mut out);
    glow.note_synthetic_move(t);
    // A jump's meteor: a short hop's mini-fan stands on the caret's cell,
    // which no dark star stands on (looks7 M3).
    glow.tick(Some((2, 20)), t, &c, g, &mut out);
    assert!(!out.is_empty(), "ribbon present right after a move");
    let fp = glow.tick(Some((2, 20)), t + Duration::from_secs(2), &c, g, &mut out);
    assert!(out.is_empty(), "ribbon fully decayed when idle");
    assert_eq!(
        fp, 0,
        "empty snapshot fingerprints to 0 (event loop returns to idle)"
    );
}

/// Under a dim theme's letters, every pixel of each lettered cell the pet's body reaches, but the
/// caret's, keeps its bar over a coat of the letter's own colour (the worst any coat can be): the
/// veil's eased ends lie outside a word's first and last letters and stop at the caret's cell. A
/// bright theme's text, and cells outside the body, take no veil. Negative control: the veil off
/// leaves that coat whole.
#[test]
fn a_dim_theme_s_letters_read_over_the_pet_through_a_page_veil() {
    use aterm_core::terminal::RenderCell;
    let g = geom();
    let veil_at = |fg: [u8; 3], page: [u8; 3], on: bool, caret: (u16, u16)| {
        let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for (c, cell) in cells[3].iter_mut().enumerate() {
            cell.ch = if c < 20 && c % 5 != 4 { 'o' } else { ' ' };
            cell.fg = fg;
            cell.bg = page;
        }
        let mut glow = CursorGlow::default();
        glow.set_pet_veil(on);
        let now = Instant::now();
        glow.observe_frame_text(Some(caret), &cells, now);
        // The body over columns 2..12 of row 3 and the row above.
        glow.note_pet_body(Some((16, 96, 24, 64)));
        let mut out = Vec::new();
        glow.veil_letters_on_pet((&mut out, &mut GlowRamps::default()), Some(caret), g, None);
        out
    };
    let veil_of = |fg: [u8; 3], page: [u8; 3], on: bool| veil_at(fg, page, on, (3, 20));
    let rgb = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    let contrast = |a: u32, b: u32| crate::color_math::contrast_ratio(a, b);
    let (fg, page) = ([0x85, 0x85, 0x85], [0x11, 0x13, 0x18]);
    let bare = contrast(rgb(fg), rgb(page));
    // Each pixel of row 3: the coat (the ink's own colour) under the veil,
    // each quad's ramp read at the pixel's centre as the renderer reads it.
    let read = |quads: &[GlowQuad], x: i32| {
        let mut px = rgb(fg);
        for q in quads {
            let (qx, qy) = (i32::from(q.x), i32::from(q.y));
            if (qx..qx + i32::from(q.w)).contains(&x) && (qy..qy + i32::from(q.h)).contains(&56) {
                let a = aterm_render::glow_lerp(
                    u32::from(q.alpha),
                    u32::from(q.alpha2),
                    (x - qx) as u32,
                    u32::from(q.w),
                ) as u8;
                px = aterm_render::over_premul(px, aterm_render::premul_rgb(rgb(page), a), a)
                    & 0x00FF_FFFF;
            }
        }
        contrast(rgb(fg), px)
    };
    let bar = 4.6f32.min(bare * 0.97);
    let cw = g.cw as i32;
    // Every pixel of the body's columns in a lettered cell but the caret's.
    for caret_col in [20i32, 6] {
        let caret = (3u16, caret_col as u16);
        let on = veil_at(fg, page, true, caret);
        assert!(!on.is_empty(), "the pet on dim text draws a veil");
        for x in 16..96i32 {
            let col = x / cw;
            if col % 5 == 4 || col == caret_col {
                continue;
            }
            let r = read(&on, x);
            assert!(
                r >= bar,
                "caret at column {caret_col}: column {col}'s pixel {} reads {r:.2}:1 over the coat, under {bar:.2}",
                x - col * cw
            );
        }
        for q in &on {
            let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
            assert!(
                x1 <= caret_col * cw || x0 >= (caret_col + 1) * cw,
                "a veil quad at {x0}..{x1} enters the caret's cell (column {caret_col})"
            );
        }
    }
    let on = veil_of(fg, page, true);
    for q in &on {
        let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
        assert!(
            x0 >= 16 - 3 && x1 <= 96 + 3,
            "the veil keeps to the body's box ({x0}..{x1})"
        );
        let rise = (g.ch as f32 * 0.25).round() as i32;
        assert!(
            i32::from(q.y) >= 48 - rise && i32::from(q.y + q.h) <= 64 + rise,
            "the veil keeps to the letters' row and its rise"
        );
    }
    assert!(
        veil_of([0xd0, 0xd0, 0xd0], [0x1e, 0x1e, 0x1e], true).is_empty(),
        "a bright theme's text keeps the knock-out alone"
    );
    let off = veil_of(fg, page, false);
    assert!(off.is_empty(), "control: the veil off draws nothing");
    assert!(
        read(&off, 2 * g.cw as i32 + g.cw as i32 / 2) < bar,
        "control: the coat unveiled reads under the bar"
    );
}

/// THE PET'S VEIL RISES OFF ITS ROW (looks7 round 2): cut at its lettered
/// row's top and foot, the page veil sliced the coat across at the row's
/// edge (the burst on Solarized Dark, the body gone under `flies` below a
/// straight line). Read down one coat column, where the rows beside the
/// lettered one hold no letter under the body, the veil's opacity climbs
/// from nothing to the row's over an eighth of a cell above it and falls
/// the same way below, no pixel row stepping by more than half the row's;
/// the lettered row keeps it whole. Negative control: sliced, it jumps from
/// nothing to its whole at the row's top.
#[test]
fn the_pet_s_veil_rises_off_its_row() {
    use aterm_core::terminal::RenderCell;
    let g = geom();
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let (fg, page) = ([0x85u8, 0x85, 0x85], [0x11u8, 0x13, 0x18]);
    let steps = |risen: bool| -> (f32, u32, Vec<u32>) {
        let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for (c, cell) in cells[3].iter_mut().enumerate() {
            cell.ch = if c < 20 { 'o' } else { ' ' };
            cell.fg = fg;
            cell.bg = page;
        }
        let mut glow = CursorGlow::default();
        glow.set_pet_veil_risen(risen);
        glow.observe_frame_text(Some((3, 20)), &cells, Instant::now());
        // The body over columns 2..12 of rows 2 to 4.
        glow.note_pet_body(Some((2 * cw, 12 * cw, 2 * ch, 5 * ch)));
        let mut out = Vec::new();
        glow.veil_letters_on_pet(
            (&mut out, &mut GlowRamps::default()),
            Some((3, 20)),
            g,
            None,
        );
        // The veil's opacity at each pixel row of one coat column, as the
        // renderer stacks it.
        let x = 7 * cw + cw / 2;
        let column: Vec<u32> = (0..g.win_h as i32)
            .map(|y| {
                out.iter()
                    .filter(|q| {
                        (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&x)
                            && (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).contains(&y)
                    })
                    .fold(0u32, |held, q| {
                        let a = aterm_render::glow_lerp(
                            u32::from(q.alpha),
                            u32::from(q.alpha2),
                            (x - i32::from(q.x)) as u32,
                            u32::from(q.w),
                        );
                        held + a * (255 - held) / 255
                    })
            })
            .collect();
        let whole = column[(3 * ch + ch / 2) as usize];
        let step = column
            .windows(2)
            .map(|w| w[0].abs_diff(w[1]))
            .max()
            .unwrap_or(0);
        (step as f32 / whole.max(1) as f32, whole, column)
    };
    let (step, whole, column) = steps(true);
    assert!(
        whole > 128,
        "fixture: the dim row takes a heavy veil ({whole})"
    );
    assert!(
        (3 * ch..4 * ch).all(|y| column[y as usize] == whole),
        "the lettered row keeps the whole veil: {:?}",
        &column[(3 * ch) as usize..(4 * ch) as usize]
    );
    let rise = (ch as f32 * 0.12).round() as i32;
    for y in [3 * ch - rise, 4 * ch + rise - 1] {
        let a = column[y as usize];
        assert!(
            a > 0 && a < whole,
            "the veil eases at pixel row {y}: {a} of {whole}"
        );
    }
    assert!(
        step <= 0.5,
        "a pixel row steps the veil by {step:.2} of its whole: {:?}",
        &column[(3 * ch - rise - 1) as usize..(4 * ch + rise + 1) as usize]
    );
    let (sliced, ..) = steps(false);
    assert!(
        sliced > 0.9,
        "control: sliced, the veil steps by only {sliced:.2} of its whole"
    );
}

/// THE VEIL'S FEATHER IS NO DEEPER THAN THE COAT'S CUT (looks7 round 2): a
/// paw grazing a lettered row's top by a pixel is cut by that pixel, and
/// the rise above the row feathers it over one pixel row, not the whole
/// rise: feathered over a quarter of a cell over the burst on Solarized
/// Dark, the paw above the cut wore a dark block. Negative control:
/// feathered its whole height, the rows above the paw's tip take the page.
#[test]
fn the_pet_s_veil_feathers_no_deeper_than_the_coat_s_cut() {
    use aterm_core::render::{FxArt, FxTexels, PxRect};
    use aterm_core::terminal::RenderCell;
    use aterm_render::sprites::{FreeSprite, FreeZ};
    let g = geom();
    let ch = g.ch as i32;
    let (fg, page) = ([0x85u8, 0x85, 0x85], [0x11u8, 0x13, 0x18]);
    // The body over rows 1 and 2, its coat (the art's columns 10..50,
    // window x 26..66) reaching one pixel into row 3, which is lettered.
    let (w, h) = (80u32, 40u32);
    let top = 3 * ch;
    let mut texels = vec![0u8; (w * h * 4) as usize];
    for r in 0..(top + 1 - 24) as u32 {
        for c in 10..50 {
            texels[((r * w + c) * 4 + 3) as usize] = 255;
        }
    }
    let sprite = FreeSprite {
        x: 16,
        y: 24,
        w: w as u16,
        h: h as u16,
        art: std::sync::Arc::new(
            FxArt::new(w as u16, h as u16, FxTexels::Rgba8Straight, texels).expect("an 80x40 art"),
        ),
        clip: PxRect::EVERYWHERE,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
    };
    // The veil's opacity down the coat column at x = 40, as stacked.
    let column = |deep: bool| -> Vec<u32> {
        let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for (c, cell) in cells[3].iter_mut().enumerate() {
            cell.ch = if c < 20 { 'o' } else { ' ' };
            cell.fg = fg;
            cell.bg = page;
        }
        let mut glow = CursorGlow::default();
        glow.set_pet_veil_feather_deep(deep);
        glow.observe_frame_text(Some((3, 20)), &cells, Instant::now());
        glow.note_pet_body(Some((16, 96, 24, 64)));
        glow.note_veil_frame(Some((3, 20)), g);
        let mut out = Vec::new();
        glow.veil_pet_letters(
            (&mut out, &mut GlowRamps::default()),
            std::slice::from_ref(&sprite),
        );
        (0..g.win_h as i32)
            .map(|y| {
                out.iter()
                    .filter(|q| {
                        (i32::from(q.x)..i32::from(q.x) + i32::from(q.w)).contains(&40)
                            && (i32::from(q.y)..i32::from(q.y) + i32::from(q.h)).contains(&y)
                    })
                    .fold(0u32, |held, q| {
                        held + u32::from(q.alpha) * (255 - held) / 255
                    })
            })
            .collect()
    };
    let rise = (ch as f32 * 0.12).round() as i32;
    assert!(
        rise >= 2,
        "fixture: the rise is deeper than the cut ({rise})"
    );
    let kept = column(false);
    assert!(
        kept[top as usize] > 128,
        "the cut pixel row takes the veil: {kept:?}"
    );
    assert!(
        kept[(top - 1) as usize] > 0,
        "one pixel row above the cut feathers: {:?}",
        &kept[(top - rise - 1) as usize..=top as usize]
    );
    assert!(
        (top - rise..top - 1).all(|y| kept[y as usize] == 0),
        "no row above the feather takes the page: {:?}",
        &kept[(top - rise - 1) as usize..=top as usize]
    );
    let deep = column(true);
    assert!(
        deep[(top - 2) as usize] > 0,
        "control: feathered its whole height, a row above the paw's tip takes the page: {:?}",
        &deep[(top - rise - 1) as usize..=top as usize]
    );
}

/// Lane B5: the pet's veil lies beneath a published under stream's ramps
/// as beneath its quads ([`CursorGlow::veil_pet_letters`]): laid into a
/// stream of quads and ramps, a ramp drawn before every quad among them, it
/// draws what the same stream's quads draw with it, in the same order.
#[test]
fn the_pet_s_veil_lies_beneath_a_stream_s_ramps_as_beneath_its_quads() {
    use aterm_core::terminal::RenderCell;
    use aterm_render::glow_end;
    let g = geom();
    let (fg, page) = ([0x85u8, 0x85, 0x85], [0x11u8, 0x13, 0x18]);
    let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
    for (c, cell) in cells[3].iter_mut().enumerate() {
        cell.ch = if c < 20 { 'o' } else { ' ' };
        cell.fg = fg;
        cell.bg = page;
    }
    // The stream as published: two quads, a ramp before the first and
    // one between them.
    let mut quads = vec![
        GlowQuad::flat(3, 20, 40, 30, 6, 0x0030_2010, 0),
        GlowQuad::flat(3, 24, 60, 40, 4, 0x0010_2030, 0x40),
    ];
    let mut ramps = GlowRamps::default();
    for (at, x, y) in [(0, 18, 44), (1, 30, 50)] {
        ramps.open(
            at,
            3,
            (x, y, 36),
            (glow_end(0x0020_2020, 0), glow_end(0x0008_1018, 0)),
        );
        for k in 1..6u32 {
            ramps.extend_last(0, (glow_end(k * 0x0004_0602, 0), glow_end(0x0010_0804, 0)));
        }
    }
    let mut legacy = Vec::new();
    ramps.expand_into(&quads, &mut legacy);
    let veiled = |stream: (&mut Vec<GlowQuad>, &mut GlowRamps)| {
        let mut glow = CursorGlow::default();
        glow.observe_frame_text(Some((3, 20)), &cells, Instant::now());
        glow.note_pet_body(Some((16, 96, 24, 64)));
        glow.note_veil_frame(Some((3, 20)), g);
        glow.veil_pet_letters(stream, &[])
    };
    let laid = legacy.len();
    assert_ne!(
        veiled((&mut legacy, &mut GlowRamps::default())),
        0,
        "fixture: the veil is laid"
    );
    assert!(legacy.len() > laid, "fixture: the veil is laid");
    veiled((&mut quads, &mut ramps));
    let mut got = Vec::new();
    ramps.expand_into(&quads, &mut got);
    assert_eq!(
        got, legacy,
        "the veil lies beneath the ramps as beneath their quads"
    );
}

/// THE PET'S VEIL KEEPS TO ITS SILHOUETTE (looks7 M10): given the body's
/// sprite, each veil run keeps to the rows its art draws, each to that row's
/// drawn span padded 3 px, so the box's transparent margin takes no page (on
/// a translucent window a box of opaque page around the coat, on any window
/// a cut in the trail's light under the letters beside it), and every
/// lettered pixel over the coat still holds the bar. Negative control:
/// boxed, the page lies off the coat.
#[test]
fn the_pet_s_veil_keeps_to_its_silhouette() {
    use aterm_core::render::{FxArt, FxTexels, PxRect};
    use aterm_core::terminal::RenderCell;
    use aterm_render::sprites::{FreeSprite, FreeZ};
    let g = geom();
    let (fg, page) = ([0x85u8, 0x85, 0x85], [0x11u8, 0x13, 0x18]);
    let rgb = |c: [u8; 3]| (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2]);
    // The body over columns 2..12 of row 3 and the row above, its coat the
    // art's columns 10..50 (window x 26..66), the rest of its box clear.
    let (w, h) = (80u32, 40u32);
    let mut texels = vec![0u8; (w * h * 4) as usize];
    for r in 0..h {
        for c in 10..50 {
            texels[((r * w + c) * 4 + 3) as usize] = 255;
        }
    }
    let sprite = FreeSprite {
        x: 16,
        y: 24,
        w: w as u16,
        h: h as u16,
        art: std::sync::Arc::new(
            FxArt::new(w as u16, h as u16, FxTexels::Rgba8Straight, texels).expect("an 80x40 art"),
        ),
        clip: PxRect::EVERYWHERE,
        tint: 0x00FF_FFFF,
        alpha: 255,
        flip_x: false,
        z: FreeZ::UnderText,
    };
    let veil = |boxed: bool| {
        let mut cells = vec![vec![RenderCell::default(); g.cols]; g.rows];
        for (c, cell) in cells[3].iter_mut().enumerate() {
            cell.ch = if c < 20 && c % 5 != 4 { 'o' } else { ' ' };
            cell.fg = fg;
            cell.bg = page;
        }
        let mut glow = CursorGlow::default();
        glow.set_pet_veil_boxed(boxed);
        glow.observe_frame_text(Some((3, 20)), &cells, Instant::now());
        glow.note_pet_body(Some((16, 96, 24, 64)));
        glow.note_veil_frame(Some((3, 20)), g);
        let mut out = Vec::new();
        glow.veil_pet_letters(
            (&mut out, &mut GlowRamps::default()),
            std::slice::from_ref(&sprite),
        );
        out
    };
    // The coat (the ink's own colour) under the veil at window pixel (x, 56).
    let read = |quads: &[GlowQuad], x: i32| {
        let mut px = rgb(fg);
        for q in quads {
            let (qx, qy) = (i32::from(q.x), i32::from(q.y));
            if (qx..qx + i32::from(q.w)).contains(&x) && (qy..qy + i32::from(q.h)).contains(&56) {
                let a = aterm_render::glow_lerp(
                    u32::from(q.alpha),
                    u32::from(q.alpha2),
                    (x - qx) as u32,
                    u32::from(q.w),
                ) as u8;
                px = aterm_render::over_premul(px, aterm_render::premul_rgb(rgb(page), a), a)
                    & 0x00FF_FFFF;
            }
        }
        crate::color_math::contrast_ratio(rgb(fg), px)
    };
    let bare = crate::color_math::contrast_ratio(rgb(fg), rgb(page));
    let bar = 4.6f32.min(bare * 0.97);
    let off_coat = |quads: &[GlowQuad]| {
        quads
            .iter()
            .map(|q| {
                let (x0, x1) = (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
                (x0.max(16)..x1.min(96))
                    .filter(|&x| !(26 - 3..66 + 3).contains(&x))
                    .count()
                    * usize::from(q.h)
            })
            .sum::<usize>()
    };
    let kept = veil(false);
    assert!(!kept.is_empty(), "the pet on dim text draws a veil");
    assert_eq!(off_coat(&kept), 0, "the veil lays page off the coat");
    for x in 26..66i32 {
        let col = x / g.cw as i32;
        if col % 5 == 4 {
            continue;
        }
        let r = read(&kept, x);
        assert!(
            r >= bar,
            "column {col}'s pixel {} reads {r:.2}:1 over the coat",
            x - col * g.cw as i32
        );
    }
    assert!(
        off_coat(&veil(true)) > 0,
        "control: boxed, the veil lays no page off the coat"
    );
}

/// The word pass draws its icons after the trail, so with the frame's icons noted and shaded
/// (`note_word_icons`, `shade_word_icons`) no light of the frame lies on an icon above its foot,
/// even one new to the frame, on either page. Negative control: unshaded, the frame lights it.
#[test]
fn a_word_icon_new_to_the_frame_wears_no_trail_light() {
    let g = geom();
    // A face over cols 6..12 of row 2, its chin two pixels into row 3.
    let icon = (48, 96, 20, 50);
    let on_icon = |dark: bool, shade: bool| -> usize {
        let mut c = cfg_for_style_name("rainbow kitty pet", true);
        c.intensity = 1.0;
        if !dark {
            c.dark_theme = false;
            c.theme_fg = 0x001F_2328;
            c.theme_bg = 0x00FF_FFFF;
        }
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let t0 = Instant::now();
        glow.tick(Some((3, 1)), t0, &c, g, &mut out);
        let t = type_echoed(&mut glow, 3, 1, "the quick brown", t0, &c, g);
        glow.tick(Some((3, 16)), t + ms(20), &c, g, &mut out);
        glow.note_word_icons([icon]);
        // The streams as a host publishes them, before its word pass.
        let (mut under, mut halos) = (glow.under_quads().to_vec(), glow.halos().to_vec());
        let (mut out_ramps, mut under_ramps) =
            (glow.out_ramps().clone(), glow.under_ramps().clone());
        if shade {
            glow.shade_word_icons((
                (&mut out, &mut out_ramps),
                (&mut under, &mut under_ramps),
                &mut halos,
                &mut Vec::new(),
            ));
        }
        // Every light, each lit ramp row the quad it stands for.
        let mut lights = Vec::new();
        out_ramps.expand_into(&out, &mut lights);
        under_ramps.expand_into(&under, &mut lights);
        let hits = |(x0, x1, y0, y1): (i32, i32, i32, i32)| {
            x0 < icon.1 && icon.0 < x1 && y0 < icon.3 && icon.2 < y1
        };
        let rect = |x: u16, y: u16, w: u16, h: u16| {
            let (x, y) = (i32::from(x), i32::from(y));
            (x, x + i32::from(w), y, y + i32::from(h))
        };
        lights
            .iter()
            .filter(|q| q.w > 0 && q.h > 0 && hits(rect(q.x, q.y, q.w, q.h)))
            .count()
            + halos
                .iter()
                .filter(|h| h.w > 0 && h.h > 0 && hits(rect(h.x, h.y, h.w, h.h)))
                .count()
    };
    for dark in [true, false] {
        assert!(
            on_icon(dark, false) > 0,
            "fixture (dark {dark}): the unshaded frame lays light on the icon"
        );
        assert_eq!(
            on_icon(dark, true),
            0,
            "a light lies on the word icon above its foot (dark {dark})"
        );
    }
}
