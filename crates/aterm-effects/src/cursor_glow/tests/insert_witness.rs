// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Insert witnesses, receipts and spinner rows.

use super::*;

/// Seed the hidden-caret TUI shape the row tests share: a spinner row
/// at `(10, 19)`, the input row at `(12, 2)`, one typed echo `2 → 3` on
/// it at 245 ms establishing it (`spawns == 1`, `last_anchor_sweep` on
/// row 12). Every judgement the tests then make is placed past the
/// spoken-for hold so the row law alone decides.
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

/// THE ROW WITNESS (the verifier's stale stamp): an insert
/// of UNKNOWN width — a Tab, a ⌃V — is bound to the row the hand was on
/// when it was armed and spends on no other. Visible caret: the Tab is
/// pressed with the caret on row 3; 300 ms later (the gesture stale)
/// the program parks the caret on row 5 and advances it 8 cells inside
/// the two-second insert window. That advance is program output and
/// stays dark. The same Tab's own same-row completion on row 3 lights.
///
/// RED-PROOF: `insert_row_identity` answered `true` for
/// ANY row when `cells == INSERT_GESTURE_CELLS`, so the row-5 hop was
/// laid `licence=insert` — `spawns == 4`, not 3.
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

/// THE ROW WITNESS, hidden caret (Claude Code's shape): the input row is
/// established, a Tab is pressed on it, and from 300 ms later a spinner
/// row advances — one cell, one cell, eight cells — inside the
/// two-second insert window. The spinner spends nothing and lights
/// nothing; the input row's own 8-cell completion after it is laid
/// `licence=insert`.
///
/// RED-PROOF: the any-row `cells == INSERT_GESTURE_CELLS`
/// term admitted the spinner's first advance as the Tab's echo —
/// `spawns == 2` at the spinner, the one-shot spent on it.
/// **A BARE TAB'S LICENCE IS A GESTURE'S, NOT A PASTE'S** (2026-09-21).
/// The unknown-width class has no width to check — its admission is the
/// row witness and the 32-cell bound — so for the whole two seconds of
/// [`CursorGlow::INSERT_HINT_FRESH`] the FIRST same-row forward advance
/// on the hand's row was taken as the Tab's echo, 1.75 s of it past
/// every other licence's freshness. A background job printing on the
/// prompt row was lit as a 32-cell sweep: ink over bytes no key asked
/// for. It now expires at [`CursorGlow::INSERT_GESTURE_HINT_FRESH`].
///
/// The control is the SAME hop inside the window, which must still
/// light — `a_spinner_row_advancing_after_a_tab_stays_dark` holds the
/// 800 ms end of that, and this holds the far end.
#[test]
fn a_program_hop_after_a_tab_s_gesture_window_stays_dark() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    // AN ABSOLUTE instant, not one derived from the constant under
    // test: the first spelling computed `FRESH * 1000 + 200` and so
    // moved with whatever it was given, passing with the 2.0 s paste
    // window restored — a pin that cannot fail. 1400 ms is past the
    // gesture window and comfortably INSIDE the paste one, so it reads
    // the difference between them and nothing else.
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
        // The hand's own row advances ten cells with no key behind it.
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

/// THE ROW WITNESS RIDES THE BAND (the stale witness): Codex's composer
/// sits on row 13 with the caret visible and
/// three keys echoed there; an insert of UNKNOWN width — a multi-line
/// paste's `[Pasted text #1 +N lines] `, a Tab's completion — is
/// delivered with row 13 as its witness. Before its echo lands Codex
/// streams a line and the viewport `[11..56]` rides down one row: the
/// composer and its caret are now on row 14 and row 13 holds the
/// streamed program line. That line then grows 8 cells (judged by the
/// anchored lane — the caret is visible on another row, so the advance
/// reads as parked) and the insert's echo lands on row 14 as one
/// 26-cell hop. The witness moved with the text it names: the program
/// line is not the insert's row and spends nothing; the composer's echo
/// is laid `licence=insert`.
///
/// RED-PROOF: `translate_band_state` left every insert
/// member untouched, so the witness still said 13 after the band move
/// (`Some(Some(13))`, the second assert); with that assert removed, the
/// program line's 8-cell advance was laid `licence=insert` (`spawns ==
/// 4` there, the one-shot spent on a streamed line).
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
    // Codex streams a line: the viewport rides down one row.
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
    // The streamed line on row 13 grows 8 cells under the parked caret.
    glow.observe_print_anchor(Some((13, 10, 1)));
    glow.tick(Some((14, 5)), at(710), &c, g, &mut out);
    glow.observe_print_anchor(Some((13, 18, 2)));
    glow.tick(Some((14, 5)), at(715), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        3,
        "the row the composer left is a program row: it spends nothing"
    );
    // The insert's echo on the composer's new row.
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

    // CARRIED PAST THE EDGE: a band the witness's row leaves takes the
    // row with it — no honest row, an unknown width spends nowhere.
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

/// THE UNKNOWN WIDTH'S LATE RECEIPT: a multi-line paste is
/// an insert of UNKNOWN width delivered by the writer thread, and its
/// receipt can land AFTER the frame that observed the echo exactly as a
/// priced one's can (`a_receipt_that_lands_after_the_echoing_frame_still_lights_the_insert`).
/// Visible caret on row 3, three keys laid `2..5`; the app's `[Pasted
/// text #1 +2 lines] ` echoes as one 26-cell hop `5 → 31` one frame
/// BEFORE the receipt: refused, remembered. The receipt — unknown
/// width, witness row 3 — has no width to match and claims the hop by
/// the ROW WITNESS alone, bounded at `INSERT_GESTURE_CELLS`, and lays
/// it `licence=insert`. Control: a 33-cell hop is past the bound and
/// nobody's.
///
/// RED-PROOF: the retro guard required the hop's width to
/// EQUAL the unknown class's 32-cell bound beside the row witness, so no
/// unknown-width receipt ever claimed a hop — `spawns == 3`, the ring's
/// last row `declined no-fresh-hint 3,5→3,31`.
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

/// THE PARKED CARET'S WITNESS (the status-row stray): `fc_parked.py`'s
/// shape — the DEC cursor visible but PARKED
/// at the origin cell every frame while the input row 2 is repainted
/// under it. Three keys echo on row 2 through the anchored lane; an
/// insert of unknown width is delivered (a multi-line paste's receipt;
/// a Tab's stamp beside its gesture class is the same at this seam).
/// Its witness is the INPUT row the lane proved, not the row the caret
/// is parked on: when the TUI then walks the parked cursor 8 cells
/// along row 0, that visible move spends nothing (`no-fresh-hint`),
/// and the completion's 8-cell advance on row 2 is laid
/// `licence=insert`. Control: a caret that merely differs from the
/// print anchor while the anchored lane has licensed nothing — Codex's
/// visible caret beside a printing transcript — keeps its own row.
///
/// RED-PROOF: `hand_row` took the visible caret's row
/// unconditionally, so the witness was 0 (`Some(Some(0))`, the first
/// assert after the arm); with that assert removed, the walk along row
/// 0 was laid `licence=insert` (`spawns == 4` there, the one-shot spent
/// on the parked row).
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
    // The TUI walks the parked cursor along row 0.
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
    // The completion on the input row.
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

    // CONTROL — Codex's shape: the caret is visible on the composer row
    // while the transcript prints above it (the print anchor on another
    // row), and the anchored lane has licensed nothing. The caret's row
    // IS the hand's; a Tab after a quiet gap keeps it.
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

/// THE PARKED PROOF IS CURRENT: a visible caret sitting ON the row the
/// anchored lane
/// established is the hand's, whatever the program last printed. The
/// anchored lane licenses one echo on row 20 under a hidden caret
/// (`last_anchor_sweep` names row 20); the TUI then shows the caret at
/// `(20, 3)`; after a quiet gap past the chain window the program
/// prints a fresh transcript line on row 15; the user presses Tab. The
/// witness is 20 — so the transcript row's next 8-cell advance inside
/// the insert window is a program row (dark, `spawns` unmoved), and
/// the Tab's completion walking the caret along row 20 is laid
/// `licence=insert`.
///
/// RED-PROOF: `hand_row` read "parked" as "the visible
/// caret's row differs from the print anchor's AND the anchored lane
/// has licensed somewhere in this coordinate space" — a lifetime
/// identity, not a current one — and fell back to the print anchor:
/// the witness was 15, the transcript hop was laid `licence=insert`
/// (`spawns == 2` at the "transcript hop" assert), and the completion
/// on row 20 was refused `no-fresh-hint` (the one-shot spent).
#[test]
fn a_visible_caret_on_the_established_echo_row_is_not_parked_by_a_transcript_print() {
    let g = geom_codex();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    // The TUI drew its input row 20; the caret is hidden from the start.
    glow.observe_print_anchor(Some((20, 2, 1)));
    glow.tick(None, t0, &c, g, &mut out);
    // One key echoes on row 20 through the anchored lane.
    glow.note_typed(at(100));
    glow.observe_print_anchor(Some((20, 3, 2)));
    glow.tick(None, at(108), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "fixture: the anchored lane lit one echo on row 20 under a hidden caret"
    );
    assert_eq!(glow.last_anchor_sweep.map(|(row, _)| row), Some(20));
    // The TUI shows the caret on the row it just echoed on.
    glow.tick(Some((20, 3)), at(200), &c, g, &mut out);
    // Past the chain window, the program prints a transcript line on
    // row 15 — the print anchor leaves the caret's row.
    glow.observe_print_anchor(Some((15, 40, 3)));
    glow.tick(Some((20, 3)), at(6000), &c, g, &mut out);
    // The user presses Tab.
    glow.note_insert_delivered(at(6010), InsertWidth::Unknown);
    // The transcript row advances 8 cells inside the insert window: a
    // program row, dark.
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
    // The Tab's completion walks the visible caret along row 20.
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

/// THE BOUND IS THE INSERT ROW'S: with a 36-cell paste
/// delivered and a key typed behind it, the anchored lane's cap is
/// raised to `insert_reach` for the row the insert admits and NO other.
/// A spinner row advancing 34 cells — PAST the typed coalesce cap (32),
/// inside the raised bound — stays dark; the input row's own 36-cell
/// echo lights. The 34-cell hop is the whole point: a hop ≤ 32 would
/// pass the unraised cap too and prove nothing.
///
/// RED-PROOF: the raised bound applied to every row, so
/// the spinner's 34-cell hop reached `spawn` under the fresh typed
/// stamp and was laid as a re-anchor — `spawns == 2` at the spinner.
#[test]
fn a_spinner_past_the_typed_cap_beside_a_fresh_insert_stays_dark() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    // A spinner seeded LOW (so a >32-cell advance fits the 40-col grid)
    // beside an established input row.
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
    // The spinner advances 1 → 35 = 34 cells: past the typed cap.
    glow.observe_print_anchor(Some((10, 35, 4)));
    glow.tick(None, at(610), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "a spinner past the typed cap is not the insert's row"
    );
    // The input row advances 3 → 39 = 36 cells: the insert's own width,
    // on the insert's own row.
    glow.observe_print_anchor(Some((12, 39, 5)));
    glow.tick(None, at(620), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "the input row's 36-cell echo lights");
    assert_eq!(
        glow.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );
}

/// A DROP AS THE FIRST ACTION on a never-typed hidden row whose receipt
/// lands AFTER the echoing frame (a preempted writer thread): the frame
/// refuses the keyless advance and brands the row; the late receipt,
/// exactly the hop's width, retro-licenses it — laid as one sweep — and
/// LIFTS the brand, so the keys typed after it anchor with zero
/// program-row refusals.
///
/// RED-PROOF: the pending hop was recorded only for an
/// unbranded row, and the brand had just been applied by the same frame
/// — the drop stayed dark (`spawns == 0`) and every later key on the
/// row was refused `program-row`.
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

/// A PRICED 32-cell paste is not the unknown class: its
/// width equals `INSERT_GESTURE_CELLS`, and read as an unknown-width
/// sentinel that number admitted a spinner's
/// one-cell advance on ANY row. The spinner stays dark; the input row's
/// own 32-cell echo lights.
///
/// RED-PROOF: `spawns == 2` at the spinner.
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

/// A REVOKE IS EXACT: a Tab dispatched while a paste drains
/// is armed at dispatch and revoked at its own instant in the same
/// turn. The paste's still-fresh, unspent credit the Tab's arm had
/// ACCUMULATED into is restored, not discarded with it — the paste's
/// echo lights; a second revoke at the PASTE's own instant then takes
/// exactly that.
///
/// RED-PROOF: the accumulation folded the paste into the
/// Tab's instant and the Tab's revoke took both — the paste's echo
/// declined `no-fresh-hint`.
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

/// TWO RECEIPTS STRADDLING THE ECHOING FRAME CLAIM THE HOP TOGETHER. A
/// multi-file drop is one paste per file; the shell echoes both as ONE
/// hop, and a preempted writer thread can
/// publish the second receipt after the frame that saw it. The second
/// arm accumulates into the first (cells = 5 + 6), but the late-receipt
/// claim matched the refused hop against THIS delivery's width alone,
/// so an 11-cell hop was not claimed, the pending hop was dropped, and
/// the accumulated 11-cell licence stayed armed for two seconds — the
/// whole drop dark and any later 11-cell advance on a row lit as the
/// insert. The claim now matches and lays the ACCUMULATED licence; a
/// single receipt is field-for-field the same licence, so every
/// one-receipt path is byte-identical.
///
/// RED before the fix: ring `("no-fresh-hint","none",(3,10),(3,21))`
/// unchanged after the second receipt, `lit: 0`, `insert_hint =
/// Some(cells: 11)`, cells 10..21 dark.
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
    // The frame sees both echoes as one hop; only the first receipt is in.
    let frame = r1 + ms(5);
    glow.tick(Some((3, 21)), frame, &c, g, &mut out);
    assert_eq!(
        ring_rows(&glow).last(),
        Some(&("no-fresh-hint", "none", (3, 10), (3, 21))),
        "{:?}",
        ring_rows(&glow)
    );
    // The second receipt lands after the frame.
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

/// THE UNKNOWN INSERT'S BOUND IS COUNTED ONCE PER LICENCE. Every Tab
/// press and auto-repeat arms a fresh unknown-width
/// insert at dispatch, and the arm accumulated into the still-armed one
/// — 32 cells per repeat, so after three Tabs a 68-cell same-row program
/// hop on the hand's row was lit as `insert` for two seconds where one
/// Tab refuses it (a held Tab for two seconds: a ~1900-cell bound). A
/// PRICED part still adds its exact width (the multi-file drop, the
/// paste + queued-Tab pair keep their reach); a second unspent unknown
/// arm — an auto-repeat, a double-tap that beeped — does not widen it.
///
/// RED before the fix: `3 tabs: last_insert_cells=96`, the 68-cell hop
/// `("insert","licensed",(2,2),(2,70))`.
#[test]
fn repeated_unknown_inserts_do_not_widen_the_bound_past_the_gesture_cap() {
    let g = wide_geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let ms = Duration::from_millis;
    // Three unknown arms with nothing echoed between: the bound is one
    // gesture's, and a 68-cell hop past the gesture window is refused.
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
    // Control: a 30-cell hop inside the bound is still the insert's.
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
    // Control: a priced 11 plus an unknown arm still lights a 43-cell hop.
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

/// THE DISPATCH-TIME ARM CLAIMS NO PENDING HOP: `note_insert_armed` is
/// the delivery arm without
/// the late-receipt claim. A keyless `+2` refused 200 ms before a Tab
/// is left as it was (the pending hop kept for a real receipt), and
/// the completion that follows is the insert's; `note_insert_delivered`
/// at the same instant still claims it — the receipt's law.
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

/// THE LICENSED INSERT'S REAPPEARANCE IS BRIDGED (2026-09-22). A Tab's
/// completion or a paste whose echo frame reached the glass torn before
/// its glyphs (a 1024-byte read ending at the hide) shows the caret
/// HIDDEN, then visible the insert's width on, with no print the
/// anchored lane could judge under the hide. The hide bridge admits the
/// reappearance on the insert's own reach and the insert arm lays it
/// `licence=insert`. The same reappearance with no licence is a declined
/// relocation, and so is one wider than a priced paste: the bridge
/// admits exactly what the insert arm lays (`visible_insert_echo`).
///
/// RED before the fix: the Tab read `declined hidden-relocation`, the
/// paste declined silently (no gesture fresh to log it), and
/// `insert_tally().lit == 0` for both.
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

/// A LATE RECEIPT CLAIMS NO HOP REFUSED BEFORE ITS OWN DISPATCH. The
/// dispatch-time arm above claims nothing, but a QUEUED bare Tab's
/// delivery receipt
/// still claimed any hop refused within `INSERT_RETRO_FRESH` of the
/// writer's completion instant — a window that reaches back past the
/// Tab's own dispatch. A Tab queued behind a short-draining paste
/// could so claim a program hop that predated its press: refused at T,
/// the Tab dispatched at T+50 ms, its receipt at T+100 ms → lit as the
/// insert. The receipt now carries the dispatch instant
/// (`note_insert_delivered_from`) and claims only a hop refused AT OR
/// AFTER it: the key's bytes could not have echoed before they were
/// dispatched. A hop refused after the dispatch (the preempted-writer
/// shape the retro claim exists for) is claimed as before.
///
/// RED before the fix: `spawns == 1`, `insert_tally().lit == 1` for
/// the hop refused 50 ms before the dispatch.
#[test]
fn a_late_receipt_claims_no_hop_refused_before_its_dispatch() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut out = Vec::new();
    // The hop refused BEFORE the dispatch: not the Tab's.
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
    // The hop refused AFTER the dispatch — the preempted writer's
    // receipt landing a frame late — is the insert's, as before.
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
    // A PRICED paste's receipt takes the same rule: its bytes could only
    // echo after its dispatch too.
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

/// A REVOKED ARM IS NOT A DELIVERY: a bare Tab / ⌃V is
/// armed at dispatch, revoked by exact instant when its write was
/// queued behind a draining paste, and re-armed by the prelude when the
/// write lands — one gesture, and `trail status`'s `inserts_delivered=`
/// counted it twice, because the dispatch-time arm ticked the tally and
/// the revoke restored only the licence. The revoke now undoes that
/// tick, so the delivery re-arm counts the gesture once; a FAILED write
/// (`revoke_failed_input_at`, which delegates) leaves the count where it
/// was, since nothing was delivered. `last_insert_cells=` goes back
/// with it: the width the row showed before the arm, not the revoked
/// arm's 32 beside a `0` count. Diagnostic only — no licence changes.
///
/// RED-PROOF: `delivered == 2` for the queued Tab, `1` for
/// the failed one; `last_cells == 32` after the revoked Tab (right 0,
/// and right 8 behind a delivered paste) before the width was put back.
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

    // A paste delivered (8 cells), then a Tab queued behind a draining
    // paste whose write FAILS: the row reads the paste's width again,
    // not `inserts_delivered=1 last_insert_cells=32`.
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

    // A revoke at ANOTHER instant leaves the tally alone, as it leaves
    // the licence alone.
    let mut other = CursorGlow::default();
    other.note_insert_delivered(at(200), InsertWidth::Cells(8));
    other.revoke_input_hints_at(at(100));
    assert_eq!(other.insert_tally().delivered, 1);
    assert_eq!(other.insert_tally().last_cells, 8);
}

/// THE ROW LAW, directly: an insert of UNKNOWN width is the
/// witnessed row's alone — never another row's, whatever the hop, and
/// no row's at all when nothing witnessed the arm; a PRICED width is
/// the witnessed row's too, and is also recognised by its WHOLE width on
/// a row nobody typed on — a partial advance there is a program row.
#[test]
fn insert_row_identity_is_the_witnessed_row_or_a_priced_whole_width() {
    let now = Instant::now();
    let unknown = InsertLicence {
        at: now,
        dispatched_at: now,
        cells: CursorGlow::INSERT_GESTURE_CELLS,
        known: false,
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

/// A BRAND OUTRANKS THE INSERT: a row that advanced
/// keylessly before the arm is a program row for its entry's lifetime,
/// and even a PRICED insert whose whole width the row's next advance
/// matches is refused there `program-row` — the whole-width identity
/// recognises a drop on a never-typed row, not a spinner that happens to
/// cross one cell when the paste was one cell wide. The input row's own
/// echo still spends it.
#[test]
fn a_row_branded_before_the_arm_refuses_even_a_whole_width_match() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    established_input_row_beside_a_spinner(&mut glow, t0);
    // The spinner's keyless advance with nothing fresh: branded.
    glow.observe_print_anchor(Some((10, 20, 4)));
    glow.tick(None, at(550), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1);
    // A one-cell paste, past the retro window of that advance; the
    // spinner crosses one more cell — the insert's whole width.
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
