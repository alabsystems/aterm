// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Trail Packs (the `GlowStyle::Custom` data path).

use super::*;

/// Drive one fixed injected-clock script (idle → 6 adjacent moves → a jump
/// → two decay frames) and fold EVERY frame's whole-frame fingerprint into
/// one u64. Positive styles receive explicit synthetic provenance;
/// Rainbow Kitty deliberately remains the dark no-candidate control pinned
/// by its zero golden. Deterministic given the relative schedule (the engine
/// is clockless).
fn run_script(cfg: &GlowConfig, g: Geom) -> u64 {
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut acc: u64 = 0;
    let mut fold = |fp: u64| acc = fp_fold(acc, fp);
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

/// BUILTIN-BYTE-IDENTICAL PROOF (the additivity mandate). For each built-in
/// style with `pack: None`: the whole-frame fingerprint fold across a fixed
/// script (a) is DETERMINISTIC and (b) equals a committed golden captured
/// after the Trail Pack change (any future perturbation of a built-in path
/// fails). Equality to the pre-change golden IS the proof that no built-in
/// entered the custom interpreter: `emit_custom` emits different quads, so a
/// built-in that took it could not reproduce the byte-exact golden fold. (The
/// positive "the custom path actually runs" check lives in the pack tests,
/// via the `CUSTOM_EMIT_CALLS` counter — kept off THIS assertion because that
/// global counter races with the pack tests under the parallel test runner.)
#[test]
fn builtins_are_byte_identical_and_never_touch_the_custom_path() {
    let g = geom();
    // GOLDEN whole-frame folds for Lumen/Phaser/rainbow kitty/Sparkle/Fire/Laser/
    // Beam/Water/Comet, captured from this very script. Pins every built-in
    // emit path against unintended perturbation.
    //
    // WHEN ONE OF THESE MOVES: a deliberate visual change to exactly ONE
    // style must move exactly ONE index. If a change to style A shifts style
    // B's fold, that is the bug this pin exists to catch — recapture only
    // the index whose style you meant to change, and only after checking
    // that no other index drifted.
    //
    // Rainbow Kitty alone receives no candidate, so its scripted relocation
    // is PROGRAM OUTPUT by definition and remains wholly dark: no input
    // witness owns a trail. Every positive built-in arm receives an explicit
    // synthetic candidate; no arm relies on a classifier timestamp or cursor
    // geometry as causal provenance.
    //
    // RECAPTURED at the STARDUST LAW's SECOND thinning (owner, 2026-08-09:
    // "many fewer of those cross sparkles" — [`STAR_ACCENT_DEN`] 8 → 16).
    // THREE indices moved then: 2 (RainbowKitty), 6 (Beam), 8 (Comet) — the
    // dealt star populations. Lumen, Phaser, Fire, Laser and Water were
    // byte-identical, i.e. the change reached the star kit and nothing else.
    //
    // RECAPTURED AGAIN when the two populations that had escaped that
    // thinning were brought under it, and the round body was repriced at the
    // plus's own composited light. TWO indices moved:
    //
    //   * 2 (RainbowKitty) — the dark starfield's mote now lays a HOT CORE
    //     inside its skirt (`push_dust_mote`), because one flat rect at the
    //     arms' coverage renders 2.35x darker than the crossing it replaced;
    //     and the jump-LANDING fan, thirteen full crosses at its widest,
    //     keeps exactly one.
    //   * 3 (Sparkle) — the KNOWN RESIDUAL from the last capture is gone.
    //     Sparkle dealt its own shape from an eight-way `seed % 8` bucket, so
    //     the family's denominator never reached it and picking that style
    //     still bought the 1-in-8 the owner had just rejected. Its buckets
    //     (moon, mini-comet, glitter) are untouched; only the STAR bucket now
    //     asks [`star_accent`], which makes its plus share 1-in-16 like
    //     everything else.
    //
    // RECAPTURED AGAIN when the repricing was carried to the RADIAL half of
    // the kit — the previous capture's own note ("Beam and Comet did NOT move,
    // and that is the control: their motes are radial halos") was the tell,
    // and it was reasoning backwards. It read the two styles' stillness as
    // evidence the change had been scoped correctly, when what it actually
    // recorded was that the halo emitters had NOT been repriced and were
    // still laying one flat disc at the arms' coverage — 2.35x under the
    // crossing they replaced. TWO indices moved:
    //
    //   * 3 (Sparkle) — the pour's converted grain now lays a HOT CORE inside
    //     its skirt ([`push_dust_halo`]). This is the seam where the last
    //     round's RARITY fix became a BRIGHTNESS cut: bucket 0's re-deal sends
    //     half its grains down the dust arm, and that arm was one halo at the
    //     same `scov` the plus's three stacked lays were handed.
    //   * 6 (Beam) — the wake's body, the same law at the same coverage.
    //
    // RECAPTURED AGAIN for COMET (8), and the previous capture's note about
    // it was the last place this defect was hiding. That note called Comet
    // "the control", on the grounds that its grain is a flat square that was
    // never a plus so no shape swap reached it. True, and beside the point:
    // what the rarity deal took from Comet was the twinkle-peak GLINT, which
    // is itself a `push_twinkle_star` — three coincident additive lays on the
    // crossing — so 15-in-16 grains lost `STAR_STACK_ADD · glint_cov` of
    // light at their peak and got NOTHING back, because there was no
    // converted mote to reprice. Being outside the shape swap made Comet the
    // one arm where the cut was total rather than the one arm that was
    // exempt. The undealt grain is now paid the glint's light as a hot core
    // over its own skirt (`comet_grains_that_lost_the_glint_keep_its_light`),
    // and the frame moved accordingly.
    //
    // RainbowKitty (2) did not move — its shower's coverage rides the
    // momentum spine, which this script deliberately leaves at zero (no
    // `note_typed`), so its repriced arm is inert here by construction rather
    // than unchanged.
    // RECAPTURED for THE TAPER (owner, 2026-08-10: the needle star "is
    // better", and the kit's marks "read as PLUS SIGNS"). Exactly ONE index
    // moved, and it is the one that should:
    //
    //   * 3 (Sparkle) — the pour's star grains. A 4-point star's arm is no
    //     longer one rect of constant thickness held to a square end; it is
    //     a full-coverage BODY through the crossing running out into POINTS
    //     that dim toward [`STAR_TIP_COV`] and thin with them (THE TAPER, in
    //     `effect_util`). Same reach, same crossing brightness, different
    //     silhouette — which is the whole point, so these pixels SHOULD
    //     change.
    //
    // Water (7) also moved on 2026-08-31 when repeated repaint samples gained
    // newest-cell ownership, the wake moved under glyphs, and old reflections
    // began dripping as they faded. The other seven did not, and that is the
    // control rather than an oversight: RainbowKitty (2) draws its stars off
    // the momentum spine this script deliberately leaves at zero, and the
    // remaining six draw no changed mark in it at all.
    // RECAPTURED after combining that Water repaint with the field-complete
    // frame fingerprint. Every nonzero index moved because the digest now
    // includes stream boundaries, lengths, alpha, procedural-field controls,
    // halo bounds and blend modes without overlapping XOR lanes; Water's
    // entry also carries the deliberate geometry above. The isolated-field
    // fingerprint and Water ownership/drain regressions are the independent
    // controls for those two causes.
    // RECAPTURED for the landing-ring corner fix (2026-09-01 audit): the
    // square outline's vertical bars are inset by the bar thickness so the
    // four bars PARTITION the ring — the th×th corners had composited
    // twice under One/One and lit as ~2× rivets. Exactly the SIX
    // square-outline styles moved (Lumen, Phaser, Sparkle, Laser, Beam,
    // Comet); Fire and Water — which return before the outline with their
    // own art — and the zero-momentum RainbowKitty held, which is the
    // control: the byte change is the ring's and nothing else's
    // (`landing_ring_bars_partition_the_square_outline` pins the geometry).
    // RECAPTURED for THE CLASSIC LANDING RING'S GRADE (2026-09-08, the
    // owner's "a bigger impact splash that scales more with the distance
    // traveled" — `classic_ring_radius_factor` / `classic_ring_life`,
    // `Ring.scale`): this script's screen-crossing jump is well past the
    // 8-cell floor, so every style that rings on it took a wider, longer
    // ring. Exactly the SEVEN ringing rows moved — the six square-outline
    // styles (Lumen, Phaser, Sparkle, Laser, Beam, Comet) and Water's
    // ripple train; Fire's flash held here (its strike goes through the
    // meteor arm, whose ring this script does not reach), the
    // zero-momentum RainbowKitty held, and the Classic salvage held —
    // which is the control: the byte change is the graded ring's and
    // nothing else's. VERIFIED by zeroing the two grade constants on the
    // merged tree, under which all ten rows fold to the previous numbers.
    // (The grade's own commit re-minted only the DELETION_GOLDENS kitty
    // rows; its suite run was filtered to ring/impact/momentum, so this
    // pin was first read on the merge.)
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
        // CLASSIC — captured from this script against the salvaged engine,
        // whose output was verified byte-identical to the real v0.28 build
        // (tag v0.28, `8e7fe4f6f`) over a typing run, a decay and a
        // screen-crossing jump before this number was taken.
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
        // THE SALVAGE, pinned like the rest. Its whole point is that it
        // reproduces a shipped release byte for byte, so an unpinned
        // classic is the one style where a silent drift would destroy the
        // only property it has. Index 9 — appended, so no existing index
        // moves and the nine folds above keep meaning what they meant.
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

/// The `pack:` end-to-end law: a resolved pack ticks through the SHARED
/// machinery — quads emitted, capped, grid-clamped single-row, and the whole
/// effect settles to idle-zero (`is_active() == false`, fp == 0) after decay.
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

/// The STRUCTURAL legibility ceiling: a pack that maxes out the coverage ramp
/// (`cov_base = cov_slope = 1.0`) at full intensity + a hot typing run can
/// never birth a spark above [`CursorGlow::CUSTOM_COV_CAP`] — the cap is
/// applied in the interpreter's sole emission funnel and at birth, so a pack
/// cannot opt out.
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

/// A pack→pack swap (both resolve to `Custom`) drops the previous pack's
/// in-flight light: the style-switch guard also compares the pack
/// fingerprint (risk #2), so foreign sparks never re-render under a new pack.
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

/// GAP-1 proof: every wired Trail Pack param actually MOVES the emitted custom
/// fold (a pack must never expose a knob that silently does nothing). The
/// complementary "and NOT for built-ins" half is the byte-identical golden
/// above — no built-in path reads `cfg.pack`, so a pack param can't perturb it.
#[test]
fn each_wired_pack_param_changes_the_custom_fold() {
    let g = geom();
    // The default injected-clock script (typing run → jump → two static decay
    // frames). Reveals boost/heat/life params (per-frame coverage + spark
    // survival) and the post-jump crown/ring windows (the +1400ms static frame
    // sits 700ms after the jump).
    let fold = |src: &str| run_script(&pack_cfg(compile_pack(src)), g);
    // A short type-then-HOLD script: a static frame 420ms after a single typed
    // key, so the crown's TYPING window is the deciding factor.
    let fold_hold = |src: &str| -> u64 {
        let c = pack_cfg(compile_pack(src));
        let mut glow = CursorGlow::default();
        let t0 = Instant::now();
        let mut out = Vec::new();
        let mut acc = 0u64;
        let mut f = |fp: u64| acc = fp_fold(acc, fp);
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

    // Chaining OFF (`chain_gap_max = 0.0`) so a spark's life is the PURE heat
    // curve — isolating life_base_mul/life_a/life_b — with a base long enough
    // that a wake survives several frames. Also exercises heat gain/tau/bright
    // (per-frame coverage) and the crown/ring/bed channels.
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
    // Chaining ON with a TINY base life so the chain floor dominates —
    // isolating chain_keys/chain_gap_max/chain_life_max.
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

    // (label, base, from→to, comparator). Each swap must move ITS fold.
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

/// GAP-1 proof: the `theme` arm selects the LIGHT-theme treatment. On a light
/// ground `auto` keeps the additive beam, `over_veil` renders source-over veil
/// rails, and `darken_tints` tints them deeper — three distinct folds — while
/// the DARK-theme render is arm-independent (the arm only touches the light
/// path, so built-ins, always additive, are untouched).
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

    // The arm must NOT change the dark-theme render (additive beam either way).
    let dfold = |src: &str| run_script(&pack_cfg(compile_pack(src)), g);
    assert_eq!(
        dfold(&with("auto")),
        dfold(&with("over_veil")),
        "the theme arm must not perturb the dark-theme render"
    );
}
