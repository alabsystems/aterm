// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! LICENSED-MOVE PARITY — the other half of the license
//! (`docs/design/EFFECTS-LICENSE-REDESIGN.md`).
//!
//! The license is a PRECONDITION, not a classifier: once a move is licensed —
//! a fresh key hint stands behind it — the v0.43.0 classifier runs exactly as
//! it did, and every pixel it lays must be the pixel it laid before. This file
//! is the proof of that clause, and its golden was captured deliberately:
//!
//!   1. the script below was written against the tree WITHOUT the license
//!      seam (the pre-license `HEAD`), run, and its fold printed;
//!   2. the number was pasted into `GOLDEN`;
//!   3. the same script was run against the tree WITH the seam.
//!
//! So a passing assertion here is a literal before/after equality across the
//! license commit, per style, over emitted quads, frame fingerprints, spawn
//! counts, live spark counts, momentum, and sound cues. It uses the PUBLIC API
//! only, so the same file can be replayed against either tree.
//!
//! Set `ATERM_CAPTURE_TYPED_PARITY=1` to reprint the fold instead of asserting
//! (that is how step 1 and step 3 were run).

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
        radius: 0.6,
        ring: true,
        beam: !matches!(style, GlowStyle::Water | GlowStyle::RainbowKitty),
        head_dx: 0.5,
        pack: None,
        wake_persist_s: 1.2,
        ribbon_tall: false,
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

/// A TYPED script — the shape the license exists to admit. Six glyph echoes at
/// human cadence, a wrap at the right margin, three more glyphs on the new
/// row, a gesture jump, and the decay tail.
fn typed_script(style: GlowStyle) -> u64 {
    let g = geom();
    let c = cfg(style);
    let tc = trail_cfg();
    let mut glow = CursorGlow::default();
    let mut trail = CursorTrail::default();
    let mut out: Vec<GlowQuad> = Vec::new();
    let mut trail_out = Vec::new();
    let t0 = Instant::now();
    let mut acc: u64 = 0;

    let step = |glow: &mut CursorGlow,
                trail: &mut CursorTrail,
                out: &mut Vec<GlowQuad>,
                trail_out: &mut Vec<_>,
                acc: &mut u64,
                at: Instant,
                cell: (u16, u16)| {
        let fp = glow.tick(Some(cell), at, &c, g, out);
        trail.tick(Some(cell), at, &tc, trail_out);
        fold(acc, &fp.to_le_bytes());
        fold(acc, &glow.spawns().to_le_bytes());
        fold(acc, &(glow.live_sparks() as u64).to_le_bytes());
        fold(acc, &(glow.ribbon_segments() as u64).to_le_bytes());
        fold(acc, &glow.typing_momentum(at).to_bits().to_le_bytes());
        for q in out.iter() {
            fold(acc, format!("{q:?}").as_bytes());
        }
        for cell in trail_out.iter() {
            fold(acc, format!("{cell:?}").as_bytes());
        }
        for cue in glow.drain_sound_cues() {
            fold(acc, format!("{cue:?}").as_bytes());
        }
    };

    // Seed the anchor.
    step(
        &mut glow,
        &mut trail,
        &mut out,
        &mut trail_out,
        &mut acc,
        t0,
        (2, 30),
    );
    // Six typed glyph echoes at 90 ms — the licensed typing run.
    for k in 1..=6u64 {
        let at = t0 + Duration::from_millis(90 * k);
        glow.note_synthetic_typed(at, 1);
        trail.note_synthetic_typed(at);
        step(
            &mut glow,
            &mut trail,
            &mut out,
            &mut trail_out,
            &mut acc,
            at,
            (2, 30 + k as u16),
        );
    }
    // The wrap at the right margin, then three glyphs on the new row.
    for (k, cell) in [(7u64, (3u16, 0u16)), (8, (3, 1)), (9, (3, 2))] {
        let at = t0 + Duration::from_millis(90 * k);
        glow.note_synthetic_typed(at, 1);
        trail.note_synthetic_typed(at);
        step(
            &mut glow,
            &mut trail,
            &mut out,
            &mut trail_out,
            &mut acc,
            at,
            cell,
        );
    }
    // A licensed gesture jump.
    let jump = t0 + Duration::from_millis(1_100);
    glow.note_synthetic_move(jump);
    trail.note_synthetic_move(jump);
    step(
        &mut glow,
        &mut trail,
        &mut out,
        &mut trail_out,
        &mut acc,
        jump,
        (5, 30),
    );
    // The decay tail.
    for ms in [1_150u64, 1_400, 2_200, 4_000] {
        let at = t0 + Duration::from_millis(ms);
        step(
            &mut glow,
            &mut trail,
            &mut out,
            &mut trail_out,
            &mut acc,
            at,
            (5, 30),
        );
    }
    acc
}

/// RED-FIRST, and it was run that way: replayed against the pre-license tree
/// this test FAILS (`earned light was wiped by a cold program move`), because
/// the old denial path reached `clear_denied_move_visuals` and took the ribbon
/// with it. It is the retention half of the license — the v0.43.0 law that
/// earned light is never destroyed by someone else's output.
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
        let earned = glow.live_sparks();
        let earned_trail = trail_out.len();
        assert!(earned > 0, "{style:?}: the typed echo must earn light");
        assert!(earned_trail > 0, "{style:?}: the comet must exist");
        // …and a cold program hop at the SAME instant (so nothing can decay)
        // must leave every cell of it alone.
        glow.tick(Some((4, 20)), typed, &c, g, &mut out);
        trail.tick(Some((4, 20)), typed, &tc, &mut trail_out);
        assert_eq!(
            glow.live_sparks(),
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

#[test]
fn a_licensed_typed_move_is_byte_identical_across_the_license_commit() {
    // Captured on the PRE-license tree (see the module header).
    //
    // ONE ENTRY HAS BEEN RE-BASELINED, deliberately and on the record:
    // `RainbowKitty` (index 2) moved 5_198_079_201_314_408_078 →
    // 7_427_253_331_732_318_204 when the default dark ribbon's SPECTRUM and
    // its typed-streak bloom were restored (owner, 2026-08-24: "I want it to
    // look more like a rainbow!", "there used to be what looked like the jump
    // streak drawing behind the cursor that was painted as typing"). That is a
    // repaint of the default dark mark's emitter (`emit_rainbow_mark_dark`),
    // not a license effect, and no
    // byte-exact golden can survive an intentional repaint.
    //
    // THE CLAUSE THIS FILE PROVES IS UNDAMAGED, and the diff is the proof: the
    // other EIGHT styles' folds are bit-for-bit the pre-license numbers, and
    // rainbow's own admission, envelopes, retract and spark population are
    // untouched by the restoration (only the emitter that turns those sparks
    // into quads changed). Re-baseline an entry here ONLY for a deliberate,
    // owner-driven change to that style's emitter — never to quiet a failure
    // whose cause is the license seam itself, which is the one thing this
    // number exists to catch.
    const GOLDEN: [u64; 9] = [
        6_526_463_453_780_881_225,
        6_256_934_022_851_981_454,
        // RE-BASELINED AGAIN 2026-08-26, entry 2 (rainbow) ONLY, for the
        // deliberate owner-driven restoration of the ADVANCING rainbow and the
        // HIGHLIGHTER: *"it still doesn't look like an advancing rainbow like
        // before it did, and you also removed the 'highlighter' cursor trail
        // behind the text"*. Three things moved, all inside the rainbow
        // emitter and its hue bookkeeping: the dark mark now colours each cell
        // from that cell's OWN LAID hue (`rainbow_laid_sweep`) instead of from
        // its head-to-tail ordinal, a coalesced rainbow sweep lays each swept
        // cell its own successive hue instead of one hue for the whole batch,
        // and the default look composes the v0.43 highlighter behind the
        // glyphs with the strip in the leading. Every other style folded
        // BYTE-IDENTICAL across the change (`3_323_371_464_999_701_743` was
        // the previous rainbow value), which is the evidence that only the
        // rainbow path moved — the license seam itself still lays the pixel it
        // always laid.
        3_947_126_183_161_842_362,
        10_295_259_453_273_105_322,
        3_062_372_403_814_732_219,
        11_710_665_231_074_982_027,
        10_555_482_947_263_687_286,
        5_389_426_601_699_088_895,
        827_602_822_369_475_671,
    ];
    let styles = ALL_STYLES;
    let mut actual = [0u64; 9];
    for (i, &style) in styles.iter().enumerate() {
        let a = typed_script(style);
        let b = typed_script(style);
        assert_eq!(a, b, "{style:?}: the typed script is nondeterministic");
        assert_ne!(a, 0, "{style:?}: the typed script folded nothing");
        actual[i] = a;
    }
    if std::env::var_os("ATERM_CAPTURE_TYPED_PARITY").is_some() || GOLDEN.iter().all(|&v| v == 0) {
        panic!("CAPTURE TYPED PARITY GOLDEN = {actual:?}");
    }
    assert_eq!(
        actual, GOLDEN,
        "a licensed typed move stopped being byte-identical to the pre-license tree"
    );
}
