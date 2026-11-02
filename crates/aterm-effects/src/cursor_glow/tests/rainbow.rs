// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow kitty ribbon.

use super::*;
use aterm_models_effects::{rainbow_occupied_coverage_model, rainbow_retract_rearm_model};

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
    let model = rainbow_occupied_coverage_model();
    let cap = model
        .consts
        .iter()
        .find(|(name, _)| *name == "Cap")
        .expect("Cap const")
        .1;
    let requested_max = model
        .consts
        .iter()
        .find(|(name, _)| *name == "RequestedMax")
        .expect("RequestedMax const")
        .1;
    assert_eq!(
        crate::rainbow_kitty::ribbon::UNDER_COV_CAP as i64,
        cap,
        "RainbowOccupiedCoverage::Cap must equal ribbon::UNDER_COV_CAP"
    );
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);

    let v2_cfg =
        crate::rainbow_kitty::Config::from_glow(&cfg_for_style_name("rainbow kitty", true), false);
    let mut state = model.init_state();
    assert_eq!(
        crate::rainbow_kitty::ribbon::clamp_bed_cov(&v2_cfg, false, 0.0).round() as i64,
        state["emitted"]
    );
    let mut caught_unclamped = 0usize;
    for req in 1..=requested_max {
        assert!(model.fire("Raise", &mut state), "Raise at {req}");
        let actual =
            crate::rainbow_kitty::ribbon::clamp_bed_cov(&v2_cfg, false, req as f32).round() as i64;
        assert_eq!(actual, state["emitted"], "requested coverage {req}");
        assert!(model.check_invariant("OccupiedBodyBounded", &state));
        let glass_actual =
            crate::rainbow_kitty::ribbon::clamp_bed_cov(&v2_cfg, true, req as f32).round() as i64;
        assert_eq!(glass_actual, req.min(255), "glass requested coverage {req}");
        if req > cap {
            let mut unclamped = state.clone();
            unclamped.insert("emitted", req);
            assert!(
                !model.check_invariant("OccupiedBodyBounded", &unclamped),
                "negative control: unclamped request {req} must break OccupiedBodyBounded"
            );
            caught_unclamped += 1;
        }
    }
    assert!(caught_unclamped > 0);
}

#[test]
fn rainbow_retract_rearm_model_proves_and_catches_brightness_snap() {
    use crate::rainbow_kitty::{
        Config as V2Config, Ctx as V2Ctx, Dir, Event, Flow, Licence, ribbon::Ribbon,
        timing::EDGE_IN_S,
    };

    let model = rainbow_retract_rearm_model();
    aterm_spec::verify::prove_and_catch_scalar(&model, model.name);

    let peak = model
        .consts
        .iter()
        .find(|(name, _)| *name == "Peak")
        .expect("Peak const")
        .1 as usize;
    let g = retina_geom();
    let v2_cfg = V2Config::from_glow(&cfg_for_style_name("rainbow kitty", true), false);
    let ctx_at = |now: Instant, caret: (u16, u16)| V2Ctx {
        now,
        geom: g,
        cfg: &v2_cfg,
        disp: 1.0,
        birth_disp: 1.0,
        phase: 0.0,
        caret,
        caret_t: 0.0,
        caret_walk: None,
        mend: None,
        surge: 0.0,
        flow: Flow::EMPTY,
    };

    let mut caught_snap = 0usize;
    for fade_steps in 0..=peak {
        let mut state = model.init_state();
        let mut ribbon = Ribbon::new();
        let t0 = Instant::now();
        ribbon.on_event(
            &Event::Move {
                from: (2, 2),
                to: (2, 10),
                licence: Licence::Nav,
                dir: Dir::Right,
            },
            t0,
            &ctx_at(t0, (2, 10)),
        );
        let mut now = t0 + Duration::from_millis(150);
        ribbon.plan(&ctx_at(now, (2, 10)));

        // Abandon the wake cohort on row 2 while staying on row 2 so its cells enter retract.
        assert!(model.fire("BeginFade", &mut state));
        ribbon.on_event(
            &Event::Move {
                from: (2, 10),
                to: (2, 20),
                licence: Licence::Pty,
                dir: Dir::Right,
            },
            now,
            &ctx_at(now, (2, 20)),
        );
        ribbon.plan(&ctx_at(now, (2, 20)));

        let probe_env = |r: &Ribbon, now: Instant, caret: (u16, u16)| -> f32 {
            let ctx = ctx_at(now, caret);
            let cell = r
                .cells()
                .iter()
                .find(|c| c.row == 2 && c.col == 2)
                .expect("probed cell (2, 2) stays live through fade and re-arm");
            r.debug_cell_envelope(&ctx, cell)
        };

        let mut prev_env = probe_env(&ribbon, now, (2, 20));
        for step_idx in 0..fade_steps {
            assert!(model.fire("Fade", &mut state));
            now += Duration::from_millis(if step_idx == 0 { 105 } else { 45 });
            ribbon.plan(&ctx_at(now, (2, 20)));
            let cur_env = probe_env(&ribbon, now, (2, 20));
            assert!(
                cur_env < prev_env,
                "fade step {step_idx} must lower envelope: {cur_env} vs {prev_env}"
            );
            prev_env = cur_env;
            assert!(model.check_invariant("VisibilityBounded", &state));
            assert!(model.check_invariant("ReversalContinuous", &state));
        }

        // Reclaim every fading wake cell on 2..=10 at the exact reversal instant `now`.
        let ctx_before = ctx_at(now, (2, 20));
        let before: Vec<(u16, f32)> = ribbon
            .cells()
            .iter()
            .filter(|c| c.row == 2 && (2..=9).contains(&c.col))
            .map(|c| (c.col, ribbon.debug_cell_envelope(&ctx_before, c)))
            .collect();
        assert!(!before.is_empty());

        assert!(model.fire("Reclaim", &mut state));
        ribbon.on_event(
            &Event::Move {
                from: (2, 20),
                to: (2, 2),
                licence: Licence::Nav,
                dir: Dir::Left,
            },
            now,
            &ctx_at(now, (2, 2)),
        );
        ribbon.plan(&ctx_at(now, (2, 2)));
        let ctx_after = ctx_at(now, (2, 2));
        let mut any_jump = false;
        for &(col, env_before) in &before {
            let cell = ribbon
                .cells()
                .iter()
                .find(|c| c.row == 2 && c.col == col)
                .unwrap_or_else(|| panic!("reclaimed cell (2, {col}) missing"));
            let env_after = ribbon.debug_cell_envelope(&ctx_after, cell);
            if (env_after - env_before).abs() >= 1e-5 {
                any_jump = true;
            }
            assert!(
                (env_after - env_before).abs() < 1e-5,
                "col {col} at fade_steps={fade_steps}: envelope jumped {env_before} -> {env_after}"
            );
        }
        assert_eq!(i64::from(any_jump), state["discontinuity"]);
        assert!(model.check_invariant("VisibilityBounded", &state));
        assert!(model.check_invariant("ReversalContinuous", &state));

        if fade_steps > 0 {
            let env_before = before[0].1;
            assert!(
                (1.0 - env_before).abs() > 1e-3,
                "faded envelope must sit strictly below peak before reclaim"
            );
            let mut snapped = state.clone();
            snapped.insert("discontinuity", 1);
            assert!(
                !model.check_invariant("ReversalContinuous", &snapped),
                "negative control: snapping faded envelope to peak breaks ReversalContinuous"
            );
            caught_snap += 1;
        }

        let reclaim_at = now;
        let rearm_dt = Duration::from_secs_f32(EDGE_IN_S / (peak as f32));
        let mut last_rearm_env = probe_env(&ribbon, now, (2, 2));
        while (state["visibility"] as usize) < peak {
            assert!(model.fire("Rearm", &mut state));
            now += rearm_dt;
            ribbon.plan(&ctx_at(now, (2, 2)));
            let cur_env = probe_env(&ribbon, now, (2, 2));
            assert!(
                cur_env + 1e-6 >= last_rearm_env,
                "re-arm envelope must climb monotonically: {cur_env} < {last_rearm_env}"
            );
            last_rearm_env = cur_env;
            assert!(model.check_invariant("VisibilityBounded", &state));
            assert!(model.check_invariant("ReversalContinuous", &state));
        }
        now = reclaim_at + Duration::from_secs_f32(EDGE_IN_S + 0.01);
        ribbon.plan(&ctx_at(now, (2, 2)));
        let settled = probe_env(&ribbon, now, (2, 2));
        assert!(
            (settled - 1.0).abs() < 1e-4,
            "completed re-arm returns the cell envelope to full strength (got {settled} at fade_steps={fade_steps})"
        );
    }
    assert!(caught_snap > 0);
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
                            light: crate::cursor_glow::Light::OFF,
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
    let expected: Vec<u16> = (2..6).collect();
    assert!(
        !expected.is_empty(),
        "the paid span must contain glyph cells"
    );
    let cols = v2_cols(&glow, 3);
    assert_eq!(
        cols.iter().copied().collect::<Vec<_>>(),
        expected,
        "the V2 resident field covers exactly the four paid cells"
    );
    assert!(
        !cols.contains(&6),
        "the landing is the caret's cell, not a glyph's: {cols:?}"
    );
    // This observer must fail when the actual V2 field disappears even if the
    // legacy sparks remain. Resident-field coverage is not emitted-pixel coverage.
    let legacy = glow.sparks.len();
    glow.v2.reset();
    assert_eq!(glow.sparks.len(), legacy);
    assert_eq!(
        dark_in(&glow, 3, 2..6),
        expected,
        "negative control: resetting V2 loses the entire nonempty paid span"
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

/// A CLEARED SCREEN TAKES THE TRAIL'S LIGHTS WITH ITS LETTERS (looks8 K7):
/// a line typed, Enter, and 300 ms on the screen is cleared (`clear`, Ctrl-L)
/// and the caret leaps to its top. The host's content-scroll snapshot counts
/// the clear, and from the frame it lands no light of the old screen (a
/// ribbon's bed and Enter tracer, a sparkle's dust, fire's heat) lies below
/// the top rows the caret's own light reaches. Negative control: the same
/// frames without the clear keep them floating on the empty screen.
#[test]
fn a_primary_screen_clear_drops_the_lights_the_old_screen_carried() {
    use aterm_core::terminal::ContentScrollState;
    let g = geom();
    let below_top = |glow: &CursorGlow, out: &[GlowQuad]| {
        let floor = 2 * g.ch as i32 + i32::from(g.origin_y);
        let lit = |c: u32| c & 0x00FF_FFFF != 0;
        out.iter()
            .chain(glow.under_quads())
            .filter(|q| q.w > 0 && q.h > 0 && (lit(q.color) || lit(q.color2)))
            .filter(|q| i32::from(q.y) + i32::from(q.h) > floor)
            .count()
            + glow
                .halos()
                .iter()
                .filter(|h| h.w > 0 && h.h > 0 && lit(h.color))
                .filter(|h| i32::from(h.y) + i32::from(h.h) > floor)
                .count()
    };
    for style in [GlowStyle::RainbowKitty, GlowStyle::Sparkle, GlowStyle::Fire] {
        let c = cfg(style, true);
        let run = |cleared: bool| {
            let mut pipe = crate::pipeline::EffectsPipeline::new();
            let base = ContentScrollState::default();
            let t0 = Instant::now();
            let _ = pipe.sync_scroll(base, t0);
            let glow = &mut pipe.cursor_mut().glow;
            let mut out = Vec::new();
            glow.tick(Some((3, 2)), t0, &c, g, &mut out);
            let t = type_echoed(glow, 3, 2, "echo quick fox", t0 + ms(16), &c, g);
            let enter = t + ms(30);
            glow.note_synthetic_move(enter);
            glow.tick(Some((4, 2)), enter, &c, g, &mut out);
            for k in 1..=18u64 {
                out.clear();
                glow.tick(Some((4, 2)), enter + ms(16 * k), &c, g, &mut out);
            }
            let clear = enter + ms(300);
            let snapshot = ContentScrollState {
                screen_clears: u64::from(cleared),
                ..base
            };
            let _ = pipe.sync_scroll(snapshot, clear);
            let glow = &mut pipe.cursor_mut().glow;
            let mut worst = 0;
            for k in 0..=4u64 {
                out.clear();
                glow.tick(Some((0, 2)), clear + ms(16 * k), &c, g, &mut out);
                worst = worst.max(below_top(glow, &out));
            }
            worst
        };
        assert_eq!(
            run(true),
            0,
            "{style:?}: light of the cleared screen floats below its top"
        );
        assert!(
            run(false) > 0,
            "control: {style:?} without the clear keeps no light below the top"
        );
    }
}

/// Classic keeps its ruled face through a clear (looks8 K7): its tracer runs
/// to the caret, and the frames after the clear are the frames without it.
#[test]
fn a_primary_screen_clear_leaves_classic_as_it_was() {
    use aterm_core::terminal::ContentScrollState;
    let g = geom();
    let c = cfg(GlowStyle::Classic, true);
    let run = |cleared: bool| {
        let mut pipe = crate::pipeline::EffectsPipeline::new();
        let base = ContentScrollState::default();
        let t0 = Instant::now();
        let _ = pipe.sync_scroll(base, t0);
        let glow = &mut pipe.cursor_mut().glow;
        let mut out = Vec::new();
        glow.tick(Some((3, 2)), t0, &c, g, &mut out);
        let t = type_echoed(glow, 3, 2, "echo quick fox", t0 + ms(16), &c, g);
        glow.tick(Some((4, 2)), t + ms(30), &c, g, &mut out);
        let clear = t + ms(330);
        let snapshot = ContentScrollState {
            screen_clears: u64::from(cleared),
            ..base
        };
        let _ = pipe.sync_scroll(snapshot, clear);
        let glow = &mut pipe.cursor_mut().glow;
        let mut frames = Vec::new();
        for k in 0..=4u64 {
            out.clear();
            glow.tick(Some((0, 2)), clear + ms(16 * k), &c, g, &mut out);
            frames.push((out.clone(), glow.under_quads().to_vec()));
        }
        frames
    };
    let (kept, plain) = (run(true), run(false));
    assert!(
        plain.iter().any(|(o, u)| !o.is_empty() || !u.is_empty()),
        "fixture: Classic draws after the leap"
    );
    assert_eq!(kept, plain, "a clear changed Classic's frames");
}

/// A COMPOSER'S SKY UNDER ITS RULE (Prism Glass P0.11): Claude Code's
/// composer stands one row under a `─` rule. The sky births its stars in
/// that row, under the rule's stroke (stardust's
/// `a_light_rule_over_the_typing_row_keeps_the_sky_alive`), but the rule's
/// cells are lettered cells of a row the caret is not on, so the frame's
/// keep-out (`CursorGlow::keep_out_boxes`) holds each of them off the glass:
/// on either page no sky star is painted there, nor anywhere else, while the
/// sky keeps them. Controls: with the rule left out of the frame's text (the
/// probe still names it) the same stars are painted under it, and under a
/// blank row the same keys paint stars in it.
#[test]
fn no_composer_sky_star_is_painted_under_a_rule_on_the_row_above() {
    use aterm_core::terminal::RenderCell;
    let g = Geom {
        cw: 15,
        ch: 28,
        rows: 8,
        cols: 80,
        origin_x: 0,
        origin_y: 0,
        win_w: 1200,
        win_h: 224,
        head: 0,
    };
    let (rule_row, row) = (4u16, 5u16);
    let text = "the quick brown fox jumps over the lazy dog, typed into the box";
    // The stars painted in the rule's row from its stroke's top down, the
    // stars painted anywhere, and the most the sky held in a frame; `kept`
    // hands the rule to the frame's text, which the keep-out is read from.
    let run = |dark: bool, ruled: bool, kept: bool| -> (usize, usize, u32) {
        let mut c = cfg(GlowStyle::RainbowKitty, true);
        if !dark {
            c.dark_theme = false;
            c.theme_fg = 0x002B_2D33;
            c.theme_bg = 0x00F6_F7F9;
        }
        let rgb = |v: u32| [(v >> 16) as u8, (v >> 8) as u8, v as u8];
        let blank = RenderCell {
            fg: rgb(c.theme_fg),
            bg: rgb(c.theme_bg),
            ..RenderCell::default()
        };
        let mut cells = vec![vec![blank; g.cols]; g.rows];
        if ruled {
            for cell in &mut cells[usize::from(rule_row)] {
                cell.ch = '─';
            }
        }
        cells[usize::from(row)][0].ch = '>';
        let chars = |cells: &[Vec<RenderCell>], r: u16| -> Vec<char> {
            cells[usize::from(r)].iter().map(|cell| cell.ch).collect()
        };
        let y0 = i32::from(g.origin_y) + i32::from(rule_row) * g.ch as i32;
        let stroke_top = y0 + (rk::stardust::RULE_INK_TOP_CH * g.ch as f32).floor() as i32;
        let y1 = y0 + g.ch as i32;
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        let mut t = Instant::now();
        let (mut under_rule, mut painted, mut sky) = (0usize, 0usize, 0u32);
        for (k, ch) in text.chars().enumerate() {
            let col = 2 + k as u16;
            t += ms(100);
            let class = if ch == ' ' {
                rk::TypedClass::Space
            } else {
                rk::TypedClass::Glyph
            };
            glow.note_typed_glyph(t, 1, false, class);
            cells[usize::from(row)][usize::from(col)].ch = ch;
            let caret = (row, col + 1);
            for dt in [3u64, 36, 69] {
                let now = t + ms(dt);
                let mut framed = cells.clone();
                if !kept {
                    framed[usize::from(rule_row)].fill(blank);
                }
                glow.observe_frame_text(Some(caret), &framed, now);
                glow.observe_row(row, caret.1, &chars(&cells, row), now);
                glow.observe_neighbor_rows(
                    Some(&chars(&cells, rule_row)),
                    Some(&chars(&cells, row + 1)),
                );
                glow.tick(Some(caret), now, &c, g, &mut out);
                sky = sky.max(glow.v2_status().map_or(0, |s| s.stars));
                let marks = glow.v2.sky_marks();
                painted += marks.len();
                under_rule += marks
                    .iter()
                    .filter(|m| m.rect.2 < y1 && m.rect.3 > stroke_top)
                    .count();
            }
        }
        (under_rule, painted, sky)
    };
    for dark in [true, false] {
        let page = if dark {
            "dark"
        } else {
            "light (main's paper arm)"
        };
        let (under_rule, painted, sky) = run(dark, true, true);
        assert!(
            sky > 0,
            "fixture ({page}): the sky over the composer holds no star"
        );
        assert_eq!(
            under_rule, 0,
            "{page}: {under_rule} sky stars painted under the composer's rule"
        );
        assert_eq!(
            painted, 0,
            "{page}: {painted} sky stars painted while the composer stands under a rule"
        );
        let (unkept, ..) = run(dark, true, false);
        assert!(
            unkept > 0,
            "control ({page}): with the rule out of the frame's text no star is painted under it"
        );
        let (blank, ..) = run(dark, false, true);
        assert!(
            blank > 0,
            "control ({page}): under a blank row no sky star is painted in it"
        );
    }
}
