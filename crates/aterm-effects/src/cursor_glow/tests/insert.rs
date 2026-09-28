// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Delivered inserts.

use super::*;

// -- THE DELIVERED INSERT ("the image insert breaks the
// rainbow") ---------------------------------------------------------------
//
// Measured on the shipped v0.81.0 binary in a private headless instance
// (`ctl trail`): on a plain zsh prompt six keys were licensed one cell
// each, the paste of `foo bar ` was judged as ONE move `origin=3,50
// target=3,58` and DECLINED `no-fresh-hint`, and `x`/`y` were licensed
// again from 58 — two ribbons severed by an 8-cell hole. Under a hidden
// DEC caret (Claude Code's Ink prompt, and real Claude Code 2.1.268) the
// 63-cell drop and its ~300 ms swap to `[Image #1] ` were never judged at
// all (no ring row), and ` and` was licensed from the new caret: the
// placeholder dark, the walk restarted. The paste reached the seam as
// program output because the host stamped its gesture at the input
// boundary and revoked it in the same turn (enqueue is not delivery) and
// nothing re-stamped it when the bytes landed. The fix: the host's
// completed write arms `note_insert_delivered`, the DELIVERED-INSERT
// licence, and the seam lays its echo as ONE sweep joining the cohort.

/// The seam's two cells of a row `a` and `b` walk the same step — the
/// hue-continuity oracle: a cell that joined the cohort takes the stop
/// its column has in it; a cold restart re-anchors at red instead.
fn walk_step(glow: &CursorGlow, row: u16, a: u16, b: u16) -> f32 {
    let at = |col: u16| {
        glow.v2
            .field_at(row, col)
            .unwrap_or_else(|| panic!("cell ({row}, {col}) is dark"))
    };
    at(b) - at(a)
}

/// THE DELIVERED INSERT, visible caret: three keys lay `2..5`; 400 ms of
/// quiet (every stamp stale); the host reports an 11-cell insert
/// DELIVERED; the shell echoes it 20 ms later as one hop `5 → 16`. The
/// hop is licensed under the insert class, the eleven cells are laid as
/// ONE sweep joining the cohort (the walk steps by the same amount across
/// `4 → 5` as it did across `3 → 4`), no momentum pulse and no cue are
/// minted for it, and two keys typed after it continue the same walk.
///
/// RED-PROOF (the note stubbed to a tally): fails at
/// `spawns == 4` with 3 — the ring's last row is `declined
/// reason=no-fresh-hint origin=3,5 target=3,16`, the shipped verdict.
#[test]
fn a_delivered_insert_lays_its_span_as_one_sweep_under_a_visible_caret() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut glow, t0, &mut out);
    let _ = glow.take_momentum_pulse();
    let _ = glow.drain_sound_cues().count();
    let delivered = at(700);
    glow.note_insert_delivered(delivered, InsertWidth::Cells(11));
    glow.tick(Some((3, 16)), at(720), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        4,
        "the delivered insert's echo is licensed: {}",
        glow.admission_log()
            .last()
            .map_or_else(String::new, |r| r.line(at(720)))
    );
    let last = glow.admission_log().last().expect("a ring row");
    assert_eq!(
        (last.phase, last.licence, last.origin, last.target),
        (
            AdmissionPhase::Licensed,
            AdmissionRecord::LICENCE_INSERT,
            (3, 5),
            (3, 16)
        ),
        "the ring names the insert licence: {}",
        last.line(at(720))
    );
    assert!(
        last.line(at(720)).ends_with(" licence=insert"),
        "`licence=` is the row's last token: {}",
        last.line(at(720))
    );
    let lit = v2_cols(&glow, 3);
    assert!(
        (2..16u16).all(|col| lit.contains(&col)),
        "every glyph cell 2..16 is lit as one run: {lit:?}"
    );
    let typed_step = walk_step(&glow, 3, 3, 4);
    assert!(
        (walk_step(&glow, 3, 4, 5) - typed_step).abs() < 1e-4
            && (walk_step(&glow, 3, 14, 15) - typed_step).abs() < 1e-4,
        "the insert's cells join the cohort and the hue walks on through them"
    );
    assert!(
        glow.take_momentum_pulse().is_none(),
        "an insert is not typing: no momentum pulse"
    );
    assert_eq!(
        glow.drain_sound_cues().count(),
        0,
        "an insert is not typing: no click"
    );
    // Two keys after the drop continue the walk from the new caret.
    for (i, col) in (17..=18u16).enumerate() {
        let key = at(800 + 100 * i as u64);
        glow.note_typed(key);
        glow.tick(
            Some((3, col)),
            key + Duration::from_millis(8),
            &c,
            g,
            &mut out,
        );
    }
    assert_eq!(glow.spawns(), 6, "typing after the insert stays licensed");
    let lit = v2_cols(&glow, 3);
    assert!(
        lit.contains(&16) && lit.contains(&17),
        "the keys after the drop lay: {lit:?}"
    );
    assert!(
        (walk_step(&glow, 3, 15, 16) - typed_step).abs() < 1e-4,
        "the walk continues through the insert into the keys after it"
    );
}

/// THE DELIVERED INSERT, hidden caret (Claude Code's shape): the
/// established input row's print anchor advances 60 cells with the
/// delivered stamp behind it — wider than `RAINBOW_TYPED_SWEEP_MAX`, so
/// this also pins that the bound is the insert's OWN width, not the typed
/// coalesce cap. Licensed, laid `4..64`, the row is not branded, and the
/// three typed echoes after it still anchor.
///
/// RED-PROOF (the note stubbed): fails at `spawns == 3` with
/// 2 — the lane returns at its typed-bank gate and writes no ring row,
/// the measured Claude Code shape.
#[test]
fn a_delivered_insert_lights_a_hidden_caret_tui_at_its_print_anchor() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 2, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.note_typed(at(30));
    glow.observe_print_anchor(Some((11, 3, 2)));
    glow.tick(None, at(35), &c, g, &mut out);
    glow.note_typed(at(120));
    glow.observe_print_anchor(Some((11, 4, 3)));
    glow.tick(None, at(125), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "phase-1 typed echoes must light");
    // >250 ms quiet, then the drop: DELIVERED 33 cells wide (the test grid is 40
    // columns); the TUI echoes the path and the row's end advances 4 -> 37.
    glow.note_insert_delivered(at(420), InsertWidth::Cells(33));
    glow.observe_print_anchor(Some((11, 37, 4)));
    glow.tick(None, at(430), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        3,
        "the delivered insert's anchor advance is licensed under a hidden caret"
    );
    let last = glow.admission_log().last().expect("a ring row");
    assert_eq!(
        (last.licence, last.origin, last.target),
        (AdmissionRecord::LICENCE_INSERT, (11, 4), (11, 37)),
        "{}",
        last.line(at(430))
    );
    let lit = v2_cols(&glow, 11);
    assert!(
        (4..37u16).all(|col| lit.contains(&col)),
        "the 33-cell span is lit — wider than the typed cap: {lit:?}"
    );
    for (i, key_ms) in [520u64, 610, 700].into_iter().enumerate() {
        let i = i as u64;
        glow.note_typed(at(key_ms));
        glow.observe_print_anchor(Some((11, 38 + i as u16, 5 + i)));
        glow.tick(None, at(key_ms + 6), &c, g, &mut out);
    }
    let program_row_refusals = glow
        .admission_log()
        .filter(|r| r.origin.0 == 11 && r.reason == CursorGlow::DECLINE_PROGRAM_ROW)
        .count();
    assert_eq!(program_row_refusals, 0, "a delivered insert brands no row");
    assert_eq!(
        glow.spawns(),
        6,
        "every post-insert typed echo still anchors"
    );
}

/// THE PLACEHOLDER REWRITE: after the hidden-caret insert above, the TUI
/// swaps the path for `[Image #1] ` 300 ms later and the row's end pulls
/// back 64 -> 15 with no key. The seam reads the keyless retreat inside
/// the fresh insert span as the insert's own rewrite: the ring records it
/// `licensed licence=rewrite`, the cells at and right of the new caret
/// retract farthest-first and are gone by `12·n + 240` ms plus the fade
/// while the cells left of it keep their light under a live hand, and
/// keys typed after it continue the same cohort from the new caret. The
/// visible-caret twin runs the same shape through `spawn`. Two controls:
/// a Backspace inside the span keeps its own class (it is the erase's
/// retract, not a rewrite), and a keyless retreat with NO insert span
/// behind it declines and retires nothing.
///
/// RED-PROOF (the note stubbed): fails at the first
/// `spawns == 3` (the insert itself is refused); with the insert lit but
/// no rewrite arm, at the `licence=rewrite` assert — the retreat is
/// re-seeded silently and cells `15..64` stay lit under the blanks.
#[test]
fn the_placeholder_rewrite_retracts_the_ribbon_to_the_new_caret_like_a_kill() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    // HIDDEN CARET (the anchored lane).
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 2, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.note_typed(at(30));
    glow.observe_print_anchor(Some((11, 3, 2)));
    glow.tick(None, at(35), &c, g, &mut out);
    glow.note_typed(at(120));
    glow.observe_print_anchor(Some((11, 4, 3)));
    glow.tick(None, at(125), &c, g, &mut out);
    glow.note_insert_delivered(at(420), InsertWidth::Cells(33));
    glow.observe_print_anchor(Some((11, 37, 4)));
    glow.tick(None, at(430), &c, g, &mut out);
    assert_eq!(glow.spawns(), 3, "the insert lights first");
    // The rewrite: `> [Image #1] ` — the row's end is now 15, keyless.
    glow.observe_print_anchor(Some((11, 15, 5)));
    glow.tick(None, at(730), &c, g, &mut out);
    let last = glow.admission_log().last().expect("a ring row");
    assert_eq!(
        (last.phase, last.licence, last.origin, last.target),
        (
            AdmissionPhase::Licensed,
            AdmissionRecord::LICENCE_REWRITE,
            (11, 37),
            (11, 15)
        ),
        "the keyless retreat inside the insert span is the rewrite: {}",
        last.line(at(730))
    );
    // Keys after the rewrite keep the cohort alive (one finger holds
    // every cohort in its laying phase) while the retract completes:
    // 12·22 + 240 = 504 ms of stagger, then the 240 ms fade.
    let mut key_ms = 800u64;
    let mut col = 15u16;
    let mut seq = 6u64;
    while key_ms < 2200 {
        glow.note_typed(at(key_ms));
        col += 1;
        glow.observe_print_anchor(Some((11, col, seq)));
        glow.tick(None, at(key_ms + 6), &c, g, &mut out);
        key_ms += 100;
        seq += 1;
    }
    let lit = v2_cols(&glow, 11);
    assert!(
        (2..15u16).all(|c| lit.contains(&c)),
        "the cells left of the new caret keep their light: {lit:?}"
    );
    assert!(
        (col..37u16).all(|c| !lit.contains(&c)),
        "the cells the rewrite blanked are retracted, none left under the blanks: {lit:?}"
    );
    assert!(
        (15..col).all(|c| lit.contains(&c)),
        "the keys typed after the rewrite lay from the new caret: {lit:?}"
    );
    let step = walk_step(&glow, 11, 3, 4);
    assert!(
        (walk_step(&glow, 11, 14, 15) - step).abs() < 1e-4,
        "the hue continues across the rewrite into the keys after it"
    );

    // VISIBLE-CARET TWIN through `spawn`.
    let mut vis = CursorGlow::default();
    let mut vout = Vec::new();
    three_visible_echoes(&mut vis, t0, &mut vout);
    vis.note_insert_delivered(at(700), InsertWidth::Cells(11));
    vis.tick(Some((3, 16)), at(720), &c, g, &mut vout);
    assert_eq!(vis.spawns(), 4);
    vis.tick(Some((3, 8)), at(1020), &c, g, &mut vout);
    let last = vis.admission_log().last().expect("a ring row");
    assert_eq!(
        (last.phase, last.licence, last.origin, last.target),
        (
            AdmissionPhase::Licensed,
            AdmissionRecord::LICENCE_REWRITE,
            (3, 16),
            (3, 8)
        ),
        "the visible lane reads the same rewrite: {}",
        last.line(at(1020))
    );
    for i in 0..14u64 {
        let key = at(1100 + 100 * i);
        vis.note_typed(key);
        vis.tick(
            Some((3, 9 + i as u16)),
            key + Duration::from_millis(8),
            &c,
            g,
            &mut vout,
        );
    }
    let lit = v2_cols(&vis, 3);
    assert!(
        (2..8u16).all(|c| lit.contains(&c)),
        "left of the caret keeps its light: {lit:?}"
    );
    assert!(
        !lit.contains(&23) && !lit.contains(&30),
        "nothing right of the retyped run survives the rewrite: {lit:?}"
    );

    // CONTROL 1: a Backspace inside the span is the erase's own retract,
    // never a rewrite.
    let mut bs = CursorGlow::default();
    let mut bout = Vec::new();
    three_visible_echoes(&mut bs, t0, &mut bout);
    bs.note_insert_delivered(at(700), InsertWidth::Cells(11));
    bs.tick(Some((3, 16)), at(720), &c, g, &mut bout);
    bs.note_backspace(at(800));
    bs.tick(Some((3, 15)), at(808), &c, g, &mut bout);
    let last = bs.admission_log().last().expect("a ring row");
    assert_ne!(
        last.licence,
        AdmissionRecord::LICENCE_REWRITE,
        "a keyed retreat keeps its own class: {}",
        last.line(at(808))
    );

    // CONTROL 2: a keyless retreat with no insert span behind it is
    // program output — declined, and it retires nothing.
    let mut cold = CursorGlow::default();
    let mut cout = Vec::new();
    three_visible_echoes(&mut cold, t0, &mut cout);
    cold.tick(Some((3, 3)), at(700), &c, g, &mut cout);
    let last = cold.admission_log().last().expect("a ring row");
    assert_eq!(
        (last.phase, last.reason),
        (AdmissionPhase::Declined, CursorGlow::DECLINE_NO_FRESH_HINT),
        "{}",
        last.line(at(700))
    );
    cold.tick(Some((3, 3)), at(716), &c, g, &mut cout);
    let lit = v2_cols(&cold, 3);
    assert!(
        (2..5u16).all(|c| lit.contains(&c)),
        "an unlicensed retreat destroys no earned light: {lit:?}"
    );
}

/// THE REWRITE RE-WITNESSES ITS ROW. The content witness retires a
/// resident cell whose recorded glyph changed,
/// and Claude Code's `[Image #1] ` IS different text over the insert's
/// first eleven cells: on the tree that merged the witness with the
/// rewrite, those cells melted 120 ms after the rewrite while the suffix
/// took its designed farthest-first drain, and the next key laid beside
/// a retired cell — a new cohort, the rainbow severed by a hole exactly
/// the placeholder's width (the 0.84.0 shape, one width smaller). The
/// engine's rewrite arm knows the rewrite is the insert's own (the host
/// judged it `licence=rewrite`), so it forgets the row's records left of
/// the new caret: the next walk arms the placeholder's glyphs instead of
/// retiring the cells under them; the suffix's records fall out of the
/// walk on their own (its cells are leaving). A later re-lay of the row
/// with OTHER text still retires the placeholder's cells.
#[test]
fn the_placeholder_rewrite_re_witnesses_its_row_and_keeps_the_ribbon_whole() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut vis = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut vis, t0, &mut out);
    let path = "/Users//me/Desktop/screenshot.png";
    assert_eq!(path.chars().count(), 32);
    vis.note_insert_delivered(at(700), InsertWidth::Cells(32));
    vis.tick(Some((3, 37)), at(720), &c, g, &mut out);
    assert_eq!(vis.spawns(), 4, "the insert lights");
    let row_of = |text: &str| -> Vec<char> {
        let mut row = vec![' '; 80];
        for (i, ch) in text.chars().enumerate() {
            row[i] = ch;
        }
        row
    };
    // The witness sees the path laid, and arms every cell of it.
    let with_path = row_of(&format!("  abc{path}"));
    vis.observe_ribbon_row(3, &with_path);
    vis.tick(Some((3, 37)), at(740), &c, g, &mut out);
    assert_eq!(v2_cols(&vis, 3), (2..37u16).collect());
    // The program swaps the path for `[Image #1] `: the rewrite.
    let with_placeholder = row_of("  abc[Image #1] ");
    vis.observe_ribbon_row(3, &with_placeholder);
    vis.tick(Some((3, 16)), at(1020), &c, g, &mut out);
    let last = vis.admission_log().last().expect("a ring row");
    assert_eq!(
        (last.licence, last.origin, last.target),
        (AdmissionRecord::LICENCE_REWRITE, (3, 37), (3, 16)),
        "{}",
        last.line(at(1020))
    );
    assert_eq!(
        vis.v2_status().map_or(0, |s| s.retired),
        0,
        "the rewrite's own placeholder retires nothing"
    );
    // 280 ms on, the row re-sampled: the cells under the placeholder
    // keep their light; the suffix drains as a kill.
    vis.observe_ribbon_row(3, &with_placeholder);
    vis.tick(Some((3, 16)), at(1300), &c, g, &mut out);
    let lit = v2_cols(&vis, 3);
    assert!(
        (2..16u16).all(|col| lit.contains(&col)),
        "§27: the ribbon under `[Image #1] ` keeps its light; dark: {:?}",
        dark_in(&vis, 3, 2..16)
    );
    assert_eq!(vis.v2_status().map_or(0, |s| s.retired), 0);
    // The next key joins the cohort: one continuous walk across the
    // placeholder, no hole.
    vis.note_typed(at(1400));
    let mut typed = with_placeholder.clone();
    typed[16] = 'a';
    vis.observe_ribbon_row(3, &typed);
    vis.tick(Some((3, 17)), at(1408), &c, g, &mut out);
    assert!(
        (2..17u16).all(|col| v2_cols(&vis, 3).contains(&col)),
        "{:?}",
        v2_cols(&vis, 3)
    );
    let step = walk_step(&vis, 3, 3, 4);
    assert!(
        (walk_step(&vis, 3, 15, 16) - step).abs() < 1e-4,
        "the hue continues across the placeholder into the key after it"
    );
    // A re-lay of the row with OTHER text under ONE cell of the
    // placeholder is NOT a retirement (2026-09-16, `Witness::shape_verdicts`
    // — the owner: *"you need to be fixing in general"*). Until then the
    // changed glyph's cell retired on its own record, a one-cell hole in a
    // band whose letters stood lit either side; the run's two never-armed
    // blanks (the placeholder's spaces at 11 and 15) were already kept
    // (2026-09-15: *"odd logic of drawing a part of the trail when moving
    // the cursor and editing text"*, measured on glass as
    // `..######.#####.###.#.-###.####....`). A change strictly inside a
    // standing run is not evidence: the light stays whole and the record
    // takes the glyph standing there now.
    let mut other = typed.clone();
    other[12] = '2';
    vis.observe_ribbon_row(3, &other);
    vis.tick(Some((3, 17)), at(1600), &c, g, &mut out);
    assert_eq!(
        vis.v2_status().map_or(0, |s| s.retired),
        0,
        "one interior glyph changed under a standing run is not evidence"
    );
    let lit = v2_cols(&vis, 3);
    assert!(
        (2..17u16).all(|col| lit.contains(&col)),
        "the band is whole across the changed glyph and the run's spaces: {lit:?}"
    );
    // The witness was re-armed, not disarmed: a re-lay that reaches the
    // run's END — its last recorded cell, the key typed after the
    // placeholder — is a suffix and still retires.
    let mut tail = other.clone();
    tail[16] = 'Z';
    vis.observe_ribbon_row(3, &tail);
    vis.tick(Some((3, 17)), at(1700), &c, g, &mut out);
    assert_eq!(
        vis.v2_status().map_or(0, |s| s.retired),
        1,
        "a rewrite reaching the run's end retires the cell under it"
    );
    let lit = v2_cols(&vis, 3);
    assert!(
        (2..16u16).all(|col| lit.contains(&col)),
        "…and nothing left of it: {lit:?}"
    );
}

/// THE CONTROL THE FIX MUST NOT REGRESS: the same two hops with NO
/// delivery stamp behind them — a program flood exactly the insert's
/// width — decline exactly as shipped. Visible: `no-fresh-hint`;
/// hidden: no ring row at all, `spawns` frozen. To the PTY stream a
/// flood and a paste are the same bytes; the delivery is the only
/// discriminator.
#[test]
fn a_program_flood_of_the_inserts_width_with_no_delivery_stamp_still_declines() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.tick(Some((3, 16)), at(720), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        3,
        "an undelivered 11-cell hop is program output"
    );
    let last = glow.admission_log().last().expect("a ring row");
    assert_eq!(
        (last.phase, last.reason, last.licence),
        (
            AdmissionPhase::Declined,
            CursorGlow::DECLINE_NO_FRESH_HINT,
            AdmissionRecord::LICENCE_NONE
        ),
        "{}",
        last.line(at(720))
    );
    assert!(
        (5..16u16).all(|col| !v2_cols(&glow, 3).contains(&col)),
        "the flood's cells stay dark"
    );

    let mut hidden = CursorGlow::default();
    let mut hout = Vec::new();
    hidden.tick(None, t0, &c, g, &mut hout);
    hidden.observe_print_anchor(Some((11, 2, 1)));
    hidden.tick(None, at(16), &c, g, &mut hout);
    hidden.note_typed(at(30));
    hidden.observe_print_anchor(Some((11, 3, 2)));
    hidden.tick(None, at(35), &c, g, &mut hout);
    hidden.note_typed(at(120));
    hidden.observe_print_anchor(Some((11, 4, 3)));
    hidden.tick(None, at(125), &c, g, &mut hout);
    let rows_before = hidden.admission_log().count();
    hidden.observe_print_anchor(Some((11, 64, 4)));
    hidden.tick(None, at(430), &c, g, &mut hout);
    assert_eq!(
        hidden.spawns(),
        2,
        "an undelivered anchor advance lights nothing"
    );
    assert_eq!(
        hidden.admission_log().count(),
        rows_before,
        "…and, with no stamp banked, is not even judged"
    );
}

/// A delivery stamp is a LICENCE only while fresh and only once: stale
/// after `INSERT_HINT_FRESH`, it declines exactly as a stale typed stamp
/// does; spent by one echo, it cannot license a second hop inside the
/// window.
#[test]
fn a_stale_or_spent_delivery_stamp_licenses_nothing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let stale_ms = (CursorGlow::INSERT_HINT_FRESH * 1000.0) as u64 + 50;

    let mut stale = CursorGlow::default();
    let mut out = Vec::new();
    stale.tick(Some((3, 2)), t0, &c, g, &mut out);
    stale.note_insert_delivered(at(100), InsertWidth::Cells(11));
    stale.tick(Some((3, 13)), at(100 + stale_ms), &c, g, &mut out);
    assert_eq!(stale.spawns(), 0, "a stale delivery stamp is not a licence");
    let last = stale.admission_log().last().expect("a ring row");
    assert_eq!(last.reason, CursorGlow::DECLINE_NO_FRESH_HINT);

    let mut spent = CursorGlow::default();
    spent.tick(Some((3, 2)), t0, &c, g, &mut out);
    spent.note_insert_delivered(at(100), InsertWidth::Cells(11));
    spent.tick(Some((3, 13)), at(120), &c, g, &mut out);
    assert_eq!(spent.spawns(), 1, "the fresh stamp licenses its echo");
    spent.tick(Some((3, 24)), at(140), &c, g, &mut out);
    assert_eq!(spent.spawns(), 1, "…and is one-shot: the next hop declines");
    let last = spent.admission_log().last().expect("a ring row");
    assert_eq!(last.reason, CursorGlow::DECLINE_NO_FRESH_HINT);
}

/// A drop as the FIRST action on a never-typed input row (a fresh hidden
/// engine, no established echo row): the insert echo is licensed and the
/// row is not branded, so the keys typed after it anchor with zero
/// program-row refusals.
#[test]
fn a_delivered_insert_brands_no_row() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(None, t0, &c, g, &mut out);
    glow.observe_print_anchor(Some((11, 2, 1)));
    glow.tick(None, at(16), &c, g, &mut out);
    glow.note_insert_delivered(at(100), InsertWidth::Cells(11));
    glow.observe_print_anchor(Some((11, 13, 2)));
    glow.tick(None, at(110), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "a drop on a fresh row lights");
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
    assert_eq!(refusals, 0, "the insert's row is never branded");
    assert_eq!(glow.spawns(), 4);
}

/// The insert licence is INERT for a program row: with the input row
/// established, a spinner row's first advance inside the delivery window
/// is refused, spends nothing, and the licence survives for the input
/// row's own echo.
///
/// TIMED SO THAT ROW IDENTITY IS THE ONLY REFUSAL (the
/// verifier's vacuity): the spinner is judged 365 ms after the input
/// row's echo — past the spoken-for hold (`TYPE_HINT_FRESH`, 0.25 s) —
/// so a mutant `insert_row_identity` that answers `true` lays the
/// spinner as the insert and fails here at `spawns == 1` with 2.
#[test]
fn a_delivered_insert_is_inert_for_a_program_row() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
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
    glow.note_insert_delivered(at(600), InsertWidth::Cells(11));
    // The spinner's first-ever advance, inside the window and past the
    // spoken-for hold: only the row law can refuse it.
    glow.observe_print_anchor(Some((10, 20, 4)));
    glow.tick(None, at(610), &c, g, &mut out);
    let spinner_spends = glow
        .admission_log()
        .filter(|r| {
            (r.origin.0 == 10 || r.target.0 == 10) && r.reason != CursorGlow::DECLINE_PROGRAM_ROW
        })
        .count();
    assert_eq!(spinner_spends, 0, "a program row cannot spend the insert");
    assert_eq!(glow.spawns(), 1);
    // The input row's own echo spends it.
    glow.observe_print_anchor(Some((12, 14, 5)));
    glow.tick(None, at(620), &c, g, &mut out);
    assert_eq!(glow.spawns(), 2, "the licence survived for the input row");
    assert_eq!(
        glow.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );
}

/// The delivery stamp follows the TYPED BANK's retirement law, not the
/// one-shots': a completed hidden→visible boundary (Claude Code's
/// DECTCEM bracket, mid-burst) spares it while fresh; a class-changing
/// press (`clear_typed`) leaves it armed — its witness is the delivery,
/// not a key; and `revoke_input_hints_at` revokes it by its exact instant
/// (a Tab queued behind a draining paste) and no other.
#[test]
fn a_delivered_insert_survives_a_hidden_boundary_while_fresh_and_is_revoked_at_its_own_instant() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    glow.note_insert_delivered(at(100), InsertWidth::Cells(11));
    glow.tick(None, at(110), &c, g, &mut out);
    glow.tick(Some((3, 2)), at(120), &c, g, &mut out);
    glow.clear_typed(at(125));
    glow.tick(Some((3, 13)), at(130), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        1,
        "a fresh delivery stamp survives the hidden boundary and a class change"
    );

    let mut revoked = CursorGlow::default();
    revoked.tick(Some((3, 2)), t0, &c, g, &mut out);
    revoked.note_insert_delivered(at(500), InsertWidth::Cells(11));
    revoked.revoke_input_hints_at(at(400));
    revoked.revoke_input_hints_at(at(500));
    revoked.tick(Some((3, 13)), at(520), &c, g, &mut out);
    assert_eq!(
        revoked.spawns(),
        0,
        "revoked at its own instant, it licenses nothing"
    );
    assert_eq!(
        revoked.admission_log().last().map(|r| r.reason),
        Some(CursorGlow::DECLINE_NO_FRESH_HINT)
    );
}

/// An insert and a key delivered in ONE frame: the 12-cell hop is the
/// 11-cell insert plus the key's own glyph. The surplus is paid from the
/// press ring (one credit spent, its stamp consumed), all twelve cells
/// are lit, and the same stamps cannot fund a 13-cell hop.
#[test]
fn an_insert_and_a_key_in_one_frame_pay_the_surplus_from_the_press() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((3, 2)), t0, &c, g, &mut out);
    glow.note_insert_delivered(at(100), InsertWidth::Cells(11));
    glow.note_typed(at(105));
    glow.tick(Some((3, 14)), at(120), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "insert + key pay for twelve cells");
    assert_eq!(
        glow.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT)
    );
    let lit = v2_cols(&glow, 3);
    assert!(
        (2..14u16).all(|col| lit.contains(&col)),
        "all twelve lit: {lit:?}"
    );
    assert_eq!(
        glow.typed_credits_within(at(120)),
        0,
        "the key's credit was spent on the surplus"
    );
    // The key's stamp went with its credit: a program advance after the
    // hop finds no licence.
    glow.tick(Some((3, 15)), at(130), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the spent press licenses no later hop");

    let mut wide = CursorGlow::default();
    wide.tick(Some((3, 2)), t0, &c, g, &mut out);
    wide.note_insert_delivered(at(100), InsertWidth::Cells(11));
    wide.note_typed(at(105));
    wide.tick(Some((3, 15)), at(120), &c, g, &mut out);
    assert_ne!(
        wide.admission_log().last().map(|r| r.licence),
        Some(AdmissionRecord::LICENCE_INSERT),
        "a hop wider than insert + credits is not the insert's"
    );
    assert!(
        (3..14u16).all(|col| !v2_cols(&wide, 3).contains(&col)),
        "…and its cells stay dark"
    );
}

/// An insert builds no momentum (the FAMILY metric is the hand's), pulses
/// nothing to the cat, and cues nothing — extends
/// `program_output_alone_builds_no_momentum` to the delivered class.
#[test]
fn a_delivered_insert_builds_no_momentum_and_cues_nothing() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    glow.tick(Some((3, 0)), t0, &c, g, &mut out);
    glow.note_insert_delivered(at(100), InsertWidth::Cells(20));
    glow.tick(Some((3, 20)), at(110), &c, g, &mut out);
    assert_eq!(glow.spawns(), 1, "the insert is licensed");
    assert!(glow.take_momentum_pulse().is_none(), "no momentum pulse");
    assert_eq!(glow.typing_momentum(at(110)), 0.0, "no family momentum");
    assert_eq!(glow.momentum_display(), 0.0, "no spine advance");
    assert_eq!(glow.drain_sound_cues().count(), 0, "no cue");
    assert!(
        (0..20u16).all(|col| v2_cols(&glow, 3).contains(&col)),
        "…but the twenty cells are lit"
    );
}

/// A delivered insert mints NOTHING in every other style: the licence is
/// read only at the Rainbow Kitty seam, so the other nine are
/// byte-identical by construction — extends
/// `an_unlicensed_move_mints_nothing_and_retires_nothing_in_every_style`.
#[test]
fn a_delivered_insert_mints_nothing_in_every_other_style() {
    let g = geom();
    let t0 = Instant::now();
    for style in [
        GlowStyle::Lumen,
        GlowStyle::Phaser,
        GlowStyle::Sparkle,
        GlowStyle::Fire,
        GlowStyle::Laser,
        GlowStyle::Beam,
        GlowStyle::Water,
        GlowStyle::Comet,
        GlowStyle::Custom,
    ] {
        let mut c = cfg(style, true);
        if style == GlowStyle::Custom {
            c.pack = Some(TrailParams::defaults());
        }
        let mut glow = CursorGlow::default();
        let mut out = Vec::new();
        glow.tick(Some((2, 4)), t0, &c, g, &mut out);
        glow.note_insert_delivered(t0 + Duration::from_millis(10), InsertWidth::Cells(11));
        glow.tick(
            Some((2, 15)),
            t0 + Duration::from_millis(16),
            &c,
            g,
            &mut out,
        );
        assert!(
            !frame_has_output(&out, &glow),
            "{style:?}: a delivered insert emitted geometry"
        );
        assert_eq!(glow.spawns(), 0, "{style:?}: a delivered insert spawned");
        assert_eq!(
            glow.admission_log().last().map(|r| r.reason),
            Some(CursorGlow::DECLINE_NO_FRESH_HINT),
            "{style:?}: the hop declines as program output"
        );
    }
}

/// A receipt that lands AFTER the frame that observed the echo (a
/// preempted writer thread) still lights the insert: the refused hop of
/// exactly the insert's width is remembered for one delivery and laid
/// when the stamp arrives — both lanes. A hop of another width is not
/// the insert's and stays dark.
#[test]
fn a_receipt_that_lands_after_the_echoing_frame_still_lights_the_insert() {
    let g = geom();
    let c = cfg(GlowStyle::RainbowKitty, true);
    let t0 = Instant::now();
    let at = |ms: u64| t0 + Duration::from_millis(ms);
    let mut glow = CursorGlow::default();
    let mut out = Vec::new();
    three_visible_echoes(&mut glow, t0, &mut out);
    glow.tick(Some((3, 16)), at(720), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        3,
        "the echo arrived before its receipt: refused"
    );
    glow.note_insert_delivered(at(722), InsertWidth::Cells(11));
    glow.tick(Some((3, 16)), at(736), &c, g, &mut out);
    assert_eq!(
        glow.spawns(),
        4,
        "the late receipt retro-licenses the refused hop"
    );
    assert_eq!(
        glow.admission_log()
            .last()
            .map(|r| (r.licence, r.origin, r.target)),
        Some((AdmissionRecord::LICENCE_INSERT, (3, 5), (3, 16)))
    );
    let lit = v2_cols(&glow, 3);
    assert!(
        (5..16u16).all(|col| lit.contains(&col)),
        "the span is lit: {lit:?}"
    );

    let mut hidden = CursorGlow::default();
    let mut hout = Vec::new();
    hidden.tick(None, t0, &c, g, &mut hout);
    hidden.observe_print_anchor(Some((11, 2, 1)));
    hidden.tick(None, at(16), &c, g, &mut hout);
    hidden.note_typed(at(30));
    hidden.observe_print_anchor(Some((11, 3, 2)));
    hidden.tick(None, at(35), &c, g, &mut hout);
    hidden.observe_print_anchor(Some((11, 36, 3)));
    hidden.tick(None, at(430), &c, g, &mut hout);
    assert_eq!(hidden.spawns(), 1);
    hidden.note_insert_delivered(at(432), InsertWidth::Cells(33));
    hidden.tick(None, at(446), &c, g, &mut hout);
    assert_eq!(
        hidden.spawns(),
        2,
        "the anchored lane's refused advance is retro-licensed"
    );
    assert!(
        (3..36u16).all(|col| v2_cols(&hidden, 11).contains(&col)),
        "and laid"
    );

    let mut other = CursorGlow::default();
    let mut oout = Vec::new();
    three_visible_echoes(&mut other, t0, &mut oout);
    other.tick(Some((3, 9)), at(720), &c, g, &mut oout);
    other.note_insert_delivered(at(722), InsertWidth::Cells(11));
    other.tick(Some((3, 9)), at(736), &c, g, &mut oout);
    assert_eq!(
        other.spawns(),
        3,
        "a refused hop of another width is not the insert's"
    );
}
