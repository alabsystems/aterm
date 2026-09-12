// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! R3-c: the cost curve of EVENING the arc by H-K — how much light the cool
//! half must give up per point of apparent-brightness spread closed. The bar
//! is untouched: no stop ever exceeds `bed_luma_budget`.
use aterm_effects::rainbow_kitty::ribbon::{
    UNDER_COV_CAP, bed_ink, bed_luma_budget, relative_luminance,
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
fn lch(c: u32) -> (f32, f32, f32) {
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
    let (l, a, bb) = (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz));
    let mut h = bb.atan2(a).to_degrees();
    if h < 0.0 {
        h += 360.0;
    }
    (l, (a * a + bb * bb).sqrt(), h)
}
fn hk(c: u32) -> f32 {
    let (l, cc, h) = lch(c);
    let g = 0.116 * ((h - 90.0).to_radians() / 2.0).sin().abs() + 0.085;
    l + (2.5 - 0.025 * l) * g * cc
}
fn lit(ink: u32, bg: u32) -> u32 {
    over_premul(
        bg,
        premul_rgb(ink, UNDER_COV_CAP as u8),
        UNDER_COV_CAP as u8,
    )
}

fn main() {
    let budget = bed_luma_budget(NORD_FG);
    // today's arc
    let now: Vec<(f32, u32, f32)> = (0..=128)
        .map(|i| {
            let l = lit(bed_ink(spectrum(i as f32 / 128.0), budget), NORD_BG);
            (hk(l), l, relative_luminance(l))
        })
        .collect();
    let lo = now.iter().map(|s| s.0).fold(f32::MAX, f32::min);
    let hiv = now.iter().map(|s| s.0).fold(f32::MIN, f32::max);
    let ymean0: f32 = now.iter().map(|s| s.2).sum::<f32>() / now.len() as f32;
    println!(
        "TODAY: L**hk {lo:.1}..{hiv:.1} spread {:.1}; every stop at Y {budget:.4}; mean composited Y {ymean0:.4}",
        hiv - lo
    );
    println!(
        "\nalpha = how far each stop is pulled DOWN toward the arc's worst apparent brightness"
    );
    println!("(the bar is untouched throughout — no stop is ever brighter than today)\n");
    println!(
        "{:>6} {:>8} {:>9} {:>9} {:>9} {:>9} {:>8}",
        "alpha", "spread", "mean Y", "vs today", "blue Y", "blue peak", "worstpk"
    );
    for alpha in [0.0f32, 0.25, 0.5, 0.75, 1.0] {
        let mut ys = vec![];
        let mut hks = vec![];
        let (mut blue_y, mut blue_pk) = (0.0f32, 0u32);
        let mut worst_pk = 255u32;
        for (i, s) in now.iter().enumerate() {
            let target = s.0 - alpha * (s.0 - lo);
            let p = i as f32 / 128.0;
            let (mut a, mut b) = (0.0f32, budget);
            for _ in 0..30 {
                let m = 0.5 * (a + b);
                if hk(lit(bed_ink(spectrum(p), m), NORD_BG)) < target {
                    a = m;
                } else {
                    b = m;
                }
            }
            let l = lit(bed_ink(spectrum(p), b), NORD_BG);
            ys.push(relative_luminance(l));
            hks.push(hk(l));
            worst_pk = worst_pk.min(mx(l));
            if i == 104 {
                blue_y = relative_luminance(l);
                blue_pk = mx(l);
            }
        }
        let sp = hks.iter().fold(f32::MIN, |m, &v| m.max(v))
            - hks.iter().fold(f32::MAX, |m, &v| m.min(v));
        let ym: f32 = ys.iter().sum::<f32>() / ys.len() as f32;
        println!(
            "{alpha:>6.2} {sp:>8.1} {ym:>9.4} {:>8.0}% {blue_y:>9.4} {blue_pk:>9} {worst_pk:>8}",
            100.0 * ym / ymean0
        );
    }
    println!("\nThe warm stops NEVER move in any row above: alpha only takes light AWAY from the");
    println!(
        "stops that already read brightest. There is no alpha at which the trough gets brighter."
    );
}
