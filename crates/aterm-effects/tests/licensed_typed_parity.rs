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
        // This parity script follows the shipping default; explicit underline
        // geometry has its own resolver, geometry, and live-paint pins.
        ribbon_tall: true,
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
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct TypedScriptOutcome {
    fingerprint: u64,
    licensed: u64,
    declined: u64,
    spawns: u64,
    peak_live_sparks: usize,
    peak_ribbon_segments: usize,
}

fn typed_script(style: GlowStyle, licensed: bool) -> TypedScriptOutcome {
    let g = geom();
    let c = cfg(style);
    let tc = trail_cfg();
    let mut glow = CursorGlow::default();
    let mut trail = CursorTrail::default();
    let mut out: Vec<GlowQuad> = Vec::new();
    let mut trail_out = Vec::new();
    let t0 = Instant::now();
    let mut acc: u64 = 0;
    let mut peak_live_sparks = 0;
    let mut peak_ribbon_segments = 0;

    let mut step =
        |glow: &mut CursorGlow, trail: &mut CursorTrail, at: Instant, cell: (u16, u16)| {
            let fp = glow.tick(Some(cell), at, &c, g, &mut out);
            trail.tick(Some(cell), at, &tc, &mut trail_out);
            let live_sparks = glow.live_sparks();
            let ribbon_segments = glow.ribbon_segments();
            peak_live_sparks = peak_live_sparks.max(live_sparks);
            peak_ribbon_segments = peak_ribbon_segments.max(ribbon_segments);
            fold(&mut acc, &fp.to_le_bytes());
            fold(&mut acc, &glow.spawns().to_le_bytes());
            fold(&mut acc, &(live_sparks as u64).to_le_bytes());
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
        peak_live_sparks,
        peak_ribbon_segments,
    }
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
    // RE-BASELINED, ALL NINE, 2026-08-28 — MECHANICALLY, and the evidence is
    // why that word is allowed here. The fold hashes each quad's Debug string
    // (`fold(acc, format!("{q:?}").as_bytes())`), and `GlowQuad` gained an
    // `alpha` byte so the source-over bed could share one blend with the
    // additive family. Every Debug string therefore changed, for every style,
    // whether or not a single photon moved.
    //
    // A moving entry is normally the loudest alarm this file has, so it was
    // treated as one and CHECKED rather than re-recorded:
    //   * exactly ONE site emits `GlowBlend::Over` — `cursor_glow.rs`'s rainbow
    //     bed. Every other stream, and all eight non-rainbow styles, are
    //     `alpha == 0`.
    //   * `over_premul_is_add_sat_at_zero_alpha` (aterm-render) proves
    //     EXHAUSTIVELY, over the whole byte cross-product, that at `alpha == 0`
    //     the source-over equation IS `add_sat`.
    //   * `source_over_glow_under_is_byte_exact_and_leaves_the_additive_half_
    //     alone` (aterm-gpu) renders a mixed field over real text and finds
    //     CPU == GPU byte-exact, with the additive rows byte-identical when the
    //     over rows are removed.
    // So the light is unchanged and the licence seam did not move; the fold's
    // INPUT REPRESENTATION grew a field. If all nine ever move again without a
    // `GlowQuad` field change behind them, that is the regression this array
    // exists to catch, and it must not be re-recorded.
    // RE-CAPTURED ON THE MERGE, ALL NINE (rainbow-complete x origin/main).
    // TWO independent, already-documented causes are live in the same tree for
    // the first time, which is why NEITHER side's array survives:
    //   * this branch's `GlowQuad::alpha` field (the mechanical cause recorded
    //     above) rewrites every quad's Debug string, for every style, so all
    //     nine move whether or not a photon moved;
    //   * upstream's ROYGBIV palette and `head_hue` colour authority (the
    //     repaint cause recorded below) move index 2 on top of that.
    // THE MERGE-SPECIFIC CHECK, and it is a falsifiable one: the eight
    // non-rainbow entries must equal THIS BRANCH's committed eight byte for
    // byte, because ROYGBIV and `head_hue` can only reach rainbow while the
    // `alpha` field is already priced into our numbers. Index 2 alone must be
    // a value neither side ever recorded. Anything else — an upstream style
    // moving, or index 2 landing back on a known number — is the seam, not the
    // paint, and must not be re-recorded.
    //
    // **THE CHECK WAS RUN AND IT HELD, WHICH IS WHY THE ONE NUMBER BELOW MOVED.**
    // Measured on the merged tree: indices 0, 1, 3, 4, 5, 6, 7 and 8 came back
    // BYTE-IDENTICAL to the values this branch committed — not close, equal — and
    // index 2 went `4_687_125_888_682_913_049` → `3_301_500_979_061_321_610`,
    // which is neither this branch's six-anchor number nor upstream's
    // (`15_294_253_527_449_941_578`). That is the surgical signature this file
    // exists to police and not the seam's: a seam regression moves all nine and
    // flips the admission tally, and the tally is frozen — `an_unlicensed_script_
    // moves_every_golden_entry` still passes, as do both sibling tests in this
    // file, untouched.
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
    // RE-DERIVED 2026-08-29 after the caret/composited cyan law. Index 2 (rainbow)
    // ALONE moved; the other EIGHT are byte-identical to the values this branch
    // already carried, which is the evidence the licence seam did not move and only
    // the rainbow colour path did. The array was rebuilt from measurement rather
    // than merged, because a three-way merge of this literal had twice produced TEN
    // entries in a nine-element array — a shape that does not compile and, worse,
    // reads like a resolved conflict.
    // RE-DERIVED ON THE MERGE 2026-08-29 (caret-rim pixel law x traverse-per-mark
    // with decay): index 2 alone moved; the other EIGHT are byte-identical, the
    // evidence the licence seam did not. Rebuilt from capture, never hand-merged —
    // a three-way merge of this literal has produced ten entries in a nine-array
    // twice before.
    // RE-DERIVED 2026-08-29 (the sound-redesign merge), and the cause PRE-DATES
    // the merge: measured at the merge BASE — plain main `89d9f7b4`, the merge
    // that brought the cloud poof back to the shipped style — index 2 was
    // ALREADY `12_902_664_559_077_609_800` against the committed
    // `10_229_932_340_051_593_466`, i.e. main landed that merge without
    // re-capturing the rainbow entry and has been red here since. The
    // sound-redesign merge is measurement-NEUTRAL: the merged tree measures the
    // SAME nine values as its base, bit for bit — index 2 alone differs from
    // the committed array and the other EIGHT are byte-identical, the repaint
    // signature (rainbow is the style the cloud work repainted), not the seam
    // one. Captured from measurement, not hand-merged.
    // RE-DERIVED 2026-08-29 (the light-strike lane), and the cause PRE-DATES
    // the lane: measured at its base — the merge `7381ba1f` — index 2 was
    // ALREADY `10_097_007_343_695_914_747` against the committed
    // `12_902_664_559_077_609_800`, i.e. main once again landed a rainbow
    // merge without re-capturing this entry (the exact shape the previous
    // paragraph records). The lane itself is measurement-NEUTRAL here, and
    // that was verified rather than argued: this dark-theme script measures
    // the SAME nine values at the lane's base and at its head, bit for bit —
    // the lane's spawn-gate edit only removes a `dark_theme` conjunct that is
    // TRUE on this script, and its light draw arm is gated `!dark_theme`, so
    // no dark byte can move. Index 2 alone differs from the committed array
    // and the other EIGHT are byte-identical — the repaint signature, not the
    // seam one. Captured from measurement, not hand-merged.
    // RE-DERIVED 2026-08-30 (the PILE LAW, `settle_flying_pile`): the rainbow
    // frame now settles cross-owner flying crossings and the near-caret
    // allowance, so the rainbow entry's bytes legitimately move — index 2
    // alone, `10_097_… → 10_857_…`, with the other EIGHT byte-identical
    // (the repaint signature again). Captured from measurement on the merged
    // tree, not hand-merged.
    // RE-DERIVED 2026-08-30 (the RAINBOW METEOR lane): index 2 (rainbow) ALONE
    // moved, `10_857_… → 12_019_…`; the other EIGHT came back byte-identical —
    // the repaint signature this file's diagnostic describes, not the seam one
    // (the admission tally is frozen and both sibling tests pass untouched).
    // The rainbow bytes move for four deliberate, owner-driven reasons in the
    // jump gesture this script fires at ms 1100: (1) the flight phase — the
    // decay ticks at 1150/1400 now catch the meteor mid-flight/mid-retract on
    // a re-based clock instead of the old full-span retract; (2) the Starburst
    // is born at arrival (+RAINBOW_METEOR_FLIGHT_S), so the ring/sparkle
    // frames shift by the flight; (3) the Land chime rides the arrival edge,
    // so the cue drains one step later than it did; (4) the per-jump latch
    // roll advances the caret's worn colour one lay step across the jump.
    // Captured from measurement (ATERM_CAPTURE_TYPED_PARITY=1), not
    // hand-merged.
    // RE-DERIVED 2026-08-30 after the rainbow pile ledger began treating
    // separated emission runs as one complete particle owner. Only index 2
    // moved; the other eight and the independent licence controls stayed exact.
    // RE-DERIVED 2026-08-30 (INTEGRATION, meteor+band merged onto the classic
    // rework): the rainbow entry alone re-captured on the final tree.
    // RE-DERIVED 2026-08-30 (the BAND/CLASSIC RECONCILIATION): index 2
    // (rainbow) ALONE moved, `7_675_… → 331_874_…`; the other EIGHT came back
    // byte-identical — the repaint signature, not the seam one. The rainbow
    // bytes move for the reconciliation's four deliberate visible changes:
    // (1) the classic unroll is the 14-cell short traverse now, so this
    // script's 6- and 3-key runs lay their cells at 1/14 of the arc per cell
    // instead of 1/6 (the owner's short-text law); (2) the tall body's core
    // follows the equal-ledge law, so dim positions hold their plateau deeper
    // and the emitted quad stack changes; (3) a lone typed cell's wake
    // continues the classic walk reflected at the anchor instead of reading
    // flat red; (4) the wrap's fresh row opens on the gated re-anchor. All
    // captured from measurement (ATERM_CAPTURE_TYPED_PARITY=1), not
    // hand-merged.
    const GOLDEN: [u64; 9] = [
        10_317_128_623_903_768_537,
        17_965_605_562_081_848_086,
        331_874_870_921_564_289,
        15_654_209_172_669_807_490,
        3_818_617_666_977_618_171,
        17_432_548_801_852_476_563,
        4_382_914_939_507_566_134,
        591_884_352_308_604_767,
        3_259_176_104_562_415_775,
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
            licensed.peak_live_sparks > 0,
            "{style:?}: the licensed control never carried live light"
        );
        assert_eq!(
            cold.peak_live_sparks, 0,
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
