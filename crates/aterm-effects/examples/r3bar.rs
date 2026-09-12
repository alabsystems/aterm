// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! R3: the bar sweep — what each candidate contrast bar buys the WARM stretch,
//! measured through the real `bed_ink` on the owner's Nord theme.
use aterm_effects::rainbow_kitty::ribbon::{
    BED_LUMA_MAX, BED_LUMA_MIN, BODY_COLD_SHARE, BODY_CONTRAST_GUARD, UNDER_COV_CAP, bed_ink,
    relative_luminance,
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
fn lstar_hk(c: u32) -> f32 {
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

fn main() {
    let yfg = relative_luminance(NORD_FG);
    let cold = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
    println!(
        "# Nord fg Y={yfg:.4}; bed budget = clamp((Yfg+0.05)/(bar+{BODY_CONTRAST_GUARD}) - 0.05, {BED_LUMA_MIN}, {BED_LUMA_MAX})"
    );
    println!(
        "# composited at cov {} (hot) / {cold} (cold). 'trough' = the arc slot with the lowest L**hk.\n",
        UNDER_COV_CAP as u8
    );
    println!(
        "{:>5} {:>7} {:>9} {:>9} {:>7} {:>7} {:>7} {:>7} {:>7} {:>7} {:>6}",
        "bar",
        "budget",
        "trough",
        "cool",
        "pk_wrm",
        "pk_cool",
        "hk_wrm",
        "hk_cool",
        "spread",
        "ratio",
        "TEXT"
    );
    for bar in [
        5.25f32, 5.00, 4.75, 4.50, 4.25, 4.00, 3.75, 3.50, 3.00, 2.50,
    ] {
        let budget =
            ((yfg + 0.05) / (bar + BODY_CONTRAST_GUARD) - 0.05).clamp(BED_LUMA_MIN, BED_LUMA_MAX);
        let (mut lo, mut hi) = ((f32::MAX, 0u32), (f32::MIN, 0u32));
        let mut ymax = 0.0f32;
        for i in 0..=128 {
            let l = lit(
                bed_ink(spectrum(i as f32 / 128.0), budget),
                UNDER_COV_CAP as u8,
                NORD_BG,
            );
            let v = lstar_hk(l);
            if v < lo.0 {
                lo = (v, l);
            }
            if v > hi.0 {
                hi = (v, l);
            }
            ymax = ymax.max(relative_luminance(l));
        }
        // the ACTUAL measured text contrast: fg over the BRIGHTEST composited bed cell
        let text = (yfg + 0.05) / (ymax + 0.05);
        println!(
            "{bar:>5.2} {budget:>7.4} #{:06X} #{:06X} {:>7} {:>7} {:>7.1} {:>7.1} {:>7.1} {:>7.2} {:>6.2}{}",
            lo.1,
            hi.1,
            mx(lo.1),
            mx(hi.1),
            lo.0,
            hi.0,
            hi.0 - lo.0,
            mx(hi.1) as f32 / mx(lo.1) as f32,
            text,
            if text >= 5.25 { " OK" } else { " UNDER" }
        );
    }

    println!("\n# the same, at the COLD coverage {cold} (the body of the owner's band)");
    println!(
        "{:>5} {:>7} {:>9} {:>9} {:>7} {:>7} {:>7} {:>7} {:>7}",
        "bar", "budget", "trough", "cool", "pk_wrm", "pk_cool", "hk_wrm", "hk_cool", "spread"
    );
    for bar in [5.25f32, 5.00, 4.75, 4.50, 4.25, 4.00, 3.50, 3.00] {
        let budget =
            ((yfg + 0.05) / (bar + BODY_CONTRAST_GUARD) - 0.05).clamp(BED_LUMA_MIN, BED_LUMA_MAX);
        let (mut lo, mut hi) = ((f32::MAX, 0u32), (f32::MIN, 0u32));
        for i in 0..=128 {
            let l = lit(bed_ink(spectrum(i as f32 / 128.0), budget), cold, NORD_BG);
            let v = lstar_hk(l);
            if v < lo.0 {
                lo = (v, l);
            }
            if v > hi.0 {
                hi = (v, l);
            }
        }
        println!(
            "{bar:>5.2} {budget:>7.4} #{:06X} #{:06X} {:>7} {:>7} {:>7.1} {:>7.1} {:>7.1}",
            lo.1,
            hi.1,
            mx(lo.1),
            mx(hi.1),
            lo.0,
            hi.0,
            hi.0 - lo.0
        );
    }

    // What bar does the WARM stretch need to reach a given peak channel / L**hk?
    println!("\n# INVERSE: the bar the trough needs to reach a target");
    for target_peak in [110u32, 120, 130, 140, 160, 180] {
        let mut found = None;
        let mut bar = 5.25f32;
        while bar > 1.0 {
            let budget = ((yfg + 0.05) / (bar + BODY_CONTRAST_GUARD) - 0.05)
                .clamp(BED_LUMA_MIN, BED_LUMA_MAX);
            let mut worst = (f32::MAX, 0u32);
            for i in 0..=128 {
                let l = lit(
                    bed_ink(spectrum(i as f32 / 128.0), budget),
                    UNDER_COV_CAP as u8,
                    NORD_BG,
                );
                let v = lstar_hk(l);
                if v < worst.0 {
                    worst = (v, l);
                }
            }
            if mx(worst.1) >= target_peak {
                found = Some((bar, budget, worst.1, worst.0));
                break;
            }
            bar -= 0.01;
        }
        match found {
            Some((b, bu, c, v)) => println!(
                "  trough peak >= {target_peak:>3}: bar {b:.2} (budget {bu:.4}) -> #{c:06X} L**hk {v:.1}"
            ),
            None => println!("  trough peak >= {target_peak:>3}: unreachable above bar 1.0"),
        }
    }
}
