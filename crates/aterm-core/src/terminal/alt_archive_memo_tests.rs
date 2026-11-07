// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The archive's row memo and differ against the paths they replaced. One generated stream of
//! alt-screen frames drives two terminals. The old path re-reads every row at every commit through
//! `Grid::row_text_screen_into`, each vote indexes both frames' eligible rows above its `t` afresh,
//! and a reanchor scans the archive's tail. The new path re-reads only the rows whose cells
//! changed, a plain row by its own reader, patches an ASCII row's written cells, and those past
//! the length it was read at, into what it read before (keeping its text and version when they say
//! what it said), serves a row whose cells carry extras once a look beside them finds no combining
//! mark, indexes each frame once for every vote, takes a row whose version the previous frame
//! holds at the row it came from as that row's text without a lookup or a compare, looks a
//! reanchor's rows up in an index of the tail, and commits a frame that changes a few rows and can
//! only stay where it stands in place, without the diff. After every read of the stream their
//! archives, differ state, screens, selections and damage must be equal. The stream scrolls random
//! regions by line feeds, reverse indexes, SU/SD and IL/DL, repaints part of a scroll, writes wide
//! glyphs, emoji, combining marks, protected cells, hyperlinks, a sixel and marks, writes
//! underline-coloured text and then, in a frame of its own, a combining mark after one of its
//! cells, recolours rows without changing their text, copies a row's line onto another, rewrites a
//! few rows of a view that shows each line twice, pads a ruler with printed blanks, resizes,
//! selects, and leaves, resets and re-enters the alternate screen. Its negative controls plant a
//! missed row change, a patch that keeps the old trailing-space cut, a patch that takes a span
//! written past its text's cut as saying what the text said, a patch of a grown row that leaves
//! out the cells past its old length, a row whose cells carry extras served without the look for
//! marks, a frame index left from the rows before, a tail index that stops catching up, a patch
//! that keeps its row's version, and an in-place commit that misses a move or counts rows that
//! share their text as voting for none, which the comparison must catch.

use super::*;
use crate::selection::{SelectionSide, SelectionType};
use crate::terminal::ClockReading;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next() % n.max(1) as u64).expect("below n")
    }

    fn chance(&mut self, percent: usize) -> bool {
        self.below(100) < percent
    }
}

const TOKENS: &[&str] = &[
    "alpha",
    "beta gamma",
    "漢字",
    "テスト行",
    "😀",
    "👍🏽 ok",
    "cafe\u{301}",
    "a\u{308}\u{301}",
    "│ box │",
    "────",
    "δέλτα",
    "}",
    " ",
];

/// A document line the frames show: mostly distinct (anchors), some blank, rule or brace rows
/// that repeat, some with wide glyphs, emoji or combining marks.
fn doc_line(i: usize) -> String {
    match i % 11 {
        4 => String::new(),
        7 => "──────────────".to_string(),
        9 => "  }".to_string(),
        2 => format!("L{i:04} 漢字 wide {i}"),
        5 => format!("L{i:04} emoji 😀 {i}"),
        8 => format!("L{i:04} cafe\u{301} {i}"),
        _ => format!("L{i:04} fn item_{i}() {{"),
    }
}

fn cup(out: &mut Vec<u8>, row: usize, col: usize) {
    out.extend_from_slice(format!("\x1b[{};{}H", row + 1, col + 1).as_bytes());
}

/// The generator's view of the screen it draws on.
struct Doc {
    rows: usize,
    cols: usize,
    top: usize,
    tick: usize,
    /// Whether the view shows each line twice: no row's text is the only one of its kind.
    twice: bool,
    /// Whether the frames being drawn are a synchronized update's.
    synced: bool,
    /// Whether every line the view shows is an anchor: a listing of distinct lines.
    dense: bool,
}

impl Doc {
    fn view_rows(&self) -> usize {
        self.rows.saturating_sub(2).max(1)
    }

    fn draw_view(&self, out: &mut Vec<u8>) {
        for r in 0..self.view_rows() {
            cup(out, r, 0);
            let line = if self.twice { r / 2 } else { r };
            out.extend_from_slice(self.line(self.top + line).as_bytes());
            out.extend_from_slice(b"\x1b[K");
        }
        self.draw_status(out);
    }

    fn line(&self, i: usize) -> String {
        if self.dense {
            format!("L{i:04} fn item_{i}() {{")
        } else {
            doc_line(i)
        }
    }

    /// Draws `draw` as a frame of its own: a commit sees all of it and nothing else.
    fn alone(&self, out: &mut Vec<u8>, draw: impl FnOnce(&mut Vec<u8>)) {
        out.extend_from_slice(b"\x1b[?2026h");
        draw(out);
        out.extend_from_slice(b"\x1b[?2026l");
        if self.synced {
            out.extend_from_slice(b"\x1b[?2026h");
        }
    }

    fn commit_view(&self, out: &mut Vec<u8>) {
        self.alone(out, |out| self.draw_view(out));
    }

    fn draw_status(&self, out: &mut Vec<u8>) {
        if self.rows < 3 {
            return;
        }
        cup(out, self.rows - 2, 0);
        out.extend_from_slice(
            format!("\x1b[7m-- INSERT -- {}\x1b[m\x1b[K", self.tick / 3).as_bytes(),
        );
        // The ruler is a field padded with printed blanks: its written length stays while its
        // text grows and shrinks.
        cup(out, self.rows - 1, 0);
        let ruler = format!("{},{} All", self.tick % 97, self.tick % 13);
        out.extend_from_slice(format!("ruler {ruler:<10}\x1b[K").as_bytes());
    }

    /// One frame's operations.
    fn frame(&mut self, rng: &mut Rng, out: &mut Vec<u8>) {
        self.tick += 1;
        let (rows, cols) = (self.rows, self.cols);
        let view = self.view_rows();
        match rng.below(23) {
            0..=2 => {
                let step = 1 + rng.below(4);
                self.top = if rng.chance(25) {
                    self.top.saturating_sub(step)
                } else {
                    self.top + step
                };
                self.draw_view(out);
            }
            3 => {
                self.top = rng.below(400);
                self.draw_view(out);
            }
            4 => {
                // An app scrolling its view region by line feeds at the bottom margin.
                let n = 1 + rng.below(5);
                out.extend_from_slice(format!("\x1b[1;{view}r").as_bytes());
                cup(out, view - 1, 0);
                for k in 0..n {
                    out.extend_from_slice(b"\n\r");
                    out.extend_from_slice(doc_line(self.top + view + k).as_bytes());
                    out.extend_from_slice(b"\x1b[K");
                }
                self.top += n;
                out.extend_from_slice(b"\x1b[r");
                self.draw_status(out);
            }
            5 => {
                // Scrolling back by reverse index at the top margin.
                let n = 1 + rng.below(3);
                out.extend_from_slice(format!("\x1b[1;{view}r").as_bytes());
                for k in 0..n {
                    cup(out, 0, 0);
                    out.extend_from_slice(b"\x1bM");
                    let line = self.top.saturating_sub(k + 1);
                    out.extend_from_slice(doc_line(line).as_bytes());
                    out.extend_from_slice(b"\x1b[K");
                }
                self.top = self.top.saturating_sub(n);
                out.extend_from_slice(b"\x1b[r");
            }
            6 => {
                // Random margins: line feeds, SU/SD or IL/DL inside them.
                let a = rng.below(rows);
                let b = rng.below(rows);
                let (t, bot) = (a.min(b), a.max(b).max(a.min(b) + 1).min(rows - 1));
                out.extend_from_slice(format!("\x1b[{};{}r", t + 1, bot + 1).as_bytes());
                let n = 1 + rng.below(4);
                match rng.below(5) {
                    0 => {
                        cup(out, bot, 0);
                        for _ in 0..n {
                            out.push(b'\n');
                        }
                    }
                    1 => out.extend_from_slice(format!("\x1b[{n}S").as_bytes()),
                    2 => out.extend_from_slice(format!("\x1b[{n}T").as_bytes()),
                    3 => {
                        cup(out, t + rng.below(bot - t + 1), 0);
                        out.extend_from_slice(format!("\x1b[{n}L").as_bytes());
                    }
                    _ => {
                        cup(out, t + rng.below(bot - t + 1), 0);
                        out.extend_from_slice(format!("\x1b[{n}M").as_bytes());
                    }
                }
                cup(out, t + rng.below(bot - t + 1), rng.below(cols));
                out.extend_from_slice(TOKENS[rng.below(TOKENS.len())].as_bytes());
                out.extend_from_slice(b"\x1b[r");
            }
            7 | 8 => {
                // An edit in place: a token, sometimes DECSCA-protected (its bit is the wide
                // continuation's), then maybe ICH/DCH/ECH/EL.
                cup(out, rng.below(rows), rng.below(cols));
                let protect = rng.chance(20);
                if protect {
                    out.extend_from_slice(b"\x1b[1\"q");
                }
                out.extend_from_slice(TOKENS[rng.below(TOKENS.len())].as_bytes());
                if protect {
                    out.extend_from_slice(b"\x1b[0\"q");
                }
                let n = 1 + rng.below(6);
                match rng.below(7) {
                    0 => out.extend_from_slice(format!("\x1b[{n}@").as_bytes()),
                    1 => out.extend_from_slice(format!("\x1b[{n}P").as_bytes()),
                    2 => out.extend_from_slice(format!("\x1b[{n}X").as_bytes()),
                    3 => out.extend_from_slice(b"\x1b[K"),
                    4 => out.extend_from_slice(b"\x1b[1K"),
                    5 => out.extend_from_slice(b"\x1b[2K"),
                    _ => {}
                }
                self.draw_status(out);
            }
            9 => {
                // A recolour that leaves the text as it was.
                let r = rng.below(view);
                cup(out, r, 0);
                let colour = 31 + rng.below(7);
                out.extend_from_slice(format!("\x1b[{colour}m").as_bytes());
                out.extend_from_slice(doc_line(self.top + r).as_bytes());
                out.extend_from_slice(b"\x1b[m");
            }
            10 => {
                // A long run that autowraps, a wide glyph at the last column among them.
                cup(out, rng.below(rows), cols.saturating_sub(1 + rng.below(3)));
                out.extend_from_slice("漢字テスト wraps past the edge".as_bytes());
            }
            11 => {
                let n = self.tick;
                cup(out, rng.below(rows), rng.below(cols));
                out.extend_from_slice(
                    format!("\x1b]8;;https://x.test/{n}\x1b\\link{n}\x1b]8;;\x1b\\").as_bytes(),
                );
                if rng.chance(30) {
                    out.extend_from_slice(b"\x1b]133;A\x07$ \x1b]133;B\x07");
                }
            }
            12 => match rng.below(5) {
                0 => out.extend_from_slice(b"\x1b[2J"),
                1 => {
                    cup(out, rng.below(rows), rng.below(cols));
                    out.extend_from_slice(b"\x1b[J");
                }
                2 => {
                    cup(out, rng.below(rows), rng.below(cols));
                    out.extend_from_slice(b"\x1b[1J");
                }
                3 => out.extend_from_slice(b"\x1b#8"),
                _ => {
                    cup(out, rng.below(rows), 0);
                    out.extend_from_slice(if rng.chance(50) { b"\x1b#6" } else { b"\x1b#5" });
                }
            },
            13 => {
                cup(out, rng.below(rows), rng.below(cols));
                out.extend_from_slice(b"\x1bPq#0;2;100;0;0#0~~~~~~-~~~~~~\x1b\\");
            }
            14 | 15 => {
                // A scroll of a listing repainted in part: the top rows show the lines a row or
                // two below theirs, the rows under them as they were.
                (self.twice, self.dense) = (false, true);
                self.commit_view(out);
                let s = if rng.chance(75) { 1 } else { 2 };
                let k = (4 + rng.below(3)).min(view);
                // The ruler moves on: the diff reaches the rows under the repaint.
                self.tick += 1;
                self.alone(out, |out| {
                    for r in 0..k {
                        cup(out, r, 0);
                        out.extend_from_slice(self.line(self.top + s + r).as_bytes());
                        out.extend_from_slice(b"\x1b[K");
                    }
                    self.draw_status(out);
                });
                self.dense = false;
            }
            16 => {
                // A row takes another row's line: two rows with one text.
                cup(out, rng.below(view), 0);
                out.extend_from_slice(doc_line(self.top + rng.below(view)).as_bytes());
                out.extend_from_slice(b"\x1b[K");
            }
            17 | 18 => {
                // A few rows rewritten wherever they are, on a view of each line once or twice.
                self.twice = rng.chance(50);
                self.commit_view(out);
                self.alone(out, |out| {
                    for _ in 0..3 + rng.below(4) {
                        cup(out, rng.below(rows), 0);
                        out.extend_from_slice(doc_line(rng.below(400)).as_bytes());
                        out.extend_from_slice(b"\x1b[K");
                    }
                });
            }
            19 => {
                // A view with few distinct lines: each shown twice from now on, or until now,
                // or a few shown once.
                if self.twice || rng.chance(50) {
                    self.twice = !self.twice;
                    self.draw_view(out);
                } else {
                    out.extend_from_slice(b"\x1b[2J");
                    for r in 0..view {
                        if rng.chance(25) {
                            cup(out, r, 0);
                            out.extend_from_slice(doc_line(self.top + r).as_bytes());
                        }
                    }
                    self.draw_status(out);
                }
            }
            20 => {
                // Underline-coloured text, whose cells carry extras and no mark; then, in a frame
                // of its own, a bare combining mark after one of them, which leaves its cells as
                // they were.
                let (r, c) = (rng.below(view), rng.below(cols.saturating_sub(5)));
                self.alone(out, |out| {
                    cup(out, r, c);
                    out.extend_from_slice(b"\x1b[4:3m\x1b[58:2::255:0:0mword\x1b[m");
                });
                self.alone(out, |out| {
                    cup(out, r, c + 2);
                    out.extend_from_slice("\u{301}".as_bytes());
                });
            }
            _ => {
                // The same text written again: the content generation moves, the text does not.
                self.draw_view(out);
            }
        }
    }
}

/// Everything a commit can reach, and what the memo must not touch.
#[derive(Debug, PartialEq)]
struct Seen {
    texts: Vec<String>,
    gaps: Vec<AltArchiveGap>,
    lost: u64,
    oldest: u64,
    last: u64,
    epoch: u32,
    back: usize,
    back_at: u64,
    fence: AltArchiveFence,
    carry: AltArchiveCarry,
    screen: String,
    alt: bool,
    selection: Option<String>,
    damage: String,
}

fn seen(t: &Terminal) -> Seen {
    let a = t.alt_archive();
    Seen {
        texts: a.texts(),
        gaps: a.gaps().collect(),
        lost: a.lost(),
        oldest: a.oldest(),
        last: a.last(),
        epoch: a.epoch(),
        back: a.back(),
        back_at: a.back_at(),
        fence: a.fence(),
        carry: t.alt_archive_carry_head(true).1,
        screen: t.visible_content(),
        alt: t.modes().alternate_screen,
        selection: t.selection_to_string(),
        damage: format!("{:?}", t.grid().damage()),
    }
}

const SIZES: &[(u16, u16)] = &[(14, 32), (14, 32), (12, 40), (20, 24), (9, 32)];

/// What one stream did: where the old path and the memo first disagreed, how many rows the memo
/// served, how many of those a scroll had moved and how many it patched, and how many rows the
/// archive kept.
struct Run {
    diverged: Option<String>,
    hits: u64,
    moved: u64,
    patched: u64,
    in_place: u64,
    archived: usize,
}

/// A divergence planted in the new path.
#[derive(Clone, Copy)]
enum Plant {
    None,
    /// A change to this screen row is missed.
    MissedRow(usize),
    /// A filled frame keeps the index of the rows it held before.
    StaleIndex,
    /// A patch keeps the text's old cut when the change reaches past it.
    StaleCut,
    /// A patch takes a span as saying what the text said whatever it writes past the cut.
    LaxSame,
    /// A row that grew is patched with its changed cells alone.
    ShortGrowth,
    /// A row whose cells carry extras is served without the look for marks beside them.
    UncheckedMarks,
    /// The tail index never catches up with rows archived after it was built.
    StaleTail,
    /// A patch keeps the version of the text it changed.
    StaleVersion,
    /// An in-place commit takes a changed row whose text another row had as voting for no move.
    LaxMoves,
    /// An in-place commit counts every unchanged anchor as voting for no move.
    LaxVoters,
}

/// Runs one generated stream through the old path and the new one, with `plant` in it.
fn run(seed: u64, plant: Plant) -> Run {
    let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1);
    let (rows, cols) = SIZES[0];
    let mut old = Terminal::new(rows, cols);
    let mut new = Terminal::new(rows, cols);
    old.set_alt_archive_enabled(true);
    new.set_alt_archive_enabled(true);
    old.alt_archive.archive.memo.bypass = true;
    old.alt_archive.archive.scratch.reference = true;
    match plant {
        Plant::None => {}
        Plant::MissedRow(row) => new.alt_archive.archive.memo.missed_row = Some(row),
        Plant::StaleIndex => new.alt_archive.archive.memo.stale_index = true,
        Plant::StaleCut => new.alt_archive.archive.memo.stale_cut = true,
        Plant::LaxSame => new.alt_archive.archive.memo.lax_same = true,
        Plant::ShortGrowth => new.alt_archive.archive.memo.short_growth = true,
        Plant::UncheckedMarks => new.alt_archive.archive.memo.unchecked_marks = true,
        Plant::StaleTail => new.alt_archive.archive.scratch.stale_tail = true,
        Plant::StaleVersion => new.alt_archive.archive.memo.stale_version = true,
        Plant::LaxMoves => new.alt_archive.archive.scratch.lax_moves = true,
        Plant::LaxVoters => new.alt_archive.archive.scratch.lax_voters = true,
    }
    let base = ClockReading::now();
    let mut ms = 0u64;
    let mut doc = Doc {
        rows: usize::from(rows),
        cols: usize::from(cols),
        top: rng.below(50),
        tick: 0,
        twice: false,
        synced: false,
        dense: false,
    };
    let mut stream = b"\x1b[?1049h".to_vec();
    for _ in 0..240 {
        match rng.below(40) {
            0 => {
                stream.extend_from_slice(b"\x1b[?1049lmain text\r\n\x1b[?1049h");
                doc.draw_view(&mut stream);
            }
            1 => stream.extend_from_slice(b"\x1bc\x1b[?1049h"),
            2 => stream.extend_from_slice(b"\x1b[!p"),
            _ => {}
        }
        let synced = rng.chance(85);
        doc.synced = synced;
        if synced {
            stream.extend_from_slice(b"\x1b[?2026h");
        }
        for _ in 0..1 + rng.below(3) {
            doc.frame(&mut rng, &mut stream);
        }
        if synced {
            stream.extend_from_slice(b"\x1b[?2026l");
        }
    }
    let mut at = 0;
    let mut read = 0usize;
    while at < stream.len() {
        let end = (at + 1 + rng.below(1500)).min(stream.len());
        ms += rng.below(40) as u64;
        let clock = ClockReading {
            monotonic: base
                .monotonic
                .checked_add(std::time::Duration::from_millis(ms))
                .expect("clock"),
            wall_ms: None,
        };
        old.process_at(&stream[at..end], clock);
        new.process_at(&stream[at..end], clock);
        at = end;
        read += 1;
        if rng.chance(4) {
            let (r, c) = SIZES[rng.below(SIZES.len())];
            old.resize(r, c);
            new.resize(r, c);
            (doc.rows, doc.cols) = (usize::from(r), usize::from(c));
        }
        if rng.chance(6) {
            let row = i32::try_from(rng.below(doc.rows)).expect("row");
            for t in [&mut old, &mut new] {
                let sel = t.text_selection_mut();
                sel.start_selection(row, 0, SelectionSide::Left, SelectionType::Simple);
                sel.update_selection(row, 5, SelectionSide::Right);
                sel.complete_selection();
            }
        }
        let (a, b) = (seen(&old), seen(&new));
        if a != b {
            return Run {
                diverged: Some(format!(
                    "seed {seed}, read {read}:\nold {a:#?}\nmemo {b:#?}"
                )),
                hits: new.alt_archive.archive.memo.hits,
                moved: new.alt_archive.archive.memo.moved,
                patched: new.alt_archive.archive.memo.patched,
                in_place: new.alt_archive.archive.scratch.in_place,
                archived: old.alt_archive().len(),
            };
        }
    }
    assert_eq!(
        old.alt_archive.archive.memo.hits, 0,
        "the old path serves no row"
    );
    assert_eq!(
        old.alt_archive.archive.scratch.in_place, 0,
        "the old path commits nothing in place"
    );
    Run {
        diverged: None,
        hits: new.alt_archive.archive.memo.hits,
        moved: new.alt_archive.archive.memo.moved,
        patched: new.alt_archive.archive.memo.patched,
        in_place: new.alt_archive.archive.scratch.in_place,
        archived: old.alt_archive().len(),
    }
}

#[test]
fn the_new_path_commits_exactly_what_the_old_path_commits() {
    let (mut hits, mut moved, mut patched, mut in_place, mut archived) = (0, 0, 0, 0, 0);
    for seed in 1..=40 {
        let run = run(seed, Plant::None);
        if let Some(why) = run.diverged {
            panic!("the new path diverged from the old one: {why}");
        }
        hits += run.hits;
        moved += run.moved;
        patched += run.patched;
        in_place += run.in_place;
        archived += run.archived;
    }
    eprintln!(
        "served {hits}, moved {moved}, patched {patched}, in place {in_place}, archived {archived}"
    );
    assert!(
        hits > 10_000,
        "the memo served only {hits} rows: the comparison is vacuous"
    );
    assert!(
        moved > 2_000,
        "only {moved} served rows had moved: scrolls go untested"
    );
    assert!(
        patched > 1_000,
        "only {patched} rows patched: edits in place go untested"
    );
    assert!(
        in_place > 500,
        "only {in_place} commits made in place: the in-place path goes untested"
    );
    assert!(
        archived > 500,
        "only {archived} rows archived: the stream scrolls too little"
    );
}

#[test]
fn a_planted_missed_row_change_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::MissedRow(3)).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "a missed row change was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_stale_patch_cut_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::StaleCut).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "a patch that keeps its old cut was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_lax_unchanged_span_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::LaxSame).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "a patch that misses a write past its text's cut was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_short_growth_patch_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::ShortGrowth).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "a patch that leaves out a grown row's new cells was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_unchecked_mark_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::UncheckedMarks).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "a row served without the look for marks was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_stale_frame_index_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::StaleIndex).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "a stale frame index was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_stale_tail_index_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::StaleTail).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "a stale tail index was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_stale_row_version_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::StaleVersion).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "a patch that keeps its version was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_in_place_move_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::LaxMoves).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "an in-place commit that misses a move was caught on only {caught} of 10 streams"
    );
}

#[test]
fn a_planted_in_place_voter_count_is_caught() {
    let caught = (1..=10)
        .filter(|&seed| run(seed, Plant::LaxVoters).diverged.is_some())
        .count();
    assert!(
        caught >= 8,
        "an in-place commit that counts shared rows as voters was caught on only {caught} of 10 streams"
    );
}

#[test]
fn the_block_scans_find_what_a_scan_one_at_a_time_finds() {
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let ends = |b: &[u8]| b.iter().rposition(|&b| b != b' ').map_or(0, |i| i + 1);
    for _ in 0..20_000 {
        let n = rng.below(80);
        let said = rng.below(n + 1);
        let bytes: Vec<u8> = (0..n)
            .map(|i| {
                if i < said && rng.chance(60) {
                    b'x'
                } else {
                    b' '
                }
            })
            .collect();
        assert_eq!(trimmed_len(&bytes), ends(&bytes), "{bytes:?}");
        let cells: Vec<Cell> = bytes.iter().map(|&b| Cell::from_ascii_fast(b)).collect();
        let mut snap: Vec<u64> = cells.iter().map(|&c| cell_bits(c)).collect();
        for _ in 0..rng.below(3) {
            let i = rng.below(n);
            if i < n {
                snap[i] ^= 1 << rng.below(64);
            }
        }
        let differs = |i: &usize| cell_bits(cells[*i]) != snap[*i];
        let span = (0..n).find(differs).zip((0..n).rev().find(differs));
        assert_eq!(changed_span(&cells, &snap), span, "{snap:x?}");
    }
}
