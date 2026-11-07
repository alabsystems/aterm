// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Coalesced line feeds (a run of text lines taken whole, its line feeds at the bottom of a
//! history-free scroll region one shift of the region, the lines that scroll out never written;
//! at the bottom of a full screen each line feed and line taken in turn, the line feeds' damage
//! and scroll delta settled once) against the reference path, which takes every print and line
//! feed one at a time
//! (`Terminal::set_reference_line_runs`). Both engines read the same chunks of generated streams:
//! runs of short and long lines with LF, CR LF and blank-line endings, line feeds at random
//! region margins on both screens, lines that fill or overflow the row, wide glyphs, extras
//! (hyperlinks, truecolor pens and backgrounds an erase fills with), IND, RI, NEL, IL, DL, SU, SD,
//! REP, the modes that rule coalescing out, DEC 2026 frames and a selection the output may hit.
//! After every read they must agree on everything a reader sees.

use aterm_core::grid::cell::Cell;
use aterm_core::grid::extra::CellExtra;
use aterm_core::scrollback::Scrollback;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{AltArchiveQuery, ClockReading, ContentScrollState, Terminal};
use std::fmt::Write as _;
use std::time::Duration;

/// A seeded xorshift stream.
struct Rng(u64);

impl Rng {
    fn below(&mut self, n: usize) -> usize {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        usize::try_from(self.0 % n.max(1) as u64).unwrap_or(0)
    }
}

const CONTROLS: &[&str] = &[
    "\x1b[r",
    "\x1b[1m",
    "\x1b[0m",
    "\x1b[44m",
    "\x1b[48;2;10;20;30m",
    "\x1b[38;2;1;2;3m",
    "\x1b[4:3m\x1b[58;5;3m",
    "\x1b]8;;http://x/\x1b\\",
    "\x1b]8;;\x1b\\",
    "\x1b]133;A\x07",
    "\x1b]133;C\x07",
    "\x1b[4h",
    "\x1b[4l",
    "\x1b[?7l",
    "\x1b[?7h",
    "\x1b[20h",
    "\x1b[20l",
    "\x1b[?69h\x1b[3;9s",
    "\x1b[?69l",
    "\x1b#6",
    "\x1b#5",
    "\x1b(0",
    "\x1b(B",
    "\x0e",
    "\x0f",
    "\x1bN",
    "\x1b[K",
    "\x1b[2J",
    "\x1b[2L",
    "\x1b[2M",
    "\x1b[S",
    "\x1b[T",
    "\x1bD",
    "\x1bM",
    "\x1bE",
    "\x1b[3b",
    "\x1b[6n",
    "\x1b[?1049h",
    "\x1b[?1049l",
    "\x1b[?2026h",
    "\x1b[?2026l",
    "\x1b[?2l\x1bY%(",
    "\x1b<",
    "\t",
    "\x08",
    "\u{4e00}\u{4e01}",
    "e\u{301}",
];

/// `len` printable ASCII bytes: random letters, or long same-letter stretches (the bulk writer's
/// cell-run path takes those in lines over 64 bytes).
fn text(rng: &mut Rng, len: usize) -> String {
    let mut out = String::new();
    while out.len() < len {
        let letter = char::from(b' ' + u8::try_from(rng.below(95)).unwrap_or(0));
        let repeat = if rng.below(4) == 0 {
            1 + rng.below(12)
        } else {
            1
        };
        for _ in 0..repeat.min(len - out.len()) {
            out.push(letter);
        }
    }
    out
}

/// A run of text lines: `y\n` floods, lines of every length up to past the row's end, and line
/// ends of LF, CR LF, blank lines, and CRs before more text.
fn lines(rng: &mut Rng, rows: usize, cols: usize) -> String {
    let mut out = String::new();
    match rng.below(6) {
        0 => {
            for _ in 0..1 + rng.below(rows * 4) {
                out.push_str("y\n");
            }
        }
        1 => {
            let len = 65 + rng.below(60);
            let long = text(rng, len);
            out.push_str(&long);
            out.push('\n');
        }
        _ => {
            for _ in 0..1 + rng.below(rows * 2) {
                let len = rng.below(cols + 3);
                out.push_str(&text(rng, len));
                out.push_str(match rng.below(8) {
                    0 => "\r\n",
                    1 => "\n\n\n",
                    2 => "\r",
                    3 => "\r\r\n",
                    _ => "\n",
                });
            }
        }
    }
    out
}

fn stream(rng: &mut Rng, rows: usize, cols: usize) -> Vec<u8> {
    let mut out = String::new();
    if rng.below(2) == 0 {
        out.push_str("\x1b[?1049h");
    }
    for _ in 0..4 + rng.below(24) {
        match rng.below(10) {
            0..=3 => out.push_str(&lines(rng, rows, cols)),
            4 | 5 => {
                let top = 1 + rng.below(rows);
                let bottom = top + rng.below(rows + 1 - top);
                let _ = write!(
                    out,
                    "\x1b[{top};{bottom}r\x1b[{bottom};{}H",
                    1 + rng.below(cols)
                );
            }
            6 => {
                let _ = write!(out, "\x1b[{};{}H", 1 + rng.below(rows), 1 + rng.below(cols));
            }
            _ => out.push_str(CONTROLS[rng.below(CONTROLS.len())]),
        }
    }
    out.into_bytes()
}

/// Read boundaries: random cuts, so a run of lines arrives whole, split, or a byte at a time.
fn reads<'a>(rng: &mut Rng, bytes: &'a [u8]) -> Vec<&'a [u8]> {
    let mut cuts: Vec<usize> = (0..rng.below(6))
        .map(|_| rng.below(bytes.len() + 1))
        .collect();
    cuts.push(bytes.len());
    cuts.sort_unstable();
    cuts.dedup();
    let mut from = 0;
    cuts.iter()
        .map(|&cut| {
            let read = &bytes[from..cut];
            from = cut;
            read
        })
        .collect()
}

/// One cell as a reader sees it: the cell, its extras where it claims them or a reader finds
/// them without the claim, its complex glyph, and its RGB colours where its colours say RGB.
type CellView = (
    Cell,
    Option<CellExtra>,
    String,
    Option<[u8; 3]>,
    Option<[u8; 3]>,
);

/// One row as a reader sees it: its length, wrap flag, line size and render damage.
type RowView = (
    u16,
    bool,
    aterm_core::grid::row::LineSize,
    Option<(u16, u16)>,
);

/// Everything a read can change that a reader sees: the active grid's cursor, cells and rows,
/// the print anchor, the replies, the selection, the content-motion record, whether the content
/// generation moved (asked only of a read that ends on the screen it started on: the two screens
/// count apart) and whether the damage is full.
#[derive(Debug, PartialEq)]
struct Screen {
    cursor: (u16, u16, bool),
    print_anchor: Option<(u16, u16, u64)>,
    replies: Vec<u8>,
    selection: aterm_core::selection::TextSelection,
    motion: ContentScrollState,
    changed: Option<bool>,
    full_damage: bool,
    rows: Vec<RowView>,
    cells: Vec<CellView>,
}

fn seen_unclaimed(extra: &CellExtra) -> bool {
    !extra.combining().is_empty()
        || extra.complex_char().is_some()
        || extra.hyperlink().is_some()
        || extra.underline_color().is_some()
        || extra.underline_color_index().is_some()
        || !extra.images().is_empty()
}

fn screen(t: &mut Terminal, replies: &mut Vec<u8>, before: (bool, u64)) -> Screen {
    if let Some(r) = t.take_response() {
        replies.extend_from_slice(&r);
    }
    let selection = t.text_selection().clone();
    let motion = t.content_scroll_state();
    let print_anchor = t.print_anchor();
    let g = t.grid();
    let c = g.cursor();
    let mut rows = Vec::new();
    let mut cells = Vec::new();
    for row in 0..g.rows() {
        if let Some(r) = g.row(row) {
            rows.push((
                r.len(),
                r.is_wrapped(),
                r.line_size(),
                g.damage().row_damage_bounds(row, g.cols()),
            ));
        }
        for col in 0..g.cols() {
            let Some(&cell) = g.cell(row, col) else {
                continue;
            };
            let mut glyph = String::new();
            if cell.is_complex() {
                g.push_complex_char_at(row, col, &mut glyph);
            }
            cells.push((
                cell,
                g.cell_extra(row, col)
                    .filter(|extra| cell.has_extras() || seen_unclaimed(extra))
                    .cloned()
                    .filter(CellExtra::has_data),
                glyph,
                cell.fg_needs_overflow()
                    .then(|| g.fg_rgb_at(row, col))
                    .flatten(),
                cell.bg_needs_overflow()
                    .then(|| g.bg_rgb_at(row, col))
                    .flatten(),
            ));
        }
    }
    Screen {
        cursor: (c.row, c.col, g.pending_wrap()),
        print_anchor,
        replies: replies.clone(),
        selection,
        motion,
        changed: (t.is_alternate_screen() == before.0).then(|| g.content_gen() != before.1),
        full_damage: g.damage().is_full(),
        rows,
        cells,
    }
}

/// What a case leaves behind it: every history line and the alternate-screen archive.
fn behind(t: &Terminal) -> String {
    let g = t.grid();
    let mut s = String::new();
    for idx in 0..g.history_line_count() {
        let _ = writeln!(s, "history {idx}: {:?}", g.get_history_line(idx));
    }
    let _ = writeln!(
        s,
        "archive {:?}",
        t.alt_archive_read(AltArchiveQuery::oldest(0, 100_000))
    );
    s
}

/// One of the engines a case runs on: shape 0 the default 10 000-line ring; 1 a ring of 4 lines
/// over a tiered store that the parse thread drains, 2 the same with compression handed off (the
/// app's live shape), and 3 a ring of 6 lines over a store that keeps none, so the ring fills
/// within a case and every line feed at the bottom reuses its oldest row.
fn engine(rows: u16, cols: u16, reference: bool, shape: usize) -> Terminal {
    let mut t = match shape {
        0 => Terminal::new(rows, cols),
        3 => Terminal::with_scrollback(rows, cols, 6, Scrollback::new(4, 64, 10_000_000)),
        _ => Terminal::with_scrollback(rows, cols, 4, Scrollback::new(4, 64, 10_000_000)),
    };
    if shape == 3 {
        t.grid_mut().set_scrollback_line_limit(Some(0));
    }
    t.set_compress_offload_active(shape == 2);
    t.set_alt_archive_enabled(true);
    t.set_reference_line_runs(reference);
    t
}

/// Select from the start of row `top` to column `col` of row `bottom`, alike on each engine.
fn select(t: &mut Terminal, top: i32, bottom: i32, col: u16) {
    let sel = t.text_selection_mut();
    sel.start_selection(top, 0, SelectionSide::Left, SelectionType::Simple);
    sel.update_selection(bottom, col, SelectionSide::Right);
    sel.complete_selection();
}

#[expect(
    clippy::disallowed_methods,
    reason = "test code: the one instant a test mints and feeds in"
)]
fn clock_origin() -> aterm_time::Instant {
    aterm_time::Instant::now()
}

fn at(origin: aterm_time::Instant, elapsed: Duration) -> ClockReading {
    ClockReading {
        monotonic: origin.checked_add(elapsed).expect("clock"),
        wall_ms: None,
    }
}

/// The first generated case on which the coalescing engine leaves anything other than the
/// reference engine does, after any read; else how many cases ran.
fn first_divergence(cases: u64) -> Result<u64, String> {
    let origin = clock_origin();
    for seed in 1..=cases {
        let mut rng = Rng(0x9E37_79B9_7F4A_7C15 ^ seed.wrapping_mul(0x2545_F491_4F6C_DD1D));
        let rows = 3 + u16::try_from(rng.below(10)).unwrap_or(0);
        let cols = 4 + u16::try_from(rng.below(24)).unwrap_or(0);
        let bytes = stream(&mut rng, usize::from(rows), usize::from(cols));
        let reads = reads(&mut rng, &bytes);
        let shape = rng.below(4);
        let mut fast = engine(rows, cols, false, shape);
        let mut reference = engine(rows, cols, true, shape);
        let (mut fast_replies, mut reference_replies) = (Vec::new(), Vec::new());
        let mut elapsed = Duration::ZERO;
        for (n, read) in reads.iter().enumerate() {
            elapsed += Duration::from_millis(20 * u64::from(rng.below(2) == 0));
            let clock = at(origin, elapsed);
            if rng.below(3) == 0 {
                let top = i32::try_from(rng.below(usize::from(rows))).unwrap_or(0);
                let bottom = top + i32::try_from(rng.below(3)).unwrap_or(0);
                let col = u16::try_from(rng.below(usize::from(cols))).unwrap_or(0);
                select(&mut fast, top, bottom, col);
                select(&mut reference, top, bottom, col);
            }
            if rng.below(4) == 0 {
                fast.grid_mut().clear_damage();
                reference.grid_mut().clear_damage();
            }
            let fast_before = (fast.is_alternate_screen(), fast.grid().content_gen());
            let reference_before = (
                reference.is_alternate_screen(),
                reference.grid().content_gen(),
            );
            fast.process_at(read, clock);
            reference.process_at(read, clock);
            let got = screen(&mut fast, &mut fast_replies, fast_before);
            let want = screen(&mut reference, &mut reference_replies, reference_before);
            if got != want {
                return Err(format!(
                    "seed {seed}, {rows}x{cols}, shape {shape}, after read {n} of {:?}\nfast:\n{got:#?}\nreference:\n{want:#?}",
                    reads
                        .iter()
                        .map(|r| String::from_utf8_lossy(r))
                        .collect::<Vec<_>>()
                ));
            }
        }
        let (got, want) = (behind(&fast), behind(&reference));
        if got != want {
            return Err(format!(
                "seed {seed}, {rows}x{cols}, shape {shape}\nfast:\n{got}\nreference:\n{want}"
            ));
        }
    }
    Ok(cases)
}

#[test]
fn coalesced_line_feeds_leave_what_the_reference_path_leaves() {
    let _ = aterm_grid::test_counters::take_coalesced_shifts();
    let _ = aterm_grid::test_counters::take_screen_settles();
    let _ = aterm_grid::test_counters::take_screen_text_runs();
    if let Err(divergence) = first_divergence(4000) {
        panic!("the coalesced line-run path diverges from the reference: {divergence}");
    }
    let [shifts, multi, deep] = aterm_grid::test_counters::take_coalesced_shifts();
    assert!(
        shifts > 4000 && multi > 2000 && deep > 500,
        "the streams coalesced too little to hold the path to anything: {shifts} shifts, \
         {multi} of more than one line feed, {deep} past the region's height"
    );
    let [settles, runs] = aterm_grid::test_counters::take_screen_settles();
    assert!(
        settles > 4000 && runs > 3000,
        "the streams fed too few lines at a full screen's bottom: {settles} settles, {runs} of \
         more than one line feed"
    );
    let [text_runs, text_lines] = aterm_grid::test_counters::take_screen_text_runs();
    assert!(
        text_runs > 1000 && text_lines > 5000,
        "the full ring's line feeds took too few text lines at a full screen's bottom: \
         {text_runs} runs, {text_lines} lines"
    );
}

#[test]
fn the_differential_catches_a_coalesced_shift_one_line_short() {
    let caught = aterm_grid::test_counters::with_short_line_run_shift(|| first_divergence(500));
    assert!(
        caught.is_err(),
        "an engine whose coalesced shift falls one line short of its line feeds left what the reference leaves"
    );
}

#[test]
fn the_differential_catches_screen_line_feeds_settled_one_short() {
    let caught = aterm_grid::test_counters::with_short_screen_settle(|| first_divergence(500));
    assert!(
        caught.is_err(),
        "an engine that settles its screen line feeds one short of their count left what the reference leaves"
    );
}

#[test]
fn the_differential_catches_a_screen_text_line_one_slot_up() {
    let caught = aterm_grid::test_counters::with_screen_text_slot_one_up(|| first_divergence(1000));
    assert!(
        caught.is_err(),
        "an engine that writes its screen text lines one ring slot above their row left what the reference leaves"
    );
}
