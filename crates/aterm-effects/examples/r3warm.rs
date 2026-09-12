// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! R3-perceptual probe: the arc at the bed's fixed luminance, measured in
//! perceptual coordinates (L*, C*ab, hue, Helmholtz-Kohlrausch), the trough
//! census, and the owner's own band.
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::{
    BODY_COLD_SHARE, HOT_EDGE_COV_MAX, UNDER_COV_CAP, bed_ink, bed_luma_budget, hot_edge_ink,
    relative_luminance, walk_t,
};
use aterm_effects::spectrum::spectrum;
use aterm_render::{over_premul, premul_rgb};

const NORD_FG: u32 = 0x00D8_DEE9;
const NORD_BG: u32 = 0x002E_3440;

fn ch(c: u32, sh: u32) -> f32 {
    ((c >> sh) & 0xff) as f32
}
fn mx(c: u32) -> u32 {
    ((c >> 16) & 0xff).max((c >> 8) & 0xff).max(c & 0xff)
}
fn s2l(v: f32) -> f32 {
    let v = v / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// CIE XYZ (D65) from an sRGB byte triple.
fn xyz(c: u32) -> (f32, f32, f32) {
    let (r, g, b) = (s2l(ch(c, 16)), s2l(ch(c, 8)), s2l(ch(c, 0)));
    (
        0.4124 * r + 0.3576 * g + 0.1805 * b,
        0.2126 * r + 0.7152 * g + 0.0722 * b,
        0.0193 * r + 0.1192 * g + 0.9505 * b,
    )
}
/// CIELAB (D65 white).
fn lab(c: u32) -> (f32, f32, f32) {
    let (x, y, z) = xyz(c);
    let f = |t: f32| {
        if t > 0.008856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}
fn lch(c: u32) -> (f32, f32, f32) {
    let (l, a, b) = lab(c);
    let mut h = b.atan2(a).to_degrees();
    if h < 0.0 {
        h += 360.0;
    }
    (l, (a * a + b * b).sqrt(), h)
}
/// Fairchild & Pirrotta (1991) H-K corrected lightness: the standard
/// closed-form estimate of how BRIGHT a chromatic patch reads next to a grey
/// of the same L*.  L** = L* + (2.5 - 0.025 L*) * g(h) * C*ab
fn lstar_hk(c: u32) -> f32 {
    let (l, cc, h) = lch(c);
    let g = 0.116 * ((h - 90.0).to_radians() / 2.0).sin().abs() + 0.085;
    l + (2.5 - 0.025 * l) * g * cc
}
/// CIE94 (graphics) difference — good enough to rank "is this cell distinct
/// from the page".
fn de94(a: u32, b: u32) -> f32 {
    let (l1, a1, b1) = lab(a);
    let (l2, a2, b2) = lab(b);
    let (c1, c2) = ((a1 * a1 + b1 * b1).sqrt(), (a2 * a2 + b2 * b2).sqrt());
    let (dl, dc) = (l1 - l2, c1 - c2);
    let (da, db) = (a1 - a2, b1 - b2);
    let dh2 = (da * da + db * db - dc * dc).max(0.0);
    (dl * dl + (dc / (1.0 + 0.045 * c1)).powi(2) + dh2 / (1.0 + 0.015 * c1).powi(2)).sqrt()
}
fn lit(ink: u32, cov: u8, bg: u32) -> u32 {
    over_premul(bg, premul_rgb(ink, cov), cov)
}

fn main() {
    let budget = bed_luma_budget(NORD_FG);
    let cold = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
    println!(
        "# Nord fg#{NORD_FG:06X} bg#{NORD_BG:06X} budget Y={budget:.4} cov hot={} cold={cold}",
        UNDER_COV_CAP as u8
    );
    println!(
        "# ground: Y={:.4} peak={} Lab L*={:.1} C*={:.1} h={:.0}",
        relative_luminance(NORD_BG),
        mx(NORD_BG),
        lch(NORD_BG).0,
        lch(NORD_BG).1,
        lch(NORD_BG).2
    );

    println!(
        "\n## THE ARC AT THE BED'S ONE LUMINANCE (129 LUT slots, composited at cov {})",
        UNDER_COV_CAP as u8
    );
    println!(
        "{:>4} {:>8} {:>9} {:>5} {:>7} {:>6} {:>6} {:>5} {:>7} {:>7} {:>6}",
        "slot", "ink", "lit236", "peak", "Y", "L*", "C*ab", "h", "L**hk", "dE/bg", "hotink"
    );
    for i in (0..=128).step_by(2) {
        let p = i as f32 / 128.0;
        let ink = bed_ink(spectrum(p), budget);
        let l = lit(ink, UNDER_COV_CAP as u8, NORD_BG);
        let (ls, cs, h) = lch(l);
        println!(
            "{i:>4} #{ink:06X} #{l:06X} {:>5} {:>7.4} {:>6.1} {:>6.1} {:>5.0} {:>7.1} {:>7.1} #{:06X}",
            mx(l),
            relative_luminance(l),
            ls,
            cs,
            h,
            lstar_hk(l),
            de94(l, NORD_BG),
            hot_edge_ink(spectrum(p))
        );
    }

    // ---- trough census -------------------------------------------------
    println!(
        "\n## TROUGH CENSUS over the 129 slots (composited, cov {})",
        UNDER_COV_CAP as u8
    );
    let mut peaks = vec![];
    for i in 0..=128 {
        let ink = bed_ink(spectrum(i as f32 / 128.0), budget);
        peaks.push(mx(lit(ink, UNDER_COV_CAP as u8, NORD_BG)));
    }
    let (pmin, pmax) = (*peaks.iter().min().unwrap(), *peaks.iter().max().unwrap());
    println!(
        "peak channel: min {pmin} max {pmax}  ratio {:.2}x",
        pmax as f32 / pmin as f32
    );
    for thr in [100u32, 110, 120, 140] {
        let n = peaks.iter().filter(|&&p| p < thr).count();
        println!(
            "  slots with peak < {thr}: {n}/129 ({:.0}%)",
            100.0 * n as f32 / 129.0
        );
    }

    // ---- the owner's band ---------------------------------------------
    println!("\n## THE OWNER'S BAND — 170 cells behind the caret (walk_t, cov {cold} cold)");
    let mut dark = 0usize;
    let mut runs: Vec<usize> = vec![];
    let mut run = 0usize;
    for d in 0..170 {
        let t = tri(walk_t(d as f32)).clamp(0.0, 1.0);
        let ink = bed_ink(spectrum(t), budget);
        let l = lit(ink, cold, NORD_BG);
        let p = mx(l);
        if p < 110 {
            dark += 1;
            run += 1;
        } else {
            if run > 0 {
                runs.push(run);
            }
            run = 0;
        }
    }
    if run > 0 {
        runs.push(run);
    }
    println!(
        "cells with composited peak < 110: {dark}/170 ({:.0}%)",
        100.0 * dark as f32 / 170.0
    );
    println!("contiguous dark runs: {runs:?}");

    // print the band's first 60 cells verbatim
    println!(
        "\n{:>4} {:>8} {:>9} {:>5} {:>6} {:>6} {:>7} {:>7}",
        "d", "t", "lit", "peak", "L*", "C*ab", "L**hk", "dE/bg"
    );
    for d in 0..60 {
        let t = tri(walk_t(d as f32)).clamp(0.0, 1.0);
        let ink = bed_ink(spectrum(t), budget);
        let l = lit(ink, cold, NORD_BG);
        let (ls, cs, _) = lch(l);
        println!(
            "{d:>4} {t:>8.3} #{l:06X} {:>5} {:>6.1} {:>6.1} {:>7.1} {:>7.1}",
            mx(l),
            ls,
            cs,
            lstar_hk(l),
            de94(l, NORD_BG)
        );
    }

    // ---- R3-c: what would HK-equalisation cost the cool half? ----------
    println!("\n## R3-c: HK-EQUALISE (pin the arc's WORST L**hk, solve every stop down to it)");
    let mut worst = f32::MAX;
    let mut worst_at = 0usize;
    for i in 0..=128 {
        let v = lstar_hk(lit(
            bed_ink(spectrum(i as f32 / 128.0), budget),
            UNDER_COV_CAP as u8,
            NORD_BG,
        ));
        if v < worst {
            worst = v;
            worst_at = i;
        }
    }
    println!("worst L**hk on the arc = {worst:.1} at slot {worst_at}");
    for i in (0..=128).step_by(8) {
        let p = i as f32 / 128.0;
        // bisect the budget down until this stop's L**hk meets `worst`
        let (mut lo, mut hi) = (0.0f32, budget);
        for _ in 0..30 {
            let m = 0.5 * (lo + hi);
            if lstar_hk(lit(bed_ink(spectrum(p), m), UNDER_COV_CAP as u8, NORD_BG)) < worst {
                lo = m;
            } else {
                hi = m;
            }
        }
        let ink = bed_ink(spectrum(p), hi);
        let l = lit(ink, UNDER_COV_CAP as u8, NORD_BG);
        println!(
            "  slot {i:>3}: Y {budget:.4} -> {hi:.4} ({:>4.0}% of budget)  #{l:06X} peak {:>3}",
            100.0 * hi / budget,
            mx(l)
        );
    }

    // ---- R3-a: is there ANY chroma headroom left at the budget? --------
    println!("\n## R3-a: chroma headroom at the budget (is bed_ink already max-chroma?)");
    for i in (0..=128).step_by(16) {
        let p = i as f32 / 128.0;
        let ink = bed_ink(spectrum(p), budget);
        let (_, c_now, h_now) = lch(ink);
        // brute force: the max C*ab sRGB colour with Y <= budget, any hue
        let mut best = (0.0f32, 0u32);
        for r in (0..=255).step_by(3) {
            for g in (0..=255).step_by(3) {
                for b in (0..=255).step_by(3) {
                    let cc = ((r as u32) << 16) | ((g as u32) << 8) | b as u32;
                    if relative_luminance(cc) > budget {
                        continue;
                    }
                    let (_, cab, hh) = lch(cc);
                    if (hh - h_now).abs() > 6.0 && (hh - h_now).abs() < 354.0 {
                        continue;
                    }
                    if cab > best.0 {
                        best = (cab, cc);
                    }
                }
            }
        }
        println!(
            "  slot {i:>3} h={h_now:>3.0}: bed_ink C*={c_now:>5.1} #{ink:06X} | best-at-hue C*={:>5.1} #{:06X}",
            best.0, best.1
        );
    }

    // ---- the hot edge's own arithmetic, for reference -------------------
    println!(
        "\n## the HOT EDGE (additive, cov<={}) over the bed, per stop",
        HOT_EDGE_COV_MAX as u8
    );
    for i in (0..=128).step_by(16) {
        let p = i as f32 / 128.0;
        let bed = lit(bed_ink(spectrum(p), budget), UNDER_COV_CAP as u8, NORD_BG);
        let hot = premul_rgb(hot_edge_ink(spectrum(p)), HOT_EDGE_COV_MAX as u8);
        let sum = aterm_render::add_sat(bed, hot);
        println!(
            "  slot {i:>3}: bed #{bed:06X} peak {:>3} + hot #{hot:06X} = #{sum:06X} peak {:>3} Y {:.4} ({:.1}x bed)",
            mx(bed),
            mx(sum),
            relative_luminance(sum),
            relative_luminance(sum) / relative_luminance(bed).max(1e-6)
        );
    }
}
