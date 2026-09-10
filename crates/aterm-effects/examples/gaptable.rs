// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! D1 final table: the metric that predicts the visual is PEAK CHANNEL ABOVE
//! THE GROUND'S OWN PEAK (the bed is luminance-equalized, so Y is flat by
//! construction and cannot discriminate).
use aterm_effects::rainbow_kitty::ribbon::{
    BED_LUMA_MIN, UNDER_COV_CAP, bed_ink, bed_luma_budget, relative_luminance, walk_t,
};
use aterm_effects::spectrum::spectrum;
use aterm_render::{over_premul, premul_rgb};

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
fn hue_name(t: f32) -> &'static str {
    match (t * 100.0) as u32 {
        0..=9 => "red",
        10..=19 => "orange",
        20..=29 => "yellow",
        30..=44 => "yellow-green",
        45..=55 => "green",
        56..=66 => "green-teal",
        67..=74 => "blue",
        75..=89 => "indigo",
        _ => "violet",
    }
}
fn main() {
    let themes: [(&str, u32, u32); 6] = [
        ("Nord (owner)", 0x00D8_DEE9, 0x002E_3440),
        ("Default", 0x00D0_D0D0, 0x0011_1318),
        ("Tokyo Night", 0x00C8_D3F5, 0x001A_1B26),
        ("Dracula", 0x00F8_F8F2, 0x0028_2A36),
        ("Gruvbox Dark", 0x00EB_DBB2, 0x0028_2828),
        ("Solarized Dark", 0x0083_9496, 0x0000_2B36),
    ];
    println!(
        "PEAK-OVER-GROUND, per theme (cov {}, the body's cap)",
        UNDER_COV_CAP as u32
    );
    println!(
        "{:>16} {:>7} {:>6} {:>5} | {:>26} | {:>26} | swing",
        "theme", "budget", "gndPk", "clamp", "DIMMEST stop", "BRIGHTEST stop"
    );
    for (name, fg, bg) in themes {
        let budget = bed_luma_budget(fg);
        let clamped = budget <= BED_LUMA_MIN + 1e-6;
        let gp = mx(bg);
        let mut lo = (999i64, 0.0f32, 0u32);
        let mut hi = (-999i64, 0.0f32, 0u32);
        for i in 0..=256 {
            let t = i as f32 / 256.0;
            let lit = over_premul(bg, premul_rgb(bed_ink(spectrum(t), budget), 236), 236);
            let d = mx(lit) as i64 - gp as i64;
            if d < lo.0 {
                lo = (d, t, lit);
            }
            if d > hi.0 {
                hi = (d, t, lit);
            }
        }
        println!(
            "{name:>16} {budget:>7.4} {gp:>6} {:>5} | +{:<3} #{:06X} t{:.2} {:<12} | +{:<3} #{:06X} t{:.2} {:<10} | {:.1}x",
            if clamped { "MIN" } else { "bar" },
            lo.0,
            lo.2,
            lo.1,
            hue_name(lo.1),
            hi.0,
            hi.2,
            hi.1,
            hue_name(hi.1),
            (hi.0 as f32 + gp as f32) / (lo.0 as f32 + gp as f32)
        );
    }
    println!(
        "\nNORD, cell by cell behind the mark's first cell (what the owner reads as 'a few characters back'):"
    );
    let (fg, bg) = (0x00D8_DEE9u32, 0x002E_3440u32);
    let budget = bed_luma_budget(fg);
    let gp = mx(bg);
    print!("  d:");
    for d in 0..24 {
        print!(" {d:>3}");
    }
    println!();
    print!("  +:");
    for d in 0..24 {
        let lit = over_premul(
            bg,
            premul_rgb(bed_ink(spectrum(walk_t(d as f32).min(1.0)), budget), 236),
            236,
        );
        print!(" {:>3}", mx(lit) as i64 - gp as i64);
    }
    println!("   <- peak channel ABOVE the ground's own peak ({gp})");
    println!("\nNORD, bed over a CELL BACKGROUND that is not the theme background (cov 236):");
    println!(
        "{:>28} {:>7} {:>9} {:>9} {:>9}",
        "cell background", "Ycell", "Ybed(max)", "darker by", "verdict"
    );
    for (label, cb) in [
        ("theme bg #2E3440", 0x002E_3440u32),
        ("nord1 #3B4252 (panel)", 0x003B_4252),
        ("nord3 #4C566A (selection)", 0x004C_566A),
        ("nord9 #81A1C1 (blue bg)", 0x0081_A1C1),
        ("nord13 #EBCB8B (yellow bg)", 0x00EB_CB8B),
        ("reverse video #D8DEE9", 0x00D8_DEE9),
        ("white #FFFFFF", 0x00FF_FFFF),
    ] {
        let mut best = 0.0f32;
        for i in 0..=256 {
            let lit = over_premul(
                cb,
                premul_rgb(bed_ink(spectrum(i as f32 / 256.0), budget), 236),
                236,
            );
            best = best.max(relative_luminance(lit));
        }
        let yc = relative_luminance(cb);
        let v = if best < yc { "DARKENS" } else { "lifts" };
        println!(
            "{label:>28} {yc:>7.4} {best:>9.4} {:>9.2} {v:>9}",
            if best < yc {
                contrast(
                    cb,
                    over_premul(cb, premul_rgb(bed_ink(spectrum(0.5), budget), 236), 236),
                )
            } else {
                0.0
            }
        );
    }
    let _ = fg;
}
