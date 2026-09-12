// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The LAW's own worst-case text contrast at each candidate bar, on Nord —
//! evaluated where the ribbon is brightest: the ledger's FRAME TOP (251), the
//! bed's own coverage cap (236), and the cold body (207).
use aterm_effects::rainbow_kitty::ribbon::{
    BED_LUMA_MIN, BODY_COLD_SHARE, BODY_CONTRAST_GUARD, BODY_FRAME_TOP, UNDER_COV_CAP, bed_ink,
    relative_luminance,
};
use aterm_effects::spectrum::spectrum;
use aterm_render::{over_premul, premul_rgb};
const FG: u32 = 0x00D8_DEE9;
const BG: u32 = 0x002E_3440;
fn lit(i: u32, c: u8) -> u32 {
    over_premul(BG, premul_rgb(i, c), c)
}
fn main() {
    let yfg = relative_luminance(FG);
    let cold = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
    println!(
        "{:>5} {:>8} {:>9} {:>9} {:>9}",
        "bar", "budget", "top251", "cap236", "body207"
    );
    for bar in [5.25f32, 4.75, 4.50, 4.25, 4.00, 3.50, 3.00] {
        let hi = (1.0 + 0.05) / (bar + BODY_CONTRAST_GUARD) - 0.05;
        let b = ((yfg + 0.05) / (bar + BODY_CONTRAST_GUARD) - 0.05).clamp(BED_LUMA_MIN, hi);
        let worst = |cov: u8| {
            let mut y = 0.0f32;
            for i in 0..=128 {
                y = y.max(relative_luminance(lit(
                    bed_ink(spectrum(i as f32 / 128.0), b),
                    cov,
                )));
            }
            (yfg + 0.05) / (y + 0.05)
        };
        println!(
            "{bar:>5.2} {b:>8.4} {:>9.2} {:>9.2} {:>9.2}",
            worst(BODY_FRAME_TOP as u8),
            worst(UNDER_COV_CAP as u8),
            worst(cold)
        );
    }
}
