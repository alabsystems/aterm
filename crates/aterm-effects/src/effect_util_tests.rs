// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

const fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    }
}

/// A geometry with no cell height lays no effects-box quad, even where its head leaves the box
/// rows tall, rather than dividing by the height; the same quad at a 16 px cell lays one band.
#[test]
fn a_zero_cell_height_lays_no_effects_quad() {
    let headed = Geom {
        origin_y: 10,
        head: 10,
        ..geom()
    };
    let mut quads = Vec::new();
    push_fx_quad(
        &mut quads,
        Geom { ch: 0, ..headed },
        0,
        0,
        8,
        16,
        0x00FF_FFFF,
        0,
    );
    assert!(quads.is_empty(), "{quads:?}");
    push_fx_quad(&mut quads, headed, 0, 0, 8, 16, 0x00FF_FFFF, 0);
    assert_eq!(
        quads.len(),
        2,
        "control: the head band and row 0's: {quads:?}"
    );
}

/// Only additive zero light is skipped: a source-over quad that rounds to black still darkens.
#[test]
fn a_source_over_quad_that_rounds_to_black_still_darkens() {
    let shade = 0x0003_0201;
    let a = 20;
    assert_eq!(
        premul_rgb(shade, a),
        0,
        "fixture: the shade must round to black"
    );

    let mut out = Vec::new();
    push_grid_quad(&mut out, geom(), 8, 16, 8, 16, premul_rgb(shade, a), a);
    assert_eq!(out.len(), 1, "a black source-over quad must be pushed");
    assert_eq!((out[0].color, out[0].alpha), (0, a));

    let mut additive = Vec::new();
    push_grid_quad(&mut additive, geom(), 8, 16, 8, 16, 0, 0);
    assert!(
        additive.is_empty(),
        "zero ADDITIVE light adds nothing and is skipped"
    );
}

/// The envelope's measured flash rate matches [`TWINKLE_FLASH_HZ`]: `|sin|` peaks twice per
/// sine cycle, so dropping the rectification would halve it.
#[test]
fn twinkle_law_stays_under_the_photosensitivity_bound() {
    let (dt, span) = (1.0f32 / 240.0, 10.0f32);
    let n = (span / dt) as usize;
    let mut peaks = 0usize;
    let mut high = false;
    for i in 0..n {
        let hot = twinkle_env(i as f32 * dt, 0.0) >= TWINKLE_FLOOR + TWINKLE_DEPTH * 0.9;
        if hot && !high {
            peaks += 1;
        }
        high = hot;
    }
    let hz = peaks as f32 / span;
    assert!(
        (hz - TWINKLE_FLASH_HZ).abs() < 0.2,
        "measured {hz} Hz against a declared {TWINKLE_FLASH_HZ} Hz"
    );
}

/// The arm conversion truncates: at every shipping cell height the heaviest named grain is
/// a hairline, and somewhere in that band rounding would fatten it.
#[test]
fn the_one_arm_conversion_truncates_and_floors() {
    assert_eq!(
        star_arm_px(7.84),
        7,
        "the conversion must truncate, not round"
    );
    assert_eq!(star_arm_px(7.0), 7);
    assert_eq!(star_arm_px(0.2), 1);
    assert_eq!(star_arm_px(0.0), 1);
    let mut rounding_would_fatten = 0;
    for ch in [24.0_f32, 28.0, 32.0, 36.0, 40.0] {
        let f = star_arm(ch, STAR_ARM_HERO);
        let arm = star_arm_px(f);
        assert!(arm >= 2, "vacuous: no arm to measure at ch {ch}");
        assert_eq!(
            star_waist_px(arm),
            1,
            "the family's heaviest named grain is {} px thick at ch {ch}",
            star_waist_px(arm)
        );
        if star_waist_px((f.round() as i32).max(1)) > 1 {
            rounding_would_fatten += 1;
        }
    }
    assert!(
        rounding_would_fatten > 0,
        "no shipping cell height distinguishes truncation from rounding — \
         this rule would be pinning nothing"
    );
}

/// The additive star's proportions (arm and waist, nucleus, glint offset and dimming)
/// come from the shared constants.
#[test]
fn the_additive_star_draws_the_family_silhouette() {
    let g = geom();
    let (sx, sy, arm, cov) = (100i32, 40i32, 10i32, 200u8);
    let mut out = Vec::new();
    assert!(push_twinkle_star(
        &mut out,
        g,
        sx,
        sy,
        arm,
        cov,
        true,
        0x00FF_FFFF,
        4096
    ));
    let t = star_waist_px(arm);
    let b = star_body_px(arm);
    let span = |q: &GlowQuad| (i32::from(q.x), i32::from(q.x) + i32::from(q.w));
    let full = premul_rgb(0x00FF_FFFF, cov);
    let body = out
        .iter()
        .find(|q| span(q) == (sx - b, sx + b + 1) && q.color == full)
        .expect("a horizontal body of 2*body+1 at full coverage");
    assert_eq!(
        i32::from(body.h),
        t,
        "the horizontal body left the shared waist"
    );
    let full_span = 2 * arm + 1;
    let row: Vec<_> = out
        .iter()
        .filter(|q| i32::from(q.y) <= sy && sy < i32::from(q.y) + i32::from(q.h))
        .collect();
    let lit = |x: i32| row.iter().any(|q| span(q).0 <= x && x < span(q).1);
    assert!(
        (sx - arm..=sx + arm).all(lit) && !lit(sx - arm - 1) && !lit(sx + arm + 1),
        "the tapered arm does not span exactly {full_span} px"
    );
    let mut point: Vec<_> = row
        .iter()
        .filter(|q| span(q).0 > sx + b)
        .map(|q| (span(q).0, (q.color >> 16) & 0xff))
        .collect();
    point.sort_unstable();
    assert!(!point.is_empty(), "the arm came to a square end");
    let mut prev = u32::from(cov);
    for (x, c) in &point {
        assert!(
            *c < prev,
            "the point does not taper: {c} at x+{} under a previous {prev}",
            x - sx
        );
        prev = *c;
    }
    let tip = f32::from(cov) * star_taper_cov(1.0);
    let midway = f32::from(cov) * star_taper_cov(0.5);
    assert!(
        (tip..=midway).contains(&(prev as f32)),
        "the outermost span carries {prev}, outside the ramp's bottom half \
         ({tip:.0}..={midway:.0}) — the point is not reaching the shared tip"
    );
    let c = star_core_px(arm);
    let core = premul_rgb(0x00FF_FFFF, (f32::from(cov) * STAR_CORE_ADD) as u8);
    assert!(
        out.iter().any(|q| i32::from(q.w) == c && q.color == core),
        "no nucleus at the shared core extent"
    );
    let d = ((arm as f32 * STAR_GLINT).round() as i32).max(1);
    let dim = premul_rgb(0x00FF_FFFF, (f32::from(cov) * STAR_GLINT_COV) as u8);
    for (ox, oy) in [(-d, -d), (d, -d), (-d, d), (d, d)] {
        assert!(
            out.iter().any(|q| i32::from(q.x) == sx + ox
                && i32::from(q.y) == sy + oy
                && q.w == 1
                && q.h == 1
                && q.color == dim),
            "no glint at {ox},{oy}"
        );
    }
    let mut plain = Vec::new();
    assert!(push_twinkle_star(
        &mut plain,
        g,
        sx,
        sy,
        arm,
        cov,
        false,
        0x00FF_FFFF,
        4096
    ));
    assert!(
        plain
            .iter()
            .all(|q| !(q.w == 1 && q.h == 1 && q.color == dim))
    );
}

/// The windowed-walk ramp (`windows(2)` walk, divide guard, raw-crest fall-through): the
/// byte-identity oracle for the branchless [`ramp_5stop`].
const fn walked_ramp(stops: &[(f32, u32); 5], t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let mut i = 0;
    while i + 1 < stops.len() {
        let (t0, c0) = stops[i];
        let (t1, c1) = stops[i + 1];
        if t <= t1 {
            let local = if t1 > t0 { (t - t0) / (t1 - t0) } else { 0.0 };
            return lerp_rgb(c0, c1, local);
        }
        i += 1;
    }
    stops[stops.len() - 1].1
}

/// One f32 bit pattern through both live ramps against the walk oracle.
fn assert_ramps_match_walk(bits: u32) {
    let t = f32::from_bits(bits);
    assert_eq!(
        fire_ramp(t),
        walked_ramp(&FIRE_STOPS, t),
        "fire_ramp diverged from the stop walk at bits {bits:#010x} (t = {t:?})"
    );
    assert_eq!(
        water_ramp(t),
        walked_ramp(&WATER_STOPS, t),
        "water_ramp diverged from the stop walk at bits {bits:#010x} (t = {t:?})"
    );
}

/// The branchless ramps are bit-equal to the walk (frame fingerprints miss a one-count
/// drift): a prime-strided march over the f32 bit space, a ±16Ki-ULP band around every stop
/// with its sign-bit mirror, and the named specials.
#[test]
fn ramp_rewrite_is_bit_identical_to_the_stop_walk() {
    for bits in (0..=u32::MAX).step_by(4099) {
        assert_ramps_match_walk(bits);
    }
    for anchor in [0.0f32, 0.25, 0.35, 0.5, 0.65, 0.75, 0.85, 1.0] {
        let b = anchor.to_bits();
        for bits in b.saturating_sub(16_384)..=b.saturating_add(16_384) {
            assert_ramps_match_walk(bits);
            assert_ramps_match_walk(bits | 0x8000_0000);
        }
    }
    for t in [
        f32::NAN,
        -f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::MIN_POSITIVE,
        -f32::MIN_POSITIVE,
        f32::EPSILON,
        f32::from_bits(1),
        -0.0,
        2.0,
        f32::MAX,
        f32::MIN,
    ] {
        assert_ramps_match_walk(t.to_bits());
    }
}

/// The colour and fade helpers as fcb3a95bbf wrote them, early returns and all: the
/// frames main drew with them are the oracle the bodies without the returns must draw.
mod early_return_oracle {
    use super::{FX_EDGE_FADE, FX_EDGE_FADE_X, Geom};

    pub(super) const fn scale_premul(c: u32, share: f32) -> u32 {
        let rgb = c & 0x00FF_FFFF;
        if rgb == 0 {
            return 0;
        }
        let share = share.clamp(0.0, 1.0);
        if share <= 0.0 {
            return 0;
        }
        if share >= 1.0 {
            return rgb;
        }
        const fn ch(c: u32, share: f32, sh: u32) -> u32 {
            let v = (((c >> sh) & 0xff) as f32 * share).round() as u32;
            (if v < 255 { v } else { 255 }) << sh
        }
        ch(rgb, share, 16) | ch(rgb, share, 8) | ch(rgb, share, 0)
    }

    pub(super) const fn fx_fade_y(geom: Geom, y: f32) -> f32 {
        let reach = (geom.ch as f32 * FX_EDGE_FADE).max(1.0);
        let d = (y - geom.fx_top() as f32).min(geom.fx_bot() as f32 - y);
        if d >= reach {
            return 1.0;
        }
        if d <= 0.0 {
            return 0.0;
        }
        let k = (d / reach).clamp(0.0, 1.0);
        k * k * (3.0 - 2.0 * k)
    }

    pub(super) const fn fx_fade_x(geom: Geom, x: f32) -> f32 {
        let reach = (geom.cw as f32 * FX_EDGE_FADE_X).max(1.0);
        let d = (x - geom.fx_left() as f32).min(geom.fx_right() as f32 - x);
        if d >= reach {
            return 1.0;
        }
        if d <= 0.0 {
            return 0.0;
        }
        let k = (d / reach).clamp(0.0, 1.0);
        k * k * (3.0 - 2.0 * k)
    }

    pub(super) const fn lerp_rgb(a: u32, b: u32, t: f32) -> u32 {
        let t = t.clamp(0.0, 1.0);
        let a_rgb = a & 0x00FF_FFFF;
        let b_rgb = b & 0x00FF_FFFF;
        if t == 0.0 || (t.is_finite() && a_rgb == b_rgb) {
            return a_rgb;
        }
        if t == 1.0 {
            return b_rgb;
        }
        const fn mix(a: u32, b: u32, t: f32, sh: u32) -> u32 {
            let ca = ((a >> sh) & 0xff) as f32;
            let cb = ((b >> sh) & 0xff) as f32;
            ((ca + (cb - ca) * t) + 0.5) as u32
        }
        (mix(a_rgb, b_rgb, t, 16) << 16) | (mix(a_rgb, b_rgb, t, 8) << 8) | mix(a_rgb, b_rgb, t, 0)
    }

    pub(super) const fn mix_rgb(a: u32, b: u32, t: f32) -> u32 {
        let t = t.clamp(0.0, 1.0);
        let a_rgb = a & 0x00FF_FFFF;
        let b_rgb = b & 0x00FF_FFFF;
        if t == 0.0 || (t.is_finite() && a_rgb == b_rgb) {
            return a_rgb;
        }
        if t == 1.0 {
            return b_rgb;
        }
        const fn ch(a: u32, b: u32, t: f32, sh: u32) -> u32 {
            let ca = ((a >> sh) & 0xff) as f32;
            let cb = ((b >> sh) & 0xff) as f32;
            ((ca + (cb - ca) * t).round().clamp(0.0, 255.0) as u32) << sh
        }
        ch(a_rgb, b_rgb, t, 16) | ch(a_rgb, b_rgb, t, 8) | ch(a_rgb, b_rgb, t, 0)
    }
}

/// The `t` (and shares) the helpers are judged at: both ends and the floats beside them,
/// NaN, the infinities, below 0 and above 1 (the clamp's work), and a sweep on and off
/// the binary grid.
fn judged_ts() -> Vec<f32> {
    let mut ts = vec![
        0.0,
        -0.0,
        1.0,
        f32::NAN,
        -f32::NAN,
        f32::INFINITY,
        f32::NEG_INFINITY,
        -f32::MIN_POSITIVE,
        -0.25,
        -1.0,
        f32::MIN,
        f32::from_bits(1),
        f32::MIN_POSITIVE,
        f32::EPSILON,
        1.0 - f32::EPSILON / 2.0,
        1.0 + f32::EPSILON,
        1.5,
        f32::MAX,
    ];
    ts.extend((1..64).map(|i| i as f32 / 64.0));
    ts.extend((0..37).map(|i| (i as f32 + 0.5) / 37.0));
    ts
}

/// The colour mixes without early returns draw what they drew with them, over every pair
/// of channel values (in all three channels at once, under a high byte to mask) at every
/// one of [`judged_ts`].
#[test]
fn the_colour_mixes_draw_what_their_early_returns_drew() {
    let ts = judged_ts();
    for pair in 0..=u16::MAX {
        let (ca, cb) = (u32::from(pair >> 8), u32::from(pair & 0xff));
        let a = (ca * 0x0001_0101) | (u32::from(pair) << 24);
        let b = (cb * 0x0001_0101) | (u32::from(!pair) << 24);
        for &t in &ts {
            assert_eq!(
                lerp_rgb(a, b, t),
                early_return_oracle::lerp_rgb(a, b, t),
                "lerp_rgb({a:#010x}, {b:#010x}, {t:?})"
            );
            assert_eq!(
                mix_rgb(a, b, t),
                early_return_oracle::mix_rgb(a, b, t),
                "mix_rgb({a:#010x}, {b:#010x}, {t:?})"
            );
        }
    }
}

/// `scale_premul` and the edge fades without early returns give what they gave with them:
/// every channel value (alone, beside black and beside its complement) at every share of
/// [`judged_ts`], and every 1/64 px across both edges of the box and past them, on a
/// box whose fade reaches less than a pixel and on one that reaches several.
#[test]
fn the_light_scale_and_edge_fades_give_what_their_early_returns_gave() {
    let ts = judged_ts();
    for v in 0..=255u32 {
        for c in [
            v * 0x0001_0101,
            v << 16,
            (v << 8) | (255 - v),
            0xFF00_0000 | v,
        ] {
            for &share in &ts {
                assert_eq!(
                    scale_premul(c, share),
                    early_return_oracle::scale_premul(c, share),
                    "scale_premul({c:#010x}, {share:?})"
                );
            }
        }
    }
    let tiny = Geom {
        cw: 1,
        ch: 1,
        origin_x: 3,
        origin_y: 5,
        head: 2,
        ..geom()
    };
    for g in [geom(), tiny] {
        let same = |what: &str, at: f32, got: f32, want: f32| {
            assert_eq!(
                got.to_bits(),
                want.to_bits(),
                "{what} at {at:?}: {got} for {want}"
            );
        };
        let (left, right) = (g.fx_left() as f32, g.fx_right() as f32);
        let (top, bot) = (g.fx_top() as f32, g.fx_bot() as f32);
        let mut xs: Vec<f32> = (-640..=(right as i32 + 10) * 64)
            .map(|i| i as f32 / 64.0)
            .collect();
        let mut ys: Vec<f32> = (-640..=(bot as i32 + 10) * 64)
            .map(|i| i as f32 / 64.0)
            .collect();
        for special in [
            f32::NAN,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::MAX,
            f32::MIN,
        ] {
            xs.push(special);
            ys.push(special);
        }
        for edge in [left, right] {
            xs.extend([edge.next_down(), edge, edge.next_up()]);
        }
        for edge in [top, bot] {
            ys.extend([edge.next_down(), edge, edge.next_up()]);
        }
        for &x in &xs {
            same(
                "fx_fade_x",
                x,
                fx_fade_x(g, x),
                early_return_oracle::fx_fade_x(g, x),
            );
        }
        for &y in &ys {
            same(
                "fx_fade_y",
                y,
                fx_fade_y(g, y),
                early_return_oracle::fx_fade_y(g, y),
            );
        }
    }
}

/// A star whose body paints nothing draws nothing and answers the budget as the piece
/// loop's check at its second piece does: `true` below the cap, `false` at it and past it,
/// where the callers stop laying stars. Judged over every coverage that rounds the body to
/// black of colours from black to white, arms from one to past the cached ones, plain and
/// gold, with the budget empty, under, at and past the cap.
#[test]
fn a_star_that_paints_nothing_answers_the_budget_as_the_piece_loop_did() {
    let g = geom();
    let (mut below, mut at_or_past) = (0u32, 0u32);
    for color in [
        0,
        0x0000_0001,
        0x0001_0101,
        0x0000_0080,
        0xFF00_0080,
        0x00A9_8BFF,
        0x00FF_FFFF,
    ] {
        for cov in 1..=u8::MAX {
            if premul_rgb(color, cov) != 0 {
                continue;
            }
            for arm in [1, 2, 3, 5, 8, 16, 17, 24] {
                for gold in [false, true] {
                    for (filled, cap) in [(0, 0), (0, 1), (0, 4096), (3, 2), (3, 3), (3, 4)] {
                        let laid = vec![GlowQuad::default(); filled];
                        let mut out = laid.clone();
                        let said =
                            push_twinkle_star(&mut out, g, 100, 40, arm, cov, gold, color, cap);
                        assert_eq!(
                            out, laid,
                            "push_twinkle_star(arm {arm}, cov {cov}, gold {gold}, \
                             {color:#010x}) with {filled} of {cap} quads laid drew light"
                        );
                        assert_eq!(
                            said,
                            filled < cap,
                            "push_twinkle_star(arm {arm}, cov {cov}, gold {gold}, \
                             {color:#010x}) with {filled} of {cap} quads laid answered {said}"
                        );
                        if filled < cap {
                            below += 1;
                        } else {
                            at_or_past += 1;
                        }
                    }
                }
            }
        }
    }
    assert!(
        below > 0 && at_or_past > 0,
        "no star that paints nothing was laid below the cap ({below}) or at it ({at_or_past})"
    );
}
