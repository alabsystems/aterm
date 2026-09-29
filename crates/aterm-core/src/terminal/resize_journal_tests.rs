// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The resize journal against the incident's geometry: a Claude-shaped frame on
//! the alternate screen, a rows-only flap (8→7→8 here, 64→63→64 live), and
//! nothing processed between the two halves.

use super::{RESIZE_JOURNAL_CAP, ResizeReport};
use crate::terminal::Terminal;

/// The visible rows' text, trailing blanks trimmed.
fn screen(t: &Terminal) -> Vec<String> {
    (0..usize::from(t.rows()))
        .map(|r| t.row_text(r).unwrap_or_default().trim_end().to_string())
        .collect()
}

/// Write `L<r>` on each row in `rows` with absolute cursor moves only (the
/// diff renderer's vocabulary: no LF, so nothing scrolls).
fn paint(t: &mut Terminal, rows: std::ops::Range<u16>) {
    for r in rows {
        t.process(format!("\x1b[{};1HL{r}", r + 1).as_bytes());
    }
}

/// The incident's shape at `rows` rows: the alternate screen, every row
/// painted (a non-blank footer under the composer), and the cursor parked on
/// the composer three rows from the bottom, as Claude Code parks it.
fn claude_shaped_alt(rows: u16) -> Terminal {
    let mut t = Terminal::new(rows, 20);
    t.process(b"\x1b[?1049h\x1b[2J");
    paint(&mut t, 0..rows);
    t.process(format!("\x1b[{};3H", rows - 2).as_bytes());
    assert_eq!(t.cursor().row, rows - 3);
    t
}

/// Run `resizes` and return the reports they journalled, asserting none was
/// lost.
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

/// The incident, quiet: both halves land before the app's handler runs and
/// nothing is processed between them. The shrink finds no blank tail to trim,
/// so it demotes the top row; the alt grid keeps no history, so the row leaves
/// the ring, but the grid's resize undo keeps it and the grow hands it back.
/// The app, reading 8 == 8, skips its repaint, and its diff frames land on
/// exactly the rows it painted.
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

    // The claude shape at its live size, cursor on the bottom row.
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

/// The same flap with the app's output landing BETWEEN the halves (its
/// SIGWINCH handler ran at the smaller size): the undo is dropped, the grow
/// appends a blank row, and the screen stays one row higher than painted.
/// That is the app's to repaint, and its changed-size repaint (ED 2) heals it.
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

    // The app's next diff frame moves the cursor and draws; nothing heals the
    // shift. Its ED 2 repaint does.
    t.process(b"\x1b[?2026h\x1b[6;1H\x1b[KL5*\x1b[?2026l");
    let healed = |t: &Terminal| grow.healed_by(t.full_clear_count(), t.screen_replaced_count());
    assert!(!healed(&t), "a diff frame is not a repaint");
    assert!(t.content_seq() > grow.content_seq, "the app drew on it");
    // ED 3 alone erases the (absent) alt-screen history and leaves every row
    // where the flap put it: not a heal.
    t.process(b"\x1b[3J");
    assert!(!healed(&t), "ED 3 leaves the shifted rows in place");
    assert_eq!(screen(&t)[..5], painted[1..6], "still shifted");
    t.process(b"\x1b[2J");
    assert!(healed(&t));
    // A reading taken BEFORE the flap (lower counters) is never a heal.
    assert!(!grow.healed_by(clears, 0));
}

/// What the app set around its picture survives a quiet flap too: its DECSC
/// slot (the terminal-level saved cursor, which no resize moves) still names
/// the row it saved, and its scroll region, which every resize resets, is put
/// back once the grow lands on the original height. A combining mark on the
/// demoted row comes back with it.
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

    // DECRC lands where DECSC saved, on the row it saved.
    t.process(b"\x1b[1;1H\x1b8");
    assert_eq!((t.cursor().row, t.cursor().col), (5, 2));
}

/// The selection comes through an undone flap exactly as it went in, even
/// with an anchor on the row the shrink demoted. (Adversarial review of the
/// first cut: the shrink transform CLAMPED that anchor, the alt grid keeping
/// no history to hold it, and the grow's reveal then landed it one row low
/// while every cell was back.) A selection moved between the halves is the
/// user's, and is left as they moved it.
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

    // Moved between the halves: not put back over the user's drag.
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

    // Output between the halves drops the undo, and the selection takes the
    // ordinary transform (its content really did move).
    let mut t = claude_shaped_alt(8);
    let before = select(&mut t);
    t.resize(7, 20);
    t.process(b"\x1b[?1000h");
    t.resize(8, 20);
    assert_ne!(t.text_selection(), &before);
}

/// An alt-screen exit and re-entry between the halves (a parked 47/1047
/// buffer is resized with the primary) is a screen switch, not a flap: the
/// reused buffer hands nothing back.
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

    // Parked while the primary shrinks: the parked buffer demotes its top row
    // too, and the re-entry drops what it kept.
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

/// Every engine gets its own lifetime token, fixed for its life; a restored
/// engine (a fresh one) gets a new token and restarts its
/// ordinals at 0 — what lets a reader tell a stale copy of THIS engine (a
/// lower ordinal, same token) from a restored one.
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

/// The negative control: a blank tail below the cursor absorbs the shrink by
/// TRIM, so nothing moves and the flap is an identity even with no history.
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

/// The same flap on the primary screen is lossless: the demoted row goes into
/// history and the grow reveals it back. This is why the defect is specific to
/// the alternate screen.
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

/// Both halves of a flap are journalled even with no `process()` between them,
/// which is the case a per-read geometry watermark cannot see. The ring keeps
/// the newest 16 and COUNTS what it dropped, while the ordinal stays exact.
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

    // Output between two resizes breaks the adjacency; a cursor move does not.
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

    // The offloading entry point journals too, and names an open 2026 frame.
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

    // A width change on the primary screen is a rewrap, named as such.
    t.resize(8, 30);
    let (r, _) = t.resize_journal_since(22);
    assert_eq!((r[0].to, r[0].reflowed), ((8, 30), true));
}

/// A full clear is ED 2 or ED 0 from home. ED 3 (a scrollback erase that
/// leaves every visible row in place), EL, ED 1, DECSED and an ED 0 anywhere
/// else do not count. Claude Code's clear (`ED 2` + `ED 3`) counts once, for
/// its ED 2; a reader keys on the counter advancing.
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

/// Screen replacements are counted apart from clears: every alternate-screen
/// switch (47, 1047, 1049, and the repeated 1049 set that clears) and every
/// full reset. A no-op switch is not counted.
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
