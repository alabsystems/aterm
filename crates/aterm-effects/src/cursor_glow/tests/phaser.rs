// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The phaser band and coalesced echo sweeps.

use super::*;

#[test]
fn phaser_band_spans_three_letters_and_never_parks() {
    let g = geom();
    let c = cfg(GlowStyle::Phaser, true);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut t = t0;
    glow.tick(Some((2, 0)), t, &c, g, &mut out);
    for i in 1..=12u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, i)), t, &c, g, &mut out);
    }
    for col in 9..=11u16 {
        assert!(
            glow.sparks.iter().any(|s| {
                s.row == 2
                    && s.col == col
                    && t.saturating_duration_since(s.born).as_secs_f32() < s.life
            }),
            "cell {col} inside the three-letter band is lit"
        );
    }
    t += Duration::from_secs(2);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 13)), t, &c, g, &mut out);
    let head = glow
        .sparks
        .iter()
        .rev()
        .find(|s| s.row == 2 && s.col == 12)
        .expect("the lone key laid its spark");
    assert!(
        head.life < 1.0,
        "a post-pause key fades crisply, got {}s",
        head.life
    );
}

#[test]
fn wide_coalesced_echo_still_sweeps_every_cell() {
    let g = geom();
    let c = cfg(GlowStyle::Phaser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 4)), t0, &c, g, &mut out);
    glow.note_synthetic_typed(t0 + Duration::from_millis(6), 1);
    glow.tick(Some((2, 5)), t0 + Duration::from_millis(8), &c, g, &mut out);
    glow.note_synthetic_typed(t0 + Duration::from_millis(14), 6);
    glow.tick(
        Some((2, 11)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    for col in 5..=10u16 {
        let s = glow
            .sparks
            .iter()
            .find(|s| s.row == 2 && s.col == col)
            .unwrap_or_else(|| panic!("swept cell {col} got its spark (no notch)"));
        assert!(s.typing, "cell {col} is a TYPING spark, not a jump");
    }
    assert!(glow.flare < 0.05, "no flare slam on a wide coalesced echo");
    assert!(
        glow.ring.is_none(),
        "no landing ring on a wide coalesced echo"
    );
}

#[test]
fn wrap_echo_continues_the_band_across_the_fold() {
    let g = geom(); // cols: 40
    let c = cfg(GlowStyle::Phaser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 36)), t0, &c, g, &mut out);
    glow.note_synthetic_typed(t0 + Duration::from_millis(6), 1);
    glow.tick(
        Some((2, 37)),
        t0 + Duration::from_millis(8),
        &c,
        g,
        &mut out,
    );
    glow.note_synthetic_typed(t0 + Duration::from_millis(14), 5);
    glow.tick(
        Some((3, 2)),
        t0 + Duration::from_millis(16),
        &c,
        g,
        &mut out,
    );
    for (row, col) in [(2u16, 37u16), (2, 38), (2, 39), (3, 0), (3, 1)] {
        assert!(
            glow.sparks
                .iter()
                .any(|s| s.row == row && s.col == col && s.typing),
            "the fold's typed cell ({row},{col}) is lit"
        );
    }
    assert!(glow.ring.is_none(), "a typing wrap fires no landing ring");
    assert!(
        glow.fire_meteors.is_empty(),
        "a typing wrap flies no meteor for any style"
    );
    let tail = glow
        .sparks
        .iter()
        .find(|s| s.row == 2 && s.col == 36)
        .expect("the pre-fold spark is still resident");
    let age = (t0 + Duration::from_millis(16))
        .saturating_duration_since(tail.born)
        .as_secs_f32();
    assert!(
        tail.life - age > 0.05,
        "the old row fades on its own life, not a snuff clamp"
    );
}

#[test]
fn vertical_navigation_timestamps_stay_dark() {
    let g = geom();
    let c = cfg(GlowStyle::Lumen, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 10)), t0, &c, g, &mut out);
    let mut t = t0;
    for row in 3..=5u16 {
        t += Duration::from_millis(35);
        glow.note_navigation(t);
        glow.tick(
            Some((row, 10)),
            t + Duration::from_millis(2),
            &c,
            g,
            &mut out,
        );
    }
    assert_eq!(glow.heat, 0.0, "held vertical arrows earn no heat");
    assert!(glow.sparks.is_empty(), "nav scrubbing lays no wake");
    assert!(glow.flare < 0.05, "nav scrubbing never slams the flare");
}

#[test]
fn phaser_burst_origin_cannot_outlive_the_band() {
    let g = geom();
    let c = cfg(GlowStyle::Phaser, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    let mut t = t0 + Duration::from_millis(5);
    glow.note_synthetic_typed(t, 1);
    glow.tick(Some((2, 1)), t, &c, g, &mut out);
    let first_life = glow.sparks.iter().find(|s| s.col == 0).unwrap().life;
    assert!(first_life >= 0.40, "a lone first key keeps the base life");
    for col in 2..=8u16 {
        t += Duration::from_millis(8);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    let origin = glow
        .sparks
        .iter()
        .find(|s| s.col == 0)
        .expect("origin spark still resident");
    let age = t.saturating_duration_since(origin.born).as_secs_f32();
    assert!(
        origin.life - age
            <= CursorGlow::PHASER_CHAIN_KEYS * 0.008 + CursorGlow::CHAIN_MARGIN + 1e-3,
        "the origin's lone-key bet is voided by the burst (remaining {})",
        origin.life - age
    );
}

#[test]
fn light_theme_band_is_ink_not_additive() {
    let g = geom();
    let mut c = cfg(GlowStyle::Phaser, true);
    c.dark_theme = false;
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 4)), t0, &c, g, &mut out);
    let mut t = t0;
    for col in 5..=8u16 {
        t += Duration::from_millis(50);
        glow.note_synthetic_typed(t, 1);
        glow.tick(Some((2, col)), t, &c, g, &mut out);
    }
    assert!(
        out.is_empty(),
        "no additive quads on a light theme (band, crown, ring all ink/skip)"
    );
    assert!(
        !glow.under_quads().is_empty() && glow.under_quads().iter().all(|q| q.alpha > 0),
        "the light-theme band renders as source-over ink under the letters"
    );
    assert!(
        glow.halos().iter().all(|h| h.mode == HaloMode::Over),
        "no additive halos sneak in on light"
    );
}

#[test]
fn disabled_or_zero_intensity_emits_nothing() {
    let g = geom();
    let mut glow = CursorGlow::default();
    let now = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), now, &cfg(GlowStyle::Fire, false), g, &mut out);
    let fp = glow.tick(
        Some((2, 30)),
        now,
        &cfg(GlowStyle::Fire, false),
        g,
        &mut out,
    );
    assert_eq!(fp, 0);
    assert!(out.is_empty());
    let mut c = cfg(GlowStyle::Lumen, true);
    c.intensity = 0.0;
    glow.tick(Some((2, 0)), now, &c, g, &mut out);
    let fp = glow.tick(Some((2, 30)), now, &c, g, &mut out);
    assert_eq!(fp, 0);
}

/// The paper Phaser on GitHub Light (the gallery's 13x26 cell, row 4): `keys`
/// keys typed from col 2 at 60 ms, then `erase` Backspaces at 120 ms, each
/// tick at its key; its lights under the letters alone on the page, and the
/// caret's column.
fn paper_phaser(keys: u16, erase: u16, setup: &dyn Fn(&mut CursorGlow)) -> (Glass, u16) {
    paper_phaser_at(keys, erase, 120, setup)
}

fn paper_phaser_at(
    keys: u16,
    erase: u16,
    erase_ms: u64,
    setup: &dyn Fn(&mut CursorGlow),
) -> (Glass, u16) {
    let g = tall_geom();
    let pair = (0x001F_2328u32, 0x00FF_FFFFu32);
    let (row, start) = (4u16, 2u16);
    let mut c = cfg(GlowStyle::Phaser, true);
    (c.intensity, c.dark_theme) = (1.0, false);
    (c.theme_fg, c.theme_bg) = pair;
    let mut glow = CursorGlow::default();
    setup(&mut glow);
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    let mut t = Instant::now();
    let mut col = start;
    glow.observe_row(row, col, &text, t);
    glow.tick(Some((row, col)), t, &c, g, &mut out);
    for _ in 0..keys {
        t += Duration::from_millis(60);
        text[usize::from(col)] = 'm';
        col += 1;
        glow.note_synthetic_typed(t, 1);
        glow.observe_row(row, col, &text, t);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
    }
    for _ in 0..erase {
        t += Duration::from_millis(erase_ms);
        col -= 1;
        text[usize::from(col)] = ' ';
        glow.note_backspace(t);
        glow.observe_row(row, col, &text, t);
        glow.tick(Some((row, col)), t, &c, g, &mut out);
    }
    let mut glass = Glass::new(g, pair.1);
    glass.add_quads(glow.under_quads());
    (glass, col)
}

/// How far the page at (`y`, `x`) is darkened: its deepest channel's fall
/// from white.
fn paper_fall(glass: &Glass, y: i32, x: i32) -> i32 {
    let p = glass.at(y, x);
    [16u32, 8, 0]
        .iter()
        .map(|&sh| 255 - ((p >> sh) & 0xff) as i32)
        .max()
        .unwrap_or(0)
}

/// after-land L4, THE PAPER PHASER IS A NEON TUBE: on both paper themes the
/// phaser's bar was a pastel pink-green smear over the last word
/// (`phaser-light.t0`, its band's saturation 0.3-0.6, its edges melting
/// over half its height). Typing on GitHub Light, along the row's middle
/// behind the caret the band's lit pixels wear their spectrum at a mean
/// saturation of 0.65 or more (0.68 since its spectrum runs one way round,
/// [`the_paper_phaser_s_spectrum_runs_one_way_round`]: the wrong-way trip
/// through the violets read higher), and its edge is crisp: above its deepest
/// column the page goes from untouched to nine tenths of that column's depth
/// within 2 px. Negative control: laid as the composed land laid it
/// (`phaser_paper_pastel`), the mean is under 0.55 and the edge spreads
/// over 4 px or more.
#[test]
fn the_paper_phaser_is_a_neon_tube() {
    let (g, row) = (tall_geom(), 4i32);
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let read = |setup: &dyn Fn(&mut CursorGlow)| {
        let (glass, caret) = paper_phaser(12, 0, setup);
        let (x0, x1) = ((i32::from(caret) - 7) * cw, i32::from(caret) * cw);
        let mid = row * ch + ch / 2;
        let lit: Vec<f32> = (x0..x1)
            .filter(|&x| paper_fall(&glass, mid, x) >= 24)
            .map(|x| crate::color_math::rgb2hsv(glass.at(mid, x)).1)
            .collect();
        let sat = lit.iter().sum::<f32>() / lit.len().max(1) as f32;
        let x = (x0..x1)
            .max_by_key(|&x| paper_fall(&glass, mid, x))
            .expect("seven cells");
        let peak = paper_fall(&glass, mid, x);
        let first = |test: &dyn Fn(i32) -> bool| (row * ch..=mid).find(|&y| test(y)).unwrap_or(mid);
        let edge = first(&|y| paper_fall(&glass, y, x) * 10 >= peak * 9)
            - first(&|y| paper_fall(&glass, y, x) > 0);
        (sat, edge, peak, lit.len())
    };
    let (sat, edge, peak, lit) = read(&|_| {});
    let (was_sat, was_edge, _, _) = read(&|glow| glow.phaser_paper_pastel = true);
    assert!(
        peak >= 48 && lit >= 3 * cw as usize,
        "the tube darkens {lit} px of the row's middle by up to {peak}"
    );
    assert!(sat >= 0.65, "the tube's mean saturation is {sat:.2}");
    assert!(edge <= 2, "the tube's edge spreads over {edge} px");
    assert!(
        was_sat < 0.55 && was_edge >= 4,
        "control: the composed land's band reads saturation {was_sat:.2}, edge {was_edge} px"
    );
}

/// after-land L4, THE PAPER PHASER'S BAND STAYS LIT TO THE CARET: after
/// Backspace on paper the band over the erased cells faded in toward the
/// caret, so it read detached (`phaser-light-steam.bs06`, x308-330:
/// saturation 0.07-0.20 beside the caret, 0.37 at the far end). Backspace
/// folds the run back over itself, and the ease in at its tail, measured
/// across the row from its first sample, dimmed its way back where it came
/// near that sample. Twenty-four keys typed, then six Backspaces at 150 ms,
/// on GitHub Light: at the row's middle the half cell right of the caret is
/// darkened on average to two fifths or more of the deepest half cell over
/// the four cells right of it (the spectrum's inks fall from white by their
/// hue). Negative control: eased across the row as the composed land eased
/// it (`phaser_band_detached`), that half cell reaches under a sixth of it.
#[test]
fn the_paper_phaser_s_band_stays_lit_to_the_caret() {
    let (g, row) = (tall_geom(), 4i32);
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let read = |setup: &dyn Fn(&mut CursorGlow)| {
        let (glass, caret) = paper_phaser_at(24, 6, 150, setup);
        let mid = row * ch + ch / 2;
        let fall = |x0: i32, x1: i32| {
            (x0..x1).map(|x| paper_fall(&glass, mid, x)).sum::<i32>() / (x1 - x0)
        };
        let next = (i32::from(caret) + 1) * cw;
        let deepest = (0..8)
            .map(|k| fall(next + k * cw / 2, next + (k + 1) * cw / 2))
            .max()
            .unwrap_or(0);
        (fall(next, next + cw / 2), deepest)
    };
    let (next, deepest) = read(&|_| {});
    let (was_next, was_deepest) = read(&|glow| glow.phaser_band_detached = true);
    assert!(
        deepest >= 48,
        "the band over the erased cells darkens the page by {deepest}"
    );
    assert!(
        next * 5 >= deepest * 2,
        "the half cell beside the caret is darkened {next} of the band's {deepest}"
    );
    assert!(
        was_next * 6 < was_deepest,
        "control: eased across the row, the half cell beside the caret is darkened {was_next} of {was_deepest}"
    );
}

/// after-land B7, THE PAPER PHASER'S TUBE TURNS THROUGH YELLOW: its wheel stepped over the hues a
/// body darkened to paper's bar lays as olive, but the tube lays each colour as itself, so the
/// step was only a hard orange-to-green edge in a spectrum that otherwise turns smoothly
/// (`phaser-light.t0`, x316-324). Twelve keys typed on GitHub Light (a whole wheel lies in the
/// last seven cells): along the row's middle behind the caret, no lit, saturated pixel's hue lies
/// more than 8 degrees round from its neighbour's, and a cell's width of them wears the hues the
/// step left out. Negative control: on the stepping wheel (`phaser_tube_steps_hues`) a step passes
/// 9 degrees and none wears those hues.
#[test]
fn the_paper_phaser_s_tube_turns_through_yellow() {
    let (g, row) = (tall_geom(), 4i32);
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let (b0, b1) = crate::color_math::LIGHT_MUDDY_HUES;
    let read = |setup: &dyn Fn(&mut CursorGlow)| {
        let (glass, caret) = paper_phaser(12, 0, setup);
        let (x0, x1) = ((i32::from(caret) - 7) * cw, i32::from(caret) * cw);
        let mid = row * ch + ch / 2;
        let hue = |x: i32| {
            let (h, s, _) = crate::color_math::rgb2hsv(glass.at(mid, x));
            (paper_fall(&glass, mid, x) >= 24 && s >= 0.2).then_some(h)
        };
        let (mut step, mut left_out) = (0.0f32, 0usize);
        for x in x0..x1 {
            let Some(a) = hue(x) else { continue };
            left_out += usize::from((b0 * 360.0..b1 * 360.0).contains(&a));
            if let Some(b) = hue(x + 1) {
                step = step.max(((b - a + 540.0).rem_euclid(360.0) - 180.0).abs());
            }
        }
        (step, left_out)
    };
    let (step, left_out) = read(&|_| {});
    let (was_step, was_left_out) = read(&|glow| glow.phaser_tube_steps_hues = true);
    assert!(
        step <= 8.0,
        "a pixel's hue lies {step:.1} degrees from its neighbour's"
    );
    assert!(
        left_out >= cw as usize,
        "{left_out} px of the tube wear the hues the step left out"
    );
    assert!(
        was_step >= 9.0 && was_left_out == 0,
        "control: on the stepping wheel the largest step is {was_step:.1} degrees, {was_left_out} \
         px in the band"
    );
}

/// The paper Phaser with `keys` wide glyphs typed from col 4 at 60 ms, and its text's pair.
fn paper_phaser_wide(
    keys: u16,
    setup: &dyn Fn(&mut CursorGlow),
) -> (Glass, (i32, i32), (u32, u32)) {
    let g = tall_geom();
    let pair = (0x001F_2328u32, 0x00FF_FFFFu32);
    let (row, start) = (4u16, 4u16);
    let mut c = cfg(GlowStyle::Phaser, true);
    (c.intensity, c.dark_theme) = (1.0, false);
    (c.theme_fg, c.theme_bg) = pair;
    let mut glow = CursorGlow::default();
    setup(&mut glow);
    let mut out = Vec::new();
    let mut text = vec![' '; g.cols];
    let mut t = Instant::now();
    glow.observe_row(row, start, &text, t);
    glow.tick(Some((row, start)), t, &c, g, &mut out);
    for k in 0..keys {
        t += Duration::from_millis(60);
        let col = usize::from(start + 2 * k);
        text[col] = '你';
        text[col + 1] = '\0';
        let caret = start + 2 * (k + 1);
        glow.note_synthetic_typed(t, 2);
        glow.observe_row(row, caret, &text, t);
        glow.tick(Some((row, caret)), t, &c, g, &mut out);
    }
    let mut glass = Glass::new(g, pair.1);
    glass.add_quads(glow.under_quads());
    let cw = g.cw as i32;
    (
        glass,
        (i32::from(start) * cw, i32::from(start + 2 * keys) * cw),
        pair,
    )
}

/// after-land B7, OVER WIDE GLYPHS THE PAPER PHASER STAYS A TUBE: held as a soft body's band is
/// held, it lay half pastel over CJK by the caret, saturation 0.14-0.25 against the 0.68-0.86 of
/// the narrow cells two back, with a hard edge where the hold began (`phaser-light-cjk.t0`,
/// x156-222). Six wide glyphs typed on GitHub Light: along the row's middle the lit pixels wear a
/// mean saturation of 0.55 or more, and no pixel of the tube leaves the text under
/// [`CursorGlow::PHASER_PAPER_WIDE_KEEP`] of its clean contrast. Negative control:
/// `phaser_tube_wide_pastel` wears under 0.40.
#[test]
fn over_wide_glyphs_the_paper_phaser_stays_a_tube() {
    let (g, row) = (tall_geom(), 4i32);
    let ch = g.ch as i32;
    let read = |setup: &dyn Fn(&mut CursorGlow)| {
        let (glass, (x0, x1), (fg, bg)) = paper_phaser_wide(6, setup);
        let mid = row * ch + ch / 2;
        let lit: Vec<f32> = (x0..x1)
            .filter(|&x| paper_fall(&glass, mid, x) >= 24)
            .map(|x| crate::color_math::rgb2hsv(glass.at(mid, x)).1)
            .collect();
        let least = (x0..x1)
            .map(|x| crate::color_math::contrast_ratio(fg, glass.at(mid, x)))
            .fold(f32::INFINITY, f32::min);
        (
            lit.iter().sum::<f32>() / lit.len().max(1) as f32,
            least,
            crate::color_math::contrast_ratio(fg, bg),
            lit.len(),
        )
    };
    let (sat, least, clean, lit) = read(&|_| {});
    let (was_sat, _, _, _) = read(&|glow| glow.phaser_tube_wide_pastel = true);
    assert!(
        lit >= g.cw * 6,
        "the tube lights {lit} px of the wide glyphs' row"
    );
    assert!(sat >= 0.55, "the tube's mean saturation is {sat:.2}");
    assert!(
        least >= clean * CursorGlow::PHASER_PAPER_WIDE_KEEP * 0.98,
        "the tube leaves the text {least:.2}:1 of its clean {clean:.2}"
    );
    assert!(
        was_sat < 0.40,
        "control: held as a soft body's band the tube's mean saturation is {was_sat:.2}"
    );
}

/// after-land r1's judge, THE PAPER PHASER'S SPECTRUM RUNS ONE WAY ROUND: laid every quarter
/// cell, the paper tube's knots took the straight mean of their two cells' spectrum places,
/// and where a run's hue passes the wheel's start those places wrap from near one to near
/// nought, so across that one cell the tube walked the whole wheel back, a wrong-way rainbow
/// about ten pixels wide that read as a pale seam between two words (`phaser-light.t0`,
/// x326-342: green, pink, violet, blue, then green again). Twelve keys typed on GitHub Light
/// (the spectrum steps a seventh of its wheel a key, so it wraps within the last seven
/// cells): along the row's middle behind the caret, across its lit, saturated pixels, no run
/// of consecutive steps walks its hue back by more than 30 degrees. Negative control: the
/// knots at the straight mean (`phaser_hue_long_way`) walk back 150 degrees or more in one
/// run.
#[test]
fn the_paper_phaser_s_spectrum_runs_one_way_round() {
    let (g, row) = (tall_geom(), 4i32);
    let (cw, ch) = (g.cw as i32, g.ch as i32);
    let read = |setup: &dyn Fn(&mut CursorGlow)| {
        let (glass, caret) = paper_phaser(12, 0, setup);
        let (x0, x1) = ((i32::from(caret) - 7) * cw, i32::from(caret) * cw);
        let mid = row * ch + ch / 2;
        let hue = |x: i32| {
            let (h, s, _) = crate::color_math::rgb2hsv(glass.at(mid, x));
            (paper_fall(&glass, mid, x) >= 24 && s >= 0.2).then_some(h)
        };
        let (mut worst, mut back, mut lit) = (0.0f32, 0.0f32, 0usize);
        for x in x0..x1 {
            let (Some(a), Some(b)) = (hue(x), hue(x + 1)) else {
                back = 0.0;
                continue;
            };
            lit += 1;
            let step = (b - a + 540.0).rem_euclid(360.0) - 180.0;
            back = if step < 0.0 { back - step } else { 0.0 };
            worst = worst.max(back);
        }
        (worst, lit)
    };
    let (back, lit) = read(&|_| {});
    let (was_back, _) = read(&|glow| glow.phaser_hue_long_way = true);
    assert!(
        lit >= 5 * cw as usize,
        "the tube lights {lit} px of the row's middle"
    );
    assert!(
        back <= 30.0,
        "the tube's hue walks back {back:.0} degrees in one run"
    );
    assert!(
        was_back >= 150.0,
        "control: at the straight mean the hue walks back only {was_back:.0} degrees"
    );
}
