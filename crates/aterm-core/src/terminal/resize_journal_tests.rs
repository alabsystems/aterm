// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Tests for the resize journal, heal counters, and render debt across viewport flaps.

use super::{RESIZE_JOURNAL_CAP, RenderDebt, ResizeReport};
use crate::terminal::Terminal;

fn screen(t: &Terminal) -> Vec<String> {
    (0..usize::from(t.rows()))
        .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
        .collect()
}

/// Write `L<r>` on each row in `rows` using absolute cursor positioning.
fn paint(t: &mut Terminal, rows: std::ops::Range<u16>) {
    for r in rows {
        t.process(format!("\x1b[{};1HL{r}", r + 1).as_bytes());
    }
}

/// Alternate screen with every row painted and the cursor three rows from the bottom.
fn claude_shaped_alt(rows: u16) -> Terminal {
    let mut t = Terminal::new(rows, 20);
    t.process(b"\x1b[?1049h\x1b[2J");
    paint(&mut t, 0..rows);
    t.process(format!("\x1b[{};3H", rows - 2).as_bytes());
    assert_eq!(t.cursor().row, rows - 3);
    t
}

/// Apply `resizes` and return their journalled reports.
fn flap(t: &mut Terminal, resizes: &[u16]) -> Vec<ResizeReport> {
    let since = t.resize_ordinal();
    for &rows in resizes {
        t.resize(rows, 20);
    }
    let (reports, lost) = t.resize_journal_since(since);
    assert_eq!(lost, 0);
    assert_eq!(reports.len(), resizes.len(), "one report per resize");
    reports
}

/// Verify that a quiet alternate-screen flap with a full tail restores the demoted top row.
#[test]
fn a_quiet_alt_flap_with_a_full_tail_is_an_identity() {
    let mut t = claude_shaped_alt(8);
    let painted = screen(&t);
    let cursor = t.cursor();

    let r = flap(&mut t, &[7, 8]);
    let (shrink, grow) = (r[0], r[1]);
    assert_eq!((shrink.from, shrink.to), ((8, 20), (7, 20)));
    assert_eq!((grow.from, grow.to), ((7, 20), (8, 20)));
    assert!(shrink.alt && grow.alt);
    assert_eq!(
        (
            shrink.trimmed,
            shrink.demoted,
            shrink.pushed,
            shrink.stashed
        ),
        (0, 1, 0, 1),
        "a full tail leaves nothing to trim: the top row is demoted, and kept"
    );
    assert_eq!((shrink.shift(), shrink.lossy()), (-1, false));
    assert_eq!(
        (grow.revealed, grow.restored_top, grow.appended),
        (1, 1, 0),
        "the grow hands the demoted row back instead of appending a blank"
    );
    assert_eq!(shrink.shift() + grow.shift(), 0, "the shifts cancel");
    assert!(grow.adjoins(&shrink));

    assert_eq!(screen(&t), painted, "the app's picture survives the flap");
    assert_eq!(t.cursor(), cursor);

    let mut t = Terminal::new(52, 121);
    t.process(b"\x1b[?1049h\x1b[2J");
    for r in 0..52u16 {
        t.process(format!("\x1b[{};1HL{r}", r + 1).as_bytes());
    }
    t.process(b"\x1b[52;5H");
    let painted = screen(&t);
    let since = t.resize_ordinal();
    t.resize(51, 121);
    t.resize(52, 121);
    let (r, _) = t.resize_journal_since(since);
    assert_eq!((r[0].demoted, r[1].restored_top), (1, 1));
    assert_eq!(screen(&t), painted);
    assert_eq!((t.cursor().row, t.cursor().col), (51, 4));
}

/// Verify that output between shrink and grow drops the undo until an `ED 2` repaint heals the shift.
#[test]
fn alt_flap_with_output_between_the_halves_keeps_the_shift() {
    let mut t = claude_shaped_alt(8);
    let painted = screen(&t);
    let clears = t.full_clear_count();

    let since = t.resize_ordinal();
    t.resize(7, 20);
    t.process(b"\x1b[?1000h");
    t.resize(8, 20);
    let (r, _) = t.resize_journal_since(since);
    let (shrink, grow) = (r[0], r[1]);
    assert_eq!((shrink.demoted, shrink.stashed), (1, 1));
    assert_eq!(
        (grow.revealed, grow.restored_top, grow.appended),
        (0, 0, 1),
        "the undo is gone: no history to reveal, the grow appends a blank row"
    );
    assert_eq!((grow.shift(), grow.lossy()), (0, false));
    assert!(!shrink.reflowed && !grow.reflowed);
    assert!(
        shrink.content_seq < grow.content_seq,
        "each resize re-lays the grid out"
    );
    assert_eq!(
        (shrink.full_clears, grow.full_clears),
        (clears, clears),
        "nothing cleared between the halves"
    );

    let now = screen(&t);
    assert_eq!(now[..7], painted[1..], "shifted up by exactly one row");
    assert_eq!(now[7], "", "a blank row where the footer was painted");
    assert_eq!(t.cursor().row, 4, "the cursor moved up with its row");

    t.process(b"\x1b[?2026h\x1b[6;1H\x1b[KL5*\x1b[?2026l");
    let healed = |t: &Terminal| grow.healed_by(t.full_clear_count(), t.screen_replaced_count());
    assert!(!healed(&t), "a diff frame is not a repaint");
    assert!(t.content_seq() > grow.content_seq, "the app drew on it");
    t.process(b"\x1b[3J");
    assert!(!healed(&t), "ED 3 leaves the shifted rows in place");
    assert_eq!(screen(&t)[..5], painted[1..6], "still shifted");
    t.process(b"\x1b[2J");
    assert!(healed(&t));
    assert!(!grow.healed_by(clears, 0));
}

/// Verify that a quiet flap preserves DECSC, DECSTBM, and combining marks on demoted rows.
#[test]
fn a_quiet_alt_flap_keeps_decsc_the_scroll_region_and_extras() {
    let mut t = claude_shaped_alt(8);
    t.process("\x1b[1;1HLe\u{301}".as_bytes());
    t.process(b"\x1b[2;7r\x1b[6;3H\x1b7");
    let painted = screen(&t);
    assert!(painted[0].contains('\u{301}'), "{painted:?}");

    let r = flap(&mut t, &[6, 8]);
    assert_eq!((r[0].demoted, r[0].stashed, r[1].restored_top), (2, 2, 2));
    assert_eq!(screen(&t), painted);
    let region = t.grid().scroll_region();
    assert_eq!((region.top, region.bottom), (1, 6), "DECSTBM put back");

    t.process(b"\x1b[1;1H\x1b8");
    assert_eq!((t.cursor().row, t.cursor().col), (5, 2));
}

/// Verify that an undone flap restores the selection unless moved or interrupted by output.
#[test]
fn an_undone_flap_puts_the_selection_back() {
    use crate::selection::{SelectionSide, SelectionType};
    let select = |t: &mut Terminal| {
        let sel = t.text_selection_mut();
        sel.start_selection(0, 1, SelectionSide::Left, SelectionType::Simple);
        sel.update_selection(3, 4, SelectionSide::Right);
        sel.complete_selection();
        sel.clone()
    };
    for heights in [&[7u16, 8][..], &[7, 6, 8], &[6, 7, 8], &[7, 6, 7, 8]] {
        let mut t = claude_shaped_alt(8);
        let before = select(&mut t);
        let painted = screen(&t);
        flap(&mut t, heights);
        assert_eq!(screen(&t), painted, "{heights:?}");
        assert_eq!(t.text_selection(), &before, "{heights:?}");
    }

    let mut t = claude_shaped_alt(8);
    select(&mut t);
    t.resize(7, 20);
    let moved = {
        let sel = t.text_selection_mut();
        sel.start_selection(2, 0, SelectionSide::Left, SelectionType::Simple);
        sel.complete_selection();
        sel.clone()
    };
    t.resize(8, 20);
    assert_ne!(t.text_selection(), &moved, "the reveal shifts it down");
    assert_eq!(t.text_selection().start().row, 3, "and only that");

    let mut t = claude_shaped_alt(8);
    let before = select(&mut t);
    t.resize(7, 20);
    t.process(b"\x1b[?1000h");
    t.resize(8, 20);
    assert_ne!(t.text_selection(), &before);
}

/// Verify that an alternate-screen switch between shrink and grow drops the resize undo.
#[test]
fn an_alt_screen_switch_between_the_halves_drops_the_undo() {
    let mut t = Terminal::new(8, 20);
    t.process(b"\x1b[?47h");
    paint(&mut t, 0..8);
    t.process(b"\x1b[6;3H");
    t.resize(7, 20);
    t.process(b"\x1b[?47l");
    t.process(b"\x1b[?47h");
    let r = flap(&mut t, &[8]);
    assert_eq!((r[0].restored_top, r[0].appended), (0, 1));
    assert_eq!(screen(&t)[0], "L1", "the switch kept today's shift");

    let mut t = Terminal::new(8, 20);
    t.process(b"\x1b[?47h");
    paint(&mut t, 0..8);
    t.process(b"\x1b[6;3H\x1b[?47l");
    t.resize(7, 20);
    t.process(b"\x1b[?47h");
    let r = flap(&mut t, &[8]);
    assert_eq!((r[0].restored_top, r[0].appended), (0, 1));
    assert_eq!(screen(&t)[0], "L1");
}

/// Verify that each `Terminal` has a fixed lifetime token and checkpoint restore mints a new one at ordinal 0.
#[test]
fn every_engine_has_its_own_lifetime_token() {
    let mut a = Terminal::new(8, 20);
    let b = Terminal::new(8, 20);
    let token = a.resize_lifetime();
    assert_ne!(token, 0, "0 is never a token");
    assert_ne!(token, b.resize_lifetime(), "two engines, two tokens");
    a.resize(7, 20);
    a.process(b"\x1b[2J\x1bc");
    assert_eq!(a.resize_lifetime(), token, "fixed for the engine's life");
    let restored = Terminal::from_checkpoint(&a.checkpoint());
    assert_ne!(
        restored.resize_lifetime(),
        token,
        "a restored engine is new"
    );
    assert_eq!(restored.resize_ordinal(), 0);
}

/// Verify that an alternate-screen shrink with a blank tail below the cursor only trims.
#[test]
fn alt_shrink_with_a_blank_tail_only_trims() {
    let mut t = Terminal::new(8, 20);
    t.process(b"\x1b[?1049h");
    paint(&mut t, 0..6);
    t.process(b"\x1b[6;3H");
    let painted = screen(&t);

    let r = flap(&mut t, &[7, 8]);
    assert!(r[0].alt);
    assert_eq!((r[0].trimmed, r[0].demoted, r[0].pushed), (1, 0, 0));
    assert_eq!((r[0].shift(), r[0].lossy()), (0, false));
    assert_eq!((r[1].revealed, r[1].appended, r[1].shift()), (0, 1, 0));
    assert_eq!(
        (r[0].stashed, r[1].restored_top, r[1].restored_bottom),
        (0, 0, 0),
        "a trimmed blank row comes back as the blank the grow appends"
    );
    assert_eq!(screen(&t), painted, "text unchanged");
    assert_eq!(t.cursor().row, 5);
}

/// Verify that a primary-screen row flap demotes into history and reveals back losslessly.
#[test]
fn primary_rows_only_flap_is_lossless() {
    let mut t = Terminal::new(8, 20);
    paint(&mut t, 0..8);
    t.process(b"\x1b[6;3H");
    let painted = screen(&t);

    let r = flap(&mut t, &[7, 8]);
    assert!(!r[0].alt && !r[1].alt);
    assert_eq!((r[0].demoted, r[0].shift(), r[0].lossy()), (1, -1, false));
    assert_eq!((r[1].revealed, r[1].appended, r[1].shift()), (1, 0, 1));
    assert_eq!(r[0].shift() + r[1].shift(), 0, "the shifts cancel");
    assert_eq!(
        (r[0].stashed, r[1].restored_top),
        (0, 0),
        "the primary's history holds the row; the resize undo is not involved"
    );
    assert_eq!(screen(&t), painted);
    assert_eq!(t.cursor().row, 5);
}

/// Verify that the resize journal records adjacent resizes, tracks ring overflow, and flags rewraps.
#[test]
fn journal_keeps_both_halves_of_a_flap() {
    let mut t = Terminal::new(8, 20);
    assert_eq!(t.resize_ordinal(), 0);
    assert_eq!(t.resize_journal_since(0), (Vec::new(), 0));

    t.resize(7, 20);
    t.resize(8, 20);
    assert_eq!(t.resize_ordinal(), 2);
    let (r, lost) = t.resize_journal_since(0);
    assert_eq!(lost, 0);
    assert_eq!(
        r.iter()
            .map(|x| (x.ordinal, x.from, x.to))
            .collect::<Vec<_>>(),
        vec![(1, (8, 20), (7, 20)), (2, (7, 20), (8, 20))]
    );
    assert!(r[1].adjoins(&r[0]), "no output between the halves");
    assert_eq!(r[1].content_seq_before, r[0].content_seq);
    assert!(r[1].content_seq > r[1].content_seq_before, "the re-layout");

    t.process(b"\x1b[3;3H");
    t.resize(7, 20);
    t.process(b"x");
    t.resize(8, 20);
    let (r, _) = t.resize_journal_since(2);
    assert!(
        r[0].adjoins(&t.resize_journal_since(1).0[0]),
        "a cursor move"
    );
    assert!(!r[1].adjoins(&r[0]), "the app drew between");

    t.process(b"\x1b[?2026h");
    assert!(t.resize_offloading_scrollback(7, 20).is_none());
    let (r, lost) = t.resize_journal_since(4);
    assert_eq!((r.len(), lost), (1, 0));
    assert_eq!((r[0].ordinal, r[0].in_sync), (5, true));
    t.process(b"\x1b[?2026l");

    for i in 0..17u16 {
        t.resize(8 - i % 2, 20);
    }
    assert_eq!(t.resize_ordinal(), 22, "the ordinal counts every resize");
    let (r, lost) = t.resize_journal_since(0);
    assert_eq!(r.len(), RESIZE_JOURNAL_CAP);
    assert_eq!(lost, 6, "ordinals 1..=6 fell out of the ring");
    assert_eq!(
        r.iter().map(|x| x.ordinal).collect::<Vec<_>>(),
        (7..=22).collect::<Vec<_>>(),
        "oldest first, contiguous"
    );
    assert!(
        r.iter().all(|x| !x.in_sync),
        "the frame closed before these"
    );
    assert_eq!(t.resize_journal_since(5).1, 1, "17 behind, 16 kept");
    assert_eq!(t.resize_journal_since(22), (Vec::new(), 0));
    let (r, lost) = t.resize_journal_since(21);
    assert_eq!((r.len(), r[0].ordinal, lost), (1, 22, 0));
    assert_eq!(
        t.resize_journal_since(99),
        t.resize_journal_since(0),
        "a watermark from another engine lifetime reads as 0"
    );

    t.resize(8, 30);
    let (r, _) = t.resize_journal_since(22);
    assert_eq!((r[0].to, r[0].reflowed), ((8, 30), true));
}

/// Verify that `full_clear_count` increments only on `ED 2` and home `ED 0`.
#[test]
fn full_clear_count_counts_ed2_and_home_ed0_not_ed3_el_or_mid_screen_ed0() {
    let mut t = Terminal::new(8, 20);
    paint(&mut t, 0..8);
    assert_eq!(t.full_clear_count(), 0);
    let mut step = |bytes: &[u8]| {
        let before = t.full_clear_count();
        t.process(bytes);
        t.full_clear_count() - before
    };
    assert_eq!(step(b"\x1b[2J"), 1, "ED 2");
    assert_eq!(step(b"\x1b[3J"), 0, "ED 3 erases history, not the screen");
    assert_eq!(step(b"\x1b[H\x1b[J"), 1, "ED 0 from home");
    assert_eq!(step(b"\x1b[1;1H\x1b[0J"), 1, "explicit ED 0 from home");
    assert_eq!(step(b"\x1b[5;3H\x1b[J"), 0, "mid-screen ED 0");
    assert_eq!(step(b"\x1b[1;2H\x1b[J"), 0, "ED 0 one column off home");
    assert_eq!(step(b"\x1b[H\x1b[K\x1b[2K\x1b[1K"), 0, "EL");
    assert_eq!(step(b"\x1b[8;20H\x1b[1J"), 0, "ED 1");
    assert_eq!(step(b"\x1b[H\x1b[?2J"), 0, "DECSED");
    assert_eq!(step(b"\x1b[2J\x1b[3J\x1b[H"), 1, "Claude Code's clear");

    let clears = t.full_clear_count();
    t.resize(7, 20);
    let (r, _) = t.resize_journal_since(0);
    assert_eq!(r[0].full_clears, clears, "a report carries the count");
    t.process(b"\x1bc");
    assert_eq!(
        t.full_clear_count(),
        clears,
        "RIS counts apart, never resets"
    );
}

/// Verify that `screen_replaced_count` tracks effective alternate-screen switches and full resets.
#[test]
fn screen_replaced_counts_alt_screen_switches_and_resets() {
    let mut t = Terminal::new(8, 20);
    let mut step = |bytes: &[u8]| {
        let before = t.screen_replaced_count();
        t.process(bytes);
        t.screen_replaced_count() - before
    };
    assert_eq!(step(b"\x1b[?1049h"), 1);
    assert_eq!(step(b"\x1b[?1049h"), 1, "a repeated 1049 set clears");
    assert_eq!(step(b"\x1b[?1049l"), 1);
    assert_eq!(step(b"\x1b[?1049l"), 0, "already on the primary screen");
    assert_eq!(step(b"\x1b[?47h\x1b[?47l"), 2);
    assert_eq!(step(b"\x1b[?1047h\x1b[?1047l"), 2);
    assert_eq!(step(b"\x1bc"), 1, "RIS");
    t.resize(7, 20);
    let (r, _) = t.resize_journal_since(0);
    assert_eq!(r[0].screen_replaced, t.screen_replaced_count());
    assert!(!r[0].healed_by(t.full_clear_count(), t.screen_replaced_count()));
    t.process(b"\x1b[?1049h");
    assert!(r[0].healed_by(t.full_clear_count(), t.screen_replaced_count()));
    let before = t.screen_replaced_count();
    t.reset();
    assert_eq!(t.screen_replaced_count(), before + 1, "Terminal::reset");
    assert_eq!(t.full_clear_count(), 0, "no ED was processed");
}

/// Non-drawing mode set that drops the resize undo without moving rows.
const MODE_SET: &[u8] = b"\x1b[?1004h";

/// Flap `rows -> rows - 1 -> rows` with `between` processed in between.
fn flap_with(t: &mut Terminal, rows: u16, between: &[u8]) {
    t.resize(rows - 1, 20);
    if !between.is_empty() {
        t.process(between);
    }
    t.resize(rows, 20);
}

#[test]
fn a_quiet_flap_leaves_no_debt() {
    let mut t = claude_shaped_alt(8);
    assert_eq!(t.render_debt(), RenderDebt::default());
    flap_with(&mut t, 8, b"");
    assert_eq!(
        t.render_debt(),
        RenderDebt::default(),
        "the undo handed the row back"
    );
    t.resize(7, 20);
    t.resize(6, 20);
    assert_eq!(
        t.render_debt(),
        RenderDebt::of_rows(-2, 0),
        "two rows are held by the undo"
    );
    t.resize(7, 20);
    t.resize(8, 20);
    assert_eq!(t.render_debt(), RenderDebt::default());
}

#[test]
fn the_push_corner_leaves_the_bottom_row_as_debt() {
    let mut t = Terminal::new(8, 20);
    t.process(b"\x1b[?1049h\x1b[2J");
    paint(&mut t, 0..8);
    t.process(b"\x1b[1;1H");
    flap_with(&mut t, 8, MODE_SET);
    assert_eq!(
        t.render_debt(),
        RenderDebt::of_rows(0, 1),
        "the bottom row is lost, nothing moved"
    );
    let mut t = Terminal::new(8, 20);
    t.process(b"\x1b[?1049h\x1b[2J");
    paint(&mut t, 0..8);
    t.process(b"\x1b[1;1H");
    flap_with(&mut t, 8, b"");
    assert_eq!(t.render_debt(), RenderDebt::default());
}

#[test]
fn debt_accumulates_over_a_run_of_flaps_and_is_exact_after_a_journal_overrun() {
    let mut t = claude_shaped_alt(8);
    let flaps = 20;
    for _ in 0..flaps {
        paint(&mut t, 7..8);
        flap_with(&mut t, 8, MODE_SET);
    }
    let (_, lost) = t.resize_journal_since(0);
    assert!(
        lost > 0,
        "{flaps} flaps are {} resizes: past the journal",
        2 * flaps
    );
    assert_eq!(
        t.render_debt(),
        RenderDebt::of_rows(-flaps, 0),
        "the counter is exact though the journal is not"
    );
}

#[test]
fn a_full_clear_a_screen_switch_and_ris_each_reset_the_debt() {
    let displaced = || {
        let mut t = claude_shaped_alt(8);
        flap_with(&mut t, 8, MODE_SET);
        assert_eq!(t.render_debt(), RenderDebt::of_rows(-1, 0));
        t
    };
    let mut t = displaced();
    t.process(b"\x1b[2J");
    assert_eq!(
        t.render_debt(),
        RenderDebt::default(),
        "ED 2 repaints from scratch"
    );
    let mut t = displaced();
    t.process(b"\x1b[H\x1b[J");
    assert_eq!(
        t.render_debt(),
        RenderDebt::default(),
        "a home ED 0 is a full clear"
    );
    let mut t = displaced();
    t.process(b"\x1b[2K\x1b[1J");
    assert_eq!(
        t.render_debt(),
        RenderDebt::of_rows(-1, 0),
        "EL and ED 1 repaint nothing"
    );
    let mut t = displaced();
    t.process(b"\x1b[?1049l");
    assert_eq!(
        t.render_debt(),
        RenderDebt::default(),
        "leaving the alternate screen"
    );
    let mut t = displaced();
    t.process(b"\x1b[?1049h");
    assert_eq!(t.render_debt(), RenderDebt::default(), "entering it again");
    let mut t = displaced();
    t.process(b"\x1bc");
    assert_eq!(t.render_debt(), RenderDebt::default(), "RIS");
}

#[test]
fn a_primary_screen_resize_owes_nothing() {
    let mut t = Terminal::new(8, 20);
    for r in 0..8u16 {
        t.process(format!("\x1b[{};1HL{r}", r + 1).as_bytes());
    }
    t.process(b"\x1b[8;3H");
    flap_with(&mut t, 8, MODE_SET);
    assert_eq!(t.render_debt(), RenderDebt::default());
}

/// Verify that a primary-screen row flap over a blank tail appends back a blank rather than revealing history.
#[test]
fn a_primary_row_flap_over_a_blank_tail_leaves_every_row_where_it_was() {
    let mut t = Terminal::new(8, 20);
    for i in 0..20 {
        t.process(format!("h{i}\r\n").as_bytes());
    }
    t.process(b"\x1b[H\x1b[2Jtop\r\n");
    assert!(t.grid().scrollback_lines() > 0, "history above the screen");
    let painted = screen(&t);
    let cursor = t.cursor();

    let r = flap(&mut t, &[7, 8]);
    assert_eq!(screen(&t), painted, "every row where the program drew it");
    assert_eq!(
        (t.cursor().row, t.cursor().col),
        (cursor.row, cursor.col),
        "the cursor where the program left it"
    );
    assert_eq!(r[0].trimmed, 1, "the shrink trimmed the blank bottom row");
    assert_eq!(
        (r[1].revealed, r[1].appended, r[1].shift()),
        (0, 1, 0),
        "the grow handed the blank back and moved nothing"
    );
}

/// Verify that primary-screen flaps that grow first demote revealed history rows before trimming the blank tail.
#[test]
fn a_primary_row_flap_that_grows_first_leaves_every_row_where_it_was() {
    for heights in [&[9u16, 8][..], &[7, 9, 8], &[5, 10, 8], &[7, 8, 9, 8]] {
        let mut t = Terminal::new(8, 20);
        for i in 0..20 {
            t.process(format!("h{i}\r\n").as_bytes());
        }
        t.process(b"\x1b[H\x1b[2Jtop\r\n");
        let painted = screen(&t);
        let cursor = t.cursor();

        let r = flap(&mut t, heights);
        assert_eq!(
            screen(&t),
            painted,
            "{heights:?}: every row where it was drawn"
        );
        assert_eq!(
            (t.cursor().row, t.cursor().col),
            (cursor.row, cursor.col),
            "{heights:?}: the cursor where the program left it"
        );
        let shift: i32 = r.iter().map(|r| r.shift()).sum();
        assert_eq!(shift, 0, "{heights:?}: the row shifts net to zero");
    }
}

/// Run an offloaded resize to completion by reflowing and re-attaching scrollback chunks.
fn offloaded_resize(t: &mut Terminal, rows: u16, cols: u16) {
    let mut job = t.resize_offloading_scrollback(rows, cols);
    while let Some(pending) = job {
        job = t.finish_resize_offload(pending.reflow());
    }
}

/// Verify that primary-screen flaps that also change width preserve row positions across sync and offloaded paths.
#[test]
fn a_primary_flap_that_changes_the_width_too_leaves_every_row_where_it_was() {
    let paths: [&[(u16, u16)]; 4] = [
        &[(45, 131), (50, 144)],
        &[(55, 158), (50, 144)],
        &[(45, 144), (45, 131), (50, 131), (50, 144)],
        &[(47, 135), (53, 150), (50, 144)],
    ];
    for path in paths {
        for offloaded in [false, true] {
            let at = format!("{path:?} offloaded={offloaded}");
            let mut t = Terminal::with_scrollback(
                50,
                144,
                1000,
                crate::scrollback::Scrollback::with_defaults(),
            );
            for i in 1..=120 {
                t.process(format!("{i}\r\n").as_bytes());
            }
            t.process(b"\x1b[H\x1b[2Jtop row\r\n");
            let painted = screen(&t);
            let cursor = (t.cursor().row, t.cursor().col);
            for &(rows, cols) in path {
                if offloaded {
                    offloaded_resize(&mut t, rows, cols);
                } else {
                    t.resize(rows, cols);
                }
            }
            assert_eq!(screen(&t), painted, "{at}: every row where it was drawn");
            assert_eq!(
                (t.cursor().row, t.cursor().col),
                cursor,
                "{at}: the cursor where the program left it"
            );
        }
    }
    for path in [&[(7u16, 19u16), (8, 20)][..], &[(9, 21), (8, 20)]] {
        let mut t = Terminal::new(8, 20);
        for i in 0..20 {
            t.process(format!("h{i}\r\n").as_bytes());
        }
        t.process(b"\x1b[H\x1b[2Jtop\r\n");
        let painted = screen(&t);
        for &(rows, cols) in path {
            t.resize(rows, cols);
        }
        assert_eq!(screen(&t), painted, "{path:?}");
        assert_eq!((t.cursor().row, t.cursor().col), (1, 0), "{path:?}");
    }
}

/// Verify that redrawing from the top after a grow claims the top rows so shrinking back trims below.
#[test]
fn a_redraw_from_the_top_after_a_grow_keeps_its_rows_through_the_shrink_back() {
    let build = || {
        let mut t = Terminal::new(8, 20);
        for i in 0..20 {
            t.process(format!("h{i}\r\n").as_bytes());
        }
        t.process(b"\x1b[H\x1b[2Jtop\r\n");
        t
    };
    for redraw in [&b"\x1b[H"[..], b"\x1b[H\x1b[J", b"\x1b[999A\r"] {
        let at = String::from_utf8_lossy(redraw).escape_debug().to_string();
        let mut never = build();
        let mut flapped = build();
        flapped.resize(9, 20);
        for t in [&mut never, &mut flapped] {
            t.process(redraw);
            t.process(b"N0p\r\nN1p\r\nN2p\r\n");
        }
        flapped.resize(8, 20);
        assert_eq!(
            screen(&flapped),
            screen(&never),
            "{at}: every row where it was drawn"
        );
        assert_eq!(
            (flapped.cursor().row, flapped.cursor().col),
            (never.cursor().row, never.cursor().col),
            "{at}: the cursor where the program left it"
        );
    }
}

#[test]
fn a_conpty_resize_owes_nothing() {
    let mut t = claude_shaped_alt(8);
    t.resize_with_policy(7, 20, crate::grid::ResizePolicy::ConPty);
    t.process(MODE_SET);
    t.resize_with_policy(8, 20, crate::grid::ResizePolicy::ConPty);
    assert_eq!(t.render_debt(), RenderDebt::default());
    let mut t = claude_shaped_alt(8);
    flap_with(&mut t, 8, MODE_SET);
    assert_eq!(t.render_debt(), RenderDebt::of_rows(-1, 0));
}

/// Verify that narrowing an alternate screen across drawn cells records `cut` debt until a full clear.
#[test]
fn a_column_flap_that_cuts_drawn_cells_owes_them_until_a_full_clear() {
    let ruled = || {
        let mut t = Terminal::new(8, 20);
        t.process(b"\x1b[?1049h\x1b[2J\x1b[6;1H");
        t.process("-".repeat(20).as_bytes());
        t.process(b"\x1b[1;1Hshort");
        t
    };
    let mut t = ruled();
    t.resize(8, 19);
    t.process(MODE_SET);
    t.resize(8, 20);
    let (r, _) = t.resize_journal_since(0);
    assert_eq!((r[0].cut, r[1].cut), (1, 0), "the narrowing cut one column");
    assert_eq!(t.row_text(5).unwrap_or_default().trim_end().len(), 19);
    let debt = t.render_debt();
    assert_eq!(debt.rows(), (0, 0), "no row moved");
    assert_eq!(debt.cut, 1);
    assert!(debt.owed(), "a cut cell is owed though no row moved");

    t.resize(8, 17);
    t.resize(8, 20);
    assert_eq!(t.render_debt().cut, 3, "columns 17 and 18 too");

    t.process(b"\x1b[2J");
    assert!(!t.render_debt().owed(), "ED 2 repaints from scratch");

    let mut t = ruled();
    t.process(b"\x1b[2J\x1b[1;1Hshort");
    t.resize(8, 10);
    t.resize(8, 20);
    assert_eq!(t.render_debt(), RenderDebt::default());
    let mut t = Terminal::new(8, 20);
    t.process("-".repeat(20).as_bytes());
    t.resize(8, 19);
    t.resize(8, 20);
    assert_eq!(t.render_debt(), RenderDebt::default());
}

#[test]
fn seeding_the_debt_survives_a_checkpoint_restore_and_a_full_clear_resets_it() {
    let mut t = claude_shaped_alt(8);
    flap_with(&mut t, 8, MODE_SET);
    assert_eq!(t.render_debt(), RenderDebt::of_rows(-1, 0));
    let mut restored = claude_shaped_alt(8);
    assert_eq!(restored.render_debt(), RenderDebt::default());
    restored.seed_render_debt(RenderDebt::of_rows(-1, 0));
    assert_eq!(restored.render_debt(), RenderDebt::of_rows(-1, 0));
    restored.process(b"\x1b[2J");
    assert_eq!(restored.render_debt(), RenderDebt::default());
    let cut = RenderDebt {
        cut: 2,
        ..RenderDebt::default()
    };
    restored.seed_render_debt(cut);
    assert_eq!(restored.render_debt(), cut, "a cut crosses as the rows do");
    let mut primary = Terminal::new(8, 20);
    primary.seed_render_debt(RenderDebt::of_rows(-1, 0));
    assert_eq!(
        primary.render_debt(),
        RenderDebt::default(),
        "the primary screen owes nothing"
    );
}
