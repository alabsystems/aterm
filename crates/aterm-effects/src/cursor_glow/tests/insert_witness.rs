// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Insert witnesses, receipts and spinner rows.

use super::*;

/// Seed the hidden-caret TUI shape the row tests share: a spinner row at
/// `(10, 19)`, the input row at `(12, 2)`, one typed echo `2 → 3` at 245 ms
/// establishing it (`spawns == 1`, `last_anchor_sweep` on row 12).
fn established_input_row_beside_a_spinner(glow: &mut CursorGlow, t0: Instant) {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((10, 19, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.observe_print_anchor(Some((12, 2, 2)));
    glow.tick(None, at(32), &c, g, &mut out);
    glow.note_typed(at(240));
    glow.observe_print_anchor(Some((12, 3, 3)));
    glow.tick(None, at(245), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the input echo establishes the row");
}

/// The row witness: an insert of unknown width (a Tab, a ⌃V) is bound to the row
/// the hand was on when it was armed and spends on no other.
#[test]
fn a_tabs_insert_stamp_is_bound_to_the_row_the_hand_was_on() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    let mut strayed = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut strayed, t0, &mut out);
    strayed.note_user_gesture(at(700));
    strayed.note_insert_delivered(at(700), InsertWidth::Unknown);
    strayed.tick(Some((5, 0)), at(1000), &c, g, &mut out);
    assert_eq!(strayed.spawns(), 3, "the cross-row park is program output");
    strayed.tick(Some((5, 8)), at(1020), &c, g, &mut out);
    assert_eq!(
        strayed.spawns(),
        3,
        "a Tab's stamp licenses no advance on a row the hand was never on"
    );
    assert_eq!(
        strayed.admission_log().last().map(|r| r.reason),
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );

    let mut own = CursorGlow::default();
    let mut oout = Vec::new();
    three_visible_echoes(&mut own, t0, &mut oout);
    own.note_user_gesture(at(700));
    own.note_insert_delivered(at(700), InsertWidth::Unknown);
    own.tick(Some((3, 13)), at(720), &c, g, &mut oout);
    assert_eq!(own.spawns(), 4, "the Tab's own-row completion lights");
    assert_eq!(
        own.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );
}

/// A bare Tab's licence is a gesture's, not a paste's: an unknown width has
/// nothing to check but the row witness and the 32-cell bound, so it expires
/// at [`CursorGlow::INSERT_GESTURE_HINT_FRESH`] rather than
/// [`CursorGlow::INSERT_HINT_FRESH`], or a background job printing on the
/// prompt row would light as a 32-cell sweep.
#[test]
fn a_program_hop_after_a_tab_s_gesture_window_stays_dark() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let stale = 1400u64;
    assert!(
        f64::from(CursorGlow::INSERT_GESTURE_HINT_FRESH) < 1.4
            && f64::from(CursorGlow::INSERT_HINT_FRESH) > 1.4,
        "this pin only means something while 1400 ms separates the two \
         windows"
    );
    let run = |gap_ms: u64| -> u64 {
        let t0 = Instant::now();
        let at = |ms: u64| t0 + Duration::from_millis(ms);
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        established_input_row_beside_a_spinner(&mut glow, t0);
        let before = glow.spawns();
        glow.note_user_gesture(at(600));
        glow.note_insert_delivered(at(600), InsertWidth::Unknown);
        glow.observe_print_anchor(Some((12, 13, 7)));
        glow.tick(None, at(600 + gap_ms), &c, g, &mut out);
        glow.spawns() - before
    };
    assert_eq!(
        run(300),
        1,
        "fixture: inside the window the gesture still licenses its echo"
    );
    assert_eq!(
        run(stale),
        0,
        "{stale} ms after a bare Tab the same keyless hop is program \
         output and must stay dark"
    );
}

/// The row witness with a hidden caret (Claude Code's shape): the input row is
/// established, a Tab is pressed on it, and from 300 ms later a spinner row
/// advances one, one, then eight cells inside the insert window.
#[test]
fn a_spinner_row_advancing_after_a_tab_stays_dark() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    established_input_row_beside_a_spinner(&mut glow, t0);
    glow.note_user_gesture(at(600));
    glow.note_insert_delivered(at(600), InsertWidth::Unknown);
    for (i, (col, ms)) in [(20u16, 900u64), (21, 1100), (29, 1300)]
        .into_iter()
        .enumerate()
    {
        glow.observe_print_anchor(Some((10, col, 4 + i as u64)));
        glow.tick(None, at(ms), &c, g, &mut out);
    }
    assert_eq!(glow.spawns(), 1, "a spinner row cannot spend a Tab's stamp");
    let spinner_spends = glow
        .admission_log()
        .filter(|r| r.origin.0 == 10 && r.reason != CursorGlow::DECLINE_PROGRAM_ROW)
        .count();
    assert_eq!(spinner_spends, 0);
    glow.observe_print_anchor(Some((12, 11, 7)));
    glow.tick(None, at(1400), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "the input row's completion is the Tab's");
    assert_eq!(
        glow.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );
}

/// The row witness rides the band: an unknown-width insert (a multi-line
/// paste's `[Pasted text #1 +N lines] `, a Tab's completion) is armed on Codex's
/// composer row 13, and before its echo lands a streamed line moves the viewport
/// `[11..56]` down one row.
#[test]
fn an_insert_witness_rides_a_band_move_and_the_row_it_left_spends_nothing() {
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((13, 2)), t0, &c, g, &mut out);
    for (i, col) in (3..=5u16).enumerate() {
        let key = at(100 * (i as u64 + 1));
        glow.note_typed(key);
        glow.tick(
            Some((13, col)),
            key + Duration::from_millis(8),
            &c,
            g,
            &mut out,
        );
    }
    assert_eq!(glow.spawns(), 3, "fixture: three typed echoes light");
    glow.note_insert_delivered(at(700), InsertWidth::Unknown);
    assert_eq!(
        glow.insert.armed.map(|i| i.row),
        Some(Some(13)),
        "fixture: the witness is the caret's row"
    );
    glow.note_band_move(11, 56, 1);
    assert_eq!(
        glow.last,
        Some((14, 5)),
        "the caret rode down with the composer"
    );
    assert_eq!(
        glow.insert.armed.map(|i| i.row),
        Some(Some(14)),
        "the witness rode with the text it names"
    );
    glow.observe_print_anchor(Some((13, 10, 1)));
    glow.tick(Some((14, 5)), at(710), &c, g, &mut out);
    glow.observe_print_anchor(Some((13, 18, 2)));
    glow.tick(Some((14, 5)), at(715), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        3,
        "the row the composer left is a program row: it spends nothing"
    );
    glow.tick(Some((14, 31)), at(720), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        4,
        "the insert's echo on the moved row is its own (last_decline_reason={:?})",
        glow.admission_log().last().map(|r| r.reason)
    );
    assert_eq!(
        glow.admission_log()
            .last()
            .map(|r| (r.licence, r.origin, r.target)),
        Some((AdmissionRecord::LICENCE_INSERT, (14, 5), (14, 31)))
    );
    let lit = v2_cols(&glow, 14);
    assert!(
        (5..31u16).all(|col| lit.contains(&col)),
        "the span is lit on row 14: {lit:?}"
    );

    let mut gone = CursorGlow::default();
    let mut gout = Vec::new();
    gone.tick(Some((13, 2)), t0, &c, g, &mut gout);
    gone.note_insert_delivered(at(700), InsertWidth::Unknown);
    gone.note_band_move(10, 13, 1);
    assert_eq!(gone.last, None, "fixture: the caret left the band");
    assert_eq!(
        gone.insert.armed.map(|i| i.row),
        Some(None),
        "a witness carried past the band's edge is gone, not clamped"
    );
}

/// The unknown width's late receipt: a multi-line paste's receipt can land
/// after the frame that observed the echo, like a priced one's
/// (`a_receipt_that_lands_after_the_echoing_frame_still_lights_the_insert`).
#[test]
fn an_unknown_width_receipt_that_lands_late_claims_the_hop_by_its_row_witness() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.tick(Some((3, 31)), at(720), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        3,
        "the echo arrived before its receipt: refused"
    );
    glow.note_insert_delivered(at(722), InsertWidth::Unknown);
    glow.tick(Some((3, 31)), at(736), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        4,
        "the late unknown-width receipt claims the hop by its row witness"
    );
    assert_eq!(
        glow.admission_log()
            .last()
            .map(|r| (r.licence, r.origin, r.target)),
        Some((AdmissionRecord::LICENCE_INSERT, (3, 5), (3, 31)))
    );
    let lit = v2_cols(&glow, 3);
    assert!(
        (5..31u16).all(|col| lit.contains(&col)),
        "the span is lit: {lit:?}"
    );

    let mut wide = CursorGlow::default();
    let mut wout = Vec::new();
    three_visible_echoes(&mut wide, t0, &mut wout);
    wide.tick(Some((3, 38)), at(720), &c, g, &mut wout);
    wide.note_insert_delivered(at(722), InsertWidth::Unknown);
    wide.tick(Some((3, 38)), at(736), &c, g, &mut wout);
    assert_eq!(
        wide.spawns(),
        3,
        "a hop past the unknown class's bound is not the insert's"
    );
}

/// The parked caret's witness: with the DEC cursor visible but parked at the
/// origin while input row 2 is repainted under it, an unknown-width insert's
/// witness is the row the anchored lane proved (`hand_row`), not the parked
/// caret's.
#[test]
fn a_parked_carets_insert_witness_is_the_anchored_input_row_not_the_parked_row() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((0, 0)), t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((2, 2, 1)));
    glow.tick(Some((0, 0)), at(16), &c, g, &mut out);
    for (i, col) in (3..=5u16).enumerate() {
        let key = at(100 * (i as u64 + 1));
        glow.note_typed(key);
        glow.observe_print_anchor(Some((2, col, 2 + i as u64)));
        glow.tick(
            Some((0, 0)),
            key + Duration::from_millis(8),
            &c,
            g,
            &mut out,
        );
    }
    assert_eq!(
        glow.spawns(),
        3,
        "fixture: the anchored lane lit three echoes on row 2"
    );
    glow.note_insert_delivered(at(700), InsertWidth::Unknown);
    assert_eq!(
        glow.insert.armed.map(|i| i.row),
        Some(Some(2)),
        "the witness is the input row the anchored lane proved"
    );
    glow.tick(Some((0, 8)), at(720), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        3,
        "the parked row's walk is not the insert's echo"
    );
    assert_eq!(
        glow.admission_log().last().map(|r| r.reason),
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );
    glow.observe_print_anchor(Some((2, 13, 5)));
    glow.tick(Some((0, 8)), at(740), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        4,
        "the input row's completion is the insert's (last_decline_reason={:?})",
        glow.admission_log().last().map(|r| r.reason)
    );
    assert_eq!(
        glow.admission_log()
            .last()
            .map(|r| (r.licence, r.origin, r.target)),
        Some((AdmissionRecord::LICENCE_INSERT, (2, 5), (2, 13)))
    );

    let mut codex = CursorGlow::default();
    let mut cout = Vec::new();
    codex.tick(Some((13, 2)), t0, &c, geom_codex(), &mut cout);
    codex.observe_print_anchor(Some((9, 40, 1)));
    codex.tick(Some((13, 2)), at(16), &c, geom_codex(), &mut cout);
    codex.note_insert_delivered(at(6000), InsertWidth::Unknown);
    assert_eq!(
        codex.insert.armed.map(|i| i.row),
        Some(Some(13)),
        "a visible caret beside program output elsewhere is still the hand's"
    );
}

/// The parked proof is current: a visible caret on the row the anchored lane
/// established is the hand's, whatever the program last printed.
#[test]
fn a_visible_caret_on_the_established_echo_row_is_not_parked_by_a_transcript_print() {
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.observe_print_anchor(Some((20, 2, 1)));
    glow.tick(None, t0, &c, g, &mut out);
    glow.note_typed(at(100));
    glow.observe_print_anchor(Some((20, 3, 2)));
    glow.tick(None, at(108), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "fixture: the anchored lane lit one echo on row 20 under a hidden caret"
    );
    assert_eq!(glow.last_anchor_sweep.map(|(row, _)| row), Some(20));
    glow.tick(Some((20, 3)), at(200), &c, g, &mut out);
    glow.observe_print_anchor(Some((15, 40, 3)));
    glow.tick(Some((20, 3)), at(6000), &c, g, &mut out);
    glow.note_insert_delivered(at(6010), InsertWidth::Unknown);
    glow.observe_print_anchor(Some((15, 48, 4)));
    glow.tick(Some((20, 3)), at(6030), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "the transcript hop on row 15 is not the insert's (last record={:?})",
        glow.admission_log()
            .last()
            .map(|r| (r.reason, r.licence, r.origin, r.target))
    );
    glow.tick(Some((20, 11)), at(6050), &c, g, &mut out);
    assert_eq!(
        glow.admission_log()
            .last()
            .map(|r| (r.reason, r.licence, r.origin, r.target)),
        Some((
            "licensed",
            AdmissionRecord::LICENCE_INSERT,
            (20, 3),
            (20, 11)
        )),
        "the completion on the caret's row is the insert's"
    );
    assert_eq!(glow.spawns(), 2);
    assert_eq!(
        glow.insert.armed, None,
        "the one-shot was spent on the completion"
    );
}

/// The raised bound is the insert row's: with a 36-cell paste delivered and a
/// key typed behind it, the anchored lane's cap rises to `insert_reach` for the
/// insert's row only.
#[test]
fn a_spinner_past_the_typed_cap_beside_a_fresh_insert_stays_dark() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((10, 1, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.observe_print_anchor(Some((12, 2, 2)));
    glow.tick(None, at(32), &c, g, &mut out);
    glow.note_typed(at(240));
    glow.observe_print_anchor(Some((12, 3, 3)));
    glow.tick(None, at(245), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the input echo establishes the row");
    glow.note_insert_delivered(at(600), InsertWidth::Cells(36));
    glow.note_typed(at(605));
    glow.observe_print_anchor(Some((10, 35, 4)));
    glow.tick(None, at(610), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "a spinner past the typed cap is not the insert's row"
    );
    glow.observe_print_anchor(Some((12, 39, 5)));
    glow.tick(None, at(620), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "the input row's 36-cell echo lights");
    assert_eq!(
        glow.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );
}

/// A drop as the first action on a never-typed hidden row whose receipt lands
/// after the echoing frame: the frame refuses the keyless advance and brands the
/// row; the late receipt, exactly the hop's width, retro-licenses it as one
/// sweep and lifts the brand, so later keys anchor with no program-row
/// refusals.
#[test]
fn a_first_action_drop_with_a_late_receipt_is_retro_licensed_and_unbrands_its_row() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 2, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 13, 2)));
    glow.tick(None, at(100), &c, g, &mut out);
    assert_eq!(glow.spawns(), 0, "the echo arrived before its receipt");
    glow.note_insert_delivered(at(102), InsertWidth::Cells(11));
    glow.tick(None, at(116), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "the late receipt retro-licenses the first-action drop"
    );
    assert_eq!(
        glow.admission_log()
            .last()
            .map(|r| (r.licence, r.origin, r.target)),
        Some((AdmissionRecord::LICENCE_INSERT, (11, 2), (11, 13)))
    );
    assert!(
        (2..13u16).all(|col| v2_cols(&glow, 11).contains(&col)),
        "and laid"
    );
    for (i, key_ms) in [200u64, 300, 400].into_iter().enumerate() {
        let i = i as u64;
        glow.note_typed(at(key_ms));
        glow.observe_print_anchor(Some((11, 14 + i as u16, 3 + i)));
        glow.tick(None, at(key_ms + 6), &c, g, &mut out);
    }
    let refusals = glow
        .admission_log()
        .filter(|r| r.reason == CursorGlow::DECLINE_PROGRAM_ROW)
        .count();
    assert_eq!(
        refusals, 0,
        "the brand the echoing frame applied is lifted with the receipt"
    );
    assert_eq!(glow.spawns(), 4);
}

/// A priced 32-cell paste is not the unknown class: its width equals
/// `INSERT_GESTURE_CELLS` but it admits no spinner's one-cell advance on
/// another row.
#[test]
fn a_priced_paste_the_width_of_the_gesture_cap_is_not_the_unknown_class() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    established_input_row_beside_a_spinner(&mut glow, t0);
    glow.note_insert_delivered(
        at(600),
        InsertWidth::Cells(CursorGlow::INSERT_GESTURE_CELLS),
    );
    glow.observe_print_anchor(Some((10, 20, 4)));
    glow.tick(None, at(610), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "a priced 32-cell paste inherits no any-row bypass"
    );
    glow.observe_print_anchor(Some((12, 35, 5)));
    glow.tick(None, at(620), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "the input row's own 32-cell echo lights");
    assert_eq!(
        glow.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );
}

/// A revoke is exact: a Tab dispatched while a paste drains is armed and revoked
/// at its own instant in the same turn.
#[test]
fn a_revoked_tab_restores_the_paste_credit_it_accumulated_into() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();

    let mut glow = CursorGlow::default();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    glow.note_insert_delivered(at(100), InsertWidth::Cells(11));
    glow.note_user_gesture(at(200));
    glow.note_insert_delivered(at(200), InsertWidth::Unknown);
    glow.revoke_input_hints_at(at(200));
    glow.tick(Some((3, 13)), at(220), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "the paste's credit survives the queued Tab's revoke"
    );
    assert_eq!(
        glow.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );

    let mut both = CursorGlow::default();
    both.tick(Some((3, 2)), t0, &c, g, &mut out);
    both.note_insert_delivered(at(100), InsertWidth::Cells(11));
    both.note_user_gesture(at(200));
    both.note_insert_delivered(at(200), InsertWidth::Unknown);
    both.revoke_input_hints_at(at(200));
    both.revoke_input_hints_at(at(100));
    both.tick(Some((3, 13)), at(220), &c, g, &mut out);
    assert_eq!(both.spawns(), 0, "a revoke at the paste's instant takes it");
    assert_eq!(
        both.admission_log().last().map(|r| r.reason),
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );
}

/// Two receipts straddling the echoing frame claim the hop together. A
/// multi-file drop is one paste per file, echoed as one hop, and the second
/// receipt can publish after the frame that saw it.
#[test]
fn two_receipts_straddling_the_frame_claim_the_refused_hop_together() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((3, 10)), t0, &c, g, &mut out);
    let r1 = t0 + ms(50);
    glow.note_insert_delivered(r1, InsertWidth::Cells(5));
    let frame = r1 + ms(5);
    glow.tick(Some((3, 21)), frame, &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-fresh-hint", "none", (3, 10), (3, 21))),
        "{:?}",
        ring_rows(&glow)
    );
    glow.note_insert_delivered(frame + ms(1), InsertWidth::Cells(6));
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("licensed", "insert", (3, 10), (3, 21))),
        "the accumulated width claims the hop: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(
        glow.insert.armed, None,
        "the licence is spent by the sweep that lit it"
    );
    assert_eq!(glow.insert_tally().lit, 1);
    glow.tick(Some((3, 21)), frame + ms(20), &c, g, &mut out);
    let dark = dark_in(&glow, 3, 10..21);
    assert!(dark.is_empty(), "dark: {dark:?}");
}

/// The unknown insert's bound counts once per licence. Every Tab press and
/// auto-repeat arms a fresh unknown-width insert; a second unspent unknown arm
/// must not add another 32 cells (a held Tab would reach ~1900), so a 68-cell
/// program hop after three Tabs stays refused.
#[test]
fn repeated_unknown_inserts_do_not_widen_the_bound_past_the_gesture_cap() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    let t0 = Instant::now();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    for i in 0..3u64 {
        let at = t0 + ms(10 + 30 * i);
        glow.note_user_gesture(at);
        glow.note_insert_delivered(at, InsertWidth::Unknown);
    }
    assert_eq!(
        glow.insert.armed.map(|i| (i.cells, i.known)),
        Some((CursorGlow::INSERT_GESTURE_CELLS, false))
    );
    glow.tick(Some((2, 70)), t0 + ms(400), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-fresh-hint", "none", (2, 2), (2, 70))),
        "{:?}",
        ring_rows(&glow)
    );
    assert_eq!(glow.spawns(), 0);
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    for i in 0..3u64 {
        let at = t0 + ms(10 + 30 * i);
        glow.note_user_gesture(at);
        glow.note_insert_delivered(at, InsertWidth::Unknown);
    }
    glow.tick(Some((2, 32)), t0 + ms(400), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last().map(|r| r.1),
        Some("insert"),
        "{:?}",
        ring_rows(&glow)
    );
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    glow.note_insert_delivered(t0 + ms(10), InsertWidth::Cells(11));
    glow.note_user_gesture(t0 + ms(40));
    glow.note_insert_delivered(t0 + ms(40), InsertWidth::Unknown);
    assert_eq!(glow.insert.armed.map(|i| i.cells), Some(43));
    glow.tick(Some((2, 45)), t0 + ms(400), &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last().map(|r| r.1),
        Some("insert"),
        "{:?}",
        ring_rows(&glow)
    );
}

/// The dispatch-time arm claims no pending hop: `note_insert_armed` is the
/// delivery arm without the late-receipt claim.
#[test]
fn a_dispatch_time_insert_arm_claims_no_hop_refused_before_the_key() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    for delivered in [false, true] {
        let mut glow = CursorGlow::default();
        glow.tick(Some((2, 2)), t0, &c, g, &mut out);
        glow.tick(Some((2, 4)), at(20), &c, g, &mut out);
        assert_eq!(glow.spawns(), 0, "the keyless hop is refused");
        assert!(
            glow.insert.pending_hop.is_some(),
            "…and remembered for one receipt"
        );
        glow.note_user_gesture(at(220));
        if delivered {
            glow.note_insert_delivered(at(220), InsertWidth::Unknown);
            assert_eq!(glow.spawns(), 1, "a receipt claims the pending hop");
            assert_eq!(glow.insert_tally().lit, 1);
            assert!(glow.insert.pending_hop.is_none());
            continue;
        }
        glow.note_insert_armed(at(220), InsertWidth::Unknown);
        assert_eq!(glow.spawns(), 0, "the dispatch arm lays nothing");
        assert_eq!(
            glow.insert_tally().delivered,
            1,
            "…but the licence is armed"
        );
        assert!(
            glow.insert.pending_hop.is_some(),
            "the pending hop is left for a receipt"
        );
        glow.tick(Some((2, 12)), at(280), &c, g, &mut out);
        assert_eq!(
            glow.admission_log().last().map(|r| (r.licence, r.reason)),
            Some(("insert", "licensed")),
            "the completion is the insert's: {:?}",
            ring_rows(&glow)
        );
    }
}

/// The licensed insert's reappearance is bridged. A Tab completion or paste
/// whose echo frame was read torn before its glyphs shows the caret hidden, then
/// visible the insert's width on, with no print the anchored lane could judge.
#[test]
fn a_licensed_insert_s_hidden_to_visible_reappearance_is_laid_as_the_insert() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    for (what, arm, landing, lit) in [
        ("a Tab", Some(InsertWidth::Unknown), 12u16, 1u64),
        ("a 3-cell paste", Some(InsertWidth::Cells(3)), 12, 1),
        (
            "a hop wider than the paste",
            Some(InsertWidth::Cells(3)),
            20,
            0,
        ),
        ("program output", None, 12, 0),
    ] {
        let mut glow = CursorGlow::default();
        glow.tick(Some((2, 9)), t0, &c, g, &mut out);
        match arm {
            Some(InsertWidth::Unknown) => {
                glow.note_user_gesture(at(100));
                glow.note_insert_armed(at(100), InsertWidth::Unknown);
            }
            Some(width) => glow.note_insert_delivered_from(at(100), at(100), width),
            None => {}
        }
        glow.tick(None, at(120), &c, g, &mut out);
        glow.tick(Some((2, landing)), at(150), &c, g, &mut out);
        assert_eq!(
            glow.insert_tally().lit,
            lit,
            "{what}: {:?}",
            ring_rows(&glow)
        );
        assert_eq!(glow.spawns(), lit, "{what}: {:?}", ring_rows(&glow));
        if lit == 1 {
            assert_eq!(
                glow.admission_log().last().map(|r| (r.licence, r.reason)),
                Some(("insert", "licensed")),
                "{what}: {:?}",
                ring_rows(&glow)
            );
        }
    }
}

/// A late receipt claims no hop refused before its own dispatch. A queued bare
/// Tab's receipt carries its dispatch instant (`note_insert_delivered_from`) and
/// claims only hops refused at or after it, since its bytes could not echo
/// before dispatch; a program hop refused 50 ms earlier stays dark.
#[test]
fn a_late_receipt_claims_no_hop_refused_before_its_dispatch() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    glow.tick(Some((2, 4)), at(20), &c, g, &mut out);
    assert_eq!(glow.spawns(), 0, "the keyless hop is refused");
    assert!(glow.insert.pending_hop.is_some(), "…and remembered");
    glow.note_delivered(at(120), DeliveredClass::Gesture);
    glow.note_insert_delivered_from(at(70), at(120), InsertWidth::Unknown);
    assert_eq!(
        glow.spawns(),
        0,
        "a hop refused before the dispatch is program output: {:?}",
        ring_rows(&glow)
    );
    assert_eq!(glow.insert_tally().lit, 0);
    assert_eq!(
        glow.insert_tally().delivered,
        1,
        "…but the licence is armed for the completion to come"
    );
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    glow.tick(Some((2, 4)), at(20), &c, g, &mut out);
    glow.note_delivered(at(120), DeliveredClass::Gesture);
    glow.note_insert_delivered_from(at(10), at(120), InsertWidth::Unknown);
    assert_eq!(
        glow.spawns(),
        1,
        "a receipt claims the hop after its dispatch"
    );
    assert_eq!(glow.insert_tally().lit, 1);
    assert!(glow.insert.pending_hop.is_none());
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    glow.tick(Some((2, 10)), at(20), &c, g, &mut out);
    assert!(glow.insert.pending_hop.is_some());
    glow.note_insert_delivered_from(at(70), at(120), InsertWidth::Cells(8));
    assert_eq!(glow.spawns(), 0, "{:?}", ring_rows(&glow));
    let mut glow = CursorGlow::default();
    glow.tick(Some((2, 2)), t0, &c, g, &mut out);
    glow.tick(Some((2, 10)), at(20), &c, g, &mut out);
    glow.note_insert_delivered_from(at(10), at(120), InsertWidth::Cells(8));
    assert_eq!(glow.spawns(), 1, "{:?}", ring_rows(&glow));
}

/// A revoked arm is not a delivery: a bare Tab / ⌃V armed at dispatch, revoked
/// when its write queued behind a draining paste, and re-armed when the write
/// lands counts once in `inserts_delivered=` (the revoke undoes the arm's
/// tick).
#[test]
fn a_revoked_insert_arm_undoes_its_delivered_tally_tick() {
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);

    let mut queued = CursorGlow::default();
    queued.note_insert_delivered(at(200), InsertWidth::Unknown);
    queued.revoke_input_hints_at(at(200));
    assert_eq!(
        queued.insert_tally().delivered,
        0,
        "a revoked arm was not a delivery"
    );
    assert_eq!(
        queued.insert_tally().last_cells,
        0,
        "…and its width is not the last delivered one"
    );
    queued.note_insert_delivered(at(300), InsertWidth::Unknown);
    assert_eq!(
        queued.insert_tally().delivered,
        1,
        "the delivery re-arm counts the queued Tab once"
    );
    assert_eq!(
        queued.insert_tally().last_cells,
        CursorGlow::INSERT_GESTURE_CELLS,
        "…and its width is the re-arm's"
    );

    let mut failed = CursorGlow::default();
    failed.note_insert_delivered(at(100), InsertWidth::Cells(8));
    failed.note_insert_delivered(at(200), InsertWidth::Unknown);
    assert_eq!(failed.insert_tally().last_cells, 32);
    failed.revoke_failed_input_at(at(200));
    assert_eq!(
        failed.insert_tally().delivered,
        1,
        "a failed write delivered nothing"
    );
    assert_eq!(
        failed.insert_tally().last_cells,
        8,
        "the width goes back to the last insert that stood"
    );

    let mut other = CursorGlow::default();
    other.note_insert_delivered(at(200), InsertWidth::Cells(8));
    other.revoke_input_hints_at(at(100));
    assert_eq!(other.insert_tally().delivered, 1);
    assert_eq!(other.insert_tally().last_cells, 8);
}

/// The row law: an unknown-width insert belongs to the witnessed row only, and
/// to no row when nothing witnessed the arm; a priced width belongs to the
/// witnessed row and is also recognised by its whole width on a row nobody
/// typed on (a partial advance there is a program row).
#[test]
fn insert_row_identity_is_the_witnessed_row_or_a_priced_whole_width() {
    let now = Instant::now();
    let unknown = InsertLicence {
        at: now,
        dispatched_at: now,
        cells: CursorGlow::INSERT_GESTURE_CELLS,
        known: false,
        breaks: false,
        row: Some(12),
    };
    assert!(
        CursorGlow::insert_row_identity(12, 1, unknown),
        "an unknown width on its witnessed row"
    );
    assert!(
        !CursorGlow::insert_row_identity(10, 1, unknown),
        "an unknown width on another row: refused"
    );
    assert!(
        !CursorGlow::insert_row_identity(10, 32, unknown),
        "…whatever the hop"
    );
    let unwitnessed = InsertLicence {
        row: None,
        ..unknown
    };
    assert!(
        !CursorGlow::insert_row_identity(12, 32, unwitnessed),
        "an unknown width nothing witnessed is no row's"
    );
    let priced = InsertLicence {
        at: now,
        dispatched_at: now,
        cells: 11,
        known: true,
        breaks: false,
        row: Some(12),
    };
    assert!(
        CursorGlow::insert_row_identity(12, 5, priced),
        "a priced width on its witnessed row, any fit"
    );
    assert!(
        CursorGlow::insert_row_identity(10, 11, priced),
        "a priced whole width is the insert's on any row"
    );
    assert!(
        !CursorGlow::insert_row_identity(10, 5, priced),
        "a partial advance on a row nobody typed on is a program row"
    );
    let cap = InsertLicence {
        cells: CursorGlow::INSERT_GESTURE_CELLS,
        ..priced
    };
    assert!(
        !CursorGlow::insert_row_identity(10, 5, cap),
        "a priced 32-cell width is priced, not the unknown class"
    );
}

/// A brand outranks the insert: a row that advanced keylessly before the arm is
/// a program row for its entry's lifetime, and even a priced insert whose whole
/// width matches its next advance is refused there `program-row`.
#[test]
fn a_row_branded_before_the_arm_refuses_even_a_whole_width_match() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    established_input_row_beside_a_spinner(&mut glow, t0);
    glow.observe_print_anchor(Some((10, 20, 4)));
    glow.tick(None, at(550), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1);
    glow.note_insert_delivered(at(900), InsertWidth::Cells(1));
    glow.observe_print_anchor(Some((10, 21, 5)));
    glow.tick(None, at(910), &c, g, &mut out);
    let last = glow.admission_log().last().expect("a ring row");
    assert_eq!(
        (glow.spawns(), last.reason, last.origin, last.target),
        (1, CursorGlow::DECLINE_PROGRAM_ROW, (10, 20), (10, 21)),
        "{}",
        last.line(at(910))
    );
    glow.observe_print_anchor(Some((12, 4, 6)));
    glow.tick(None, at(920), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "the input row's own echo spends it");
    assert_eq!(
        glow.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );
}
