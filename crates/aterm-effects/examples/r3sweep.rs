// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE RULING SWEEP — what each candidate contrast bar actually buys the warm
//! stretch of the owner's own band, on his own Nord theme, measured through the
//! shipping `bed_ink`.  `BED_LUMA_MAX` is DERIVED from the candidate bar (the
//! constant documents itself as "the bar's own answer at a WHITE foreground"),
//! so a lower bar is not artificially clamped by today's 0.150.
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::{
    BED_LUMA_MIN, BODY_COLD_SHARE, BODY_CONTRAST_GUARD, UNDER_COV_CAP, bed_ink, relative_luminance,
    walk_t,
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
fn lab(c: u32) -> (f32, f32, f32) {
    let (r, g, b) = (s2l(ch(c, 16)), s2l(ch(c, 8)), s2l(ch(c, 0)));
    let (x, y, z) = (
        0.4124 * r + 0.3576 * g + 0.1805 * b,
        0.2126 * r + 0.7152 * g + 0.0722 * b,
        0.0193 * r + 0.1192 * g + 0.9505 * b,
    );
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
fn hk(c: u32) -> f32 {
    let (l, a, b) = lab(c);
    let cc = (a * a + b * b).sqrt();
    let mut h = b.atan2(a).to_degrees();
    if h < 0.0 {
        h += 360.0;
    }
    let g = 0.116 * ((h - 90.0).to_radians() / 2.0).sin().abs() + 0.085;
    l + (2.5 - 0.025 * l) * g * cc
}
fn lit(ink: u32, cov: u8, bg: u32) -> u32 {
    over_premul(bg, premul_rgb(ink, cov), cov)
}

/// The budget the shipping solve would answer at a candidate `bar`, with the
/// ceiling derived from that same bar.
fn budget_at(bar: f32, fg: u32) -> f32 {
    let hi = (1.0 + 0.05) / (bar + BODY_CONTRAST_GUARD) - 0.05;
    ((relative_luminance(fg) + 0.05) / (bar + BODY_CONTRAST_GUARD) - 0.05).clamp(BED_LUMA_MIN, hi)
}

fn main() {
    let yfg = relative_luminance(NORD_FG);
    let cold = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
    println!(
        "# Nord fg #D8DEE9 (Y {yfg:.4}) on bg #2E3440 (Y {:.4}, peak {})",
        relative_luminance(NORD_BG),
        mx(NORD_BG)
    );
    println!(
        "# BED_LUMA_MAX derived per bar; body coverage {cold}/255, head {}/255",
        UNDER_COV_CAP as u8
    );
    println!(
        "# TEXT = the real worst contrast of the theme fg over the BRIGHTEST composited bed cell."
    );
    println!("# darkrun = longest run of consecutive cells in the owner's 170-cell band whose");
    println!(
        "#           apparent brightness (L**hk) is >= 20 below the band's own brightest cell.\n"
    );
    println!(
        "{:>5} {:>7} {:>8} {:>8} {:>7} {:>7} {:>7} {:>8} {:>8}",
        "bar", "budget", "warm_pk", "cool_pk", "ratio", "hk_lo", "hk_hi", "darkrun", "TEXT"
    );
    let bars: Vec<f32> = vec![
        5.25, 5.00, 4.75, 4.50, 4.25, 4.00, 3.50, 3.00, 2.50, 2.00, 1.50,
    ];
    for &bar in &bars {
        let budget = budget_at(bar, NORD_FG);
        let (mut wpk, mut cpk) = (255u32, 0u32);
        let (mut hlo, mut hhi) = (f32::MAX, f32::MIN);
        let mut ymax = 0.0f32;
        for i in 0..=128 {
            let l = lit(
                bed_ink(spectrum(i as f32 / 128.0), budget),
                UNDER_COV_CAP as u8,
                NORD_BG,
            );
            wpk = wpk.min(mx(l));
            cpk = cpk.max(mx(l));
            hlo = hlo.min(hk(l));
            hhi = hhi.max(hk(l));
            ymax = ymax.max(relative_luminance(l));
        }
        // the owner's band: longest run >= 20 L**hk below the band's own max
        let band: Vec<f32> = (0..170)
            .map(|d| {
                let t = tri(walk_t(d as f32)).clamp(0.0, 1.0);
                hk(lit(bed_ink(spectrum(t), budget), cold, NORD_BG))
            })
            .collect();
        let top = band.iter().fold(f32::MIN, |m, &v| m.max(v));
        let (mut run, mut best) = (0usize, 0usize);
        for &v in &band {
            if v <= top - 20.0 {
                run += 1;
                best = best.max(run);
            } else {
                run = 0;
            }
        }
        let text = (yfg + 0.05) / (ymax + 0.05);
        println!(
            "{bar:>5.2} {budget:>7.4} {wpk:>8} {cpk:>8} {:>7.2} {hlo:>7.1} {hhi:>7.1} {best:>8} {text:>8.2}",
            cpk as f32 / wpk as f32
        );
    }

    println!(
        "\n# THE WARM TROUGH ITSELF (the darkest stop of the arc), per bar — composited body colour"
    );
    println!(
        "{:>5} {:>9} {:>7} {:>7} {:>26}",
        "bar", "trough", "peak", "L**hk", "vs the ground (peak 64)"
    );
    for &bar in &bars {
        let budget = budget_at(bar, NORD_FG);
        let mut worst = (f32::MAX, 0u32);
        for i in 0..=128 {
            let l = lit(bed_ink(spectrum(i as f32 / 128.0), budget), cold, NORD_BG);
            let v = hk(l);
            if v < worst.0 {
                worst = (v, l);
            }
        }
        println!(
            "{bar:>5.2} #{:06X}   {:>7} {:>7.1} {:>24.2}x",
            worst.1,
            mx(worst.1),
            worst.0,
            mx(worst.1) as f32 / 64.0
        );
    }
}
