// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WALK, CELL BY CELL — the stop each cell of a band takes, the bed ink
//! and the rail ink it composites to on the owner's Nord ground, and the
//! CIEDE-ish step between one cell and the next. The question this answers is
//! the owner's fourth: is the GREEN-TO-BLUE step bigger than its neighbours,
//! and where does the size come from — the arc's own pace or the walk's?
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::{
    UNDER_COV_CAP, bed_ink, bed_luma_budget, rail_ink, relative_luminance, walk_arc, walk_t,
};
use aterm_effects::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN, spectrum, spectrum_hsv};
use aterm_render::{over_premul, premul_rgb};

const NORD_FG: u32 = 0x00D8_DEE9;
const NORD_BG: u32 = 0x002E_3440;

fn ch(c: u32, sh: u32) -> f32 {
    ((c >> sh) & 0xff) as f32
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
        if t > 0.008_856 {
            t.cbrt()
        } else {
            7.787 * t + 16.0 / 116.0
        }
    };
    let (fx, fy, fz) = (f(x / 0.95047), f(y), f(z / 1.08883));
    (116.0 * fy - 16.0, 500.0 * (fx - fy), 200.0 * (fy - fz))
}
fn de(a: u32, b: u32) -> f32 {
    let (l1, a1, b1) = lab(a);
    let (l2, a2, b2) = lab(b);
    ((l1 - l2).powi(2) + (a1 - a2).powi(2) + (b1 - b2).powi(2)).sqrt()
}
fn hexs(c: u32) -> String {
    format!(
        "#{:06X}({:3},{:3},{:3})",
        c & 0x00ff_ffff,
        (c >> 16) & 0xff,
        (c >> 8) & 0xff,
        c & 0xff
    )
}
/// The bed as the glass shows it: the ink composited source-over the ground
/// at the bed's published ceiling.
fn glass(ink: u32, cov: f32) -> u32 {
    let a = cov.clamp(0.0, 255.0) as u16;
    let a = u8::try_from(a).unwrap_or(255);
    over_premul(NORD_BG, premul_rgb(ink, a), a)
}

fn leg_of(t: f32) -> &'static str {
    let names = [
        "red>orange",
        "orange>yellow",
        "yellow>green",
        "green>blue",
        "blue>indigo",
        "indigo>violet",
    ];
    let x = (t.clamp(0.0, 1.0) * (SPECTRUM_LUT_LEN - 1) as f32) as usize;
    for i in 0..6 {
        if x >= SPECTRUM_ANCHOR_AT[i] && x <= SPECTRUM_ANCHOR_AT[i + 1] {
            return names[i];
        }
    }
    "?"
}

fn main() {
    let budget = bed_luma_budget(NORD_FG);
    println!(
        "# Nord fg {} bg {}  bed_luma_budget={budget:.4}  UNDER_COV_CAP={UNDER_COV_CAP}",
        hexs(NORD_FG),
        hexs(NORD_BG)
    );
    println!("# SPECTRUM_ANCHOR_AT = {SPECTRUM_ANCHOR_AT:?} of {SPECTRUM_LUT_LEN}");
    let widths: Vec<usize> = (0..6)
        .map(|i| SPECTRUM_ANCHOR_AT[i + 1] - SPECTRUM_ANCHOR_AT[i])
        .collect();
    println!("# leg widths in LUT entries = {widths:?}");
    let cell_t = 1.0f32 / 16.0;
    // THERE IS NO EXEMPT CROSSING SPAN any more (2026-09-15): the green→blue
    // leg is paced like every other one, so the only question left about its
    // width is the leg's own.
    println!(
        "# the walk spends {cell_t:.4} of t per cell ({:.1} LUT entries)",
        cell_t * (SPECTRUM_LUT_LEN - 1) as f32
    );
    println!(
        "# the whole green>blue leg is {} entries = {:.2} cells",
        widths[3],
        widths[3] as f32 / (cell_t * (SPECTRUM_LUT_LEN - 1) as f32)
    );
    println!();
    println!(
        "d  t      lut   leg            stop                   S     bedink                 glass                  dE_glass  railink                dE_rail"
    );
    let cells = 40usize;
    let mut prev_glass: Option<u32> = None;
    let mut prev_rail: Option<u32> = None;
    let mut by_leg: std::collections::BTreeMap<&'static str, Vec<(usize, f32, f32)>> =
        Default::default();
    for d in 0..=cells {
        // THE ARC THE BAND ACTUALLY READS ([`walk_arc`]): `tri` through the
        // walk's pace, which is where a cell lands on the table.
        let t = walk_arc(walk_t(d as f32));
        let stop = spectrum(t);
        let (_, s, _) = spectrum_hsv(stop);
        let bed = bed_ink(stop, budget);
        let g = glass(bed, UNDER_COV_CAP);
        let rail = rail_ink(stop);
        let dg = prev_glass.map_or(0.0, |p| de(p, g));
        let dr = prev_rail.map_or(0.0, |p| de(p, rail));
        let leg = leg_of(t);
        println!(
            "{d:<2} {t:.4} {:>4}   {leg:<14} {:<22} {s:.2}  {:<22} {:<22} {dg:8.2}  {:<22} {dr:7.2}",
            (t * (SPECTRUM_LUT_LEN - 1) as f32) as usize,
            hexs(stop),
            hexs(bed),
            hexs(g),
            hexs(rail)
        );
        if d > 0 && (d as f32) <= aterm_effects::rainbow_kitty::ribbon::WALK_FAST_CELLS {
            by_leg.entry(leg).or_default().push((d, dg, dr));
        }
        prev_glass = Some(g);
        prev_rail = Some(rail);
    }
    println!();
    println!("# per-leg step sizes over the FIRST pass of the walk (one full arc)");
    println!("leg             n  dE_glass(mean/max)   dE_rail(mean/max)");
    for (leg, v) in &by_leg {
        let first: Vec<_> = v.iter().filter(|(d, _, _)| *d <= 16).copied().collect();
        if first.is_empty() {
            continue;
        }
        let n = first.len() as f32;
        let mg = first.iter().map(|x| x.1).sum::<f32>() / n;
        let xg = first.iter().map(|x| x.1).fold(0.0f32, f32::max);
        let mr = first.iter().map(|x| x.2).sum::<f32>() / n;
        let xr = first.iter().map(|x| x.2).fold(0.0f32, f32::max);
        println!(
            "{leg:<14} {:>2}  {mg:7.2} / {xg:7.2}   {mr:7.2} / {xr:7.2}",
            first.len()
        );
    }
    println!();
    println!(
        "# the crossing under a microscope: the drawn stop, its saturation, and what BED_SAT_FLOOR does to it"
    );
    println!(
        "lut   t      stop                   S_auth  after_sat_floor        S_after  bed                    rail"
    );
    for x in (SPECTRUM_ANCHOR_AT[2]..=SPECTRUM_ANCHOR_AT[4]).step_by(8) {
        let t = x as f32 / (SPECTRUM_LUT_LEN - 1) as f32;
        let stop = spectrum(t);
        let (_, s0, _) = spectrum_hsv(stop);
        let bed = bed_ink(stop, budget);
        let rail = rail_ink(stop);
        let (_, s1, _) = spectrum_hsv(rail);
        println!(
            "{x:>4}  {t:.4} {:<22} {s0:.3}   {:<22} {s1:.3}   {:<22} {:<22}",
            hexs(stop),
            hexs(rail),
            hexs(bed),
            hexs(rail)
        );
    }
    println!();
    println!("# luminance of the glass bed per cell (the 'bright not dim' read)");
    for d in 0..=16 {
        let t = tri(walk_t(d as f32));
        let g = glass(bed_ink(spectrum(t), budget), UNDER_COV_CAP);
        println!(
            "d={d:<2} t={t:.4} leg={:<14} Y={:.4} {}",
            leg_of(t),
            relative_luminance(g),
            hexs(g)
        );
    }
}
