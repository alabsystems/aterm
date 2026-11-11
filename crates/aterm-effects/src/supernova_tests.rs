// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;

fn max_cell_shade_alpha(decos: &[WordDecoration]) -> u32 {
    decos
        .iter()
        .filter(|d| matches!(d.blend, DecoBlend::Over) && matches!(d.glyph, DecoGlyph::Shade))
        .map(|d| {
            decos
                .iter()
                .filter(|other| {
                    matches!(other.blend, DecoBlend::Over)
                        && matches!(other.glyph, DecoGlyph::Shade)
                        && other.row == d.row
                        && other.col == d.col
                })
                .map(|other| u32::from(other.alpha))
                .sum()
        })
        .max()
        .unwrap_or(0)
}

const fn env(ch: i32, light: bool, rows: i32) -> SuperEnv {
    let cw = if ch / 2 > 1 { ch / 2 } else { 1 };
    let cols = 240i32;
    SuperEnv {
        grid_w: cols * cw,
        grid_h: rows * ch,
        cell_w: cw,
        cell_h: ch,
        cx: cols * cw / 2,
        cy: rows * ch / 2,
        r_max: r_max_for(ch, rows * ch),
        row: (rows / 2) as u16,
        start_col: 118,
        end_col: 121,
        cols: cols as u16,
        light,
        intensity: 1.0,
        seed: 0xDECAF_C0FFEE,
        base_hue: 42.0,
    }
}

#[test]
fn r_max_for_matches_engine_clamp() {
    assert_eq!(r_max_for(20, 64 * 20), 6.0 * 20.0);
    assert_eq!(r_max_for(20, 8 * 20), 4.0 * 20.0);
    assert_eq!(r_max_for(0, 0), 1.0);
}

#[test]
fn super_budget_s_max_bound() {
    for ch in [14i32, 20, 40, 56] {
        for rows in [64i32, 160] {
            for light in [false, true] {
                let e = env(ch, light, rows);
                for t in (0..SUPER_TOTAL_MS).step_by(10) {
                    let mut capped = Vec::new();
                    let counts = emit_super(t, &e, MAX_SUPER_QUADS_PER, &mut capped);
                    let mut free = Vec::new();
                    emit_super(t, &e, usize::MAX, &mut free);
                    assert_eq!(
                        capped, free,
                        "budget bound at t={t} ch={ch} rows={rows} light={light}"
                    );
                    let worst = capped.len() + 2 * counts.wash;
                    assert!(
                        worst <= S_MAX_BOUND,
                        "S_max: {worst} > {S_MAX_BOUND} at t={t} ch={ch} rows={rows} light={light} ({counts:?})"
                    );
                    assert!(capped.len() <= MAX_SUPER_QUADS_PER);
                    assert!(
                        peak_additive_channel(&capped) <= MAX_VIEWPORT_OVERLAY,
                        "aggregate additive bound at t={t} ch={ch} rows={rows} light={light}"
                    );
                    for q in &capped {
                        assert!(i32::from(q.h) <= ch, "band split at t={t}");
                        assert_eq!(i32::from(q.y) / ch, i32::from(q.row));
                    }
                }
            }
        }
    }
}

#[test]
fn super_deco_stream_bounded() {
    for light in [false, true] {
        let e = env(20, light, 64);
        for t in (0..SUPER_TOTAL_MS).step_by(10) {
            let mut out = Vec::new();
            emit_super_decos(t, &e, &mut out, 256);
            assert!(out.len() <= 256, "t={t} light={light}: {}", out.len());
            assert!(
                max_cell_shade_alpha(&out) <= MAX_VIEWPORT_OVERLAY,
                "aggregate Shade opacity at t={t} light={light}"
            );
            if light && t < CHARGE_END_MS {
                assert!(
                    out.len() <= CHARGE_MOTES,
                    "light charge ≤ {CHARGE_MOTES} decos, got {} at t={t}",
                    out.len()
                );
            }
            if light && (CHARGE_END_MS..DETONATION_END_MS).contains(&t) {
                assert!(
                    out.iter()
                        .filter(|d| matches!(d.blend, DecoBlend::Over))
                        .count()
                        <= MAX_VEIL_CELLS + 24,
                    "veil cap"
                );
            }
            for d in &out {
                if !light {
                    assert!(
                        matches!(d.blend, DecoBlend::Add),
                        "dark decos stay additive, got {:?} at t={t}",
                        d.blend
                    );
                }
                assert_eq!(
                    d.color == VEIL_TONE,
                    matches!(d.glyph, DecoGlyph::Shade),
                    "veil-tone ⇔ Shade (§3.3), got {:?}/{:06x} at t={t}",
                    d.glyph,
                    d.color
                );
                if matches!(d.glyph, DecoGlyph::Shade) {
                    assert!(
                        matches!(d.blend, DecoBlend::Over),
                        "Shade stamps blend Over at t={t}"
                    );
                }
            }
        }
    }
}

#[test]
fn detonation_is_theme_branched() {
    let t = 500u64;
    let (dark, light) = (env(20, false, 64), env(20, true, 64));
    let mut dq = Vec::new();
    let dc = emit_super(t, &dark, MAX_SUPER_QUADS_PER, &mut dq);
    assert!(dc.wash > 0 && dc.crown > 0, "dark: wash + crown ({dc:?})");
    let mut lq = Vec::new();
    let lc = emit_super(t, &light, MAX_SUPER_QUADS_PER, &mut lq);
    assert_eq!(lc.wash, 0, "light: additive wash is INVISIBLE on white");
    let mut ld = Vec::new();
    emit_super_decos(t, &light, &mut ld, 256);
    let veil = ld
        .iter()
        .filter(|d| matches!(d.blend, DecoBlend::Over))
        .count();
    assert!(
        veil > 100,
        "light: the Over-blend dark veil ({veil} stamps)"
    );
    let mut dd = Vec::new();
    emit_super_decos(t, &dark, &mut dd, 256);
    assert!(dd.is_empty(), "dark detonation emits no veil");
}

#[test]
fn detonation_has_a_hard_aggregate_readability_ceiling() {
    let t = 500;

    let raw_wash = max_channel(premul(WASH_CORE, 0.9));
    assert!(
        raw_wash > MAX_VIEWPORT_OVERLAY,
        "negative control must exceed the new ceiling: {raw_wash}"
    );
    let dark = env(20, false, 64);
    let mut quads = Vec::new();
    let counts = emit_super(t, &dark, MAX_SUPER_QUADS_PER, &mut quads);
    assert!(counts.wash > 0 && counts.crown > 0, "real peak emitted");
    let bounded_peak = peak_additive_channel(&quads);
    assert!(
        (MAX_VIEWPORT_OVERLAY / 2..=MAX_VIEWPORT_OVERLAY).contains(&bounded_peak),
        "the peak stays vivid but cannot cross the aggregate light ceiling: {bounded_peak}"
    );

    let add_bound = |rgb: u32| {
        let add = |shift: u32| (((rgb >> shift) & 0xff) + MAX_VIEWPORT_OVERLAY).min(255) << shift;
        add(16) | add(8) | add(0)
    };
    let theme = aterm_render::Theme::default();
    let lit_fg = crate::color_math::relative_luminance(add_bound(theme.fg));
    let lit_bg = crate::color_math::relative_luminance(add_bound(theme.bg));
    let contrast = (lit_fg + 0.05) / (lit_bg + 0.05);
    assert!(contrast >= 4.5, "default-theme contrast fell to {contrast}");

    let raw_shade_alpha = 235u32;
    assert!(raw_shade_alpha > MAX_VIEWPORT_OVERLAY);
    let light = env(20, true, 64);
    let mut decos = Vec::new();
    emit_super_decos(t, &light, &mut decos, 256);
    assert!(
        decos.iter().any(|d| matches!(d.glyph, DecoGlyph::Shade)),
        "real eclipse emitted"
    );
    let bounded_alpha = max_cell_shade_alpha(&decos);
    assert!(
        (MAX_VIEWPORT_OVERLAY / 2..=MAX_VIEWPORT_OVERLAY).contains(&bounded_alpha),
        "the eclipse stays visible but cannot cross the aggregate opacity ceiling: {bounded_alpha}"
    );

    let mut debris = Vec::new();
    emit_super_decos(2000, &light, &mut debris, 256);
    assert!(
        debris.iter().any(|d| {
            !matches!(d.glyph, DecoGlyph::Shade)
                && matches!(d.blend, DecoBlend::Over)
                && u32::from(d.alpha) > MAX_VIEWPORT_OVERLAY
        }),
        "small rainbow debris stays vivid"
    );
}

#[test]
fn shade_peak_probe_is_bit_identical_to_quadratic_reference() {
    fn reference_bound(decos: &mut [WordDecoration]) {
        let peak = peak_shade_quadratic(decos);
        if peak <= MAX_VIEWPORT_OVERLAY {
            return;
        }
        for d in decos.iter_mut().filter(|d| is_shade(d)) {
            d.alpha = (u32::from(d.alpha) * MAX_VIEWPORT_OVERLAY / peak) as u8;
        }
    }
    let check = |label: &str, stream: &[WordDecoration]| {
        assert_eq!(
            peak_shade_probe(stream),
            peak_shade_quadratic(stream),
            "peak diverged: {label}"
        );
        let mut fast = stream.to_vec();
        let mut slow = stream.to_vec();
        bound_shade_opacity(&mut fast);
        reference_bound(&mut slow);
        assert_eq!(fast, slow, "bounded bytes diverged: {label}");
    };

    for ch in [14i32, 20, 40, 56] {
        for rows in [64i32, 160] {
            for light in [false, true] {
                let e = env(ch, light, rows);
                for t in (0..SUPER_TOTAL_MS).step_by(10) {
                    let mut out = Vec::new();
                    emit_super_decos(t, &e, &mut out, 256);
                    check(
                        &format!("emitted t={t} ch={ch} rows={rows} light={light}"),
                        &out,
                    );
                }
            }
        }
    }

    let stamp =
        |row: u16, col: u16, glyph: DecoGlyph, blend: DecoBlend, alpha: u8| WordDecoration {
            row,
            col,
            dx: 0,
            dy: 0,
            glyph,
            blend,
            color: VEIL_TONE,
            alpha,
        };
    let mut veil = Vec::new();
    for r in 0..7u16 {
        for c in 0..28u16 {
            veil.push(stamp(
                30 + r,
                40 + c,
                DecoGlyph::Shade,
                DecoBlend::Over,
                200,
            ));
        }
    }
    for k in 0..24u16 {
        veil.push(stamp(33, 40 + k, DecoGlyph::Shade, DecoBlend::Over, 180));
    }
    veil.push(stamp(33, 44, DecoGlyph::Dot, DecoBlend::Over, 255));
    veil.push(stamp(33, 44, DecoGlyph::Shade, DecoBlend::Add, 255));
    veil.push(stamp(0, 0, DecoGlyph::Shade, DecoBlend::Over, 0));
    veil.push(stamp(
        u16::MAX,
        u16::MAX,
        DecoGlyph::Shade,
        DecoBlend::Over,
        7,
    ));
    assert!(
        peak_shade_quadratic(&veil) > MAX_VIEWPORT_OVERLAY,
        "the synthetic eclipse must exceed the cap, or the scale-arm \
         comparison below is two no-ops agreeing"
    );
    check("synthetic pre-bound eclipse", &veil);

    let mut big = Vec::new();
    for r in 0..24u16 {
        for c in 0..24u16 {
            big.push(stamp(r, c, DecoGlyph::Shade, DecoBlend::Over, 100));
        }
    }
    big.push(stamp(0, 0, DecoGlyph::Shade, DecoBlend::Over, 100));
    let mut fast = big.clone();
    let mut slow = big;
    bound_shade_opacity(&mut fast);
    reference_bound(&mut slow);
    assert_eq!(fast, slow, "fallback-path bytes diverged");
}

#[test]
fn overlap_bound_preserves_falloff_and_finds_crossed_edges() {
    let quad = |row, x, y, w, h, color| GlowQuad {
        row,
        x,
        y,
        w,
        h,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    };
    let mut gradient = vec![
        quad(0, 0, 0, 20, 10, 0x00C8_C8C8),
        quad(1, 0, 10, 20, 10, 0x0064_6464),
    ];
    bound_additive_overlap(&mut gradient);
    assert_eq!(max_channel(gradient[0].color), MAX_VIEWPORT_OVERLAY);
    assert_eq!(max_channel(gradient[1].color), MAX_VIEWPORT_OVERLAY / 2);

    let mut crossed = vec![
        quad(0, 0, 5, 10, 10, 0x00FF_0000),
        quad(0, 5, 0, 10, 10, 0x00FF_0000),
    ];
    assert_eq!(
        peak_additive_channel(&crossed),
        510,
        "negative control: crossed rectangles really overlap"
    );
    bound_additive_overlap(&mut crossed);
    assert!(peak_additive_channel(&crossed) <= MAX_VIEWPORT_OVERLAY);
    assert!(
        crossed.iter().all(|q| q.color != 0),
        "both crossed shapes remain visible"
    );
}

#[test]
fn peak_sweep_is_bit_identical_to_exhaustive_probe() {
    fn exhaustive(quads: &[GlowQuad]) -> u32 {
        let mut by_row: Vec<&GlowQuad> = quads.iter().collect();
        by_row.sort_unstable_by_key(|q| q.row);
        let mut peak = 0u32;
        for row in by_row.chunk_by(|a, b| a.row == b.row) {
            for x_edge in row {
                let px = u32::from(x_edge.x);
                for y_edge in row {
                    let py = u32::from(y_edge.y);
                    let mut sum = [0u32; 3];
                    for q in row {
                        if px >= u32::from(q.x)
                            && px < u32::from(q.x) + u32::from(q.w)
                            && py >= u32::from(q.y)
                            && py < u32::from(q.y) + u32::from(q.h)
                        {
                            sum[0] += (q.color >> 16) & 0xff;
                            sum[1] += (q.color >> 8) & 0xff;
                            sum[2] += q.color & 0xff;
                        }
                    }
                    peak = peak.max(sum.into_iter().max().unwrap_or(0));
                }
            }
        }
        peak
    }
    for ch in [14i32, 20, 40, 56] {
        for rows in [64i32, 160] {
            for light in [false, true] {
                let e = env(ch, light, rows);
                for t in (0..SUPER_TOTAL_MS).step_by(10) {
                    let mut quads = Vec::new();
                    emit_super(t, &e, MAX_SUPER_QUADS_PER, &mut quads);
                    assert_eq!(
                        peak_additive_channel(&quads),
                        exhaustive(&quads),
                        "sweep drifted at t={t} ch={ch} rows={rows} light={light}"
                    );
                }
            }
        }
    }
    let quad = |row, x, y, w, h, color| GlowQuad {
        row,
        x,
        y,
        w,
        h,
        color,
        alpha: 0,
        color2: color,
        alpha2: 0,
    };
    let shapes: [&[GlowQuad]; 5] = [
        &[],
        &[
            quad(0, 0, 5, 10, 10, 0x00FF_0000),
            quad(0, 5, 0, 10, 10, 0x00FF_0000),
        ],
        &[
            quad(3, 4, 4, 8, 8, 0x0011_2233),
            quad(3, 4, 4, 8, 8, 0x0044_5566),
            quad(3, 4, 8, 2, 2, 0x0077_8899),
        ],
        &[
            quad(1, 2, 2, 0, 4, 0x00FF_FFFF),
            quad(1, 2, 2, 4, 0, 0x00FF_FFFF),
            quad(1, 0, 0, 6, 6, 0x0010_2030),
        ],
        &[
            quad(2, 0, 0, 4, 4, 0x00A0_0000),
            quad(2, 2, 1, 4, 4, 0x0000_B000),
            quad(2, 4, 2, 4, 4, 0x0000_00C0),
            quad(9, 1, 1, 3, 3, 0x0055_5555),
        ],
    ];
    for (i, s) in shapes.into_iter().enumerate() {
        assert_eq!(
            peak_additive_channel(s),
            exhaustive(s),
            "adversarial shape {i}"
        );
    }
}

#[test]
fn wash_window_centers_on_blast_row() {
    let rows = 200i32;
    let mut e = env(20, false, rows);
    let blast_row = 190i32;
    e.row = blast_row as u16;
    e.cy = blast_row * e.cell_h + e.cell_h / 2;
    let mut q = Vec::new();
    let c = emit_super(500, &e, MAX_SUPER_QUADS_PER, &mut q);
    assert_eq!(c.wash, MAX_WASH_ROWS, "row count unchanged (closed form)");
    let wash_rows: Vec<i32> = q[..c.wash].iter().map(|d| i32::from(d.row)).collect();
    assert!(
        wash_rows.contains(&blast_row),
        "the blast row itself is washed (rows {:?}..{:?})",
        wash_rows.first(),
        wash_rows.last()
    );
    assert!(
        wash_rows.iter().all(|r| (40..rows).contains(r)),
        "window clamps inside the grid"
    );
    let e = env(20, false, 64);
    let mut q = Vec::new();
    let c = emit_super(500, &e, MAX_SUPER_QUADS_PER, &mut q);
    assert_eq!(c.wash, 64);
    assert_eq!(i32::from(q[0].row), 0);
}

#[test]
fn light_charge_and_debris_ride_over_stream() {
    let (dark, light) = (env(20, false, 64), env(20, true, 64));
    let mut lq = Vec::new();
    let lc = emit_super(200, &light, MAX_SUPER_QUADS_PER, &mut lq);
    assert_eq!(lc.charge, 0, "light charge leaves the additive channel");
    assert!(lq.is_empty());
    let mut ld = Vec::new();
    emit_super_decos(200, &light, &mut ld, 256);
    assert!(
        !ld.is_empty() && ld.len() <= CHARGE_MOTES,
        "1..=12 charge decos, got {}",
        ld.len()
    );
    let mut dq = Vec::new();
    let dc = emit_super(200, &dark, MAX_SUPER_QUADS_PER, &mut dq);
    assert!(dc.charge > 0, "dark charge stays on the additive channel");
    let mut dd = Vec::new();
    emit_super_decos(200, &dark, &mut dd, 256);
    assert!(dd.is_empty(), "dark charge emits no decos");
    let probe = |e: &SuperEnv| {
        let mut d = Vec::new();
        emit_super_decos(2000, e, &mut d, 256);
        d
    };
    let (ldeb, ddeb) = (probe(&light), probe(&dark));
    assert!(!ldeb.is_empty() && ldeb.len() == ddeb.len());
    assert!(ddeb.iter().all(|m| matches!(m.blend, DecoBlend::Add)));
    for m in ld.iter().chain(&ldeb) {
        assert!(matches!(m.blend, DecoBlend::Over), "light motes blend Over");
    }
}

#[test]
fn the_tier_decode_is_independent_of_the_detonate_decode() {
    let mut all = [0u32; 3];
    let mut detonated = [0u32; 3];
    for i in 0..200_000u64 {
        let mut x = i ^ SUPERNOVA_SALT;
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let draw = x ^ (x >> 31);
        let tier = tier_of(draw) as usize;
        all[tier] += 1;
        if draw % 100 < 30 {
            detonated[tier] += 1;
        }
    }
    let share = |c: &[u32; 3], k: usize| f64::from(c[k]) * 100.0 / f64::from(c.iter().sum::<u32>());
    assert!(
        detonated.iter().sum::<u32>() > 1000,
        "too few detonations to judge"
    );
    for k in 0..3 {
        assert!(
            (share(&all, k) - share(&detonated, k)).abs() < 2.0,
            "tier {k} skews with the detonate decode: {all:?} vs {detonated:?}"
        );
    }
}

#[test]
fn combo_baseline_is_byte_identical() {
    for i in 0..200_000u64 {
        let mut x = i ^ SUPERNOVA_SALT;
        x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        let draw = x ^ (x >> 31);
        assert_eq!(tier_of(draw), tier_for(draw, 0));
    }
}

#[test]
fn combo_ladder_climbs_to_guaranteed_nuke() {
    let mut counts = Vec::new();
    for level in 1..=COMBO_EXTREME_LEVEL {
        let (flash_end, nova_end) = COMBO_TIER_WINDOWS[level as usize - 1];
        let (mut flash, mut nova, mut nuke) = (0u64, 0u64, 0u64);
        for bucket in 0..1_000u64 {
            match tier_for(bucket << 32, level) {
                SuperTier::Flash => flash += 1,
                SuperTier::Nova => nova += 1,
                SuperTier::Nuke => nuke += 1,
            }
        }
        assert_eq!(
            (flash, nova, nuke),
            (flash_end, nova_end - flash_end, 1_000 - nova_end),
            "level {level} residue counts must match COMBO_TIER_WINDOWS"
        );
        counts.push((flash, nuke));
    }
    for w in counts.windows(2) {
        assert!(
            w[1].1 > w[0].1,
            "Nuke buckets must climb each level: {counts:?}"
        );
        assert!(
            w[1].0 <= w[0].0,
            "Flash buckets must never climb: {counts:?}"
        );
    }
    assert_eq!(
        counts[COMBO_EXTREME_LEVEL as usize - 1],
        (0, 1_000),
        "EXTREME level is Nuke on every residue class"
    );
    assert_eq!(tier_for(7, COMBO_EXTREME_LEVEL + 3), SuperTier::Nuke);

    assert_eq!(combo_chance(10, 1), 10, "a lone f-bomb keeps its chance");
    assert!(combo_chance(10, 2) > 10);
    assert_eq!(combo_chance(1, COMBO_EXTREME_LEVEL), 100);
    assert_eq!(combo_chance(100, 1), 100);
    for level in 0..=COMBO_EXTREME_LEVEL + 2 {
        assert_eq!(combo_chance(0, level), 0, "0 is the off-switch");
    }
}

#[test]
fn phases_live_in_their_windows() {
    let e = env(20, false, 64);
    let probe = |t: u64| {
        let mut q = Vec::new();
        let c = emit_super(t, &e, MAX_SUPER_QUADS_PER, &mut q);
        let mut d = Vec::new();
        emit_super_decos(t, &e, &mut d, 256);
        (c, q, d)
    };
    let (c, q1, _) = probe(100);
    assert!(c.charge > 0 && c.wash == 0 && c.ring == 0);
    let (_, q2, _) = probe(100);
    assert_eq!(q1, q2, "pure: same inputs, same bytes");
    let (c, ..) = probe(500);
    assert!(c.wash > 0 && c.charge == 0);
    let (c, _, d) = probe(1000);
    assert!(c.ring > 0 && c.wash == 0);
    assert!(d.is_empty(), "debris starts at 1200");
    let (c, _, d) = probe(1500);
    assert!(
        c.ring > 0 && !d.is_empty(),
        "ring/debris overlap 1200..1600"
    );
    let (c, q, d) = probe(2000);
    assert!(c.ring == 0 && q.is_empty() && !d.is_empty());
    let (_, q, d) = probe(SUPER_TOTAL_MS);
    assert!(q.is_empty() && d.is_empty(), "afterglow emits nothing here");
}

#[test]
fn const_dir_tables_match_sin_cos_within_one_ulp() {
    for (point, &(s, c)) in crown_dirs().iter().enumerate() {
        let (es, ec) = (core::f32::consts::TAU * point as f32 / 8.0).sin_cos();
        assert!(s.to_bits().abs_diff(es.to_bits()) <= 1);
        assert!(c.to_bits().abs_diff(ec.to_bits()) <= 1);
    }
    for (k, &(s, c)) in shock_dirs().iter().enumerate() {
        let (es, ec) = (core::f32::consts::TAU * k as f32 / 16.0).sin_cos();
        assert!(s.to_bits().abs_diff(es.to_bits()) <= 1);
        assert!(c.to_bits().abs_diff(ec.to_bits()) <= 1);
    }
}
