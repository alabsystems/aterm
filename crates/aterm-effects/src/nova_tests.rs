// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::genome::nova_features;

const fn worst_feats() -> NovaFeatures {
    let mut g = 0u64;
    while g < (1u64 << 15) {
        let f = nova_features(g);
        if f.rays == 8 && f.radius > 2.19 && f.chroma > 2.49 && f.ring_thick > 3.49 {
            return f;
        }
        g += 1;
    }
    panic!("genome-max feature combination is reachable (§3.4 reachability)");
}

const WORST_FEATS: NovaFeatures = worst_feats();

const fn env_at(ch: i32, magic: Option<NovaMagic>) -> NovaEnv {
    let feats = WORST_FEATS;
    let cw = ch / 2;
    NovaEnv {
        grid_w: 200 * cw,
        grid_h: 60 * ch,
        cell_w: cw,
        cell_h: ch,
        cx: 100 * cw,
        cy: 30 * ch,
        r_max: feats.radius * ch as f32,
        feats,
        magic,
        core: NOVA_PALETTES[0].0,
        fringe: NOVA_PALETTES[0].1,
        intensity: 1.0,
        seed: 0xDECAF,
    }
}

#[test]
fn nova_budget_geometry_invariant() {
    for ch in [14i32, 20, 40, 56] {
        let env = env_at(ch, None);
        let extent = 2.0 * (env.r_max + env.feats.chroma + 0.5 * env.feats.ring_thick);
        let bands = ring_band_count(extent);
        assert_eq!(
            bands, 30,
            "band count must be exactly 30 at ch={ch} (extent {extent:.1} px), counted {bands}"
        );
        let mut late = Vec::new();
        let cl = emit_nova(860, &env, MAX_NOVA_QUADS_PER, &mut late);
        let nrays = usize::from(env.feats.rays);
        assert_eq!(nrays, 8, "genome-max ray count, counted {nrays}");
        assert_eq!(
            cl.ray_samples,
            RAY_SAMPLES * nrays,
            "ray samples must be 15 per ray at ch={ch}: counted {} over {nrays} rays \
             (want {} = 15 × {nrays})",
            cl.ray_samples,
            RAY_SAMPLES * nrays
        );
        let mut out = Vec::new();
        let mut uncapped = Vec::new();
        let mut worst = 0usize;
        for t in (0..1400u64).step_by(10) {
            out.clear();
            let counts = emit_nova(t, &env, MAX_NOVA_QUADS_PER, &mut out);
            worst = worst.max(out.len());
            assert!(
                out.len() <= 392,
                "Q ≤ 392 violated at ch={ch} t={t}: counted {} (ring {} rays {} crown {})",
                out.len(),
                counts.ring,
                counts.rays,
                counts.crown
            );
            uncapped.clear();
            emit_nova(t, &env, usize::MAX, &mut uncapped);
            assert_eq!(
                out,
                uncapped,
                "the {MAX_NOVA_QUADS_PER}/nova cap bound at ch={ch} t={t} \
                 (capped {} vs unbounded {} quads)",
                out.len(),
                uncapped.len()
            );
            for q in &out {
                let band = i32::from(q.row) * ch;
                assert!(
                    i32::from(q.y) >= band && i32::from(q.y) + i32::from(q.h) <= band + ch,
                    "quad escapes its row band at ch={ch} t={t}: {q:?}"
                );
                assert!(i32::from(q.x) + i32::from(q.w) <= env.grid_w);
            }
        }
        assert!(worst > 0, "the nova must actually emit at ch={ch}");
        assert!(
            worst <= 392 && 392 < MAX_NOVA_QUADS_PER,
            "headroom must hold at ch={ch}: worst {worst} ≤ 392 < {MAX_NOVA_QUADS_PER}"
        );
    }
    const { assert!(MAX_ACTIVE_NOVAS * 392 <= MAX_NOVA_QUADS) };
}

#[test]
fn spend_order_truncation_ring_first_crown_drops() {
    let env = env_at(20, None);
    let t = 600u64;
    let mut full = Vec::new();
    let counts = emit_nova(t, &env, MAX_NOVA_QUADS_PER, &mut full);
    assert!(counts.ring > 0 && counts.rays > 0);
    let mut capped = Vec::new();
    let c = emit_nova(t, &env, counts.ring + 10, &mut capped);
    assert_eq!(c.ring, counts.ring, "the ring is never gapped by a cap");
    assert_eq!(c.rays, 10, "rays truncate to the remaining budget");
    assert_eq!(c.crown, 0);
    assert_eq!(
        capped[..counts.ring],
        full[..counts.ring],
        "identical ring bytes"
    );
    let tf = 180u64;
    let mut crown_full = Vec::new();
    let cf = emit_nova(tf, &env, MAX_NOVA_QUADS_PER, &mut crown_full);
    assert!(
        (1..=CROWN_QUADS).contains(&cf.crown),
        "crown ≤ ~12 quads: {}",
        cf.crown
    );
    let mut crown_capped = Vec::new();
    let cc = emit_nova(tf, &env, CROWN_QUADS - 1, &mut crown_capped);
    assert_eq!(
        cc.crown, 0,
        "a partial crown never emits (crown drops first)"
    );
    assert!(crown_capped.is_empty());
}

#[test]
fn coupling_crossing_is_deterministic_and_monotone() {
    let rm = 44.0f32;
    let a = crossing_ms(10.0, rm, false).unwrap();
    let b = crossing_ms(10.0, rm, false).unwrap();
    assert_eq!(a, b, "pure function of (dist, r_max)");
    let far = crossing_ms(40.0, rm, false).unwrap();
    assert!(far > a, "the expanding ring reaches farther words later");
    assert!(
        crossing_ms(50.0, rm, false).is_none(),
        "beyond r_max: never"
    );
    let sa = crossing_ms(10.0, rm, true).unwrap();
    let sfar = crossing_ms(40.0, rm, true).unwrap();
    assert!(sfar < sa, "the contracting ring reaches far words FIRST");
    for (d, sing) in [
        (0.0f32, false),
        (10.0, false),
        (40.0, false),
        (rm, false),
        (0.0, true),
        (10.0, true),
        (rm, true),
    ] {
        let t = crossing_ms(d, rm, sing).unwrap();
        assert!((FLASH_END_MS..RING_END_MS).contains(&t));
        let (r, _) = ring_radius(t, rm, sing).unwrap();
        assert!((r - d).abs() < 2.5, "R(t_cross) ≈ dist: {r} vs {d}");
    }
    assert!(pulse_env(299, 300).is_none());
    assert!(pulse_env(300, 300).is_some());
    assert!(pulse_env(375, 300).unwrap() > 0.9, "peak mid-window");
    assert!(pulse_env(450, 300).is_none());
}

#[test]
fn delta_l_bound_over_palette_table() {
    use crate::color_math::{hue_nudge, relative_luminance};
    let mut anchors: Vec<u32> = vec![0x007C_C8FF, 0x00C8_9AFF, 0x00F7_A8B8, 0x00FF_D9C2];
    for (core, fringe) in NOVA_PALETTES {
        anchors.push(core);
        anchors.push(fringe);
    }
    for &base in &anchors {
        for code in 0..16u8 {
            let anchor = hue_nudge(base, code);
            let la = relative_luminance(anchor);
            for (_, fringe) in NOVA_PALETTES {
                let tone = constant_lum_toward(anchor, fringe);
                let dl = (relative_luminance(tone) - la).abs();
                assert!(
                    dl <= 0.05,
                    "|ΔL| {dl:.4} > 5% for anchor {anchor:06x} → fringe {fringe:06x}"
                );
                for amp in [0.1f32, 0.2, PULSE_AMP] {
                    let mixed = pulse_color(anchor, fringe, amp);
                    let dm = (relative_luminance(mixed) - la).abs();
                    assert!(
                        dm <= 0.05,
                        "pulse |ΔL| {dm:.4} > 5% at amp {amp} for {anchor:06x}"
                    );
                    let (r, g, b) = ((mixed >> 16) & 0xff, (mixed >> 8) & 0xff, mixed & 0xff);
                    let red_frac = r as f32 / (r + g + b).max(1) as f32;
                    assert!(red_frac < 0.8, "pulse produced saturated red {mixed:06x}");
                }
            }
        }
    }
}

#[test]
fn singularity_contracts_and_darkens() {
    let env = env_at(20, Some(NovaMagic::Singularity));
    let (r_early, _) = ring_radius(300, env.r_max, true).unwrap();
    let (r_late, _) = ring_radius(800, env.r_max, true).unwrap();
    assert!(r_early > r_late, "the Singularity ring contracts");
    let mut decos = Vec::new();
    emit_ring_arc(300, &env, &mut decos, MAX_RING_ARC_CELLS);
    assert!(!decos.is_empty() && decos.len() <= MAX_RING_ARC_CELLS);
    for d in &decos {
        assert_eq!(d.glyph, DecoGlyph::RingArc);
        assert!(
            matches!(d.blend, DecoBlend::Over),
            "darkening must be Over, not Add"
        );
    }
    let mut late = Vec::new();
    emit_ring_arc(850, &env, &mut late, MAX_RING_ARC_CELLS);
    let mean_dist = |ds: &[WordDecoration]| {
        ds.iter()
            .map(|d| {
                let x = i32::from(d.col) * env.cell_w + env.cell_w / 2 - env.cx;
                let y = i32::from(d.row) * env.cell_h + env.cell_h / 2 - env.cy;
                ((x * x + y * y) as f32).sqrt()
            })
            .sum::<f32>()
            / ds.len().max(1) as f32
    };
    assert!(
        mean_dist(&late) < mean_dist(&decos),
        "the shadow ring falls inward"
    );
    let mote_dist = |t: u64| {
        let mut motes = Vec::new();
        emit_debris(t, &env, &mut motes, 64);
        assert!(!motes.is_empty());
        mean_dist(&motes)
    };
    let late_t = u64::from(env.feats.duration_ms) - 100;
    assert!(
        mote_dist(late_t) < mote_dist(600),
        "Singularity debris collapses inward"
    );
}
