// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D1: per-CELL composited colour behind the caret, and the bed over
//! non-default cell backgrounds.
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::{
    BODY_COLD_SHARE, BODY_FRAME_TOP, HOT_EDGE_COV_MAX, STRIP_LIFT_GAIN, UNDER_COV_CAP, bed_ink,
    bed_luma_budget, hot_edge_ink, relative_luminance, walk_t,
};
use aterm_effects::spectrum::spectrum;
use aterm_render::{add_sat, over_premul, premul_rgb};

fn ch(c: u32, sh: u32) -> u32 {
    (c >> sh) & 0xff
}
fn mx(c: u32) -> u32 {
    ch(c, 16).max(ch(c, 8)).max(ch(c, 0))
}
fn contrast(a: u32, b: u32) -> f32 {
    let (la, lb) = (relative_luminance(a), relative_luminance(b));
    let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
    (hi + 0.05) / (lo + 0.05)
}
fn dist(a: u32, b: u32) -> f32 {
    let d = |sh: u32| (ch(a, sh) as f32 - ch(b, sh) as f32).powi(2);
    (d(16) + d(8) + d(0)).sqrt()
}
/// The LUT read: fold + lerp over 129 entries, exactly BedInkLut::sample.
fn lut_ink(t: f32, budget: f32) -> u32 {
    let x = tri(t).clamp(0.0, 1.0) * 128.0;
    let i = (x as usize).min(128);
    let j = (i + 1).min(128);
    let a = bed_ink(spectrum(i as f32 / 128.0), budget);
    let b = bed_ink(spectrum(j as f32 / 128.0), budget);
    let f = x - i as f32;
    let m = |sh: u32| {
        (((ch(a, sh) as f32) + ((ch(b, sh) as f32) - (ch(a, sh) as f32)) * f) + 0.5) as u32
    };
    (m(16) << 16) | (m(8) << 8) | m(0)
}
fn hot_lut(t: f32) -> u32 {
    hot_edge_ink(spectrum(tri(t).clamp(0.0, 1.0)))
}

fn main() {
    for (name, fg, bg) in [
        ("Nord", 0x00D8_DEE9u32, 0x002E_3440u32),
        ("Default", 0x00D0_D0D0, 0x0011_1318),
    ] {
        let budget = bed_luma_budget(fg);
        println!(
            "\n##### {name} budget={budget:.4} Ybg={:.4}",
            relative_luminance(bg)
        );
        println!(
            "{:>3} {:>6} {:>8} {:>8} {:>5} {:>6} {:>6} | strip(251) | hot-edge d<3",
            "d", "t", "ink", "lit236", "peak", "c/gnd", "dE"
        );
        for d in 0..=44 {
            let t = walk_t(d as f32);
            let ink = lut_ink(t, budget);
            let cov = UNDER_COV_CAP as u8;
            let lit = over_premul(bg, premul_rgb(ink, cov), cov);
            let strip_cov = (f32::from(cov)
                + (f32::from(cov) * STRIP_LIFT_GAIN)
                    .min(BODY_FRAME_TOP - f32::from(cov))
                    .max(0.0)) as u8;
            let strip = over_premul(bg, premul_rgb(ink, strip_cov), strip_cov);
            let hot = if d < 3 {
                let a = 0.38 * (1.0 - d as f32 / 3.0).powi(2);
                let c = (HOT_EDGE_COV_MAX * (a / 0.38)) as u8;
                let px = add_sat(lit, premul_rgb(hot_lut(t), c));
                format!("hot#{px:06X} peak{}", mx(px))
            } else {
                String::new()
            };
            println!(
                "{d:>3} {t:>6.3} #{ink:06X} #{lit:06X} {:>5} {:>6.2} {:>6.1} | #{strip:06X} | {hot}",
                mx(lit),
                contrast(lit, bg),
                dist(lit, bg)
            );
        }
        // cold cells
        println!("  cold (cov {}):", (UNDER_COV_CAP * BODY_COLD_SHARE) as u8);
        for d in [4u32, 8, 12, 20, 28, 36] {
            let ink = lut_ink(walk_t(d as f32), budget);
            let c = (UNDER_COV_CAP * BODY_COLD_SHARE) as u8;
            let lit = over_premul(bg, premul_rgb(ink, c), c);
            println!(
                "    d={d} #{lit:06X} peak={} c/gnd={:.2} dE={:.1}",
                mx(lit),
                contrast(lit, bg),
                dist(lit, bg)
            );
        }
    }

    // --- the bed over a NON-DEFAULT cell background ------------------------
    let fg = 0x00D8_DEE9u32; // Nord
    let budget = bed_luma_budget(fg);
    println!("\n##### Nord bed over NON-DEFAULT cell backgrounds (cov 236)");
    println!(
        "{:>22} {:>9} {:>7} | {:>9} {:>6} {:>6} {:>7}",
        "cell bg", "Ycell", "", "worst lit", "peak", "c/cell", "darker?"
    );
    let cellbgs: [(&str, u32); 12] = [
        ("theme bg #2E3440", 0x002E_3440),
        ("nord1 #3B4252", 0x003B_4252),
        ("nord2 #434C5E", 0x0043_4C5E),
        ("nord3/selection #4C566A", 0x004C_566A),
        ("nord9 blue #81A1C1", 0x0081_A1C1),
        ("nord8 cyan #88C0D0", 0x0088_C0D0),
        ("nord14 green #A3BE8C", 0x00A3_BE8C),
        ("nord11 red #BF616A", 0x00BF_616A),
        ("nord13 yellow #EBCB8B", 0x00EB_CB8B),
        ("fg/reverse #D8DEE9", 0x00D8_DEE9),
        ("white #FFFFFF", 0x00FF_FFFF),
        ("ansi bright black #4C566A", 0x004C_566A),
    ];
    for (label, cb) in cellbgs {
        // over each cell bg, take the arc's brightest and dimmest composited results
        let mut best = (0.0f32, 0u32);
        let mut worst = (1e9f32, 0u32);
        for i in 0..=128 {
            let ink = bed_ink(spectrum(i as f32 / 128.0), budget);
            let lit = over_premul(cb, premul_rgb(ink, 236), 236);
            let y = relative_luminance(lit);
            if y > best.0 {
                best = (y, lit);
            }
            if y < worst.0 {
                worst = (y, lit);
            }
        }
        let darker = relative_luminance(best.1) < relative_luminance(cb);
        println!(
            "{label:>22} {:>9.4} {:>7} | #{:06X} {:>6} {:>6.2} {:>7}",
            relative_luminance(cb),
            "",
            worst.1,
            mx(worst.1),
            contrast(worst.1, cb),
            if darker { "YES" } else { "no" }
        );
        println!(
            "{:>22} brightest arc stop composites #{:06X} (Y {:.4}) — c/cell {:.2}",
            "",
            best.1,
            best.0,
            contrast(best.1, cb)
        );
    }
}
