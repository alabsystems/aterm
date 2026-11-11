// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn params(temp: i32, lean: i32, phase: u32) -> FireFieldParams {
    FireFieldParams {
        base_y: 200,
        peak_h: 120,
        phase,
        temp,
        strength: 230,
        lean,
        cov_cap: 200,
        cell_h: 40,
        top_fade_y: 0,
    }
}

fn core_at(px: i32, py: i32, p: &FireFieldParams) -> FireCore {
    FireRow::new(py, px, p, &fire_precomp(p)).core(px)
}

#[test]
fn root_skirt_mirrors_compressed_below_the_root() {
    let p = FireFieldParams {
        base_y: 200,
        peak_h: 60,
        phase: 7,
        temp: 128,
        strength: 220,
        lean: 0,
        cov_cap: 200,
        cell_h: 16,
        top_fade_y: 0,
    };
    for px in [3, 40, 77] {
        for d in 1..=4 {
            let below = core_at(px, p.base_y + d, &p);
            let above = core_at(px, p.base_y - 5 * d, &p);
            assert_eq!(below, above, "5x mirror at d={d} px={px}");
        }
        let deep = core_at(px, p.base_y + p.peak_h / 5 + 2, &p);
        assert_eq!(deep.idx, 0, "skirt extinct below peak/5: px={px}");
    }
}

/// Under `cell_h >= 40` (`aa = max(cell_h / 4, 3) >= 10 px` and root lift
/// `1536 / cell_h <= 38.4`), vertical steps stay within `0.6 * cov_cap`.
#[test]
fn aa_law_no_hard_silhouette_edges_at_40px_and_above() {
    let mut worst = 0i32;
    for cell_h in [40, 56] {
        for temp in [51, 128, 230] {
            for phase in [0u32, 8192, 1 << 20] {
                let mut p = params(temp, -50, phase);
                p.cell_h = cell_h;
                p.peak_h = 3 * cell_h;
                p.base_y = 5 * cell_h;
                let bound = (p.cov_cap * 3) / 5; // 120 at cap 200
                let top_y = p.base_y - 4 * cell_h;
                for px in 0..200 {
                    let mut prev = 0i32;
                    for py in top_y..=p.base_y {
                        let c = core_at(px, py, &p);
                        let cov = fire_cov_add(&c, &p).max(fire_alpha_over(&c, &p));
                        let delta = (cov - prev).abs();
                        assert!(
                            delta <= bound,
                            "cov step {delta} > {bound} at ({px},{py}) cell_h={cell_h} temp={temp} phase={phase}"
                        );
                        worst = worst.max(delta);
                        prev = cov;
                    }
                }
            }
        }
    }
    eprintln!("aa law: worst adjacent cov/alpha step = {worst} (bound 120, cap 200)");
}

#[test]
fn fire_row_matches_stateless_vnoise_and_stitches_seams() {
    let p = params(180, -37, (1 << 24) - 256);
    let pc = fire_precomp(&p);

    for py in [90, 145, 200, 204] {
        let mut full_row = FireRow::new(py, -32, &p, &pc);
        let mut left_half = FireRow::new(py, -32, &p, &pc);
        let mut right_half = FireRow::new(py, 64, &p, &pc);
        for px in -32..160 {
            let full = full_row.core(px);
            let fresh = core_at(px, py, &p);
            assert_eq!(full, fresh, "stateless equality at ({px},{py})");
            let stitched = if px < 64 {
                left_half.core(px)
            } else {
                right_half.core(px)
            };
            assert_eq!(stitched, full, "patch seam equality at ({px},{py})");
        }

        let mut sparse = FireRow::new(py, -32, &p, &pc);
        for px in [-32, -31, 0, 19, 63, 64, 159, 512] {
            assert_eq!(
                sparse.core(px),
                core_at(px, py, &p),
                "multi-cell jump equality at ({px},{py})"
            );
        }
    }

    let mut oct = FireOctave::new(0x3FFF << 8 | 137, 0x3FFF, FIRE_SEED_R0, 12);
    for x in [12u32, 13, 255, 256, 777, 4096] {
        assert_eq!(
            oct.sample(x),
            fire_vnoise(x, 0x3FFF << 8 | 137, 0x3FFF, FIRE_SEED_R0),
            "FireOctave::sample refines fire_vnoise at x={x}"
        );
    }
}

#[test]
fn field_is_total_on_degenerate_params() {
    let degens = [
        FireFieldParams {
            base_y: 0,
            peak_h: 0,
            phase: u32::MAX,
            temp: 255,
            strength: 255,
            lean: -128,
            cov_cap: 255,
            cell_h: 0,
            top_fade_y: 0,
        },
        FireFieldParams {
            base_y: 65535 + 64,
            peak_h: 65535,
            phase: u32::MAX,
            temp: 255,
            strength: 255,
            lean: 127,
            cov_cap: 255,
            cell_h: 65535,
            top_fade_y: 0,
        },
        FireFieldParams {
            base_y: 100,
            peak_h: 1,
            phase: 0,
            temp: 0,
            strength: 0,
            lean: 0,
            cov_cap: 0,
            cell_h: 2,
            top_fade_y: 0,
        },
    ];
    for p in &degens {
        let pc = fire_precomp(p);
        for py in 0..80 {
            let tf = fire_top_fade(py, p);
            for (lo, hi) in [(0, 65), (16300, 16384)] {
                let mut row = FireRow::new(py, lo, p, &pc);
                for px in lo..hi {
                    let c = row.core(px);
                    let _ = fire_shade_add(&c, p, tf);
                    let _ = fire_shade_over(&c, p, tf);
                }
            }
        }
    }
}

#[test]
fn field_burns_with_structure() {
    let p = params(230, 0, 4096);
    let mut lit = 0usize;
    let mut root_idx_max = 0i32;
    let mut tip_idx_max = 0i32;
    for px in 0..300 {
        for py in 60..200 {
            let c = core_at(px, py, &p);
            if fire_cov_add(&c, &p) > 0 {
                lit += 1;
                if c.q < 48 {
                    root_idx_max = root_idx_max.max(c.idx);
                }
                if c.q > 208 {
                    tip_idx_max = tip_idx_max.max(c.idx);
                }
            }
        }
    }
    assert!(
        lit > 4000,
        "a hot 300px burn must be substantially lit ({lit})"
    );
    assert!(
        root_idx_max > 700,
        "the root must reach the amber/white palette range ({root_idx_max})"
    );
    assert!(
        tip_idx_max < root_idx_max,
        "tips must stay cooler than the root ({tip_idx_max} vs {root_idx_max})"
    );
}

#[test]
fn the_hottest_light_bounds_every_additive_sample() {
    let over = |bound: u32| {
        let mut over = 0usize;
        for temp in [0, 128, 255] {
            let p = params(temp, 0, 7_000);
            for px in (0..300).step_by(3) {
                for py in (40..200).step_by(2) {
                    let c = core_at(px, py, &p);
                    let cov = fire_cov_add(&c, &p);
                    let lit = fire_shade_add(&c, &p, 255);
                    let ch = |v: u32, sh: u32| (v >> sh) & 0xff;
                    if [16, 8, 0]
                        .iter()
                        .any(|&sh| ch(lit, sh) > (ch(bound, sh) * cov as u32 + 127) / 255)
                    {
                        over += 1;
                    }
                }
            }
        }
        over
    };
    assert_eq!(
        over(FIRE_ADD_HOTTEST),
        0,
        "a sample outshines the hottest light"
    );
    let amber = {
        let [r, g, b] = FIRE_PAL_ADD[3];
        ((r as u32) << 16) | ((g as u32) << 8) | (b as u32)
    };
    assert!(
        over(amber) > 0,
        "control: the hot cores pass the amber anchor"
    );
}

#[test]
fn additive_fire_laid_thin_over_a_dark_page_reads_amber_not_tan() {
    // Held a breath past amber, thin additive fire over a dark page reads tan (warm, greyed,
    // mid-bright) in only a few samples at the band's edge; up to the field's own ceiling
    // (the negative control), many times as many do.
    const PAGE: u32 = 0x0011_1318;
    let tan = |cap: i32| {
        let mut tan = 0usize;
        for temp in [128, 200, 255] {
            for phase in [7_000u32, 91_337, 400_001] {
                let p = params(temp, 0, phase);
                for px in (0..300).step_by(2) {
                    for py in (40..230).step_by(2) {
                        let c = core_at(px, py, &p);
                        let lit = fire_shade_add_to(&c, &p, 255, cap);
                        let ch = |sh: u32| {
                            (((PAGE >> sh) & 0xff) + ((lit >> sh) & 0xff)).min(255) as f32 / 255.0
                        };
                        let (r, g, b) = (ch(16), ch(8), ch(0));
                        let (hi, lo) = (r.max(g).max(b), r.min(g).min(b));
                        if hi <= 0.5 || hi >= 0.86 || r < hi {
                            continue;
                        }
                        let sat = (hi - lo) / hi;
                        let hue = 60.0 * (g - b) / (hi - lo);
                        if (0.30..=0.72).contains(&sat) && (25.0..=55.0).contains(&hue) {
                            tan += 1;
                        }
                    }
                }
            }
        }
        tan
    };
    let (held, pale) = (tan(FIRE_ADD_IDX_MAX), tan(1023));
    assert!(held <= 400, "thin additive fire reads tan {held} times");
    assert!(
        pale >= 8 * held.max(250),
        "control: up to the field's own ceiling, thin fire reads tan only {pale} times ({held} held)"
    );
}

#[test]
fn the_light_at_a_height_bounds_every_sample_on_it() {
    let over = |up: i32| {
        let mut over = 0usize;
        for (temp, strength, lean) in [(0, 255, 0), (128, 230, 40), (255, 255, -60)] {
            let mut p = params(temp, lean, 7_000);
            p.strength = strength;
            for phase in [7_000u32, 91_337, 400_001] {
                p.phase = phase;
                for py in (40..230).step_by(2) {
                    let bound = fire_add_hottest_at(&p, py - up);
                    for px in (0..300).step_by(2) {
                        let c = core_at(px, py, &p);
                        let cov = fire_cov_add(&c, &p);
                        let lit = fire_shade_add(&c, &p, 255);
                        let ch = |v: u32, sh: u32| (v >> sh) & 0xff;
                        if [16, 8, 0]
                            .iter()
                            .any(|&sh| ch(lit, sh) > (ch(bound, sh) * cov as u32 + 127) / 255 + 1)
                        {
                            over += 1;
                        }
                    }
                }
            }
        }
        over
    };
    assert_eq!(over(0), 0, "a sample outshines its scanline's bound");
    let p = params(128, 0, 7_000);
    let high = fire_add_hottest_at(&p, p.base_y - p.peak_h * 3 / 4);
    assert!(
        (high >> 8) & 0xff < 0x60,
        "three quarters up a tongue its light is still #{high:06x}"
    );
    assert!(over(40) > 0, "control: the bound read higher up is passed");
}
