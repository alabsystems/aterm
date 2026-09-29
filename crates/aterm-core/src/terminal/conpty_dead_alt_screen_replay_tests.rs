// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! A full-screen app killed under Windows ConPTY, replayed from the reads the
//! `cast` tap recorded on 2026-09-27 (the fixtures' headers say how). The
//! scaled streams in `handler_dec.rs` (`conhost_left_alt_screen_tests`) pin
//! each rule; these pin the whole measured session, byte for byte, and with it
//! what the terminal side can and cannot do about conhost's own screen state
//! (`TerminalHandler::leave_orphaned_alternate_screen` has the account).
use crate::terminal::Terminal;

/// 40 history rows, `less`, `Stop-Process` on it, two commands, a second
/// `less` (on win.ini) and `q`.
const KILLED_THEN_SECOND_PAGER: &str =
    include_str!("testdata/conpty_killed_pager_then_second_pager.reads");

/// The same, with one command right after the kill that writes `ESC[?1049l`
/// to the console: a console CLIENT leaving the dead app's buffer.
const KILLED_THEN_CONSOLE_RESYNC: &str =
    include_str!("testdata/conpty_killed_pager_console_resync.reads");

const PROMPT: &str = "PS C:\\>";
const WIN_INI: &str =
    "PS C:\\> & 'C:\\Program Files\\Git\\usr\\bin\\less.exe' C:\\Windows\\win.ini";
const TYPED_AFTER_THE_KILL: [&str; 5] = [
    "PS C:\\> Write-Output post-kill-A",
    "post-kill-A",
    "PS C:\\> Write-Output post-kill-B",
    "post-kill-B",
    WIN_INI,
];

/// The fixture's reads, one per non-comment line, escapes decoded.
fn reads(fixture: &str) -> std::vec::IntoIter<Vec<u8>> {
    fixture
        .lines()
        .filter(|line| !line.starts_with('#'))
        .map(unescape)
        .collect::<Vec<_>>()
        .into_iter()
}

fn unescape(line: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(line.len());
    let mut bytes = line.bytes();
    while let Some(b) = bytes.next() {
        if b != b'\\' {
            out.push(b);
            continue;
        }
        match bytes.next() {
            Some(b'e') => out.push(0x1b),
            Some(b'a') => out.push(0x07),
            Some(b'b') => out.push(0x08),
            Some(b'r') => out.push(b'\r'),
            Some(b'n') => out.push(b'\n'),
            Some(b'\\') => out.push(b'\\'),
            other => panic!("unknown escape {other:?} in {line:?}"),
        }
    }
    out
}

/// Process reads up to and including the first one that contains `marker`.
fn replay_through(term: &mut Terminal, reads: &mut impl Iterator<Item = Vec<u8>>, marker: &str) {
    for read in reads.by_ref() {
        term.process(&read);
        if read.windows(marker.len()).any(|w| w == marker.as_bytes()) {
            return;
        }
    }
    panic!("no read contains {marker:?}");
}

fn replay_rest(term: &mut Terminal, reads: impl Iterator<Item = Vec<u8>>) {
    for read in reads {
        term.process(&read);
    }
}

/// Every retained row, scrollback first, trailing blanks trimmed.
fn all_rows(term: &Terminal) -> Vec<String> {
    let first = term.grid().oldest_absolute_row();
    let n = term.grid().scrollback_lines() + usize::from(term.grid().rows());
    (first..)
        .take(n)
        .map(|abs| {
            term.abs_row_text(abs)
                .unwrap_or_default()
                .trim_end()
                .to_string()
        })
        .collect()
}

fn screen(term: &Terminal) -> Vec<String> {
    (0..usize::from(term.grid().rows()))
        .map(|r| term.row_text(r).unwrap_or_default().trim_end().to_string())
        .collect()
}

fn count(rows: &[String], want: &str) -> usize {
    rows.iter().filter(|r| *r == want).count()
}

fn history(n: usize) -> String {
    format!("clean history {n}")
}

/// Replay through the shell's prompt on the dead pager's screen — the read
/// whose `133;A` makes the engine leave that screen — and check the recovery.
fn recovered(fixture: &str) -> (Terminal, std::vec::IntoIter<Vec<u8>>) {
    let mut term = Terminal::new(24, 80);
    let mut reads = reads(fixture);
    replay_through(&mut term, &mut reads, "133;D;-1");
    assert!(
        !term.modes().alternate_screen,
        "the prompt left the dead screen"
    );
    // conhost's cursor: its alternate buffer scrolled for the prompt's `\r\n`.
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (23, 8));
    let rows = screen(&term);
    assert_eq!(rows[23], PROMPT);
    assert!(rows[..23].iter().all(String::is_empty), "{rows:?}");
    let all = all_rows(&term);
    for n in 1..=40 {
        assert_eq!(count(&all, &history(n)), 1, "{n}: {all:?}");
    }
    (term, reads)
}

#[test]
fn the_next_pager_after_a_kill_shows_the_consoles_frozen_screen_and_loses_no_row() {
    let (mut term, mut reads) = recovered(KILLED_THEN_SECOND_PAGER);
    // Two commands on conhost's dead buffer, then the second pager's command
    // line, all where conhost put them: the bottom of the screen.
    replay_through(&mut term, &mut reads, "win.ini\x07");
    let rows = screen(&term);
    assert_eq!(rows[18..23], TYPED_AFTER_THE_KILL, "{rows:?}");
    replay_rest(&mut term, reads);
    assert!(!term.modes().alternate_screen);
    let all = all_rows(&term);
    for want in TYPED_AFTER_THE_KILL {
        assert_eq!(count(&all, want), 1, "{want:?}: {all:?}");
    }
    // conhost's `?1049h` replaced the buffer the rows above were printed on,
    // and its `?1049l` went back to the primary buffer it froze when the killed
    // pager entered: its repaint is the screen from before the kill, with the
    // prompt on the row that pager's `?1049h` saved. The terminal cannot keep
    // conhost from drawing it, so the screen is conhost's and the rows it lost
    // are in the scrollback just above.
    let rows = screen(&term);
    for (row, n) in (20..=40).enumerate() {
        assert_eq!(rows[row], history(n));
    }
    assert!(rows[21].starts_with("PS C:\\> & 'C:\\Program Files\\Git\\usr\\bin\\less.exe' "));
    assert_eq!(rows[23], PROMPT);
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (23, 8));
    // So the frozen screen's rows exist twice: in the history the recovery
    // kept, and on conhost's repaint of them.
    assert_eq!(count(&all, &history(40)), 2, "{all:?}");
    assert_eq!(count(&all, &history(1)), 1, "{all:?}");
}

#[test]
fn the_same_reads_without_conhosts_mode_request_lose_the_rows_typed_after_the_kill() {
    // Negative control, so the pass above is not vacuous: drop `?9001h` from
    // the first read and nothing tells the engine a console host sits in
    // between, so the recovery lays the screen out the unix way and conhost's
    // blank repaint ahead of the second pager's `?1049h` erases the rows.
    let mut reads = reads(KILLED_THEN_SECOND_PAGER);
    assert_eq!(
        reads.next().as_deref(),
        Some(&b"\x1b[?9001h\x1b[?1004h"[..])
    );
    let mut term = Terminal::new(24, 80);
    term.process(b"\x1b[?1004h");
    replay_rest(&mut term, reads);
    let all = all_rows(&term);
    for want in TYPED_AFTER_THE_KILL {
        assert_eq!(count(&all, want), 0, "{want:?}: {all:?}");
    }
}

#[test]
fn a_console_client_leaving_the_dead_buffer_puts_the_next_pager_back_in_step() {
    let (mut term, mut reads) = recovered(KILLED_THEN_CONSOLE_RESYNC);
    // `[Console]::Write("$([char]27)[?1049l")`: conhost leaves the dead buffer
    // at once and repaints its frozen primary, the same repaint the second
    // pager's exit brings without it.
    replay_through(&mut term, &mut reads, "clean history 20\x1b[K");
    assert!(!term.modes().alternate_screen);
    let rows = screen(&term);
    for (row, n) in (20..=40).enumerate() {
        assert_eq!(rows[row], history(n), "{rows:?}");
    }
    let resync = "PS C:\\> [Console]::Write(\"$([char]27)[?1049l\")";
    assert_eq!(count(&all_rows(&term), resync), 1);
    // From then on conhost's primary is the buffer the shell prints on, so the
    // second pager's exit repaints the live screen: the commands typed after
    // the kill, the pager's own command line and the new prompt.
    replay_rest(&mut term, reads);
    assert!(!term.modes().alternate_screen);
    let rows = screen(&term);
    assert_eq!(rows[18..23], TYPED_AFTER_THE_KILL, "{rows:?}");
    assert_eq!(rows[23], PROMPT);
    let all = all_rows(&term);
    for want in TYPED_AFTER_THE_KILL.into_iter().chain([resync]) {
        assert_eq!(count(&all, want), 1, "{want:?}: {all:?}");
    }
}
