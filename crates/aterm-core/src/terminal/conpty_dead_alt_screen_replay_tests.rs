// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Replay tests for killed full-screen applications under Windows ConPTY.
use crate::terminal::Terminal;

const KILLED_THEN_SECOND_PAGER: &str =
    include_str!("testdata/conpty_killed_pager_then_second_pager.reads");

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

fn conpty_term() -> Terminal {
    let mut term = Terminal::new(24, 80);
    term.set_win32_input_enabled(true);
    term
}

fn recovered(fixture: &str) -> (Terminal, std::vec::IntoIter<Vec<u8>>) {
    let mut term = conpty_term();
    let mut reads = reads(fixture);
    replay_through(&mut term, &mut reads, "133;D;-1");
    assert!(
        !term.modes().alternate_screen,
        "the prompt left the dead screen"
    );
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
    replay_through(&mut term, &mut reads, "win.ini\x07");
    let rows = screen(&term);
    assert_eq!(rows[18..23], TYPED_AFTER_THE_KILL, "{rows:?}");
    replay_rest(&mut term, reads);
    assert!(!term.modes().alternate_screen);
    let all = all_rows(&term);
    for want in TYPED_AFTER_THE_KILL {
        assert_eq!(count(&all, want), 1, "{want:?}: {all:?}");
    }
    let rows = screen(&term);
    for (row, n) in (20..=40).enumerate() {
        assert_eq!(rows[row], history(n));
    }
    assert!(rows[21].starts_with("PS C:\\> & 'C:\\Program Files\\Git\\usr\\bin\\less.exe' "));
    assert_eq!(rows[23], PROMPT);
    let cursor = term.cursor();
    assert_eq!((cursor.row, cursor.col), (23, 8));
    assert_eq!(count(&all, &history(40)), 2, "{all:?}");
    assert_eq!(count(&all, &history(1)), 1, "{all:?}");
}

#[test]
fn the_same_reads_without_conhosts_mode_request_lose_the_rows_typed_after_the_kill() {
    let mut reads = reads(KILLED_THEN_SECOND_PAGER);
    assert_eq!(
        reads.next().as_deref(),
        Some(&b"\x1b[?9001h\x1b[?1004h"[..])
    );
    let mut term = conpty_term();
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
    replay_through(&mut term, &mut reads, "clean history 20\x1b[K");
    assert!(!term.modes().alternate_screen);
    let rows = screen(&term);
    for (row, n) in (20..=40).enumerate() {
        assert_eq!(rows[row], history(n), "{rows:?}");
    }
    let resync = "PS C:\\> [Console]::Write(\"$([char]27)[?1049l\")";
    assert_eq!(count(&all_rows(&term), resync), 1);
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
