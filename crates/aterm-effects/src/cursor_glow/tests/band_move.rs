// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Band moves and the Codex composer.

use super::*;

/// The row-band law on the classifier anchors (`band_row`): a band above the
/// caret's row (the transcript archiving) or below it leaves `last` and
/// `last_visible` alone; Codex's viewport `[11..56]` riding down carries both
/// one row with their instants; a band the row leaves drops both, so the next
/// tick starts cold from its observed cursor. Degenerate bands are no-ops and
/// are not counted.
#[test]
fn translate_band_moves_last_and_last_visible_only_inside_the_band_and_drops_what_leaves() {
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((13, 2)), t0, &c, g, &mut out);
    assert_eq!(glow.last, Some((13, 2)));
    let seen = glow
        .last_visible
        .expect("a visible caret is remembered with its instant");
    assert_eq!(seen.0, (13, 2));
    // ABOVE: the transcript archiving under a pinned composer.
    glow.note_band_move(0, 11, -1);
    assert_eq!(glow.last, Some((13, 2)));
    assert_eq!(glow.last_visible, Some(seen));
    // BELOW: a region scroll under the caret.
    glow.note_band_move(20, 56, 1);
    assert_eq!(glow.last, Some((13, 2)));
    assert_eq!(glow.last_visible, Some(seen));
    // INSIDE: Codex phase A, the viewport riding down one row.
    glow.note_band_move(11, 56, 1);
    assert_eq!(glow.last, Some((14, 2)), "the anchor rides its band");
    assert_eq!(
        glow.last_visible,
        Some(((14, 2), seen.1)),
        "with the instant it was seen at"
    );
    assert_eq!(glow.band_moves(), 3);
    // LEAVES: the band ends on the caret's row and slides down.
    glow.note_band_move(10, 14, 1);
    assert_eq!(glow.last, None, "no honest source cell past the edge");
    assert_eq!(glow.last_visible, None);
    // Degenerate: nothing moved, nothing counted.
    glow.note_band_move(5, 4, 1);
    glow.note_band_move(5, 9, 0);
    assert_eq!(glow.band_moves(), 4);
}

/// The Backspace's key-time licence survives a band move. Under Codex the
/// erase's echo often lands after a streamed line has slid the composer down a
/// row. `quench_hint` (the licence) is kept across the band move (a scroll
/// retires it); the row-content classifiers (`bs_poof_hint`, `bs_baseline`)
/// describe the pre-move row and are retired as on a scroll. The echo is
/// licensed and the deletion speaks.
#[test]
fn a_backspace_echo_that_lands_after_a_band_move_is_still_licensed() {
    use crate::trail_sound::SoundKind;
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((13, 2)), t0, &c, g, &mut out);
    // Eight keys at ~8 cps into the composer, each echoed.
    for i in 0..8u64 {
        glow.note_synthetic_typed(at(120 * i + 5), 1);
        glow.tick(Some((13, 3 + i as u16)), at(120 * i + 10), &c, g, &mut out);
    }
    let _ = glow.drain_sound_cues().count();
    let tally0 = glow.admission_tally();
    assert_eq!(tally0.declined, 0, "fixture: typing is licensed");
    // The Backspace is pressed …
    glow.note_backspace(at(1000));
    assert!(
        glow.quench_hint.is_some(),
        "fixture: the key-time licence is banked"
    );
    // … and before its echo lands, Codex streams a line.
    glow.note_band_move(11, 56, 1);
    assert!(
        glow.quench_hint.is_some(),
        "the Backspace's key-time licence survives the band move"
    );
    assert!(
        glow.bs_poof_hint.is_none() && glow.bs_baseline.is_none(),
        "the row-content classifiers describe the pre-move row and retire"
    );
    assert_eq!(
        glow.last,
        Some((14, 10)),
        "the anchor rode down with the composer"
    );
    // The echo: the caret, now on row 14, retreats one cell.
    glow.tick(Some((14, 9)), at(1020), &c, g, &mut out);
    let tally = glow.admission_tally();
    assert_eq!(
        tally.licensed,
        tally0.licensed + 1,
        "the retreat is the user's own Backspace (last_decline_reason={:?})",
        tally.last_decline_reason
    );
    assert_eq!(tally.declined, tally0.declined);
    let cues: Vec<SoundKind> = glow.drain_sound_cues().map(|cue| cue.kind).collect();
    assert!(
        cues.contains(&SoundKind::Backspace),
        "the deletion still speaks after the band move (cued {cues:?})"
    );
}

/// T1, program output mints nothing: a band move is a coordinate transform,
/// never a licence. Codex streaming six lines under a resting caret (the
/// composer riding down one row per line) judges nothing, licenses and
/// declines nothing, spawns nothing and draws nothing.
#[test]
fn a_cold_band_move_under_a_resting_caret_spawns_nothing() {
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((13, 2)), t0, &c, g, &mut out);
    let spawns0 = glow.spawns();
    let tally0 = glow.admission_tally();
    let mut row = 13u16;
    for k in 0..6u64 {
        glow.note_band_move(row - 1, 56, 1);
        row += 1;
        let fp = glow.tick(Some((row, 2)), at(40 * (k + 1)), &c, g, &mut out);
        assert_eq!(glow.spawns(), spawns0, "line {k}: a band move spawned");
        assert_eq!(
            glow.admission_tally(),
            tally0,
            "line {k}: a band move was judged"
        );
        assert_eq!(fp, 0, "line {k}: a band move drew");
        assert!(out.is_empty() && glow.under_quads().is_empty() && glow.halos().is_empty());
    }
    assert_eq!(glow.band_moves(), 6);
    assert_eq!(glow.last, Some((19, 2)), "the anchor followed the composer");
}

/// A Codex session: eight keys at ~8 cps into the composer on row 13, then
/// Codex streams 28 lines 40 ms apart while typing continues (a key every third
/// line). Phase A, the inline viewport `[vt..56]` slides down one row per line:
/// on every line the ribbon is lit, all its light is on the caret's row, no
/// meteor flew, nothing was declined, and every key lit exactly one echo.
/// Phase B, hand at rest: the transcript `[0..row−2]` archives up five lines
/// under the pinned composer; the composer's light stays, nothing is judged,
/// and typing momentum reads exactly its natural decay.
#[test]
fn codex_streaming_keeps_the_ribbon_glued_to_the_sliding_composer() {
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((13, 2)), t0, &c, g, &mut out);
    let mut col = 2u16;
    for i in 0..8u64 {
        glow.note_synthetic_typed(at(120 * i + 5), 1);
        col += 1;
        glow.tick(Some((13, col)), at(120 * i + 10), &c, g, &mut out);
    }
    assert!(glow.ribbon_segments() >= 1, "fixture: the composer is lit");
    let spawns0 = glow.spawns();
    let tally0 = glow.admission_tally();

    // PHASE A.
    let mut row = 13u16;
    let mut t = 8 * 120 + 10;
    let mut keys = 0u64;
    for k in 0..28u16 {
        glow.note_band_move(row - 1, 56, 1);
        row += 1;
        t += 40;
        if k % 3 == 2 {
            glow.note_synthetic_typed(at(t - 5), 1);
            col += 1;
            keys += 1;
        }
        glow.tick(Some((row, col)), at(t), &c, g, &mut out);
        assert!(
            glow.ribbon_segments() >= 1,
            "line {k}: the ribbon vanished under the streamed line"
        );
        assert!(
            !v2_cols(&glow, row).is_empty(),
            "line {k}: no light on the composer's row {row}"
        );
        assert!(
            v2_cols(&glow, row - 1).is_empty(),
            "line {k}: light left behind on the vacated row {}",
            row - 1
        );
        assert_eq!(
            glow.v2_status().map(|s| s.meteors),
            Some(0),
            "line {k}: a band move is not a jump"
        );
        let tally = glow.admission_tally();
        assert_eq!(
            tally.declined, tally0.declined,
            "line {k}: something was declined ({:?})",
            tally.last_decline_reason
        );
    }
    assert_eq!(
        glow.spawns(),
        spawns0 + keys,
        "every key lit one echo; the 28 band moves lit nothing"
    );
    assert_eq!(glow.band_moves(), 28);

    // PHASE B.
    let rest_end = t + 5 * 40;
    let predicted = glow.typing_momentum(at(rest_end));
    let spawns1 = glow.spawns();
    let tally1 = glow.admission_tally();
    for k in 0..5u16 {
        t += 40;
        glow.note_band_move(0, row - 2, -1);
        glow.tick(Some((row, col)), at(t), &c, g, &mut out);
        assert!(
            glow.ribbon_segments() >= 1,
            "archival line {k}: the composer's light went with the transcript"
        );
        assert!(!v2_cols(&glow, row).is_empty(), "archival line {k}");
        assert_eq!(glow.v2_status().map(|s| s.meteors), Some(0));
        assert_eq!(glow.spawns(), spawns1, "archival line {k}: spawned");
        assert_eq!(glow.admission_tally(), tally1, "archival line {k}: judged");
    }
    assert_eq!(t, rest_end);
    assert!(
        (glow.typing_momentum(at(t)) - predicted).abs() <= 1e-6,
        "the momentum decayed naturally and did not restart: {} vs {predicted}",
        glow.typing_momentum(at(t))
    );
    assert_eq!(glow.band_moves(), 33);
}

/// Codex's heartbeat excursion: every streamed line begins with a CUP to the
/// viewport's top-left (`ESC[{vt};1H`) before the RI and text, so the print
/// anchor samples `(vt−1, 1)` with a fresh sequence each tick while the echo
/// lands on the composer row below. The echo-anchor lane stays inert: no
/// sweep on the excursion row, zero `program-row` declines, and every key
/// lights exactly its own echo.
#[test]
fn the_codex_excursion_print_anchor_stays_inert() {
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((13, 2)), t0, &c, g, &mut out);
    let spawns0 = glow.spawns();
    let mut row = 13u16;
    let mut col = 2u16;
    let mut t = 0u64;
    let mut keys = 0u64;
    for k in 0..24u16 {
        let vt = row - 1;
        glow.note_band_move(vt, 56, 1);
        row += 1;
        t += 40;
        if k % 3 == 2 {
            glow.note_synthetic_typed(at(t - 5), 1);
            col += 1;
            keys += 1;
        }
        // The excursion, sampled with a fresh stamp on every tick.
        glow.observe_print_anchor(Some((vt, 1, u64::from(k) + 1)));
        glow.tick(Some((row, col)), at(t), &c, g, &mut out);
        assert!(
            v2_cols(&glow, vt).is_empty() && !glow.sparks.iter().any(|s| s.row == vt),
            "line {k}: a sweep landed on the excursion row {vt}"
        );
    }
    assert_eq!(
        glow.admission_log()
            .filter(|r| r.reason == CursorGlow::DECLINE_PROGRAM_ROW)
            .count(),
        0,
        "the excursion row was never contested"
    );
    assert_eq!(glow.admission_tally().declined, 0);
    assert_eq!(
        glow.spawns(),
        spawns0 + keys,
        "each key lit its own echo and nothing else"
    );
    assert!(
        !v2_cols(&glow, row).is_empty(),
        "the composer is lit where it now is"
    );
}
