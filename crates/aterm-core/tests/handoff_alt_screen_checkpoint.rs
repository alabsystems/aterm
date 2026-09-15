// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! REGRESSION: an in-session update refused to apply — forever — on any desk
//! where a session was sitting on the alternate screen.
//!
//! `Terminal::alt_grid` is the INACTIVE grid, not "the alternate screen". While
//! the alternate screen is up it holds the SAVED PRIMARY, scrollback and all
//! (`terminal/buffer_api.rs`: "`self.alt_grid`, which holds the SAVED PRIMARY").
//!
//! The handoff wire gives the alt blob exactly `rows` records: `CheckpointMeta`
//! carries a single `history_lines`, and that field describes the MAIN blob, so
//! the alt blob has no way to declare a history count of its own. The consumer
//! therefore validates it with `history = 0`. Projecting the inactive grid with
//! `max_history` instead made the blob `min(primary_scrollback, max) + rows`
//! records; `deserialize_lines_strict` refuses a declared count above the
//! expected one outright, so `screen_digest` returned `None` and every apply
//! failed with "visible checkpoint set could not be committed canonically".
//!
//! These tests pin the wire shape at the producer, which is the only place that
//! can honour it.

use aterm_core::scrollback::{deserialize_lines_strict, serialize_lines};
use aterm_core::terminal::{Terminal, TerminalCheckpoint};

const ROWS: u16 = 49;
const COLS: u16 = 131;
/// The producer's real carry target (`seamless::max_handoff_history_lines`).
const CARRY: usize = 256;

/// Exactly the consumer's canonicality check for a grid blob that must hold
/// `expected` records — `seamless::checkpoint_grid_is_canonical`, reproduced so
/// this test fails for the same reason the updater did.
fn is_canonical(bytes: &[u8], expected: usize, cols: u16) -> bool {
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512));
    deserialize_lines_strict(bytes, expected, usize::from(cols), content_cap, record_cap)
        .is_some_and(|lines| lines.len() == expected && serialize_lines(&lines) == bytes)
}

/// A terminal whose PRIMARY screen has real scrollback behind it.
fn primary_with_history() -> Terminal {
    let mut t = Terminal::new(ROWS, COLS);
    for i in 0..(usize::from(ROWS) + 400) {
        t.process(format!("primary line {i}\r\n").as_bytes());
    }
    t
}

fn carry(t: &Terminal) -> TerminalCheckpoint {
    t.checkpoint_carry(CARRY).expect("parser is Ground")
}

/// The precondition the regression depends on: the primary really does carry
/// history, so the alt-screen case below is not vacuous.
#[test]
fn the_primary_screen_carries_history() {
    let cp = carry(&primary_with_history());
    assert!(
        cp.history_lines > 0,
        "the primary must have scrollback for this suite to mean anything"
    );
}

/// THE REGRESSION. While the alternate screen is up, the saved primary — with
/// its scrollback — is what `alt_grid` holds, and the wire still allows it
/// exactly `rows` records.
#[test]
fn the_alt_blob_is_exactly_rows_records_while_the_alternate_screen_is_up() {
    let mut t = primary_with_history();
    // DECSET 1049 — the primary, scrollback and all, is parked in `alt_grid`.
    t.process(b"\x1b[?1049h");
    t.process(b"a full-screen app draws here");

    let cp = carry(&t);
    let alt = cp
        .alt_grid
        .as_ref()
        .expect("the saved primary is parked in alt_grid");

    assert!(
        is_canonical(alt, usize::from(cp.rows), cp.cols),
        "the alt blob must hold exactly `rows` records: the wire has one \
         `history_lines` and it describes the MAIN blob, so a carried alt \
         history is unrepresentable and the consumer refuses the whole capture"
    );
}

/// The other entry mode. 1047 reuses the PERSISTENT alternate buffer rather
/// than allocating a cleared one, so it reaches the same parked saved primary
/// by a different path.
#[test]
fn the_alt_blob_is_exactly_rows_records_under_mode_1047_too() {
    let mut t = primary_with_history();
    t.process(b"\x1b[?1047h");
    t.process(b"a full-screen app draws here");

    let cp = carry(&t);
    let alt = cp
        .alt_grid
        .as_ref()
        .expect("the saved primary is parked in alt_grid");

    assert!(
        is_canonical(alt, usize::from(cp.rows), cp.cols),
        "the alt blob must hold exactly `rows` records under mode 1047 as well"
    );
}

/// CONTROL: back on the primary screen, `alt_grid` holds the PERSISTENT
/// alternate buffer, whose ring cap is 0 by xterm spec. This passed before the
/// fix and must keep passing — the fix must not disturb the case that worked.
///
/// Exit via 1047, not 1049: 1049 DISCARDS the alternate buffer (`alt_grid`
/// becomes `None`), so it cannot exercise a parked alt blob at all.
#[test]
fn the_alt_blob_is_exactly_rows_records_after_leaving_the_alternate_screen() {
    let mut t = primary_with_history();
    t.process(b"\x1b[?1047h");
    t.process(b"a full-screen app draws here");
    t.process(b"\x1b[?1047l");

    let cp = carry(&t);
    let alt = cp
        .alt_grid
        .as_ref()
        .expect("the alternate buffer is persistent and survives the exit");

    assert!(
        is_canonical(alt, usize::from(cp.rows), cp.cols),
        "the parked alternate buffer must still be exactly `rows` records"
    );
}

/// The MAIN blob's own contract, which is the one `history_lines` describes:
/// `rows + history_lines` records. Pinned alongside so a fix to the alt side
/// cannot quietly break the side that is allowed to carry history.
#[test]
fn the_main_blob_is_rows_plus_its_declared_history() {
    let cp = carry(&primary_with_history());
    let expected = usize::from(cp.rows) + cp.history_lines as usize;

    assert!(
        is_canonical(&cp.grid, expected, cp.cols),
        "the main blob must hold exactly `rows + history_lines` records"
    );
    assert!(
        cp.history_lines as usize <= CARRY,
        "the carry bound is an upper bound on the declared history"
    );
}

// ---------------------------------------------------------------------------
// REGRESSION (2026-09-14, live): after a self-update handoff the adopted
// worker's first archived alt-screen rows were its composer's rule and footer.
//
// The adopted engine restores the checkpoint at the OLD grid size, the app
// repaints that screen, and the window's first resize converges engine and PTY
// to the new frame. The repaint diffed as identical, so no chrome height was
// ever measured, and the resize flushed the whole old screen into the archive:
// rules, prompt and footer included. The restored screen is now the baseline,
// and a resize before any chrome was measured leaves out the rows the new frame
// still shows at the bottom.

/// A synthetic Claude-Code-shaped screen (never a real capture): transcript
/// rows, then the composer — a blank, a rule, the prompt, a rule, the footer —
/// with the rules drawn one short of the screen's width.
fn claude_screen(start: usize, rows: u16, cols: u16) -> Vec<String> {
    let rule = "─".repeat(usize::from(cols) - 1);
    (start..start + usize::from(rows) - 5)
        .map(|i| format!("⏺ transcript row {i:04} with words"))
        .chain([
            String::new(),
            rule.clone(),
            "❯ draft the next instruction".to_string(),
            rule,
            "  ? for shortcuts".to_string(),
        ])
        .collect()
}

/// One synchronized-update frame painting every row of `screen` in place.
fn sync_frame(screen: &[String]) -> Vec<u8> {
    let mut v = b"\x1b[?2026h\x1b[?25l".to_vec();
    for (r, text) in screen.iter().enumerate() {
        v.extend_from_slice(format!("\x1b[{};1H\x1b[2K{text}", r + 1).as_bytes());
    }
    v.extend_from_slice(b"\x1b[?25h\x1b[?2026l");
    v
}

/// The adopting side of a handoff: the old process's app ran frames
/// `0..=last`, the checkpoint crossed the wire the way the producer makes it,
/// and a fresh engine restored it.
fn adopted_after(last: usize) -> Terminal {
    let mut old = Terminal::new(ROWS, COLS);
    old.process(b"\x1b[?1049h");
    for s in 0..=last {
        old.process(&sync_frame(&claude_screen(s, ROWS, COLS)));
    }
    let mut new = Terminal::new(ROWS, COLS);
    new.set_alt_archive_enabled(true);
    new.restore_checkpoint(&carry(&old));
    new
}

fn chrome_rows(t: &Terminal) -> Vec<String> {
    t.alt_archive()
        .texts()
        .into_iter()
        .filter(|r| r.starts_with(['─', '❯']) || r.contains("shortcuts"))
        .collect()
}

/// Design test 4: restore, a frame at the old size, then the resize.
#[test]
fn an_adopted_composer_is_never_archived_by_the_first_resize() {
    let transcript = |r: std::ops::Range<usize>| -> Vec<String> {
        r.map(|i| format!("⏺ transcript row {i:04} with words"))
            .collect()
    };
    let t_old = usize::from(ROWS) - 5; // transcript rows 3..3 + t_old are shown
    // (new size, the app's first row at it, rows the archive must hold)
    let cases = [
        // One row shorter: only the top row left the screen.
        ((ROWS - 1, COLS), 4, transcript(3..4)),
        // One row taller, an older row on top: every row is still shown.
        ((ROWS + 1, COLS), 2, Vec::new()),
        // Narrower: the transcript re-wrapped and moved up two; the rules were
        // redrawn at the new width and are still the composer's.
        ((ROWS, COLS - 11), 5, transcript(3..3 + t_old)),
    ];
    for ((rows, cols), first, want) in cases {
        let mut t = adopted_after(3);
        // The app repaints what it showed, at the old size…
        t.process(&sync_frame(&claude_screen(3, ROWS, COLS)));
        assert!(t.alt_archive().is_empty(), "{rows}x{cols}: a repaint");
        // …then the window's first resize, and the app redraws at the new size.
        t.resize(rows, cols);
        t.process(&sync_frame(&claude_screen(first, rows, cols)));
        assert_eq!(chrome_rows(&t), Vec::<String>::new(), "{rows}x{cols}");
        assert_eq!(t.alt_archive().texts(), want, "{rows}x{cols}");
    }
}

/// The resize can come first, too: the app's first frame after the adoption
/// is already at the new size. Diffed against the restored screen, it archives
/// the rows that screen showed and the new one does not — never the composer.
#[test]
fn an_adopted_screen_resized_before_its_first_frame_archives_only_what_left() {
    let mut t = adopted_after(3);
    t.resize(ROWS - 2, COLS);
    t.process(&sync_frame(&claude_screen(5, ROWS - 2, COLS)));
    assert_eq!(chrome_rows(&t), Vec::<String>::new());
    assert_eq!(
        t.alt_archive().texts(),
        vec![
            "⏺ transcript row 0003 with words".to_string(),
            "⏺ transcript row 0004 with words".to_string(),
        ]
    );
}

// ---------------------------------------------------------------------------
// Round 10: the ARCHIVE itself crosses the handoff (`AltArchiveCarry`). The
// old engine's capture is split as the GUI splits it — the head inside the
// freeze, beside the checkpoint; the rows afterwards, behind the fence — and
// the adopting engine imports it right after restoring the checkpoint.

use aterm_core::terminal::{AltArchiveCarry, AltArchiveGapKind, AltArchiveImport, AltArchiveQuery};

/// The old engine after frames `0..=last`, its checkpoint, and its archive
/// carry (the whole archive when `rows`, the counters alone otherwise).
fn old_side(last: usize, rows: bool) -> (Terminal, TerminalCheckpoint, AltArchiveCarry) {
    let mut old = Terminal::new(ROWS, COLS);
    old.set_alt_archive_enabled(true);
    old.set_alt_archive_origin(4242);
    old.process(b"\x1b[?1049h");
    for s in 0..=last {
        old.process(&sync_frame(&claude_screen(s, ROWS, COLS)));
    }
    let cp = carry(&old);
    let (fence, mut archive) = old.alt_archive_carry_head(true);
    if rows {
        assert!(old.alt_archive_carry_rows(&mut archive, fence, 0, usize::MAX));
    }
    (old, cp, archive)
}

fn adopt(cp: &TerminalCheckpoint, archive: AltArchiveCarry) -> (Terminal, AltArchiveImport) {
    let mut new = Terminal::new(ROWS, COLS);
    new.set_alt_archive_enabled(true);
    new.set_alt_archive_origin(9999); // the new process's own, replaced by the carry
    new.restore_checkpoint(cp);
    let outcome = new.alt_archive_import(archive);
    (new, outcome)
}

fn everything(t: &Terminal) -> aterm_core::terminal::AltArchiveRead {
    t.alt_archive_read(AltArchiveQuery::oldest(0, usize::MAX))
}

/// Design test 7's core half: with the archive carried, the adopted engine
/// archives every later frame exactly as the old one would have — same
/// origin, same indices, same rows, no gap, no chrome.
#[test]
fn a_carried_archive_goes_on_exactly_as_the_old_engine_would() {
    let (mut old, cp, archive) = old_side(12, true);
    let (mut new, outcome) = adopt(&cp, archive);
    assert_eq!(outcome, AltArchiveImport::Exact);
    assert_eq!(everything(&new), everything(&old), "at the handoff");
    // The app repaints what it showed, then goes on scrolling.
    for s in [12, 13, 14, 17, 18, 30] {
        let frame = sync_frame(&claude_screen(s, ROWS, COLS));
        old.process(&frame);
        new.process(&frame);
        assert_eq!(everything(&new), everything(&old), "after frame {s}");
    }
    let read = everything(&new);
    assert_eq!(read.origin, 4242, "the origin survives");
    assert!(read.gaps.is_empty(), "continuous: {:?}", read.gaps);
    assert_eq!(chrome_rows(&new), Vec::<String>::new());
    assert_eq!(
        new.alt_archive().texts(),
        (0..30)
            .map(|i| format!("⏺ transcript row {i:04} with words"))
            .collect::<Vec<_>>(),
        "every row that left the top, once"
    );
}

/// H2: the window's first resize after the adoption is a real resize — a
/// `resize` gap, honestly reported — but nothing is lost: every row that left
/// the top is still archived, and the composer never is.
#[test]
fn a_resize_after_a_carried_archive_is_a_gap_that_loses_nothing() {
    let (_old, cp, archive) = old_side(12, true);
    let (mut new, _) = adopt(&cp, archive);
    new.process(&sync_frame(&claude_screen(12, ROWS, COLS)));
    new.resize(ROWS - 1, COLS);
    for s in [14, 15, 20] {
        new.process(&sync_frame(&claude_screen(s, ROWS - 1, COLS)));
    }
    let read = everything(&new);
    assert!(
        read.gaps
            .iter()
            .any(|g| g.kind == AltArchiveGapKind::Resize),
        "{:?}",
        read.gaps
    );
    assert_eq!(read.lost, 0);
    assert_eq!(chrome_rows(&new), Vec::<String>::new());
    let texts = new.alt_archive().texts();
    for i in 0..20 {
        let row = format!("⏺ transcript row {i:04} with words");
        assert!(texts.contains(&row), "{row} was lost: {texts:?}");
    }
}

/// Counters only (the rows could not be attached): the indices go on and a
/// reader holding an old mark is told those rows are lost — never handed
/// another screen's rows under their indices — and the carried differ state
/// still archives what scrolls off next.
#[test]
fn a_counters_only_carry_says_lost_and_goes_on() {
    let (old, cp, archive) = old_side(12, false);
    let last = old.alt_archive().last();
    assert!(last >= 12);
    let (mut new, outcome) = adopt(&cp, archive);
    assert_eq!(outcome, AltArchiveImport::Exact);
    let read = new.alt_archive_read(AltArchiveQuery::oldest(3, 100));
    assert_eq!((read.rows.len(), read.lost, read.last), (0, last - 3, last));
    new.process(&sync_frame(&claude_screen(15, ROWS, COLS)));
    let read = new.alt_archive_read(AltArchiveQuery::oldest(last, 100));
    assert_eq!(read.lost, 0);
    assert_eq!(read.first, last + 1, "the next row takes the next index");
    assert_eq!(
        read.rows.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
        (12..15)
            .map(|i| format!("⏺ transcript row {i:04} with words"))
            .collect::<Vec<_>>(),
        "the screen showed rows 12.. at the handoff; frame 15 scrolled three off"
    );
}

/// No differ state (the freeze had no time for it): the restored screen is
/// the new baseline after a `restore` gap, as a restore without a carry does.
#[test]
fn without_the_differ_state_the_restored_screen_is_the_baseline() {
    let (_old, cp, mut archive) = old_side(12, true);
    archive.differ = None;
    let (mut new, outcome) = adopt(&cp, archive);
    assert_eq!(outcome, AltArchiveImport::NoBaseline);
    let read = everything(&new);
    assert_eq!(read.origin, 4242);
    assert_eq!(
        read.gaps.last().map(|g| g.kind),
        Some(AltArchiveGapKind::Restore)
    );
    let before = new.alt_archive().len();
    new.process(&sync_frame(&claude_screen(14, ROWS, COLS)));
    assert_eq!(
        new.alt_archive().texts()[before..].to_vec(),
        vec![
            "⏺ transcript row 0012 with words".to_string(),
            "⏺ transcript row 0013 with words".to_string(),
        ],
        "diffed against the restored screen"
    );
    assert_eq!(chrome_rows(&new), Vec::<String>::new());
}

/// An adopting engine whose archive is off drops the carried rows.
#[test]
fn an_adopting_engine_with_the_archive_off_drops_the_carry() {
    let (_old, cp, archive) = old_side(12, true);
    let mut new = Terminal::new(ROWS, COLS);
    new.set_alt_archive_enabled(false);
    new.restore_checkpoint(&cp);
    assert_eq!(new.alt_archive_import(archive), AltArchiveImport::Refused);
    assert!(new.alt_archive().is_empty());
}

/// A TAIL carry — what the GUI sends: the rows after its recent turns' marks
/// — and then, at the SAME size, the app scrolls back past the tail's first
/// row (someone rereading earlier context) and forward again. The tail
/// reaches as far back as the differ can point, so the adopted engine
/// recognizes every re-shown row as the old one does: nothing archived twice,
/// no gap. (Round-10 review: it archived the scrolled-back rows again, as new
/// rows, with no gap and `lost=0`.)
#[test]
fn a_tail_carry_recognizes_a_scroll_back_past_its_first_row() {
    let (mut old, cp, _) = old_side(300, false);
    let last = old.alt_archive().last();
    let (fence, mut archive) = old.alt_archive_carry_head(true);
    assert!(old.alt_archive_carry_rows(&mut archive, fence, last - 29, usize::MAX));
    assert!(archive.first < last - 29, "reaches past the host's tail");
    let (mut new, outcome) = adopt(&cp, archive);
    assert_eq!(outcome, AltArchiveImport::Exact);
    // Back 90 rows (two screens), three a frame, forward again, three new.
    let mut views: Vec<usize> = (1..=30).map(|k| 300 - 3 * k).collect();
    views.extend((0..30).rev().map(|k| 300 - 3 * k));
    views.push(303);
    for v in views {
        let frame = sync_frame(&claude_screen(v, ROWS, COLS));
        old.process(&frame);
        new.process(&frame);
    }
    let after = |t: &Terminal| t.alt_archive_read(AltArchiveQuery::oldest(last, usize::MAX));
    let (a, b) = (after(&old), after(&new));
    assert_eq!(b.rows, a.rows, "the rows archived after the handoff");
    assert_eq!((b.last, &b.gaps, b.back), (a.last, &a.gaps, a.back));
    assert_eq!(
        b.rows.iter().map(|r| r.to_string()).collect::<Vec<_>>(),
        (300..303)
            .map(|i| format!("⏺ transcript row {i:04} with words"))
            .collect::<Vec<_>>(),
        "only the three new rows"
    );
    assert!(b.gaps.is_empty(), "{:?}", b.gaps);
}

/// The freeze had no time for the differ's state: it is taken after the
/// freeze, behind the fence, and the adopted engine goes on exactly — no
/// `restore` gap, so a report across the handoff stays whole. (Round-10
/// review: without it the report said `archive-gap` with every row there.)
#[test]
fn a_differ_state_taken_after_the_freeze_goes_on_exactly() {
    let (mut old, cp, _) = old_side(12, false);
    let (fence, mut archive) = old.alt_archive_carry_head(false);
    assert!(archive.differ.is_none());
    assert!(old.alt_archive_carry_differ(&mut archive, fence));
    assert!(old.alt_archive_carry_rows(&mut archive, fence, 0, usize::MAX));
    let (mut new, outcome) = adopt(&cp, archive);
    assert_eq!(outcome, AltArchiveImport::Exact);
    assert_eq!(everything(&new), everything(&old), "at the handoff");
    for s in [12, 13, 14, 17, 18, 30] {
        let frame = sync_frame(&claude_screen(s, ROWS, COLS));
        old.process(&frame);
        new.process(&frame);
        assert_eq!(everything(&new), everything(&old), "after frame {s}");
    }
    assert!(everything(&new).gaps.is_empty());
}
