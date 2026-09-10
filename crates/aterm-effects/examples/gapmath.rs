// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Colour-math sweep for D1 (black gaps). Prints tables; no test assertions.
use aterm_effects::rainbow_kitty::ribbon::{
    BED_LUMA_MAX, BED_LUMA_MIN, BODY_COLD_SHARE, BODY_CONTRAST_BAR, BODY_FRAME_TOP,
    HOT_EDGE_COV_MAX, UNDER_COV_CAP, bed_ink, bed_luma_budget, hot_edge_ink, relative_luminance,
};
use aterm_effects::spectrum::spectrum;
use aterm_render::{add_sat, over_premul, premul_rgb};

fn ch(c: u32, sh: u32) -> u32 {
    (c >> sh) & 0xff
}
fn mx(c: u32) -> u32 {
    ch(c, 16).max(ch(c, 8)).max(ch(c, 0))
}
fn mn(c: u32) -> u32 {
    ch(c, 16).min(ch(c, 8)).min(ch(c, 0))
}
fn sat(c: u32) -> f32 {
    let m = mx(c);
    if m == 0 {
        0.0
    } else {
        (m - mn(c)) as f32 / m as f32
    }
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

fn main() {
    println!(
        "CONSTS bar={BODY_CONTRAST_BAR} cov_cap={UNDER_COV_CAP} frame_top={BODY_FRAME_TOP} cold={BODY_COLD_SHARE} luma[{BED_LUMA_MIN},{BED_LUMA_MAX}] hot_cov={HOT_EDGE_COV_MAX}"
    );
    let themes: [(&str, u32, u32); 6] = [
        ("Nord", 0x00D8_DEE9, 0x002E_3440),
        ("Default", 0x00D0_D0D0, 0x0011_1318),
        ("TokyoNight", 0x00C8_D3F5, 0x001A_1B26),
        ("Dracula", 0x00F8_F8F2, 0x0028_2A36),
        ("SolarDark", 0x0083_9496, 0x0000_2B36),
        ("GruvDark", 0x00EB_DBB2, 0x0028_2828),
    ];
    for (name, fg, bg) in themes {
        let budget = bed_luma_budget(fg);
        println!(
            "\n=== {name} fg=#{fg:06X} bg=#{bg:06X} Ybg={:.4} Yfg={:.4} budget={budget:.4} bar_raw={:.4}",
            relative_luminance(bg),
            relative_luminance(fg),
            (relative_luminance(fg) + 0.05) / (BODY_CONTRAST_BAR + 0.05) - 0.05
        );
        println!(
            "{:>5} {:>9} {:>9} {:>5} | {:>9} {:>4} {:>6} {:>6} {:>6} | glyph-over(cov236)",
            "t", "stop", "ink", "S", "cov236", "peak", "Y", "c/gnd", "dE"
        );
        let mut worst: Vec<(f32, f32, u32, u32)> = vec![];
        for i in 0..=64 {
            let t = i as f32 / 64.0;
            let stop = spectrum(t);
            let ink = bed_ink(stop, budget);
            let cov = UNDER_COV_CAP as u8;
            let lit = over_premul(bg, premul_rgb(ink, cov), cov);
            let cg = contrast(lit, bg);
            worst.push((cg, t, ink, lit));
            if i % 2 == 0 {
                println!(
                    "{t:>5.3} #{stop:06X} #{ink:06X} {:>5.2} | #{lit:06X} {:>4} {:>6.4} {:>6.2} {:>6.1} |",
                    sat(ink),
                    mx(lit),
                    relative_luminance(lit),
                    cg,
                    dist(lit, bg)
                );
            }
        }
        worst.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap());
        println!("  WORST contrast-vs-ground stops:");
        for w in worst.iter().take(4) {
            println!(
                "    t={:.3} ink=#{:06X} lit=#{:06X} c/gnd={:.2} dE={:.1} peak={}",
                w.1,
                w.2,
                w.3,
                w.0,
                dist(w.3, bg),
                mx(w.3)
            );
        }
        // coverage ramp on the worst stop and on a vivid stop
        for (label, t) in [
            ("worst", worst[0].1),
            ("vivid-red", 0.0f32),
            ("blue", 0.62f32),
        ] {
            let ink = bed_ink(spectrum(t), budget);
            print!("  cov ramp {label} t={t:.3} ink=#{ink:06X}:");
            for cov in [4u8, 8, 16, 32, 64, 128, 190, 207, 236, 251] {
                let lit = over_premul(bg, premul_rgb(ink, cov), cov);
                print!(
                    " {cov}:#{lit:06X}(c{:.2},dE{:.0})",
                    contrast(lit, bg),
                    dist(lit, bg)
                );
            }
            println!();
        }
        // over a glyph pixel (fully inked fg) at full cov
        println!("  over-glyph (dst=fg) at cov236 / cov207:");
        for t in [0.0f32, 0.25, 0.45, 0.5, 0.62, 0.75, 1.0] {
            let ink = bed_ink(spectrum(t), budget);
            let g236 = over_premul(fg, premul_rgb(ink, 236), 236);
            let g207 = over_premul(fg, premul_rgb(ink, 207), 207);
            println!(
                "    t={t:.2} ink=#{ink:06X} glyph236=#{g236:06X} c(vs bedOnBg)={:.2} glyph207=#{g207:06X}",
                contrast(g236, over_premul(bg, premul_rgb(ink, 236), 236)),
            );
        }
        // hot edge additive over the bed
        println!("  hot edge additive over bed(cov236):");
        for t in [0.0f32, 0.25, 0.45, 0.5, 0.62, 0.75, 1.0] {
            let stop = spectrum(t);
            let hot = hot_edge_ink(stop);
            let bed = over_premul(bg, premul_rgb(bed_ink(stop, budget), 236), 236);
            let px = add_sat(bed, premul_rgb(hot, HOT_EDGE_COV_MAX as u8));
            println!(
                "    t={t:.2} hot=#{hot:06X} Yhot={:.3} px=#{px:06X} peak={} c/gnd={:.2}",
                relative_luminance(hot),
                mx(px),
                contrast(px, bg)
            );
        }
    }
}
