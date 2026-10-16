// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail Packs (the `GlowStyle::Custom` data path).

use super::*;

/// Drive one fixed injected-clock script (idle, 6 adjacent moves, a jump, two
/// decay frames) and fold every frame's whole-frame fingerprint into one u64.
/// Positive styles get explicit synthetic provenance; Rainbow Kitty stays the
/// dark no-candidate control pinned by its zero golden. Deterministic given the
/// relative schedule.
fn run_script(cfg: &GlowConfig, g: Geom) -> u64 {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut acc: u64 = 0;
    let mut fold = |fp: u64| acc = fnv_mix_u64(acc, fp);
    fold(glow.tick(Some((2, 0)), t0, cfg, g, &mut out)); // seed
    for k in 1..=6u64 {
        let now = t0 + Duration::from_millis(90 * k);
        if !matches!(cfg.style, GlowStyle::RainbowKitty) {
            glow.note_synthetic_move(now);
        }
        fold(glow.tick(Some((2, k as u16)), now, cfg, g, &mut out));
    }
    let jump = t0 + Duration::from_millis(700);
    if !matches!(cfg.style, GlowStyle::RainbowKitty) {
        glow.note_synthetic_move(jump);
    }
    fold(glow.tick(Some((2, 30)), jump, cfg, g, &mut out)); // jump
    fold(glow.tick(
        Some((2, 30)),
        t0 + Duration::from_millis(1400),
        cfg,
        g,
        &mut out,
    ));
    fold(glow.tick(Some((2, 30)), t0 + Duration::from_secs(4), cfg, g, &mut out)); // decay
    acc
}

/// Built-in styles are byte-identical with `pack: None`: the whole-frame fingerprint fold over a
/// fixed script is deterministic and equals a committed golden, so no built-in entered the custom
/// interpreter. The positive `CUSTOM_EMIT_CALLS` check lives in
/// `trail_pack_ticks_capped_gridclamped_and_settles_to_idle_zero`, off this assertion, because
/// that global counter races under the parallel runner.
#[test]
fn builtins_are_byte_identical_and_never_touch_the_custom_path() {
    let g = geom();
    // Golden whole-frame folds, one per style below, captured from this script. A deliberate
    // visual change to one style must move only its own index; recapture only the intended
    // indices after checking no other drifted.
    //
    // Rainbow Kitty alone gets no candidate, so its scripted relocation is program output and
    // stays dark (its star arms ride the momentum spine, which no `note_typed` charges here).
    const GOLDEN: [u64; 10] = [
        10_825_902_740_968_678_692,
        7_284_969_456_844_567_239,
        0,
        8_478_652_148_696_807_272,
        2_367_067_016_366_301_709,
        8_472_466_222_191_431_873,
        11_179_281_213_543_892_930,
        18_232_718_546_775_126_099,
        8_446_969_558_599_158_774,
        16_670_463_046_324_493_866,
    ];
    let styles = [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::RainbowKitty,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
        // The classic wake exists to reproduce a fixed look exactly, so it is pinned too.
        GlowStyle::Classic,
    ];
    let mut actual = [0u64; 10];
    for (i, &s) in styles.iter().enumerate() {
        let c = cfg(s, true);
        let a = run_script(&c, g);
        let b = run_script(&c, g);
        assert_eq!(a, b, "style {s:?} is nondeterministic");
        actual[i] = a;
    }
    // If the golden is still the placeholder, print the captured values so a
    // maintainer can paste them in; otherwise pin against drift.
    if GOLDEN.iter().all(|&v| v == 0) {
        panic!("CAPTURE GOLDEN = {actual:?}");
    }
    assert_eq!(actual, GOLDEN, "a built-in style's emitted frame changed");
    let _ = BUILTINS;
}

fn pack_cfg(p: TrailParams) -> GlowConfig {
    let mut c = cfg(GlowStyle::Custom, true);
    c.pack = Some(p);
    c
}

fn compile_pack(src: &str) -> TrailParams {
    *crate::trail_pack::compile_trail_pack_toml(src)
        .expect("pack compiles")
        .params()
}

/// A resolved pack ticks through the shared machinery: quads emitted, capped,
/// grid-clamped single-row, and the effect settles to idle-zero
/// (`is_active() == false`, fp == 0) after decay.
#[test]
fn trail_pack_ticks_capped_gridclamped_and_settles_to_idle_zero() {
    use std::sync::atomic::Ordering;
    let g = geom();
    let src = include_str!("../../../assets/trail-packs/synthwave.toml");
    let c = pack_cfg(compile_pack(src));
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let before = CUSTOM_EMIT_CALLS.load(Ordering::Relaxed);
    glow.tick(Some((2, 0)), t0, &c, g, &mut out); // seed
    for k in 1..=6u64 {
        let now = t0 + Duration::from_millis(90 * k);
        glow.note_synthetic_typed(now, 1);
        glow.tick(Some((2, k as u16)), now, &c, g, &mut out);
    }
    let jump = t0 + Duration::from_millis(700);
    glow.note_synthetic_move(jump);
    let fp = glow.tick(Some((2, 30)), jump, &c, g, &mut out); // jump
    assert_ne!(fp, 0, "custom pack emits live light");
    assert!(!out.is_empty(), "custom beam renders while alive");
    assert!(
        out.len() <= CursorGlow::MAX_QUADS,
        "custom path honours MAX_QUADS"
    );
    assert_invariants(&out, g);
    assert!(
        CUSTOM_EMIT_CALLS.load(Ordering::Relaxed) > before,
        "the custom interpreter actually ran"
    );
    // Decay past every window → idle-zero.
    let gone = glow.tick(Some((2, 30)), t0 + Duration::from_secs(5), &c, g, &mut out);
    assert_eq!(gone, 0, "custom trail decays to EXACTLY empty (idle-zero)");
    assert!(out.is_empty());
    assert!(!glow.is_active());
}

/// The structural legibility ceiling: a pack maxing the coverage ramp
/// (`cov_base = cov_slope = 1.0`) at full intensity with a hot typing run never
/// births a spark above [`CursorGlow::CUSTOM_COV_CAP`]; the cap is applied in
/// the interpreter's sole emission funnel and at birth, so a pack cannot opt
/// out.
#[test]
fn trail_pack_cannot_exceed_the_occupied_cell_coverage_ceiling() {
    let g = geom();
    let src = "pack = 1\nid = \"blast\"\n[beam]\ncov_base = 1.0\ncov_slope = 1.0\n";
    let mut c = pack_cfg(compile_pack(src));
    c.intensity = 1.0; // overdrive
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    // A hot run of adjacent typing keys (builds heat/boost to the ceiling).
    glow.tick(Some((2, 0)), t0, &c, g, &mut out);
    for k in 1..=12u64 {
        let now = t0 + Duration::from_millis(35 * k);
        glow.note_synthetic_typed(now, 1);
        glow.tick(Some((2, k as u16)), now, &c, g, &mut out);
    }
    assert!(!glow.sparks.is_empty(), "a hot run laid custom sparks");
    for s in &glow.sparks {
        assert!(
            (s.born_cov as f32) <= CursorGlow::CUSTOM_COV_CAP,
            "custom birth coverage {} exceeds the ceiling {}",
            s.born_cov,
            CursorGlow::CUSTOM_COV_CAP
        );
    }
}

/// A pack→pack swap (both `Custom`) drops the previous pack's in-flight light:
/// the style-switch guard also compares the pack fingerprint, so foreign sparks
/// never render under a new pack.
#[test]
fn pack_to_pack_swap_drops_stale_light() {
    let g = geom();
    let a = pack_cfg(compile_pack("pack = 1\nid = \"aaa\"\n"));
    let b = pack_cfg(compile_pack("pack = 1\nid = \"bbb\"\n"));
    assert_ne!(
        a.pack.unwrap().pack_fp,
        b.pack.unwrap().pack_fp,
        "distinct packs have distinct fingerprints"
    );
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    glow.tick(Some((2, 0)), t0, &a, g, &mut out);
    for k in 1..=4u64 {
        let now = t0 + Duration::from_millis(60 * k);
        glow.note_synthetic_typed(now, 1);
        glow.tick(Some((2, k as u16)), now, &a, g, &mut out);
    }
    assert!(!glow.sparks.is_empty(), "pack A laid sparks");
    // Swap to pack B on the next frame without relocating: A's sparks must
    // be dropped by the pack fingerprint edge alone.
    glow.tick(
        Some((2, 4)),
        t0 + Duration::from_millis(300),
        &b,
        g,
        &mut out,
    );
    assert!(
        glow.last_pack_fp == b.pack.unwrap().pack_fp,
        "the animator tracks the live pack fingerprint"
    );
}

/// Every wired Trail Pack param moves the emitted custom fold (no knob does
/// nothing). The "not for built-ins" half is the golden above: no built-in path
/// reads `cfg.pack`.
#[test]
fn each_wired_pack_param_changes_the_custom_fold() {
    let g = geom();
    // The default script (typing run, jump, two static decay frames) reveals
    // boost/heat/life params (per-frame coverage and spark survival) and the
    // post-jump crown/ring windows (the +1400 ms frame sits 700 ms after the
    // jump).
    let fold = |src: &str| run_script(&pack_cfg(compile_pack(src)), g);
    // A type-then-hold script: a static frame 420 ms after a single typed key, so the crown's
    // typing window decides.
    let fold_hold = |src: &str| -> u64 {
        let c = pack_cfg(compile_pack(src));
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut acc = 0u64;
        let mut f = |fp: u64| acc = fnv_mix_u64(acc, fp);
        f(glow.tick(Some((2, 0)), t0, &c, g, &mut out));
        let typed = t0 + Duration::from_millis(80);
        glow.note_synthetic_typed(typed, 1);
        f(glow.tick(Some((2, 1)), typed, &c, g, &mut out));
        f(glow.tick(
            Some((2, 1)),
            t0 + Duration::from_millis(500),
            &c,
            g,
            &mut out,
        ));
        acc
    };

    // Chaining off (`chain_gap_max = 0.0`) so a spark's life is the pure heat
    // curve (isolating life_base_mul/life_a/life_b), with a base long enough
    // that a wake survives several frames. Also exercises heat gain/tau/bright
    // and the crown/ring/bed channels.
    const HEAT_BASE: &str = r#"
pack = 1
id = "probe"
[ramp]
kind = "hsv_sweep"
[heat]
gain = 0.10
tau = 0.90
bright_floor = 0.35
bright_slope = 0.30
life_base_mul = 1.00
life_a = 1.0
life_b = 0.0
chain_gap_max = 0.0
[crown]
enabled = true
typing_window_ms = 300
jump_window_ms = 200
[ring]
enabled = true
life_ms = 320
[channels]
glow_add = true
halo = "add"
bed = false
"#;
    // Chaining on with a tiny base life so the chain floor dominates, isolating
    // chain_keys/chain_gap_max/chain_life_max.
    const CHAIN_BASE: &str = r#"
pack = 1
id = "probe"
[ramp]
kind = "hsv_sweep"
[heat]
life_base_mul = 0.05
life_a = 0.0
life_b = 0.0
chain_keys = 3.0
chain_gap_max = 0.30
chain_life_max = 0.60
[channels]
glow_add = true
halo = "add"
"#;
    let heat_base = fold(HEAT_BASE);
    let chain_base = fold(CHAIN_BASE);

    // (label, from, to): each swap must move its fold.
    let heat_cases = [
        ("heat.gain", "gain = 0.10", "gain = 0.95"),
        ("heat.tau", "tau = 0.90", "tau = 0.10"),
        (
            "heat.bright_floor",
            "bright_floor = 0.35",
            "bright_floor = 0.90",
        ),
        (
            "heat.bright_slope",
            "bright_slope = 0.30",
            "bright_slope = 1.50",
        ),
        (
            "heat.life_base_mul",
            "life_base_mul = 1.00",
            "life_base_mul = 0.20",
        ),
        ("heat.life_a", "life_a = 1.0", "life_a = 6.0"),
        ("heat.life_b", "life_b = 0.0", "life_b = 6.0"),
        (
            "crown.jump_window_ms",
            "jump_window_ms = 200",
            "jump_window_ms = 1800",
        ),
        ("ring.life_ms", "life_ms = 320", "life_ms = 1800"),
        ("channels.bed", "bed = false", "bed = true"),
    ];
    for (label, from, to) in heat_cases {
        let variant = HEAT_BASE.replacen(from, to, 1);
        assert_ne!(variant, HEAT_BASE, "{label}: swap {from:?} did not apply");
        assert_ne!(
            fold(&variant),
            heat_base,
            "{label} did not change the custom fold"
        );
    }

    // crown.typing_window_ms decides whether the crown survives a hold 420ms
    // after a typed key — only observable in the type-then-hold script.
    let hold_base = fold_hold(HEAT_BASE);
    let hold_var = HEAT_BASE.replacen("typing_window_ms = 300", "typing_window_ms = 1500", 1);
    assert_ne!(
        fold_hold(&hold_var),
        hold_base,
        "crown.typing_window_ms did not change the fold"
    );

    let chain_cases = [
        ("heat.chain_keys", "chain_keys = 3.0", "chain_keys = 6.0"),
        (
            "heat.chain_life_max",
            "chain_life_max = 0.60",
            "chain_life_max = 0.20",
        ),
        (
            "heat.chain_gap_max",
            "chain_gap_max = 0.30",
            "chain_gap_max = 0.05",
        ),
    ];
    for (label, from, to) in chain_cases {
        let variant = CHAIN_BASE.replacen(from, to, 1);
        assert_ne!(variant, CHAIN_BASE, "{label}: swap {from:?} did not apply");
        assert_ne!(
            fold(&variant),
            chain_base,
            "{label} did not change the custom fold"
        );
    }
}

/// The `theme` arm selects the light-theme treatment: on a light ground `auto`
/// keeps the additive beam, `over_veil` renders source-over veil rails and
/// `darken_tints` tints them deeper (three distinct folds), while the dark-theme
/// render is the same for every arm.
#[test]
fn theme_arm_changes_the_light_theme_custom_fold() {
    let g = geom();
    const BODY: &str = "pack = 1\nid = \"th\"\n[ramp]\nkind = \"hsv_sweep\"\n\
                        [channels]\nglow_add = true\nhalo = \"add\"\n";
    let with = |kind: &str| format!("{BODY}[theme]\nkind = \"{kind}\"\n");
    let light = |src: &str| -> u64 {
        let mut c = pack_cfg(compile_pack(src));
        c.dark_theme = false;
        run_script(&c, g)
    };
    let (auto, over, dark) = (
        light(&with("auto")),
        light(&with("over_veil")),
        light(&with("darken_tints")),
    );
    assert_ne!(
        auto, over,
        "over_veil renders differently from auto on a light theme"
    );
    assert_ne!(
        over, dark,
        "darken_tints tints the veils darker than over_veil"
    );
    assert_ne!(
        auto, dark,
        "darken_tints differs from auto on a light theme"
    );

    // The arm does not change the dark-theme render (additive beam either way).
    let dfold = |src: &str| run_script(&pack_cfg(compile_pack(src)), g);
    assert_eq!(
        dfold(&with("auto")),
        dfold(&with("over_veil")),
        "the theme arm must not perturb the dark-theme render"
    );
}
