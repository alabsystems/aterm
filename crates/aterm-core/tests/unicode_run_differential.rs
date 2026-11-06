// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The fast print path for text in other scripts (multi-byte runs that join the ASCII between
//! their characters, narrow glyphs written to the grid a row at a time) against the reference
//! path it replaced (`Terminal::set_reference_print_path`), read for read on the same chunks of
//! generated streams: words in several scripts, wide glyphs and emoji, marks (a lone one at a
//! row start among them), ZWJ and VS16, ASCII gaps either side of the longest the parser joins,
//! scroll regions and DECSLRM margins at random, line-feed runs into history, hyperlinks,
//! underline and truecolor pens (cell extras), charsets, insert and no-wrap modes, double-width
//! rows, REP, the alternate screen and its archive, DEC 2026 frames and a selection the output
//! may hit.

use aterm_core::config::TerminalConfig;
use aterm_core::grid::cell::Cell;
use aterm_core::grid::extra::CellExtra;
use aterm_core::selection::{SelectionSide, SelectionType};
use aterm_core::terminal::{AltArchiveQuery, ClockReading, Terminal};
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

const TEXT: &[&str] = &[
    "\u{411}\u{44b}\u{441}\u{442}\u{440}\u{430}\u{44f} \u{43a}\u{43e}\u{440}\u{438}\u{447}\u{43d}\u{435}\u{432}\u{430}\u{44f} \u{43b}\u{438}\u{441}\u{430}",
    "\u{397} \u{3b3}\u{3c1}\u{3ae}\u{3b3}\u{3bf}\u{3c1}\u{3b7} \u{3ba}\u{3b1}\u{3c6}\u{3ad}",
    "\u{3c4}\u{3bf} \u{3c4}\u{3b5}\u{3bc}\u{3c0}\u{3ad}\u{3bb}\u{3b9}\u{3ba}\u{3bf}",
    "the quick \u{5feb}\u{901f}\u{7684} fox",
    "\u{901f}\u{3044}\u{8336}\u{8272}\u{306e} 0123",
    "\u{bd84}\u{b978} \u{ac08}\u{c0c9} \u{c5ec}\u{c6b0}",
    "caf\u{e9} na\u{ef}ve \u{e0} \u{20ac}5",
    "e\u{301} a\u{300}\u{301}",
    "\u{1f600} \u{1f44d}\u{1f3fd} ok",
    "\u{1f468}\u{200d}\u{1f469} x",
    "a\u{200d}b c",
    "\u{2764}\u{fe0f} \u{231a}\u{fe0e} y",
    "\u{1f1fa}\u{1f1f8} us",
    "\u{85}\u{a0}\u{ff} \u{2500}\u{2502}",
    "\u{430}\u{431}\u{432}\u{433}\u{434}\u{435}\u{436}\u{437}\u{438}\u{439}\u{43a}\u{43b}\u{43c}\u{43d}\u{43e}\u{43f} \u{440}\u{441}\u{442}\u{443}\u{444}\u{445}\u{446}\u{447}\u{448}\u{449}",
    "ascii only words",
    " ",
    "x",
    // A mark that a row start leaves no cell before: it lands on the cell under the cursor,
    // which the ASCII the run joins after it overwrites, after a C1 control decoded from UTF-8
    // too (which lands nowhere).
    "\u{301}ab \u{e9}",
    "\u{301}\u{9b}ab \u{e9}",
    "\u{9b}2J\u{e9}",
    "ok\u{85}\u{e9}",
    "\u{3099}xy \u{3042}",
    "\u{e0100}z \u{1f600}",
    "\u{fe0f}ok \u{2764}",
    // Gaps of ASCII between multi-byte characters, short and past the longest the parser joins.
    "\u{3b1} 12345678901234567890123456789 \u{3b2}",
    "\u{3b1} 123456789012345678901234567890 \u{3b2}",
    "\u{3b1} 1234567890123456789012345678901 \u{3b2}",
    "\u{2014}a long stretch of English prose between two typographic dashes, past any join\u{2014}",
];

const CONTROLS: &[&str] = &[
    "\r\n",
    "\n\n\n",
    "\r",
    "\x08",
    "\t",
    "\x1b[2;4r",
    "\x1b[r",
    "\x1b[?69h\x1b[3;9s",
    "\x1b[?69l",
    "\x1b[1;1H",
    "\x1b[3;5H",
    "\x1b[2;40H",
    "\x1b[4G",
    "\x1b[1m",
    "\x1b[0m",
    "\x1b[7m",
    "\x1b[38;2;1;2;3m",
    "\x1b[4:3m\x1b[58;5;3m",
    "\x1b]8;;http://x/\x1b\\",
    "\x1b]8;;\x1b\\",
    "\x1b[4h",
    "\x1b[4l",
    "\x1b[?7l",
    "\x1b[?7h",
    "\x1b(0",
    "\x1b(B",
    "\x0e",
    "\x0f",
    "\x1bN",
    "\x1b#6",
    "\x1b#5",
    "\x1b[K",
    "\x1b[2J",
    "\x1b[2@",
    "\x1b[2P",
    "\x1b[3X",
    "\x1b[3b",
    "\x1bM",
    "\x1bD",
    "\x1bE",
    "\x1b[?1049h",
    "\x1b[?1049l",
    "\x1b[?2026h",
    "\x1b[?2026l",
    "\x1b[6n",
    "\x1b[?2l\x1bY%(",
    "\x1b<",
];

fn stream(rng: &mut Rng) -> Vec<u8> {
    let mut out = Vec::new();
    for _ in 0..4 + rng.below(28) {
        match rng.below(20) {
            0..=11 => out.extend_from_slice(TEXT[rng.below(TEXT.len())].as_bytes()),
            12 => {
                // A scroll region, DECSLRM margins or a cursor position anywhere on (or past)
                // the grid.
                let (a, b) = (1 + rng.below(10), 1 + rng.below(26));
                let token = match rng.below(3) {
                    0 => format!("\x1b[{a};{}r", a + rng.below(8)),
                    1 => format!("\x1b[?69h\x1b[{};{b}s", 1 + rng.below(b)),
                    _ => format!("\x1b[{a};{b}H"),
                };
                out.extend_from_slice(token.as_bytes());
            }
            _ => out.extend_from_slice(CONTROLS[rng.below(CONTROLS.len())].as_bytes()),
        }
    }
    out
}

/// Read boundaries: random cuts, and cuts inside multi-byte sequences.
fn reads<'a>(rng: &mut Rng, bytes: &'a [u8]) -> Vec<&'a [u8]> {
    let inside: Vec<usize> = (1..bytes.len())
        .filter(|&i| (bytes[i] & 0xc0) == 0x80)
        .collect();
    let mut cuts: Vec<usize> = (0..rng.below(5))
        .map(|_| rng.below(bytes.len() + 1))
        .collect();
    for _ in 0..rng.below(4) {
        if !inside.is_empty() {
            cuts.push(inside[rng.below(inside.len())]);
        }
    }
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

/// One cell as a reader sees it: the cell; its extras where the cell claims them or they hold
/// what a reader finds without the claim (marks, a complex glyph, a hyperlink, an underline
/// color, an image: `chunked_process_equivalence`'s orphan check); its complex glyph; and its
/// RGB colors, in the ring or its extras, only where the cell's colors say RGB, as the grid's
/// readers ask for them. (RGB left behind a cell that no longer says RGB is no reader's, and
/// the writers already differ on clearing it: `write_char` does and the bulk writers do not,
/// so a run a read boundary cuts in two leaves it otherwise than the whole run does.)
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

/// Everything the print path can change that a reader sees after a read: the active grid's
/// cells, rows (length, wrap, line size, render damage), cursor and pending wrap; the replies;
/// the selection. Compared by value, formatted only for a failure.
#[derive(Debug, PartialEq)]
struct Screen {
    cursor: (u16, u16, bool),
    replies: Vec<u8>,
    selection: aterm_core::selection::TextSelection,
    rows: Vec<RowView>,
    cells: Vec<CellView>,
}

/// `extra` without the RGB colors `cell`'s colors do not say it has.
fn as_read(cell: Cell, extra: &CellExtra) -> CellExtra {
    let mut extra = extra.clone();
    if !cell.fg_needs_overflow() {
        extra.set_fg_rgb(Some([0; 3]));
        extra.set_fg_rgb(None);
    }
    if !cell.bg_needs_overflow() {
        extra.set_bg_rgb(Some([0; 3]));
        extra.set_bg_rgb(None);
    }
    extra
}

/// Whether a reader finds `extra` though its cell does not claim it.
fn seen_unclaimed(extra: &CellExtra) -> bool {
    !extra.combining().is_empty()
        || extra.complex_char().is_some()
        || extra.hyperlink().is_some()
        || extra.underline_color().is_some()
        || extra.underline_color_index().is_some()
        || !extra.images().is_empty()
}

fn screen(t: &mut Terminal, replies: &mut Vec<u8>) -> Screen {
    if let Some(r) = t.take_response() {
        replies.extend_from_slice(&r);
    }
    let selection = t.text_selection().clone();
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
                    .map(|extra| as_read(cell, extra))
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
        replies: replies.clone(),
        selection,
        rows,
        cells,
    }
}

/// What a case leaves behind it: every history line (frozen once pushed, so each line the
/// print path wrote that scrolled away) and the alternate-screen archive.
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

fn engine(rows: u16, cols: u16, cjk: bool, reference: bool) -> Terminal {
    let mut t = Terminal::new(rows, cols);
    if cjk {
        t.apply_config(&TerminalConfig {
            ambiguous_width_double: true,
            ..TerminalConfig::default()
        });
    }
    t.set_alt_archive_enabled(true);
    t.set_reference_print_path(reference);
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

/// The clock both engines read a chunk at: the alternate-screen archive commits a frame by the
/// time since its last, so the engines share every reading.
fn at(origin: aterm_time::Instant, elapsed: Duration) -> ClockReading {
    ClockReading {
        monotonic: origin.checked_add(elapsed).expect("clock"),
        wall_ms: None,
    }
}

/// How the engine under test reads one chunk.
type Feed = fn(&mut Terminal, &[u8], ClockReading);

fn whole(t: &mut Terminal, read: &[u8], clock: ClockReading) {
    t.process_at(read, clock);
}

/// The planted decode bug at a split boundary, for the negative control: a read's trailing
/// sequence that the boundary cut is lost instead of carried into the next read.
fn planted_split_bug(t: &mut Terminal, read: &[u8], clock: ClockReading) {
    let tail = read
        .iter()
        .rev()
        .take(3)
        .position(|&b| (0xc2..=0xf4).contains(&b))
        .filter(|&back| {
            let lead = read[read.len() - 1 - back];
            let need = if lead >= 0xf0 {
                3
            } else if lead >= 0xe0 {
                2
            } else {
                1
            };
            back < need
        })
        .map_or(0, |back| back + 1);
    t.process_at(&read[..read.len() - tail], clock);
}

/// The first generated case on which `feed` leaves an engine other than the reference path
/// does, after any read; else how many cases ran.
fn first_divergence(feed: Feed, cases: u64) -> Result<u64, String> {
    let origin = clock_origin();
    for seed in 1..=cases {
        let mut rng = Rng(0x2545_F491_4F6C_DD1D ^ seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let rows = 3 + u16::try_from(rng.below(6)).unwrap_or(0);
        let cols = 4 + u16::try_from(rng.below(20)).unwrap_or(0);
        let cjk = rng.below(5) == 0;
        let bytes = stream(&mut rng);
        let reads = reads(&mut rng, &bytes);
        let mut fast = engine(rows, cols, cjk, false);
        let mut reference = engine(rows, cols, cjk, true);
        let (mut fast_replies, mut reference_replies) = (Vec::new(), Vec::new());
        let mut elapsed = Duration::ZERO;
        for (n, read) in reads.iter().enumerate() {
            // Reads in one frame, or a frame apart (past the archive's fallback interval).
            elapsed += Duration::from_millis(20 * u64::from(rng.below(2) == 0));
            let clock = at(origin, elapsed);
            if rng.below(3) == 0 {
                let top = i32::try_from(rng.below(usize::from(rows))).unwrap_or(0);
                let bottom = top + i32::try_from(rng.below(2)).unwrap_or(0);
                let col = u16::try_from(rng.below(usize::from(cols))).unwrap_or(0);
                select(&mut fast, top, bottom, col);
                select(&mut reference, top, bottom, col);
            }
            feed(&mut fast, read, clock);
            reference.process_at(read, clock);
            let got = screen(&mut fast, &mut fast_replies);
            let want = screen(&mut reference, &mut reference_replies);
            if got != want {
                return Err(format!(
                    "seed {seed}, {rows}x{cols} cjk={cjk}, after read {n} of {reads:?}\n\
                     fast:\n{got:#?}\nreference:\n{want:#?}"
                ));
            }
        }
        let (got, want) = (behind(&fast), behind(&reference));
        if got != want {
            return Err(format!(
                "seed {seed}, {rows}x{cols} cjk={cjk}, reads {reads:?}\nfast:\n{got}\nreference:\n{want}"
            ));
        }
    }
    Ok(cases)
}

#[test]
fn the_fast_print_path_leaves_what_the_reference_path_leaves() {
    if let Err(divergence) = first_divergence(whole, 10_000) {
        panic!("the fast print path diverges from the reference: {divergence}");
    }
}

#[test]
fn a_c1_control_after_joined_ascii_at_the_row_end_brackets_as_its_own_print() {
    // The run joins "ok" and the control after it. On the reference path the control is a print
    // of its own: it begins on the glyph parked at the row's end and records the row, though it
    // lands nowhere.
    for text in ["\u{e9}ok\u{85}\r", "\u{e9}ok\u{9b}\u{85}\r"] {
        let [mut fast, mut reference] = [false, true].map(|r| engine(3, 3, false, r));
        let (mut fast_replies, mut reference_replies) = (Vec::new(), Vec::new());
        for t in [&mut fast, &mut reference] {
            select(t, 0, 0, 1);
            t.process(text.as_bytes());
        }
        assert_eq!(
            screen(&mut fast, &mut fast_replies),
            screen(&mut reference, &mut reference_replies),
            "{text:?}"
        );
        assert!(!reference.text_selection().has_selection(), "{text:?}");
    }
}

#[test]
fn the_differential_catches_a_planted_split_boundary_bug() {
    assert!(
        first_divergence(planted_split_bug, 2500).is_err(),
        "an engine that loses a sequence cut by a read boundary left what the reference leaves"
    );
}
