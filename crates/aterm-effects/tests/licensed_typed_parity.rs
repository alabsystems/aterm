// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Licensed-move parity (`docs/design/EFFECTS-LICENSE-REDESIGN.md`).
//!
//! The license is a precondition, not a classifier: once a move is licensed
//! (a fresh key hint stands behind it) the classifier runs unchanged, so the
//! pixels a licensed move lays must not depend on the license seam. This file
//! pins, per style, a fold over emitted quads, frame fingerprints, spawn
//! counts, the live light census, momentum and sound cues, using the public
//! API only.
//!
//! The light census reads whichever engine owns the frame (`live_light`): the
//! rainbow kitty is its v2 engine, whose light is ribbon cells, stars and
//! meteors, never a v1 spark, so for that style the census reads the `Status`
//! rows `trail status` reports; the other styles read `live_sparks`.
//!
//! Set `ATERM_CAPTURE_TYPED_PARITY=1` to print the fold instead of asserting.
//!
//! The golden carries its own control:
//! `an_unlicensed_script_moves_every_golden_entry` runs the identical script
//! with every key hint withheld and requires all nine entries to move.

use std::time::{Duration, Instant};

use aterm_effects::cursor_glow::{CursorGlow, Geom, GlowConfig, GlowStyle};
use aterm_effects::cursor_trail::{CursorTrail, TrailConfig};
use aterm_render::GlowQuad;

fn fold(acc: &mut u64, bytes: &[u8]) {
    for b in bytes {
        *acc = acc.wrapping_mul(1_000_003).wrapping_add(u64::from(*b));
    }
}

fn geom() -> Geom {
    Geom {
        cw: 8,
        ch: 16,
        rows: 6,
        cols: 40,
        origin_x: 0,
        origin_y: 0,
        win_w: 320,
        win_h: 96,
        head: 0,
    }
}

fn cfg(style: GlowStyle) -> GlowConfig {
    GlowConfig {
        classic_mono: false,
        // This parity script follows the shipping default; explicit underline
        // geometry has its own resolver, geometry, and live-paint pins.
        ribbon_tall: true,
        ribbon_flat: false,
        enabled: true,
        dark_theme: true,
        theme_fg: 0x00C8_D3F5,
        theme_bg: 0x001A_1B26,
        style,
        color: 0x0050_FA7B,
        accent: 0x007A_A2F7,
        duration: Duration::from_millis(240),
        length: 18,
        intensity: 0.7,
        audible: true,
        radius: 0.6,
        ring: true,
        beam: !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty),
        head_dx: 0.5,
        pack: None,
    }
}

fn trail_cfg() -> TrailConfig {
    TrailConfig {
        enabled: true,
        color: 0x0050_FA7B,
        duration: Duration::from_millis(240),
        max_len: 18,
        intensity: 0.7,
        warmth: 0.0,
    }
}

/// The live per-cell light population from whichever engine owns the frame:
/// v2's resident ribbon cells + live stars + live meteors while the rainbow
/// kitty's engine is engaged (`CursorGlow::v2_status` is `Some` exactly then),
/// v1's spark census for every other style.
fn live_light(glow: &CursorGlow) -> usize {
    glow.v2_status().map_or_else(
        || glow.live_sparks(),
        |s| (s.cells + s.stars + s.meteors) as usize,
    )
}

/// The resident typed band: the cells the ribbon holds, lit or not. Under v2
/// `CursorGlow::ribbon_segments` answers what is lit (after the edge-in fade),
/// so on the frame a press lays its first cell it reads 0 while the cell is
/// resident; `Status::cells` keeps the resident count. For other styles the
/// two readings agree.
fn resident_ribbon(glow: &CursorGlow) -> usize {
    glow.v2_status()
        .map_or_else(|| glow.ribbon_segments(), |s| s.cells as usize)
}

/// A typed script, the shape the license exists to admit: six glyph echoes at
/// human cadence, a wrap at the right margin, three more glyphs on the new
/// row, a gesture jump, and the decay tail.
///
/// With `licensed` false the identical trajectory arrives with no key hint
/// behind it (cold program movement), so the difference between the two folds
/// is the license alone. `an_unlicensed_script_moves_every_golden_entry` is
/// the control built on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TypedScriptOutcome {
    fingerprint: u64,
    licensed: u64,
    declined: u64,
    spawns: u64,
    peak_live_light: usize,
    peak_ribbon_segments: usize,
}

fn typed_script(style: GlowStyle, licensed: bool) -> TypedScriptOutcome {
    typed_script_with(&cfg(style), licensed)
}

/// [`typed_script`] on an explicit config — the flat-spelling control below
/// runs the same script with `ribbon_flat` set.
fn typed_script_with(c: &GlowConfig, licensed: bool) -> TypedScriptOutcome {
    let g = geom();
    let c = *c;
    let tc = trail_cfg();
    let mut glow = CursorGlow::default();
    let mut trail = CursorTrail::default();
    let mut out: Vec<GlowQuad> = Vec::new();
    let mut trail_out = Vec::new();
    let t0 = Instant::now();
    let mut acc: u64 = 0;
    let mut peak_live_light = 0;
    let mut peak_ribbon_segments = 0;

    let mut step =
        |glow: &mut CursorGlow, trail: &mut CursorTrail, at: Instant, cell: (u16, u16)| {
            let fp = glow.tick(Some(cell), at, &c, g, &mut out);
            trail.tick(Some(cell), at, &tc, &mut trail_out);
            let light = live_light(glow);
            let ribbon_segments = glow.ribbon_segments();
            peak_live_light = peak_live_light.max(light);
            peak_ribbon_segments = peak_ribbon_segments.max(ribbon_segments);
            fold(&mut acc, &fp.to_le_bytes());
            fold(&mut acc, &glow.spawns().to_le_bytes());
            fold(&mut acc, &(light as u64).to_le_bytes());
            fold(&mut acc, &(ribbon_segments as u64).to_le_bytes());
            fold(&mut acc, &glow.typing_momentum(at).to_bits().to_le_bytes());
            for q in &out {
                fold(&mut acc, format!("{q:?}").as_bytes());
            }
            for cell in &trail_out {
                fold(&mut acc, format!("{cell:?}").as_bytes());
            }
            for cue in glow.drain_sound_cues() {
                fold(&mut acc, format!("{cue:?}").as_bytes());
            }
        };

    // Seed the anchor.
    step(&mut glow, &mut trail, t0, (2, 30));
    // Six typed glyph echoes at 90 ms — the licensed typing run.
    for k in 1..=6u64 {
        let at = t0 + Duration::from_millis(90 * k);
        if licensed {
            glow.note_synthetic_typed(at, 1);
            trail.note_synthetic_typed(at);
        }
        step(&mut glow, &mut trail, at, (2, 30 + k as u16));
    }
    // The wrap at the right margin, then three glyphs on the new row.
    for (k, cell) in [(7u64, (3u16, 0u16)), (8, (3, 1)), (9, (3, 2))] {
        let at = t0 + Duration::from_millis(90 * k);
        if licensed {
            glow.note_synthetic_typed(at, 1);
            trail.note_synthetic_typed(at);
        }
        step(&mut glow, &mut trail, at, cell);
    }
    // A licensed gesture jump.
    let jump = t0 + Duration::from_millis(1_100);
    if licensed {
        glow.note_synthetic_move(jump);
        trail.note_synthetic_move(jump);
    }
    step(&mut glow, &mut trail, jump, (5, 30));
    // The decay tail.
    for ms in [1_150u64, 1_400, 2_200, 4_000] {
        let at = t0 + Duration::from_millis(ms);
        step(&mut glow, &mut trail, at, (5, 30));
    }
    let tally = glow.admission_tally();
    TypedScriptOutcome {
        fingerprint: acc,
        licensed: tally.licensed,
        declined: tally.declined,
        spawns: glow.spawns(),
        peak_live_light,
        peak_ribbon_segments,
    }
}

/// Earned light is never destroyed by someone else's output: a cold program
/// move must not wipe the ribbon a licensed typed run laid.
#[test]
fn earned_light_survives_a_cold_program_move() {
    let g = geom();
    let tc = trail_cfg();
    for style in ALL_STYLES {
        let c = cfg(style);
        let mut glow = CursorGlow::default();
        let mut trail = CursorTrail::default();
        let mut out: Vec<GlowQuad> = Vec::new();
        let mut trail_out = Vec::new();
        let t0 = Instant::now();
        glow.tick(Some((2, 3)), t0, &c, g, &mut out);
        trail.tick(Some((2, 3)), t0, &tc, &mut trail_out);
        // A licensed typed echo earns real light…
        let typed = t0 + Duration::from_millis(8);
        glow.note_synthetic_typed(typed, 1);
        trail.note_synthetic_typed(typed);
        glow.tick(Some((2, 4)), typed, &c, g, &mut out);
        trail.tick(Some((2, 4)), typed, &tc, &mut trail_out);
        let earned = live_light(&glow);
        let earned_trail = trail_out.len();
        assert!(earned > 0, "{style:?}: the typed echo must earn light");
        assert!(earned_trail > 0, "{style:?}: the comet must exist");
        // …and a cold program hop at the SAME instant (so nothing can decay)
        // must leave every cell of it alone.
        glow.tick(Some((4, 20)), typed, &c, g, &mut out);
        trail.tick(Some((4, 20)), typed, &tc, &mut trail_out);
        assert_eq!(
            live_light(&glow),
            earned,
            "{style:?}: earned light was wiped by a cold program move"
        );
        assert_eq!(
            trail_out.len(),
            earned_trail,
            "{style:?}: the earned comet was wiped by a cold program move"
        );
    }
}

/// One keypress can present as several cursor deltas when the response is
/// split across parser batches. The key's one-shot license belongs to the
/// first observed move; a later suffix in the same freshness window neither
/// borrows it nor erases the light the first batch earned, but becomes the
/// source anchor for what arrives next.
#[test]
fn split_batch_suffix_is_dark_but_keeps_resident_light_and_advances_anchors() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty);
    let tc = trail_cfg();
    let mut glow = CursorGlow::default();
    let mut trail = CursorTrail::default();
    let mut out: Vec<GlowQuad> = Vec::new();
    let mut trail_out = Vec::new();
    let t0 = Instant::now();

    glow.tick(Some((2, 3)), t0, &c, g, &mut out);
    trail.tick(Some((2, 3)), t0, &tc, &mut trail_out);

    // The physical press is stamped once at the input boundary. Its first
    // parser batch moves the caret and consumes that one-shot license.
    let first = t0 + Duration::from_millis(8);
    glow.note_typed_cells(first, 1);
    trail.note_typed(first);
    glow.tick(Some((2, 4)), first, &c, g, &mut out);
    trail.tick(Some((2, 4)), first, &tc, &mut trail_out);

    let earned_spawns = glow.spawns();
    let earned_light = live_light(&glow);
    let earned_ribbon = resident_ribbon(&glow);
    let earned_trail: Vec<(usize, usize)> =
        trail_out.iter().map(|cell| (cell.row, cell.col)).collect();
    assert_eq!(earned_spawns, 1, "the first batch spends the press once");
    assert!(earned_light > 0, "the first batch must earn rainbow light");
    assert!(earned_ribbon > 0, "the first batch must lay the ribbon");
    assert!(
        !earned_trail.is_empty(),
        "the first batch must lay the comet"
    );
    assert_eq!(glow.cursor_anchor(), Some((2, 4)));
    assert_eq!(trail.cursor_anchor(), Some((2, 4)));
    assert!(!glow.move_licensed(first), "the glow license is one-shot");
    assert!(!trail.move_licensed(first), "the trail license is one-shot");

    // The response's suffix lands in a later parser batch while the original
    // timestamp would still be fresh. It owns no second license: no births,
    // no resident wipe, but both engines must track its real destination.
    let suffix = first + Duration::from_millis(1);
    glow.tick(Some((2, 5)), suffix, &c, g, &mut out);
    trail.tick(Some((2, 5)), suffix, &tc, &mut trail_out);

    assert_eq!(glow.spawns(), earned_spawns, "the suffix minted glow");
    assert_eq!(
        live_light(&glow),
        earned_light,
        "the suffix erased resident rainbow light"
    );
    assert_eq!(
        resident_ribbon(&glow),
        earned_ribbon,
        "the suffix cut the resident ribbon"
    );
    assert_eq!(
        trail_out
            .iter()
            .map(|cell| (cell.row, cell.col))
            .collect::<Vec<_>>(),
        earned_trail,
        "the suffix changed the resident comet bed"
    );
    assert_eq!(glow.cursor_anchor(), Some((2, 5)));
    assert_eq!(trail.cursor_anchor(), Some((2, 5)));

    let tally = glow.admission_tally();
    assert_eq!((tally.licensed, tally.declined), (1, 1));
    assert_eq!(
        tally.last_decline_reason,
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );
}

const ALL_STYLES: [GlowStyle; 9] = [
    GlowStyle::Lumen,
    GlowStyle::Phaser,
    GlowStyle::RainbowKitty,
    GlowStyle::Sparkle,
    GlowStyle::Fire,
    GlowStyle::Laser,
    GlowStyle::Beam,
    GlowStyle::Water,
    GlowStyle::Comet,
];
// `GlowStyle::Classic` is absent: every test here asserts that a style draws
// nothing for a move no keystroke asked for, and the classic engine does not
// consult the license (it spawns on observed cursor motion). Its determinism
// and idle-zero pins live in `classic_wake`, and its whole-frame fold is
// pinned by `builtins_are_byte_identical_and_never_touch_the_custom_path`.

#[test]
fn a_licensed_typed_move_is_byte_identical_across_the_license_commit() {
    // Per-style fold of the licensed typed script, in `ALL_STYLES` order.
    // Re-capture (`ATERM_CAPTURE_TYPED_PARITY=1`) only for a deliberate change
    // to a style's emitter or to `GlowQuad`'s fields (the fold hashes each
    // quad's Debug string), and confirm which entries moved and why; never to
    // quiet a failure caused by the license seam, which this array exists to
    // catch.
    const GOLDEN: [u64; 9] = [
        2_012_044_335_639_200_132,
        9_056_904_005_182_467_320,
        9_770_655_941_870_276_673,
        6_434_568_479_864_447_486,
        1_103_233_083_236_013_119,
        10_518_610_175_969_919_396,
        9_195_345_295_744_972_650,
        16_308_416_874_437_072_549,
        9_650_234_412_234_410_662,
    ];
    let styles = ALL_STYLES;
    let mut actual = [0u64; 9];
    for (i, &style) in styles.iter().enumerate() {
        let a = typed_script(style, true);
        let b = typed_script(style, true);
        assert_eq!(a, b, "{style:?}: the typed script is nondeterministic");
        assert_ne!(
            a.fingerprint, 0,
            "{style:?}: the typed script folded nothing"
        );
        actual[i] = a.fingerprint;
    }
    if std::env::var_os("ATERM_CAPTURE_TYPED_PARITY").is_some() || GOLDEN.iter().all(|&v| v == 0) {
        panic!("CAPTURE TYPED PARITY GOLDEN = {actual:?}");
    }
    assert_eq!(
        actual, GOLDEN,
        "a licensed typed move stopped being byte-identical to the pre-license tree"
    );
}

/// With `GlowConfig::ribbon_flat` set (the `rainbow kitty flat` spelling) every
/// comet branch in `ribbon.rs` collapses to the pre-comet expression. The
/// pinned fold measures the flat body; the `assert_ne!` control keeps it a
/// measurement (the default comet body must fold differently).
#[test]
fn the_flat_spelling_keeps_the_pre_comet_typed_fold() {
    // The flat spelling's typed fold. The flag gates the comet profile, the
    // vivid rail and the from-the-hand wipe, but not other ribbon rules, so
    // those move this value too.
    const PRE_COMET: u64 = 10_810_081_371_715_245_225;
    let mut flat = cfg(GlowStyle::RainbowKitty);
    flat.ribbon_flat = true;
    let a = typed_script_with(&flat, true);
    let b = typed_script_with(&flat, true);
    assert_eq!(a, b, "the flat typed script is nondeterministic");
    assert_eq!(
        a.fingerprint, PRE_COMET,
        "the flat spelling stopped collapsing every comet branch"
    );
    // …and the control: the default body is the comet, so the same script
    // WITHOUT the flag folds to something else (entry 2 of `GOLDEN`).
    assert_ne!(
        typed_script(GlowStyle::RainbowKitty, true).fingerprint,
        PRE_COMET,
        "the comet body must move the fold, or the flat pin is vacuous"
    );
}

/// The control for the golden above: the identical script with key hints
/// withheld (cold program movement over the same trajectory) must move every
/// one of the nine entries, so the golden demonstrably depends on the license.
///
/// Per entry rather than a whole-array `assert_ne!`, which would pass as soon
/// as one style moved.
#[test]
fn an_unlicensed_script_moves_every_golden_entry() {
    for (i, &style) in ALL_STYLES.iter().enumerate() {
        let licensed = typed_script(style, true);
        let cold = typed_script(style, false);
        assert_eq!(
            cold,
            typed_script(style, false),
            "{style:?}: the unlicensed script is nondeterministic — this control \
             cannot mean anything until it is not"
        );
        assert_eq!(
            (licensed.licensed, licensed.declined),
            (10, 0),
            "{style:?}: the positive arm did not admit every authored move"
        );
        assert_eq!(
            (cold.licensed, cold.declined),
            (0, 10),
            "{style:?}: withholding hints did not exercise every denial"
        );
        assert_eq!(licensed.spawns, 10, "{style:?}: an admitted move was lost");
        assert_eq!(cold.spawns, 0, "{style:?}: a denied move minted light");
        assert!(
            licensed.peak_live_light > 0,
            "{style:?}: the licensed control never carried live light"
        );
        assert_eq!(
            cold.peak_live_light, 0,
            "{style:?}: the unlicensed control carried live light"
        );
        if style == GlowStyle::RainbowKitty {
            assert!(
                licensed.peak_ribbon_segments > 0,
                "the licensed rainbow control never laid its ribbon"
            );
            assert_eq!(
                cold.peak_ribbon_segments, 0,
                "the unlicensed rainbow control laid a ribbon"
            );
        }
        assert_ne!(
            licensed.fingerprint, cold.fingerprint,
            "{style:?} (GOLDEN entry {i}): withholding every key hint left the fold \
             UNCHANGED. The golden above is then blind to the license seam it exists \
             to guard — it would stay green through the exact regression it was \
             written for."
        );
    }
}
