// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The scroll fast paths against the row-by-row reference paths they replaced
//! ([`crate::test_counters::reference_paths`]): two grids fed one generated stream of line feeds,
//! reverse indexes, region scrolls, IL/DL, SU/SD, writes with extras (hyperlinks, true colour,
//! wide and non-BMP glyphs, combining marks, inline images) and BCE erases, one grid on each path,
//! must agree
//! after every step on everything a reader sees: the screen's cells and row flags, the history,
//! the extras, the cursor, damage and its mark clock, the row revisions, the selection damage, the
//! row-band record and the absolute-row splices. The coalesced line feeds of a run of text lines
//! ([`Grid::line_feeds_at_region_bottom`], and at the bottom of a full screen
//! [`Grid::screen_line_feed`] with [`Grid::settle_screen_line_feeds`]) are held to the line feeds
//! one at a time the same way, except that the content generation and the mark clock are only
//! asked whether they moved: a coalesced run advances each once.

use std::sync::Arc;

use aterm_scrollback::Scrollback;
use aterm_spec::rng::XorShift;

use crate::test_counters::on_reference_paths;
use crate::{
    AbsoluteRowUpdate, Cell, CellFlags, Grid, HyperlinkData, ImageData, ImageFormat, ImageRef,
    ImageScaling, PackedColors, RowBandMoves, SelectionDamage,
};

/// What the extras hold for one visible cell: its complex codepoint, foreground RGB and background
/// RGB.
type DenseCell = (Option<char>, Option<[u8; 3]>, Option<[u8; 3]>);

/// Everything a reader of the grid can see, drained where the grid drains it.
#[derive(Debug, PartialEq)]
struct Seen {
    rows: Vec<(Vec<Cell>, u16, u8)>,
    history: Vec<String>,
    extras: Vec<String>,
    dense: Vec<DenseCell>,
    cursor: (u16, u16, bool),
    region: (u16, u16),
    display_offset: usize,
    scrollback_lines: usize,
    /// The history lines a replay carries (the count `note_history_pushed` keeps).
    carried: usize,
    renumber_epoch: u64,
    absolute_top: u64,
    content_gen: u64,
    damage: Option<Vec<(u16, u16, u16)>>,
    mark_seq: Option<u64>,
    revisions: Vec<u64>,
    selection: SelectionDamage,
    band_moves: RowBandMoves,
    coordinates_invalidated: bool,
    absolute_update: Option<AbsoluteRowUpdate>,
    selection_update: Option<AbsoluteRowUpdate>,
    scroll_delta: i32,
}

fn observe(g: &mut Grid) -> Seen {
    let lines = g.scrollback_lines();
    observe_history(g, 0..lines)
}

/// [`observe`] with only the history lines in `history` read: a flood's tens of thousands of
/// staged lines are read whole only at its end.
fn observe_history(g: &mut Grid, history: std::ops::Range<usize>) -> Seen {
    let (rows, cols) = (g.rows(), g.cols());
    let mut extras: Vec<String> = g
        .extras()
        .iter()
        .map(|(at, extra)| format!("{at:?} {extra:?}"))
        .collect();
    extras.sort();
    Seen {
        rows: (0..rows)
            .map(|r| {
                g.row(r).map_or((Vec::new(), 0, 0), |row| {
                    (row.as_slice().to_vec(), row.len(), row.flags().bits())
                })
            })
            .collect(),
        history: history
            .map(|i| format!("{:?}", g.get_history_line(i)))
            .collect(),
        extras,
        dense: (0..rows)
            .flat_map(|r| (0..cols).map(move |c| (r, c)))
            .map(|(r, c)| {
                let x = g.extras();
                (
                    x.complex_codepoint_for(r, c),
                    x.fg_rgb_for(r, c),
                    x.bg_rgb_for(r, c),
                )
            })
            .collect(),
        cursor: (g.cursor_row(), g.cursor_col(), g.pending_wrap()),
        region: (g.scroll_region().top, g.scroll_region().bottom),
        display_offset: g.display_offset(),
        scrollback_lines: g.scrollback_lines(),
        carried: g
            .storage
            .replay_carried_history
            .load(std::sync::atomic::Ordering::Relaxed),
        renumber_epoch: g.history_renumber_epoch(),
        absolute_top: g.visible_to_absolute(0),
        content_gen: g.content_gen(),
        damage: (!g.damage().is_full()).then(|| {
            g.damage()
                .iter_bounds(rows, cols)
                .map(|b| (b.line, b.left, b.right))
                .collect()
        }),
        mark_seq: g.damage().mark_seq(),
        revisions: g.row_revisions().to_vec(),
        selection: g.take_selection_damage(),
        band_moves: g.take_row_band_moves(),
        coordinates_invalidated: g.take_coordinates_invalidated(),
        absolute_update: g.take_absolute_row_update(),
        selection_update: g.take_selection_row_update(),
        scroll_delta: g.take_content_scroll_delta(),
    }
}

/// The content generation and the damage mark clock as the last step left them, so a step's
/// [`Seen`] can carry how far each moved: a coalesced shift moves each once where its line feeds
/// one at a time move them per feed, so after one the two grids' counts differ for good.
#[derive(Default)]
struct Clocks {
    content_gen: u64,
    mark_seq: Option<u64>,
}

impl Clocks {
    /// Replace `seen`'s counts by how far they moved this step (only whether, after a coalesced
    /// shift). A clock that comes back from full damage (`None`) starts again at zero.
    fn step(&mut self, seen: &mut Seen, coalesced: bool) {
        let (content_gen, mark_seq) = (seen.content_gen, seen.mark_seq);
        seen.content_gen = content_gen.wrapping_sub(self.content_gen);
        seen.mark_seq = mark_seq.map(|m| m.wrapping_sub(self.mark_seq.unwrap_or(0)));
        if coalesced {
            seen.content_gen = seen.content_gen.min(1);
            seen.mark_seq = seen.mark_seq.map(|m| m.min(1));
        }
        (self.content_gen, self.mark_seq) = (content_gen, mark_seq);
    }
}

/// One step of a generated stream.
#[derive(Clone, Debug)]
enum Op {
    Text(Vec<char>),
    /// `write_ascii_run_with_extras`: a hyperlink, true-colour foreground and background.
    Styled(Vec<u8>, u8),
    /// A combining mark on the cell left of the cursor.
    Combining,
    /// An inline image's first row over this many cells from the cursor.
    Image(u16),
    Cr,
    Lf,
    /// `n` line feeds in a row.
    LfRun(usize),
    /// `n` line feeds in a row, coalesced into one shift where the cursor is at the bottom of a
    /// history-free region ([`Grid::line_run_region`]).
    CoalescedLf(usize),
    /// A line feed before each line, each line then written: at the bottom of a full screen
    /// ([`Grid::at_screen_line_run`]) the line feeds' damage and scroll delta are settled once.
    ScreenLines(Vec<Vec<u8>>),
    /// A line feed before each line, each line then written and followed by a CR where its flag
    /// says: at the bottom of a full screen the lines [`Grid::screen_text_line`] takes, its
    /// run settled once ([`Grid::settle_screen_text_lines`]), then the rest one at a time.
    ScreenText(Vec<(Vec<u8>, bool)>),
    /// Hand scrollback compression off the parse thread, or take it back.
    Offload(bool),
    /// What the app's compression worker does between two reads (aterm-gui's spawn.rs,
    /// `spawn_compress_worker`), each moving staged lines into the store: 0 its flood pressure
    /// relief, 1 its once-a-second trickle, 2 the drain it runs once a flood goes quiet.
    Worker(u8),
    Ri,
    Region(u16, u16),
    Cursor(u16, u16),
    /// The cursor to the region's bottom row, where a line feed scrolls.
    ToRegionBottom,
    /// The cursor to the region's top row, where a reverse index scrolls.
    ToRegionTop,
    Il(usize),
    Dl(usize),
    Su(usize),
    Sd(usize),
    EraseLine,
    EraseBelow,
    /// The BCE template: 0 default, 1 an indexed background, 2 a true-colour one.
    Template(u8),
    ClearDamage,
    /// Scroll the view back by `n` and leave it there for a step.
    View(i32),
    Unscroll(usize),
}

fn gen_op(rng: &mut XorShift, rows: u16, cols: u16) -> Op {
    let row = |rng: &mut XorShift| u16::try_from(rng.below(u64::from(rows))).unwrap_or(0);
    let count = |rng: &mut XorShift| 1 + rng.index(usize::from(rows) + 1);
    match rng.below(100) {
        0..=9 => Op::Lf,
        10..=11 => Op::ScreenLines(
            (0..1 + rng.index(3 * usize::from(rows) + 1))
                .map(|_| {
                    (0..rng.index(usize::from(cols) + 3))
                        .map(|_| b'a' + rng.below(4) as u8)
                        .collect()
                })
                .collect(),
        ),
        12..=13 => {
            let lines = 1 + rng.index(3 * usize::from(rows) + 1);
            screen_text(rng, lines, usize::from(cols) + 2, 5)
        }
        14..=17 => Op::CoalescedLf(1 + rng.index(3 * usize::from(rows) + 1)),
        18..=21 => Op::LfRun(count(rng)),
        22..=27 => Op::Cr,
        28..=35 => {
            let pool = ['a', 'b', 'Z', '中', '🚀', 'é', ' '];
            Op::Text((0..1 + rng.below(4)).map(|_| *rng.pick(&pool)).collect())
        }
        36..=39 => Op::Styled(
            (0..1 + rng.below(3))
                .map(|_| b'k' + rng.below(4) as u8)
                .collect(),
            rng.below(4) as u8,
        ),
        40..=41 => Op::Combining,
        42..=46 => Op::Ri,
        47..=49 => {
            let (a, b) = (row(rng), row(rng));
            Op::Region(a.min(b), a.max(b))
        }
        50..=51 => Op::Region(0, rows - 1),
        52..=55 => Op::Cursor(
            row(rng),
            u16::try_from(rng.below(u64::from(cols))).unwrap_or(0),
        ),
        56..=65 => Op::ToRegionBottom,
        66..=68 => Op::ToRegionTop,
        69..=71 => Op::Il(count(rng)),
        72..=74 => Op::Dl(count(rng)),
        75..=77 => Op::Su(count(rng)),
        78..=80 => Op::Sd(count(rng)),
        81..=82 => Op::EraseLine,
        83 => Op::EraseBelow,
        84..=87 => Op::Template(rng.below(3) as u8),
        88..=91 => Op::ClearDamage,
        92..=93 => Op::Image(1 + rng.below(3) as u16),
        94..=96 => Op::View(1 + rng.below(3) as i32),
        _ => Op::Unscroll(count(rng)),
    }
}

/// `lines` lines for [`Op::ScreenText`] of lengths below `longest`, each followed by a CR at
/// odds of `cr` in 8.
fn screen_text(rng: &mut XorShift, lines: usize, longest: usize, cr: u64) -> Op {
    Op::ScreenText(
        (0..lines)
            .map(|_| {
                let text = (0..rng.index(longest))
                    .map(|_| b'a' + rng.below(4) as u8)
                    .collect();
                (text, rng.below(8) < cr)
            })
            .collect(),
    )
}

/// The lines the app's compression worker moves in one hold of the terminal's lock, and the
/// staged lines its quiet drain leaves (aterm-gui's spawn.rs, `COMPRESS_BUDGET` and
/// `COMPRESS_LOW_WATER`).
const WORKER_BUDGET: usize = 256;

fn step(g: &mut Grid, op: &Op, link: &Arc<HyperlinkData>, image: &Arc<ImageData>) {
    g.scroll_to_bottom();
    match op {
        Op::Text(chars) => chars.iter().for_each(|&c| g.write_char(c)),
        Op::Styled(bytes, variant) => {
            let mut last = None;
            let (fg, bg) = match variant {
                0 => (Some([1, 2, 3]), None),
                1 => (None, Some([9, 8, 7])),
                _ => (None, None),
            };
            let mut colors = PackedColors::new();
            if fg.is_some() {
                colors = colors.with_rgb_fg();
            }
            if bg.is_some() {
                colors = colors.with_rgb_bg();
            }
            let url = (*variant >= 2).then_some(link);
            g.write_ascii_run_with_extras(
                bytes,
                colors,
                CellFlags::empty(),
                fg,
                bg,
                (*variant == 3).then_some(0x0102_0304),
                0,
                url,
                &mut last,
            );
        }
        Op::Combining => {
            let (row, col) = (g.cursor_row(), g.cursor_col().saturating_sub(1));
            if g.cell(row, col).is_some_and(|c| !c.is_empty()) {
                g.storage.cell_extra_mut(row, col).add_combining('\u{301}');
            }
        }
        Op::Image(width) => {
            let (row, col) = (g.cursor_row(), g.cursor_col());
            for cell_col in 0..*width {
                let Some(at) = col.checked_add(cell_col).filter(|&at| at < g.cols()) else {
                    break;
                };
                g.set_cell_image(
                    row,
                    at,
                    ImageRef {
                        image: Arc::clone(image),
                        cell_row: 0,
                        cell_col,
                        kitty: None,
                    },
                );
            }
        }
        Op::Cr => g.carriage_return(),
        Op::Lf => g.line_feed(),
        Op::LfRun(n) => (0..*n).for_each(|_| g.line_feed()),
        Op::CoalescedLf(n) => {
            if crate::test_counters::reference_paths() || g.line_run_region().is_none() {
                (0..*n).for_each(|_| g.line_feed());
            } else {
                g.line_feeds_at_region_bottom(*n);
            }
        }
        Op::ScreenLines(lines) => {
            let mut coalesce = !crate::test_counters::reference_paths();
            let mut feeds = 0;
            for text in lines {
                coalesce &= g.at_screen_line_run();
                if coalesce {
                    g.screen_line_feed();
                    feeds += 1;
                } else {
                    g.settle_screen_line_feeds(std::mem::take(&mut feeds));
                    g.line_feed();
                }
                g.write_ascii_blast(text);
            }
            g.settle_screen_line_feeds(feeds);
        }
        Op::ScreenText(lines) => {
            let mut taken = 0;
            let run = if crate::test_counters::reference_paths() {
                None
            } else {
                g.begin_screen_text_lines()
            };
            if let Some(mut run) = run {
                let mut wrote = false;
                for (text, cr) in lines {
                    if usize::from(g.cursor_col()) + text.len() > usize::from(g.cols()) {
                        break;
                    }
                    g.screen_text_line(&mut run, text);
                    wrote |= !text.is_empty();
                    if *cr {
                        g.carriage_return();
                    }
                    taken += 1;
                }
                g.settle_screen_text_lines(run, taken, wrote);
            }
            for (text, cr) in &lines[taken..] {
                g.line_feed();
                g.write_ascii_blast(text);
                if *cr {
                    g.carriage_return();
                }
            }
        }
        Op::Offload(active) => g.set_compress_offload_active(*active),
        Op::Worker(0) => {
            let owed = g.flood_promotion_due();
            if owed > 0 {
                g.drain_lazy_pressure_bounded(owed.min(WORKER_BUDGET));
            }
        }
        Op::Worker(1) => {
            g.trickle_lazy_bounded(WORKER_BUDGET);
        }
        Op::Worker(_) => {
            let mut prev = usize::MAX;
            loop {
                let remaining = g.drain_lazy_bounded(WORKER_BUDGET);
                if remaining <= WORKER_BUDGET || remaining >= prev {
                    break;
                }
                prev = remaining;
            }
        }
        Op::Ri => g.reverse_line_feed(),
        Op::Region(top, bottom) => g.set_scroll_region(*top, *bottom),
        Op::Cursor(row, col) => g.set_cursor(*row, *col),
        Op::ToRegionBottom => {
            let col = g.cursor_col();
            g.set_cursor(g.scroll_region().bottom, col);
        }
        Op::ToRegionTop => {
            let col = g.cursor_col();
            g.set_cursor(g.scroll_region().top, col);
        }
        Op::Il(n) => g.insert_lines(*n),
        Op::Dl(n) => g.delete_lines(*n),
        Op::Su(n) => g.scroll_region_up(*n),
        Op::Sd(n) => g.scroll_region_down(*n),
        Op::EraseLine => g.erase_line(),
        Op::EraseBelow => g.erase_to_end_of_screen(),
        Op::Template(kind) => match kind {
            0 => g.set_cursor_template(Cell::EMPTY, None),
            1 => g.set_cursor_template(Cell::bce_blank(PackedColors::with_indexed_bg(4)), None),
            _ => g.set_cursor_template(
                Cell::bce_blank(PackedColors::new().with_rgb_bg()),
                Some([40, 50, 60]),
            ),
        },
        Op::ClearDamage => g.clear_damage(),
        Op::View(n) => g.scroll_display(*n),
        Op::Unscroll(n) => {
            g.unscroll_from_scrollback(*n);
        }
    }
}

/// The grids a stream runs on: no history (the alternate screen's shape), a ring, and a ring
/// over a tiered store with the compression offload's staging.
fn shapes(rows: u16, cols: u16, shape: u64) -> Grid {
    match shape {
        0 => Grid::with_scrollback(rows, cols, 0),
        1 => Grid::with_scrollback(rows, cols, 6),
        _ => {
            let mut g =
                Grid::with_tiered_scrollback(rows, cols, 4, Scrollback::new(4, 64, 10_000_000));
            g.set_compress_offload_active(shape == 3);
            g
        }
    }
}

/// Run `streams` generated streams from `seed`, returning how many line feeds scrolled a region
/// narrower than the screen and after how many steps the history held an image (so the test can
/// say it exercised both), or the first step after which the two grids differ.
fn run(seed: u64, streams: usize) -> Result<(usize, usize), String> {
    let mut rng = XorShift::new(seed);
    let link = Arc::new(HyperlinkData {
        url: Arc::from("https://example.com/"),
        id: None,
    });
    let image = Arc::new(ImageData {
        bytes: vec![7; 4],
        format: ImageFormat::Png,
        cols: 3,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    let mut region_scrolls = 0usize;
    let mut image_history = 0usize;
    for stream in 0..streams {
        let rows = u16::try_from(2 + rng.below(8)).unwrap_or(2);
        let cols = u16::try_from(3 + rng.below(8)).unwrap_or(3);
        let shape = rng.below(4);
        let mut fast = shapes(rows, cols, shape);
        let mut slow = on_reference_paths(|| shapes(rows, cols, shape));
        let mut ops = Vec::new();
        let (mut fast_clocks, mut slow_clocks) = (Clocks::default(), Clocks::default());
        for _ in 0..120 {
            let op = gen_op(&mut rng, rows, cols);
            let region = fast.scroll_region();
            if matches!(op, Op::Lf | Op::LfRun(_))
                && fast.cursor_row() == region.bottom
                && !region.is_full(rows)
            {
                region_scrolls += 1;
            }
            step(&mut fast, &op, &link, &image);
            on_reference_paths(|| step(&mut slow, &op, &link, &image));
            let coalesced = matches!(
                op,
                Op::CoalescedLf(_) | Op::ScreenLines(_) | Op::ScreenText(_)
            );
            ops.push(op);
            let mut seen = observe(&mut fast);
            let mut want = on_reference_paths(|| observe(&mut slow));
            fast_clocks.step(&mut seen, coalesced);
            slow_clocks.step(&mut want, coalesced);
            image_history +=
                usize::from(seen.history.iter().any(|line| line.contains("ImageSpan")));
            if seen != want {
                return Err(format!(
                    "stream {stream} ({rows}x{cols}, shape {shape}) diverged after {ops:?}\n\
                     fast paths: {seen:#?}\nreference paths: {want:#?}"
                ));
            }
        }
    }
    Ok((region_scrolls, image_history))
}

#[test]
fn scroll_fast_paths_match_the_reference_paths_on_generated_streams() {
    let _ = crate::test_counters::take_coalesced_shifts();
    let _ = crate::test_counters::take_screen_settles();
    let (region_scrolls, image_history) =
        run(0x5C_D1FF_2026, 700).unwrap_or_else(|divergence| panic!("{divergence}"));
    assert!(
        region_scrolls > 2_000,
        "the streams must scroll partial regions at their bottom: {region_scrolls}"
    );
    assert!(
        image_history > 5_000,
        "the streams must scroll images into history: {image_history} steps held one"
    );
    let [shifts, multi, deep] = crate::test_counters::take_coalesced_shifts();
    assert!(
        shifts > 500 && multi > 400 && deep > 200,
        "the streams must coalesce line feeds, some past the region's height: {shifts} shifts, \
         {multi} of more than one, {deep} past the height"
    );
    let [settles, runs] = crate::test_counters::take_screen_settles();
    assert!(
        settles > 700 && runs > 500,
        "the streams must settle line feeds at a full screen's bottom: {settles} settles, {runs} \
         of more than one"
    );
}

#[test]
fn the_differential_catches_a_coalesced_shift_one_line_short() {
    let caught =
        crate::test_counters::with_short_line_run_shift(|| run(0x5C_D1FF_2026, 200)).is_err();
    assert!(
        caught,
        "a coalesced shift one line short of its line feeds left what the line feeds leave"
    );
}

#[test]
fn the_differential_catches_screen_line_feeds_settled_one_short() {
    let caught =
        crate::test_counters::with_short_screen_settle(|| run(0x5C_D1FF_2026, 200)).is_err();
    assert!(
        caught,
        "screen line feeds settled one short of their count left what the line feeds leave"
    );
}

/// One flood stream's grid: a small ring over a tiered store whose staging the line feeds
/// overflow, with a line limit (`limit`) or none and a memory budget (`budget`) that may or may
/// not bind, compression handed off (`offload`) or drained on the parse thread.
fn flood_grid(rows: u16, cols: u16, limit: Option<usize>, budget: usize, offload: bool) -> Grid {
    let mut g = Grid::with_tiered_scrollback(rows, cols, 4, Scrollback::new(4, 64, budget));
    g.set_scrollback_line_limit(limit);
    g.set_compress_offload_active(offload);
    g
}

/// Which streams [`run_flood`] runs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Flood {
    /// No compression worker: a steady stream's store stays empty, as in the engine benchmark's
    /// live shape.
    Plain,
    /// The app's compression worker between reads, on every stream that hands compression off.
    Worker,
    /// [`Flood::Worker`]'s streams on rows about 200 columns wide in a budget of 10 to 14 MB,
    /// which staging past the floor outweighs: a shed past the cap can take rows for memory as
    /// well as lines, which opens a flood cut, and the wide lines it truncates from the store free
    /// enough memory that later runs fit within the headroom with the cut still open.
    WideWorker,
}

/// Run `streams` streams from `seed` that feed enough lines at the bottom of a full screen to
/// flood a tiered store's staging (past [`Grid::ASYNC_COMPRESS_BACKPRESSURE`] rows), mostly as
/// [`Op::ScreenText`] runs with some other steps between (a linked write, a BCE template, an
/// image, a scrolled-back view, compression handed off or taken back, and but for a
/// [`Flood::Plain`] run the app's compression worker on every stream that hands compression off),
/// on the fast paths and on the reference paths. The two grids must agree after every step on
/// everything [`observe`] sees, with the history read at its oldest and newest lines, and on the
/// whole history at the end. Returns how many lines the flood shed in all, or the first
/// divergence.
fn run_flood(seed: u64, streams: usize, flood: Flood) -> Result<u64, String> {
    let worker = flood != Flood::Plain;
    let mut rng = XorShift::new(seed);
    let link = Arc::new(HyperlinkData {
        url: Arc::from("https://example.com/"),
        id: None,
    });
    let image = Arc::new(ImageData {
        bytes: vec![7; 4],
        format: ImageFormat::Png,
        cols: 1,
        rows: 1,
        z_index: 0,
        band_lift_px: 0,
        cell_offset_px: (0, 0),
        scaling: ImageScaling::Fit,
        source_rect: None,
    });
    let mut shed = 0;
    for stream in 0..streams {
        let rows = u16::try_from(2 + rng.below(5)).unwrap_or(2);
        let cols = u16::try_from(3 + rng.below(8)).unwrap_or(3);
        // The worker's streams always cap staging by lines, so their sheds reach the store, and
        // half of them have the memory to defer their shed at the cap.
        let limit = (rng.below(2) == 0 || worker).then(|| 20_000 + rng.index(600));
        let budget =
            800_000 + rng.index(2_000_000) + if worker { 2_000_000 * rng.index(2) } else { 0 };
        // Wide rows draw nothing more, so they keep the worker streams' other choices.
        let (cols, budget) = if flood == Flood::WideWorker {
            (cols + 190, 10_000_000 + budget % 4_000_000)
        } else {
            (cols, budget)
        };
        let offload = rng.below(4) != 0;
        // A steady stream keeps compression handed off throughout, as a live session's reader
        // does, so its staging fills to the cap or the budget. Without the compression worker
        // its store stays empty, as in the engine benchmark's live shape; with it (every stream
        // of a `worker` run that hands compression off is steady, as a session is), the worker
        // moves staged lines into the store between reads, so runs start over a store that holds
        // lines and their sheds empty it.
        let steady = offload && (worker || rng.below(2) == 0);
        let worker = worker && offload;
        let mut fast = flood_grid(rows, cols, limit, budget, offload);
        let mut slow = on_reference_paths(|| flood_grid(rows, cols, limit, budget, offload));
        let (mut fast_clocks, mut slow_clocks) = (Clocks::default(), Clocks::default());
        let mut ops = 0usize;
        let mut fed = 0usize;
        // Past the cap long enough for the worker's lines to be stored and then shed.
        let feed = Grid::ASYNC_COMPRESS_BACKPRESSURE + if worker { 26_000 } else { 6_000 };
        while fed < feed {
            // No step allocates a dense ring (a true-colour write or BCE fill), which the
            // extras keep until a resize: the fast path would take none of the stream after it.
            let op = match rng.below(40) {
                0 if !steady => Op::Offload(rng.below(3) != 0),
                1 => Op::Template(rng.below(2) as u8),
                2 => Op::Styled(vec![b'k'; 1 + rng.index(2)], 2 + rng.below(2) as u8),
                3 => Op::View(1 + rng.below(3) as i32),
                4 => Op::ClearDamage,
                5 => Op::ToRegionBottom,
                6 => Op::Cr,
                7 => Op::Image(1),
                8 | 9 if worker => Op::Worker([0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2][rng.index(12)]),
                _ => {
                    let lines = 1 + rng.index(600);
                    fed += lines;
                    screen_text(&mut rng, lines, usize::from(cols / 2) + 1, 7)
                }
            };
            step(&mut fast, &op, &link, &image);
            on_reference_paths(|| step(&mut slow, &op, &link, &image));
            ops += 1;
            let lines = fast.scrollback_lines();
            let ends = |g: &mut Grid| {
                let mut seen = observe_history(g, lines.saturating_sub(6)..lines);
                seen.history
                    .extend((0..lines.min(2)).map(|i| format!("{:?}", g.get_history_line(i))));
                seen
            };
            let mut seen = ends(&mut fast);
            let mut want = on_reference_paths(|| ends(&mut slow));
            let coalesced = matches!(op, Op::ScreenText(_));
            fast_clocks.step(&mut seen, coalesced);
            slow_clocks.step(&mut want, coalesced);
            if seen != want {
                return Err(format!(
                    "flood stream {stream} ({rows}x{cols}, limit {limit:?}, budget {budget}) \
                     diverged after op {ops} ({op:?})\nfast paths: {seen:#?}\n\
                     reference paths: {want:#?}"
                ));
            }
            if fast.truncated_lines() != slow.truncated_lines() {
                return Err(format!(
                    "flood stream {stream}: the fast paths counted {} lost lines, the reference \
                     paths {}",
                    fast.truncated_lines(),
                    slow.truncated_lines()
                ));
            }
        }
        let (seen, want) = (
            observe(&mut fast),
            on_reference_paths(|| observe(&mut slow)),
        );
        if seen.history != want.history {
            return Err(format!(
                "flood stream {stream} ({rows}x{cols}, limit {limit:?}, budget {budget}): the \
                 histories differ after {ops} ops"
            ));
        }
        shed += fast.truncated_lines();
    }
    Ok(shed)
}

#[test]
fn screen_text_lines_match_the_reference_paths_through_a_flood() {
    let _ = crate::test_counters::take_screen_text_runs();
    let _ = crate::test_counters::take_screen_text_flushes();
    let shed = run_flood(0xF1_00D5_2026, 10, Flood::Plain)
        .unwrap_or_else(|divergence| panic!("{divergence}"));
    assert!(
        shed > 3_000,
        "the streams must flood the staging past its floor: {shed} lines shed"
    );
    let [runs, lines] = crate::test_counters::take_screen_text_runs();
    assert!(
        runs > 300 && lines > 60_000,
        "the fast path must take a good share of the floods' lines: {runs} runs, {lines} lines"
    );
    let [batched, deferred, ..] = crate::test_counters::take_screen_text_flushes();
    assert!(
        batched > 300 && deferred > 30_000,
        "the runs must defer their line feeds' shed through the floods: {batched} flushes of \
         more than one, {deferred} line feeds in them"
    );
}

#[test]
fn screen_text_lines_match_the_reference_paths_over_a_store_the_worker_fills() {
    let _ = crate::test_counters::take_screen_text_flushes();
    run_flood(0x57_04ED_2026, 10, Flood::Worker)
        .unwrap_or_else(|divergence| panic!("{divergence}"));
    let [batched, _, over_store, emptied] = crate::test_counters::take_screen_text_flushes();
    assert!(
        over_store > 300 && emptied > 3,
        "the runs must defer their shed over a store the worker filled, and some flushes' shed \
         must empty it: {batched} flushes of more than one, {over_store} of them over stored \
         lines, {emptied} emptying the store"
    );
}

#[test]
fn screen_text_lines_match_the_reference_paths_through_a_flood_cut_over_wide_rows() {
    let _ = crate::test_counters::take_screen_text_cut_refusals();
    run_flood(0x57_04ED_2026, 10, Flood::WideWorker)
        .unwrap_or_else(|divergence| panic!("{divergence}"));
    let refused = crate::test_counters::take_screen_text_cut_refusals();
    assert!(
        refused > 5,
        "the wide rows' floods must open a cut while staging fits within the store's memory \
         headroom, where the open cut alone keeps a run's line feeds shedding one at a time: \
         {refused} runs' deferrals refused for it"
    );
}

#[test]
fn the_flood_differential_catches_a_batch_that_ignores_an_open_cut() {
    let caught = crate::test_counters::with_screen_text_batch_ignoring_cut(|| {
        run_flood(0x57_04ED_2026, 10, Flood::WideWorker)
    })
    .is_err();
    assert!(
        caught,
        "a run that deferred its shed with a flood cut open left what the line feeds leave"
    );
}

#[test]
fn the_flood_differential_catches_a_batch_that_ignores_the_memory_headroom() {
    let caught = crate::test_counters::with_screen_text_batch_ignoring_headroom(|| {
        run_flood(0xF1_00D5_2026, 10, Flood::Plain)
    })
    .is_err();
    assert!(
        caught,
        "a run that deferred its shed past the store's memory headroom left what the line feeds \
         leave"
    );
}

#[test]
fn the_differential_catches_a_screen_text_line_one_slot_up() {
    let caught =
        crate::test_counters::with_screen_text_slot_one_up(|| run(0x5C_D1FF_2026, 200)).is_err();
    assert!(
        caught,
        "screen text lines written one ring slot above their row left what the line feeds leave"
    );
}
