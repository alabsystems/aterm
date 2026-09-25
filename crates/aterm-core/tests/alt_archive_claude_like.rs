// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! The alt-screen scroll-off archive against a SYNTHETIC Claude-Code-shaped byte
//! stream (never a real capture: those carry private content).
//!
//! Claude Code runs on the alternate screen and repaints inside DEC 2026
//! synchronized updates with absolute cursor moves and erases only — no scroll
//! sequence of any kind — so a transcript row that leaves the top of the screen
//! is gone from the grid. This file builds that shape: `?1049h`, then frames of
//! `?2026h ?25l`, the changed rows painted with CUP/VPA+CHA + text + EL (a
//! transcript window shifted by k rows, a ticking spinner row, a fixed composer
//! box and footer), `?25h ?2026l`. It checks the archive holds EXACTLY the rows
//! that scrolled off, that feeding the same bytes in 1..=17-byte chunks (and with
//! an ESU cut in two) changes neither the archive nor the grid, that a frame the
//! sync timeout closes is committed, that main-screen output archives nothing,
//! and that the alt grid itself still keeps zero scrollback.

use std::time::Duration;

use aterm_core::terminal::{
    AltArchiveGap, AltArchiveGapKind, AltArchiveQuery, ClockReading, Terminal,
};

/// Transcript row `i`: blocks of eight — a message, two body lines, a blank, a
/// tool call, a result line that repeats in every block (never unique, so it
/// never votes), a counts line, a blank. Row 0 is not blank, and no two blanks
/// touch.
fn transcript_row(i: usize) -> String {
    match i % 8 {
        0 => format!("⏺ Step {}: reading the parser module and its tests", i / 8),
        1 => format!(
            "  The function at line {} handles the escape state machine.",
            100 + i
        ),
        2 => format!(
            "  It keeps {} bytes of partial state between reads.",
            3 + i % 89
        ),
        4 => format!(
            "⏺ Bash(targo --unverified test -p crate{} --lib)",
            i / 8 % 13
        ),
        5 => "  ⎿  test result: ok".to_string(),
        6 => format!(
            "     {} passed; 0 failed; finished in 0.{:02}s",
            40 + i % 50,
            i % 100
        ),
        _ => String::new(),
    }
}

const SPIN: [&str; 6] = ["✻", "✢", "✳", "✶", "✻", "✽"];

/// The screen layout: `t` transcript rows, the spinner row, a blank, a 3-row
/// composer box, a footer.
struct Layout {
    rows: usize,
    cols: usize,
}

impl Layout {
    fn transcript_rows(&self) -> usize {
        self.rows - 6
    }

    fn screen(&self, start: usize, tick: usize) -> Vec<String> {
        let inner = self.cols.min(80) - 2;
        let mut v: Vec<String> = (start..start + self.transcript_rows())
            .map(transcript_row)
            .collect();
        v.push(format!(
            "{} Crunching… ({}s · ↓ {} tokens · esc to interrupt)",
            SPIN[tick % SPIN.len()],
            tick,
            tick * 37 % 5000
        ));
        v.push(String::new());
        v.push(format!("╭{}╮", "─".repeat(inner)));
        v.push(format!("│ > {:<w$}│", "", w = inner - 3));
        v.push(format!("╰{}╯", "─".repeat(inner)));
        v.push("  ⏵⏵ accept edits on (shift+tab to cycle)".to_string());
        v
    }
}

/// Deterministic step sizes: mostly spinner-only ticks and 1-3 row scrolls, with
/// an occasional 12-row burst (still overlapping).
fn steps(n: usize) -> Vec<usize> {
    let mut x: u32 = 0x9e37_79b9;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            match x % 16 {
                0..=4 => 0,
                5..=9 => 1,
                10..=12 => 2,
                13 | 14 => 3,
                _ => 12,
            }
        })
        .collect()
}

/// Paint `next` over `prev` like a diffing TUI renderer: only changed rows, every
/// seventh frame everything; CUP for most rows, VPA+CHA for some.
fn paint(out: &mut Vec<u8>, prev: Option<&[String]>, next: &[String], full: bool) {
    out.extend_from_slice(b"\x1b[?2026h\x1b[?25l");
    for (r, text) in next.iter().enumerate() {
        if !full && prev.is_some_and(|p| p[r] == *text) {
            continue;
        }
        if r % 3 == 2 {
            out.extend_from_slice(format!("\x1b[{}d\x1b[1G", r + 1).as_bytes());
        } else {
            out.extend_from_slice(format!("\x1b[{};1H", r + 1).as_bytes());
        }
        out.extend_from_slice(text.as_bytes());
        out.extend_from_slice(b"\x1b[K");
    }
    out.extend_from_slice(b"\x1b[?25h\x1b[?2026l");
}

/// The stream plus what it scrolled off: `(bytes, final start, frames)`.
fn claude_like(layout: &Layout, frames: usize, alt: bool) -> (Vec<u8>, usize, usize) {
    let mut out = Vec::new();
    if alt {
        out.extend_from_slice(b"\x1b[?1049h");
    }
    let mut start = 0;
    let mut prev = layout.screen(start, 0);
    paint(&mut out, None, &prev, true);
    for (tick, k) in steps(frames).into_iter().enumerate() {
        start += k;
        let next = layout.screen(start, tick + 1);
        paint(&mut out, Some(&prev), &next, tick % 7 == 6);
        prev = next;
    }
    (out, start, frames + 1)
}

fn fixed_clock() -> ClockReading {
    ClockReading {
        monotonic: aterm_time::Instant::now(),
        wall_ms: None,
    }
}

fn term(layout: &Layout) -> Terminal {
    let mut t = Terminal::new(layout.rows as u16, layout.cols as u16);
    t.set_alt_archive_enabled(true); // regardless of ATERM_ALT_ARCHIVE
    t
}

/// Everything observable about the archive and the grid, for equivalence checks.
#[derive(Debug, PartialEq, Eq)]
struct Observed {
    rows: Vec<String>,
    gaps: Vec<AltArchiveGap>,
    last: u64,
    lost: u64,
    epoch: u32,
    back: usize,
    grid: String,
    cursor: (u16, u16),
}

fn observe(t: &Terminal) -> Observed {
    let a = t.alt_archive();
    let c = t.grid().cursor();
    Observed {
        rows: a.texts(),
        gaps: a.gaps().collect(),
        last: a.last(),
        lost: a.lost(),
        epoch: a.epoch(),
        back: a.back(),
        grid: t.visible_content(),
        cursor: (c.row, c.col),
    }
}

/// Feed `data` in chunks whose sizes cycle through `sizes`, at one fixed clock
/// (so no sync timeout or fallback cadence can depend on real pacing).
fn feed(layout: &Layout, data: &[u8], sizes: &[usize]) -> Terminal {
    let mut t = term(layout);
    let clock = fixed_clock();
    let mut off = 0;
    let mut i = 0;
    while off < data.len() {
        let end = (off + sizes[i % sizes.len()]).min(data.len());
        t.process_at(&data[off..end], clock);
        off = end;
        i += 1;
    }
    t
}

const LAYOUT: Layout = Layout {
    rows: 30,
    cols: 100,
};

#[test]
fn archive_equals_exactly_the_rows_that_scrolled_off() {
    let (data, end, _) = claude_like(&LAYOUT, 240, true);
    assert!(
        end > 3 * LAYOUT.rows,
        "the stream must scroll several screens"
    );
    let mut t = feed(&LAYOUT, &data, &[data.len()]);
    assert!(t.modes().alternate_screen);
    let want: Vec<String> = (0..end).map(transcript_row).collect();
    assert_eq!(t.alt_archive().texts(), want);
    assert_eq!(t.alt_archive().gaps().count(), 0);
    assert_eq!(t.alt_archive().lost(), 0);
    assert_eq!(t.alt_archive().back(), 0);
    // The chrome and the spinner never scrolled, so they are nowhere in it.
    assert!(
        t.alt_archive()
            .texts()
            .iter()
            .all(|r| !r.contains("Crunching") && !r.contains("accept edits") && !r.contains('╭'))
    );
    // The clone-out read hands the same rows back, row m at index first + m.
    let r = t.alt_archive_read(AltArchiveQuery::oldest(0, 2000));
    assert_eq!(r.first, 1);
    assert_eq!(r.last, end as u64);
    assert!(!r.more);
    assert!(r.rows.iter().zip(&want).all(|(a, b)| **a == **b));
    // The alt grid itself still keeps no scrollback: the archive is beside it.
    assert_eq!(t.grid().scrollback_lines(), 0);
    assert_eq!(t.grid().history_line_count(), 0);

    // Leaving flushes the last screen's scrolling region (transcript + the
    // spinner row, which sits above the chrome) and never the composer/footer.
    t.process(b"\x1b[?1049l");
    let mut after = want.clone();
    after.extend((end..end + LAYOUT.transcript_rows()).map(transcript_row));
    let flushed = t.alt_archive().texts();
    assert_eq!(flushed[..after.len()], after[..]);
    assert_eq!(flushed.len(), after.len() + 1);
    assert!(flushed[after.len()].contains("Crunching"));
    let gaps: Vec<_> = t.alt_archive().gaps().collect();
    assert_eq!(gaps.len(), 1);
    assert_eq!(gaps[0].kind, AltArchiveGapKind::Leave);
    assert!(
        t.alt_archive()
            .texts()
            .iter()
            .all(|r| !r.contains("accept edits"))
    );
}

#[test]
fn chunked_feeds_give_an_identical_archive_and_grid() {
    let (data, end, _) = claude_like(&LAYOUT, 90, true);
    let whole = observe(&feed(&LAYOUT, &data, &[data.len()]));
    assert_eq!(whole.rows.len(), end);
    for size in 1..=17 {
        let got = observe(&feed(&LAYOUT, &data, &[size]));
        assert_eq!(got, whole, "chunk size {size}");
    }
    let cycling: Vec<usize> = (1..=17).collect();
    assert_eq!(
        observe(&feed(&LAYOUT, &data, &cycling)),
        whole,
        "1..=17 cycling"
    );
}

#[test]
fn an_esu_cut_in_two_is_still_a_frame_boundary() {
    let (data, end, _) = claude_like(&LAYOUT, 30, true);
    let whole = observe(&feed(&LAYOUT, &data, &[data.len()]));
    let esu = b"\x1b[?2026l";
    let positions: Vec<usize> = data
        .windows(esu.len())
        .enumerate()
        .filter(|(_, w)| *w == esu)
        .map(|(i, _)| i)
        .collect();
    assert!(positions.len() > 10);
    // Cut inside the 3rd, 7th and 11th ESU at every interior offset.
    for &p in [positions[2], positions[6], positions[10]].iter() {
        for inner in 1..esu.len() {
            let cut = p + inner;
            let mut t = term(&LAYOUT);
            let clock = fixed_clock();
            t.process_at(&data[..cut], clock);
            t.process_at(&data[cut..], clock);
            assert_eq!(observe(&t), whole, "ESU at {p} cut after {inner} bytes");
        }
    }
    assert_eq!(whole.rows.len(), end);
}

#[test]
fn a_frame_closed_by_the_sync_timeout_is_committed() {
    let layout = &LAYOUT;
    let mut t = term(layout);
    let t0 = fixed_clock();
    let later = ClockReading {
        monotonic: t0
            .monotonic
            .checked_add(Duration::from_secs(5))
            .expect("clock"),
        wall_ms: None,
    };
    let mut first = b"\x1b[?1049h".to_vec();
    let s0 = layout.screen(0, 0);
    paint(&mut first, None, &s0, true);
    t.process_at(&first, t0);
    // Open a frame that scrolls 3 rows and never close it.
    let s3 = layout.screen(3, 1);
    let mut open = Vec::new();
    paint(&mut open, Some(&s0), &s3, false);
    open.truncate(open.len() - b"\x1b[?25h\x1b[?2026l".len());
    t.process_at(&open, t0);
    assert!(t.modes().synchronized_output());
    assert!(t.alt_archive().is_empty(), "an open frame is not committed");
    // Nothing more arrives; the next batch finds the window expired.
    t.process_at(b"", later);
    assert!(
        !t.modes().synchronized_output(),
        "the timeout force-closed it"
    );
    assert_eq!(
        t.alt_archive().texts(),
        (0..3).map(transcript_row).collect::<Vec<_>>()
    );
}

#[test]
fn a_main_screen_stream_leaves_the_archive_empty() {
    let (data, _, _) = claude_like(&LAYOUT, 60, false);
    let t = feed(&LAYOUT, &data, &[data.len()]);
    assert!(!t.modes().alternate_screen);
    assert!(t.alt_archive().is_empty());
    assert_eq!(t.alt_archive().last(), 0);
    assert_eq!(t.alt_archive().gaps().count(), 0);
}

#[test]
fn the_archive_off_changes_nothing_else() {
    let (data, _, _) = claude_like(&LAYOUT, 60, true);
    let on = feed(&LAYOUT, &data, &[7]);
    let mut off = Terminal::new(LAYOUT.rows as u16, LAYOUT.cols as u16);
    off.set_alt_archive_enabled(false);
    let clock = fixed_clock();
    for chunk in data.chunks(7) {
        off.process_at(chunk, clock);
    }
    assert!(off.alt_archive().is_empty());
    assert_eq!(off.visible_content(), on.visible_content());
    assert_eq!(off.grid().cursor(), on.grid().cursor());
}
