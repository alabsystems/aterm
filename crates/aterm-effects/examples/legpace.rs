// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! PER-LEG ANCHOR RESIDENCY AND MEAN STEP — the census finding 3 is judged on.
//!
//! * a WALK PHASE is a cohort's own walk origin, `t0 = p/P/16`;
//! * a PASS is the first sixteen cells, `d = 0..=16`, the cells a band shows;
//! * a phase RESIDES on anchor `A` when some cell of its pass lands within
//!   ten spectrum entries of `A` (`|arc − A| ≤ 10/510`) — the window under
//!   which the shipped identity walk resides on every named anchor 62.7 % of
//!   the time;
//! * the STEP of a leg is the CIE-Lab distance between one cell and the next,
//!   over every phase, counted into the leg its midpoint sits in.
use aterm_effects::rainbow_kitty::meteor::tri;
use aterm_effects::rainbow_kitty::ribbon::{
    UNDER_COV_CAP, bed_ink, bed_luma_budget, rail_ink, walk_arc, walk_t,
};
use aterm_effects::spectrum::{SPECTRUM_ANCHOR_AT, SPECTRUM_LUT_LEN, spectrum};
use aterm_render::{over_premul, premul_rgb};

const NORD_FG: u32 = 0x00D8_DEE9;
const NORD_BG: u32 = 0x002E_3440;
const PHASES: usize = 1000;
const WINDOW_ENTRIES: f32 = 10.0;
const NAMES: [&str; 7] = [
    "red", "orange", "yellow", "green", "blue", "indigo", "violet",
];
const LEGS: [&str; 6] = [
    "red>orange",
    "orange>yellow",
    "yellow>green",
    "green>blue",
    "blue>indigo",
    "indigo>violet",
];

fn s2l(v: f32) -> f32 {
    let v = v / 255.0;
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
fn lab(c: u32) -> (f32, f32, f32) {
    let ch = |sh: u32| ((c >> sh) & 0xff) as f32;
    let (r, g, b) = (s2l(ch(16)), s2l(ch(8)), s2l(ch(0)));
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
fn glass(ink: u32) -> u32 {
    let a = u8::try_from(UNDER_COV_CAP.clamp(0.0, 255.0) as u16).unwrap_or(255);
    over_premul(NORD_BG, premul_rgb(ink, a), a)
}
fn leg_of(a: f32) -> usize {
    let x = (a.clamp(0.0, 1.0) * (SPECTRUM_LUT_LEN - 1) as f32) as usize;
    for i in 0..6 {
        if x >= SPECTRUM_ANCHOR_AT[i] && x <= SPECTRUM_ANCHOR_AT[i + 1] {
            return i;
        }
    }
    5
}

fn main() {
    let budget = bed_luma_budget(NORD_FG);
    let span = (SPECTRUM_LUT_LEN - 1) as f32;
    let win = WINDOW_ENTRIES / span;
    let anchors: Vec<f32> = SPECTRUM_ANCHOR_AT
        .iter()
        .map(|&x| x as f32 / span)
        .collect();
    println!("# phases={PHASES} window=±{WINDOW_ENTRIES} entries of {SPECTRUM_LUT_LEN}");
    println!("# SHIPPED is the identity walk (v0.86.0); PACED is this build's walk.");

    let bed = |a: f32| glass(bed_ink(spectrum(a), budget));
    let rail = |a: f32| rail_ink(spectrum(a));

    let mut res_ship = [0usize; 7];
    let mut res_pace = [0usize; 7];
    let mut n = [0usize; 6];
    let mut bs = [0.0f64; 6];
    let mut bp = [0.0f64; 6];
    let mut bsx = [0.0f32; 6];
    let mut bpx = [0.0f32; 6];
    let mut rs = [0.0f64; 6];
    let mut rp = [0.0f64; 6];
    let mut rsx = [0.0f32; 6];
    let mut rpx = [0.0f32; 6];
    let mut moved = [0usize; 6];
    let (mut worst_b, mut worst_b_at) = (0.0f32, 0.0f32);
    let (mut worst_r, mut worst_r_at) = (0.0f32, 0.0f32);
    for p in 0..PHASES {
        let t0 = p as f32 / PHASES as f32 / 16.0;
        let sh: Vec<f32> = (0..=16)
            .map(|d| tri(t0 + walk_t(d as f32)).clamp(0.0, 1.0))
            .collect();
        let pa: Vec<f32> = (0..=16).map(|d| walk_arc(t0 + walk_t(d as f32))).collect();
        for (i, a) in anchors.iter().enumerate() {
            if sh.iter().any(|x| (x - a).abs() <= win) {
                res_ship[i] += 1;
            }
            if pa.iter().any(|x| (x - a).abs() <= win) {
                res_pace[i] += 1;
            }
        }
        for d in 0..16 {
            // THE BIN IS THE SHIPPED LEG, so the same steps are compared.
            let l = leg_of(0.5 * (sh[d] + sh[d + 1]));
            let (b0, b1) = (
                de(bed(sh[d]), bed(sh[d + 1])),
                de(bed(pa[d]), bed(pa[d + 1])),
            );
            let (r0, r1) = (
                de(rail(sh[d]), rail(sh[d + 1])),
                de(rail(pa[d]), rail(pa[d + 1])),
            );
            n[l] += 1;
            bs[l] += f64::from(b0);
            bp[l] += f64::from(b1);
            rs[l] += f64::from(r0);
            rp[l] += f64::from(r1);
            bsx[l] = bsx[l].max(b0);
            bpx[l] = bpx[l].max(b1);
            rsx[l] = rsx[l].max(r0);
            rpx[l] = rpx[l].max(r1);
            if (b0 - b1).abs() > 0.005 || (r0 - r1).abs() > 0.005 {
                moved[l] += 1;
            }
            if b1 > worst_b {
                worst_b = b1;
                worst_b_at = 0.5 * (pa[d] + pa[d + 1]) * span;
            }
            if r1 > worst_r {
                worst_r = r1;
                worst_r_at = 0.5 * (pa[d] + pa[d + 1]) * span;
            }
        }
    }
    println!("\nanchor residency — share of phases with a cell within the window");
    println!("  anchor   entry   SHIPPED   PACED");
    for (i, nm) in NAMES.iter().enumerate() {
        println!(
            "  {nm:<7} {:>5}   {:6.1}%  {:6.1}%",
            SPECTRUM_ANCHOR_AT[i],
            100.0 * res_ship[i] as f32 / PHASES as f32,
            100.0 * res_pace[i] as f32 / PHASES as f32
        );
    }
    println!("\nper-leg step, binned by the SHIPPED leg of the step's midpoint");
    println!(
        "  leg                n   bed mean  (was)   bed max  (was)   rail mean  (was)   rail max  (was)   steps changed"
    );
    for i in 0..6 {
        let d = n[i].max(1) as f64;
        println!(
            "  {:<14} {:>5}   {:7.2} ({:7.2})  {:7.2} ({:7.2})  {:8.2} ({:7.2})  {:8.2} ({:7.2})  {:>6}",
            LEGS[i],
            n[i],
            bp[i] / d,
            bs[i] / d,
            bpx[i],
            bsx[i],
            rp[i] / d,
            rs[i] / d,
            rpx[i],
            rsx[i],
            moved[i]
        );
    }
    println!(
        "\nworst step of this walk: bed {worst_b:.1} at entry {worst_b_at:.0}, rail {worst_r:.1} at entry {worst_r_at:.0}"
    );
    print!("phase-0 cell arc entries:");
    for d in 0..=16 {
        print!(" {}", (walk_arc(walk_t(d as f32)) * span).round() as i32);
    }
    println!();
}
