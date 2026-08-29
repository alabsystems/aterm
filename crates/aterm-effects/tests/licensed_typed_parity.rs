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
//!
//! AND THE GOLDEN CARRIES ITS OWN CONTROL. A byte-exact constant that has been
//! RE-BASELINED three times in four days is exactly the shape that quietly
//! stops testing anything: each re-baseline is a human deciding from prose
//! that the change was art rather than the seam, and nothing re-checks that
//! the number can still move for the right reason.
//! `an_unlicensed_script_moves_every_golden_entry` re-checks it every run —
//! the identical script with every key hint withheld must move all nine
//! entries — so the next author re-baselining a rainbow repaint is standing on
//! a measurement, not on this comment.

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
///
/// `licensed` is the SEAM, switched: with it false the identical cursor
/// trajectory arrives with no key hint behind any of it — the pre-license
/// denial path, cold program movement. Nothing else about the script changes,
/// so the difference between the two folds is the license and only the
/// license. `an_unlicensed_script_moves_every_golden_entry` is the standing
/// control built on it; see the module header for why a golden that nothing
/// can redden is the shape this file has to avoid.
fn typed_script(style: GlowStyle, licensed: bool) -> u64 {
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
        if licensed {
            glow.note_synthetic_typed(at, 1);
            trail.note_synthetic_typed(at);
        }
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
        if licensed {
            glow.note_synthetic_typed(at, 1);
            trail.note_synthetic_typed(at);
        }
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
    if licensed {
        glow.note_synthetic_move(jump);
        trail.note_synthetic_move(jump);
    }
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

/// One physical keypress can be presented as more than one cursor delta when
/// the terminal response is split across parser batches. The key's one-shot
/// license belongs to the first observed move; a later suffix in the same
/// freshness window must neither borrow that license nor erase the light the
/// first batch earned. It still becomes the honest source anchor for whatever
/// arrives next.
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
    let earned_sparks = glow.live_sparks();
    let earned_ribbon = glow.ribbon_segments();
    let earned_trail: Vec<(usize, usize)> =
        trail_out.iter().map(|cell| (cell.row, cell.col)).collect();
    assert_eq!(earned_spawns, 1, "the first batch spends the press once");
    assert!(earned_sparks > 0, "the first batch must earn rainbow light");
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
        glow.live_sparks(),
        earned_sparks,
        "the suffix erased resident rainbow light"
    );
    assert_eq!(
        glow.ribbon_segments(),
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
        // RE-BASELINED A THIRD TIME 2026-08-27, entry 2 (rainbow) ONLY:
        // `3_947_126_183_161_842_362` → the value below. The cause is NOT one
        // repaint but four deliberate rainbow-emitter commits that landed on
        // the codex lane and were never re-captured here — d0d0b863 (smooth
        // rainbow, coherent kitty frames), 7d712dc0 (continuous presentation),
        // 8c85c952 (unified fade + palette) and e2bc14c4 (gaps and colour
        // seams). They reached main through the merge da2832f0, which kept
        // origin/main's NUMBER while taking both sides' CODE, so this golden
        // has been stale — and main red — ever since. (da2832f0 itself does
        // not compile: it kept a use of `Self::RAINBOW_WAVE_RAD` whose const
        // the codex side had deleted. A bisect therefore stops at the merge;
        // the first commit that both builds and disagrees is d0d0b863.)
        //
        // The mechanisms are the emitter's clocks and raster: the hue and wave
        // clocks moved onto the exact phase ring (`rainbow_momentum_bands`
        // `phase * 2.2` → `rainbow_ring_turns(phase, 359.0/1024.0) * TAU`,
        // `RAINBOW_LIGHT_RAIL_FLOW` 0.35 → 179/512), `rainbow_glint_profile`
        // arrived, and a per-pixel sub-cell raster replaced the slab raster.
        //
        // WHY THIS IS A REPAINT AND NOT THE SEAM — measured, not argued. The
        // fold was decomposed per channel at the golden's own tree and at
        // HEAD. For all NINE styles, and for rainbow in particular, these are
        // BYTE-IDENTICAL: `spawns`, `live_sparks`, `ribbon_segments`,
        // `typing_momentum`, the comet-trail cells, the drained sound cues,
        // the admission tally (licensed=10, declined=0) and the whole
        // per-step trajectory including both engines' anchors. The other
        // eight styles do not move on ANY channel. Only rainbow's frame
        // fingerprint and its over-ink quad fold moved (1593 → 3047 quads).
        // A seam regression cannot produce that signature: disabling the
        // `type_hint` disjunct in `move_licensed` moves all NINE entries and
        // flips the tally to licensed=1/declined=9, while reverting a single
        // line of `emit_rainbow_mark_dark` moves index 2 alone with the tally
        // frozen — which is exactly the shape observed here.
        //
        // Read the sentence above about "admission … untouched" as a claim
        // about these MEASURED channels, not about the source: the machinery
        // did change (e2bc14c4 added `refresh_rainbow_cell_owner` and altered
        // the `hue_advances` seed), and both are provably counter-neutral on
        // this script. Note also that the mark emitter's ink reaches this
        // number only through the frame fingerprint — the ribbon is written
        // to `under_out`, which the script never folds as quads — so both
        // `fp` and the quad fold moved, and neither alone explains it.
        //
        // RE-BASELINED AGAIN (companion head-hue seam, 2026-08-28). Index 2 is
        // the RAINBOW style and it moved ALONE; the other eight are unchanged
        // bytes. That is the repaint signature this comment already documents,
        // not the seam one: a seam regression "moves all NINE entries and flips
        // the tally to licensed=1/declined=9", and the tally is frozen here —
        // both sibling tests in this file (`split_batch_suffix_...` and
        // `earned_light_survives_a_cold_program_move`) still pass untouched.
        //
        // The cause is `rainbow_head_rgb` gaining a `head_hue` colour authority.
        // It previously read "the newest TYPING spark", but tail repair appends
        // missing older cells AFTER the fresh head, so that sample was not the
        // head; it also returned `None` before the first cell and after the last
        // retired, and the `None <-> Some` swap threw the caret and its inner
        // halo a whole palette arc on first-key and final-fade frames. Rainbow
        // is the only style with a companion colour authority, so rainbow is the
        // only index that can move.
        //
        // RECAPTURED 2026-08-27 for ROYGBIV. The palette moved from six
        // anchors to the canonical seven — red, orange, yellow, green, blue,
        // INDIGO, violet — on the owner's ruling (*"a rainbow. like in the
        // sky"*), and the hand-built "neutralized handoff" that drove the
        // green→blue interval 95.3% of the way to flat grey at its midpoint
        // was deleted with it. A palette change necessarily moves a byte fold
        // of the emitter, so this golden is re-captured rather than repaired.
        // The evidence that it is still the SURGICAL change this file exists
        // to police: the recapture moved index 2 alone
        // (`3_947_126_183_161_842_362` was the six-anchor value) and every
        // other style folded byte-identical, so the license seam still lays
        // the pixel it always laid.
        // Re-captured on the MERGED tree: upstream's companion head-hue seam
        // and this branch's ROYGBIV palette are both present, which is why
        // neither side's number survived the rebase. Index 2 moved ALONE and
        // the other eight came back byte-identical to the committed values —
        // the repaint signature this file's own diagnostic describes, not the
        // seam one.
        15_294_253_527_449_941_578,
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
        let a = typed_script(style, true);
        let b = typed_script(style, true);
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

/// THE CONTROL FOR THE GOLDEN ABOVE, and the reason it may be trusted.
///
/// A byte-exact number is only coverage if something can move it, and this
/// file's has been RE-BASELINED three times in four days for deliberate
/// rainbow repaints (see the comments inside `GOLDEN`). Every one of those
/// re-baselines was a human deciding, from prose, that the change was art and
/// not the seam. That decision now has a machine behind it: run the identical
/// script with the key hints WITHHELD — the pre-license denial path, cold
/// program movement over the same trajectory — and every one of the nine
/// numbers must move.
///
/// PROVEN, not assumed: disabling the `type_hint` disjunct in
/// `CursorGlow::move_licensed` and re-running the golden moves all NINE
/// entries (measured 2026-08-28 by exactly that source mutation, reverted).
/// This test reproduces that verdict from the PUBLIC API alone, on every run,
/// so "the golden still catches a broken license" stops being a claim in a
/// comment and becomes a result in the suite.
///
/// It is deliberately per-entry rather than a whole-array `assert_ne!`: an
/// array comparison passes as soon as ONE style moves, which would let the
/// control go green while eight styles had quietly stopped depending on the
/// license at all.
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
        assert_ne!(
            licensed, cold,
            "{style:?} (GOLDEN entry {i}): withholding every key hint left the fold \
             UNCHANGED. The golden above is then blind to the license seam it exists \
             to guard — it would stay green through the exact regression it was \
             written for."
        );
    }
}
