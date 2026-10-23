// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The alt blob of a checkpoint taken on the alternate screen has exactly `rows` records.

use aterm_core::scrollback::{deserialize_lines_strict, serialize_lines};
use aterm_core::terminal::{Terminal, TerminalCheckpoint};

const ROWS: u16 = 49;
const COLS: u16 = 131;
const CARRY: usize = 256;

fn is_canonical(bytes: &[u8], expected: usize, cols: u16) -> bool {
    let content_cap = usize::from(cols).saturating_mul(256);
    let record_cap = 16usize
        .saturating_mul(1024)
        .saturating_add(usize::from(cols).saturating_mul(512));
    deserialize_lines_strict(bytes, expected, usize::from(cols), content_cap, record_cap)
        .is_some_and(|lines| lines.len() == expected && serialize_lines(&lines) == bytes)
}

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

#[test]
fn the_alt_blob_is_exactly_rows_records() {
    for (name, bytes) in [
        ("1049 up", &b"\x1b[?1049ha full-screen app draws here"[..]),
        ("1047 up", b"\x1b[?1047ha full-screen app draws here"),
        (
            "after a 1047 exit",
            b"\x1b[?1047ha full-screen app draws here\x1b[?1047l",
        ),
    ] {
        let mut t = primary_with_history();
        t.process(bytes);
        let cp = carry(&t);
        let alt = cp.alt_grid.as_ref().expect("a parked alt_grid");
        assert!(
            is_canonical(alt, usize::from(cp.rows), cp.cols),
            "{name}: the alt blob must hold exactly `rows` records"
        );
    }
}

#[test]
fn the_main_blob_is_rows_plus_its_declared_history() {
    let cp = carry(&primary_with_history());
    let expected = usize::from(cp.rows) + cp.history_lines as usize;

    assert!(
        is_canonical(&cp.grid, expected, cp.cols),
        "the main blob must hold exactly `rows + history_lines` records"
    );
    assert!(
        cp.history_lines > 0 && cp.history_lines as usize <= CARRY,
        "the primary carries history, bounded by the carry target"
    );
}

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

fn sync_frame(screen: &[String]) -> Vec<u8> {
    let mut v = b"\x1b[?2026h\x1b[?25l".to_vec();
    for (r, text) in screen.iter().enumerate() {
        v.extend_from_slice(format!("\x1b[{};1H\x1b[2K{text}", r + 1).as_bytes());
    }
    v.extend_from_slice(b"\x1b[?25h\x1b[?2026l");
    v
}

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

#[test]
fn an_adopted_composer_is_never_archived_by_the_first_resize() {
    let transcript = |r: std::ops::Range<usize>| -> Vec<String> {
        r.map(|i| format!("⏺ transcript row {i:04} with words"))
            .collect()
    };
    let t_old = usize::from(ROWS) - 5; // transcript rows 3..3 + t_old are shown
    let cases = [
        ((ROWS - 1, COLS), 4, transcript(3..4)),
        ((ROWS + 1, COLS), 2, Vec::new()),
        ((ROWS, COLS - 11), 5, transcript(3..3 + t_old)),
    ];
    for ((rows, cols), first, want) in cases {
        let mut t = adopted_after(3);
        t.process(&sync_frame(&claude_screen(3, ROWS, COLS)));
        assert!(t.alt_archive().is_empty(), "{rows}x{cols}: a repaint");
        t.resize(rows, cols);
        t.process(&sync_frame(&claude_screen(first, rows, cols)));
        assert_eq!(chrome_rows(&t), Vec::<String>::new(), "{rows}x{cols}");
        assert_eq!(t.alt_archive().texts(), want, "{rows}x{cols}");
    }
}

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

use aterm_core::terminal::{AltArchiveCarry, AltArchiveGapKind, AltArchiveImport, AltArchiveQuery};

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

#[test]
fn a_carried_archive_goes_on_exactly_as_the_old_engine_would() {
    let (mut old, cp, archive) = old_side(12, true);
    let (mut new, outcome) = adopt(&cp, archive);
    assert_eq!(outcome, AltArchiveImport::Exact);
    assert_eq!(everything(&new), everything(&old), "at the handoff");
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

#[test]
fn an_adopting_engine_with_the_archive_off_drops_the_carry() {
    let (_old, cp, archive) = old_side(12, true);
    let mut new = Terminal::new(ROWS, COLS);
    new.set_alt_archive_enabled(false);
    new.restore_checkpoint(&cp);
    assert_eq!(new.alt_archive_import(archive), AltArchiveImport::Refused);
    assert!(new.alt_archive().is_empty());
}

#[test]
fn a_tail_carry_recognizes_a_scroll_back_past_its_first_row() {
    let (mut old, cp, _) = old_side(300, false);
    let last = old.alt_archive().last();
    let (fence, mut archive) = old.alt_archive_carry_head(true);
    assert!(old.alt_archive_carry_rows(&mut archive, fence, last - 29, usize::MAX));
    assert!(archive.first < last - 29, "reaches past the host's tail");
    let (mut new, outcome) = adopt(&cp, archive);
    assert_eq!(outcome, AltArchiveImport::Exact);
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
