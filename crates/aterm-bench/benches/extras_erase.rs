// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
// Extras ERASE / SCROLL throughput (ROADMAP WS-K, ATERM_DESIGN §7).
//
// WHY THIS EXISTS: `ComplexCharRing::clear_range`, `RgbColorRing::clear_range`
// and the `CellExtras::clear_row` lane are the three remaining
// `extra_collection.rs` micro-optimizations from the audit that 438ff33d
// reverted, and they were reverted because the BUNDLE cost 1.83 % on
// `engine_throughput/sgr` while NO benchmark in the tree reached any of them.
// `engine_throughput`'s three corpora never issue an erase at all, and
// `hyperlink_screen`'s three never leave the write path. So each of these
// functions could only ever be argued from a caller chain — which is exactly how
// a 1.83 % regression gets shipped. This file prices them instead.
//
// WHAT REACHES WHAT. Read off `grid/erase.rs`, `grid/line_ops.rs`,
// `grid/scroll.rs` and `extra_collection.rs`, then CONFIRMED by counting calls
// inside the three functions with a throwaway patch (the counts below are the
// observed ones) — because the obvious guesses are wrong in both directions:
//
//   ESC[K, ESC[0K   EL-0  -> erase_to_end_of_line   -> CellExtras::clear_range
//                                                   -> RgbColorRing::clear_range
//   ESC[1K          EL-1  -> erase_from_start_of_line        -> same
//   ESC[nX          ECH   -> Grid::erase_chars               -> same
//   ESC[J / ESC[1J  ED-0/1-> one clear_range for the cursor row (measured: 1
//                            call, 121 columns, from a cursor at column 40),
//                            then clear_rows -> RgbColorRing::clear_row per row
//                            (measured: 25 for the rows below).
//   ESC[2K          EL-2  -> Grid::erase_line -> CellExtras::clear_row
//                            -> RgbColorRing::clear_ROW + a whole-map `retain`.
//                            A DIFFERENT function from clear_range, with a
//                            different cost — hence its own workload. With
//                            DECLRMM margins armed and the cursor inside them it
//                            switches lanes entirely (measured: one 101-column
//                            `clear_range`, no `clear_row` at all).
//   ESC[t;l;b;r$z   DECERA-> Grid::erase_rect -> CellExtras::clear_rect
//                            -> ComplexCharRing::clear_range (per row)
//                            +  RgbColorRing::clear_range (per row)
//   ESC[..$x / $v   DECFRA / DECCRA -> the same clear_rect lane
//   any glyph on the per-character write path (a wide/complex char, or anything
//                            written while an OSC 8 link or a transient extra is
//                            open) -> Grid::write_char_at_cursor_packed ->
//                            `clear_rgb_ring_cell` -> RgbColorRing::clear_range
//                            with a ONE- or TWO-column span. Measured: 78 emoji
//                            under a truecolor SGR = 78 calls, 156 columns. This
//                            is the highest-RATE, shortest-SPAN caller in the
//                            engine and it is why `truecolor_field_update`
//                            exists — a slice fill that wins on 160 columns can
//                            still lose the engine 1 % on a span of 2.
//
// Three results that changed what this file had to be, each of which a benchmark
// written from the obvious guess would have got wrong:
//
//   * ESC[2J — "full-screen erase", the first thing anyone reaches for — takes
//     `Grid::erase_screen`, which calls `CellExtras::clear()`. That is a whole-
//     map replace plus `ring.clear()` (one `fill` over the entire plane).
//     Measured on a saturated 50x160 screen: `CellExtras::clear()` once, and
//     ZERO calls to any of the three functions under test. A `\x1b[2J`-based
//     workload would have measured nothing while looking perfectly convincing.
//
//   * EL / ED / ECH never touch the COMPLEX-char ring. `CellExtras::clear_row`,
//     `clear_range` and `clear_rows` clear `rgb_ring` only — stale complex
//     entries are harmless because every reader gates on `CellFlags::COMPLEX`
//     first. `ComplexCharRing::clear_range` is reachable ONLY from the
//     rectangular family (DECERA/DECFRA/DECSERA/DECCRA, via `clear_rect`) and
//     from a DECLRMM-margined rect scroll. Its call rate in ordinary terminal
//     output — cat, a TUI repaint, a shell prompt — is exactly ZERO, which is
//     itself a pricing answer for that change.
//
//   * Scrolling does not reach `clear_range` either, except in one exotic form.
//     Measured on a saturated screen: three LFs at the bottom row = 0 calls to
//     all three (a full-screen scroll takes the O(1) `ring.scroll_up` lane, which
//     fills the recycled rows itself); `CSI 3 S` inside a DECSTBM region = 3
//     `RgbColorRing::clear_ROW` and no `clear_range`; only `CSI 3 S` under
//     DECLRMM left/right margins reaches it (3 complex + 3 RGB `clear_range`,
//     101 columns each, via `shift_rect_up_by` -> `shift_rings_rect_up`). That
//     last path first copies every cell of the region through a staging buffer,
//     so the ring clear is a rounding error on its own caller — a workload there
//     could not price A or B, only pretend to. Hence no scroll workload.
//
// Full-width vs partial-width really is a different lane, and not the way the
// names suggest: EL-2 (the whole line) goes to `clear_row`, EL-0 from column 0
// covers the identical columns but goes to `clear_range(row, 0, cols)`. The
// span is not what picks the lane — the OPERATION is. Both are covered.
//
// THE FOUR WORKLOADS, ordered realism-first. Every number below was counted
// inside the target function on the corpus this file builds, not estimated:
//
//   truecolor_repaint       btop/htop: CUP + EL-0 + a 24-bit repaint, per row,
//                           forever. The commercially honest one.
//                           MEASURED per corpus: 3_049 `RgbColorRing::clear_range`
//                           calls covering 487_840 columns — a mean span of
//                           exactly 160.0, i.e. every one is a full-width row —
//                           over an RGB ring saturated at 7_500 of 8_000 cells,
//                           with the extras map empty throughout. (3_050
//                           `CellExtras::clear_range` calls: the first EL-0
//                           arrives before any truecolor has allocated the ring.)
//
//   truecolor_field_update  The same dashboard repainting only what CHANGED —
//                           CUP, ECH the field, rewrite it.
//                           MEASURED: 15_000 `RgbColorRing::clear_range` calls
//                           (12_000 ECH + 3_000 EL-1) covering 252_000 columns,
//                           a mean span of 16.8. Five times the call rate of
//                           `truecolor_repaint` at a tenth of the span, into the
//                           same saturated ring (peak 7_428 cells). This is where
//                           a memset's fixed setup cost is exposed and a slice
//                           `fill` can LOSE to the scalar loop it replaced, so a
//                           change measured only on `truecolor_repaint` would be
//                           priced on its best case.
//
//   emoji_pane_clear        The only shape that reaches change A at all: an
//                           emoji canvas whose panes are cleared with DECERA
//                           each frame.
//                           MEASURED: 724 DECERAs driving 18_000
//                           `ComplexCharRing::clear_range` calls over 1_440_000
//                           columns (mean span 80.0), into a complex-char ring
//                           holding 3_900 codepoints — the ceiling for width-2
//                           glyphs, which store their codepoint in the lead
//                           column only. Zero RGB-ring calls and zero map
//                           entries: the rect erase here is unambiguously the
//                           complex ring's cost and nothing else's.
//
//   status_line_redraw      `\r\x1b[2K` + rewrite: the prompt/progress-bar
//                           redraw every shell, `cargo`, `pip` and `tqdm` does
//                           on every keystroke or tick. Isolates the `clear_row`
//                           lane from `clear_range`.
//                           MEASURED: 4_000 `CellExtras::clear_row` calls, each
//                           `retain`ing over a 1_600-entry map — 6_400_000 map
//                           entries walked per corpus — plus 4_000
//                           `RgbColorRing::clear_row` calls into a ring holding
//                           7_600 truecolor cells, and ZERO `clear_range` calls
//                           of either kind. That zero is the isolation: whatever
//                           this workload moves, it did not move through the
//                           other three workloads' function.
//
// `verify_reaches_target` is the load-bearing part, and it does three things per
// workload rather than one: it counts the reaching escapes in the corpus bytes
// (so a corpus edit that drops them fails loudly), it samples the live engine
// state the workload claims to build, and it runs a LANE WITNESS — a few hundred
// bytes through a throwaway engine that proves this workload's exact escape
// shape lands in the intended function, observed from outside via the public
// accessors. The witnesses are written so that a cell OUTSIDE the erased span
// must SURVIVE: that is what separates `clear_range` from `clear_row` from
// `CellExtras::clear()`, all three of which would satisfy a naive "the data is
// gone" assertion.

use std::fmt::Write as _;

use criterion::{BenchmarkId, Criterion, Throughput, black_box, criterion_group, criterion_main};

const CORPUS_BYTES: usize = 1 << 20; // 1 MiB per workload, matching the siblings

/// A large-but-ordinary maximized window (a 4K display at a normal font size).
/// Big enough that a row-wide erase spans a realistic 160 columns, small enough
/// that both ring planes (`50 * 160 * 4 B` each) stay in L2 — so the benchmark
/// measures the clear, not a cache miss the clear did not cause.
const ROWS: u16 = 50;
const COLS: u16 = 160;

/// Columns actually painted per repainted row. Short of `COLS` on purpose: a row
/// filled to the last column arms the deferred wrap, and the next glyph would
/// scroll the screen, which would move `row_offset` and change which lane the
/// extras map takes. Every workload here is CUP-addressed and never scrolls.
const PAINT_COLS: u16 = 150;

/// Columns per SGR segment within a painted row (5 segments of 30 = 150). A real
/// TUI row is not one flat colour; each segment is its own bulk ASCII run, so
/// each one lands its own `set_range_uniform` -> `fill_fg_run`/`fill_bg_run`.
const SEG_COLS: u16 = 30;
const SEGS_PER_ROW: u16 = PAINT_COLS / SEG_COLS;

/// ECH field widths for `truecolor_field_update`. Four distinct widths, so the
/// workload cannot be over-fitted to one span length — and each is emitted as a
/// literal byte sequence, which is what makes the corpus-side count exact.
const ECH_WIDTHS: [u16; 4] = [8, 12, 16, 24];

/// Field-delta passes per full repaint in `truecolor_field_update`. A dashboard
/// repaints wholesale on a resize and repaints VALUES every tick, so the delta
/// traffic must dominate the corpus — with one pass per repaint the full paint
/// ate more than half the bytes and dragged the ECH rate down to the level of
/// `truecolor_repaint`'s EL-0 rate, which would have made the two workloads
/// measure the same thing.
const DELTA_PASSES_PER_CYCLE: usize = 4;

/// Emoji block used for the complex-char ring: U+1F600..U+1F64F (Emoticons) are
/// all Emoji_Presentation, hence uniformly width 2, and carry no combining,
/// regional-indicator or skin-tone behaviour that would divert the write path
/// away from `write_emoji_autowrap_fast` -> `set_complex_char_*` (the ring).
const EMOJI_BASE: u32 = 0x1F600;
const EMOJI_SPAN: u32 = 0x50;

/// 78 emoji = 156 columns, two short of `COLS` for the same no-wrap reason as
/// `PAINT_COLS`. This is the densest the complex-char ring can be: a width-2
/// glyph stores its codepoint in the LEAD column only, so 78 is the per-row max.
const EMOJI_PER_ROW: u16 = 78;

/// Rows of `status_line_redraw` painted with SGR 58 coloured underlines. Those
/// are the cheapest ordinary way to put entries in the extras MAP (they defeat
/// `set_range_uniform`'s RGB-only ring fast path), and the map is what
/// `CellExtras::clear_row`'s `retain` walks. 10 * 160 = 1_600 entries — a lint
/// or diff pane's worth of wavy underlines, not a synthetic stress value.
const UNDERLINE_ROWS: u16 = 10;

/// Status-line redraws per screen cycle in `status_line_redraw`. A progress bar
/// at 60 fps redraws for several seconds between full screen repaints; 200 keeps
/// the EL-2 traffic dominant without letting the setup rows vanish from the mix.
const REDRAWS_PER_CYCLE: usize = 200;

/// Granularity of the state sampling in `verify_reaches_target`. Coarser than
/// `hyperlink_screen`'s 1 KiB because each sample scans the whole 8_000-cell
/// grid through the public accessors; 64 KiB gives 16 samples per MiB, which is
/// plenty to catch a peak that persists for a whole frame (~16 KiB).
const SAMPLE_CHUNK: usize = 64 * 1024;

/// Printable filler. No `X`, no ESC: the ECH counts in `verify_reaches_target`
/// match literal `\x1b[<n>X` byte sequences, and filler that could spell one
/// would make those counts lie.
const FILLER: &[u8] = b"abcdefghijklmnopqrstuvwzy0123456789 .-+#*/|:";

/// Deterministic 24-bit colour from a counter. Not random — the corpus must be
/// byte-identical run to run — but spread enough that consecutive segments do
/// not collapse into one SGR the engine could cache away.
fn rgb(n: u32) -> (u8, u8, u8) {
    (
        (n.wrapping_mul(83) & 0xFF) as u8,
        ((n.wrapping_mul(151) >> 3) & 0xFF) as u8,
        ((n.wrapping_mul(37) >> 1) & 0xFF) as u8,
    )
}

/// Append `len` columns of deterministic printable filler.
fn push_filler(out: &mut String, len: u16, seed: u32) {
    for i in 0..u32::from(len) {
        let idx = (seed.wrapping_add(i) as usize) % FILLER.len();
        out.push(FILLER[idx] as char);
    }
}

/// Append a 24-bit fg+bg SGR pair — the `\x1b[38;2;R;G;Bm` shape every modern
/// TUI emits (btop, fzf, delta, bat, vim with termguicolors).
fn push_truecolor(out: &mut String, n: u32) {
    let (r, g, b) = rgb(n);
    let (r2, g2, b2) = rgb(n.wrapping_add(97));
    let _ = write!(out, "\x1b[38;2;{r};{g};{b}m\x1b[48;2;{r2};{g2};{b2}m");
}

/// Paint one full row: CUP to its column 1, then `SEGS_PER_ROW` truecolor
/// segments. Leaves the SGR state coloured; callers reset when they need a
/// non-BCE erase (see `corpus_truecolor_repaint`).
fn push_painted_row(out: &mut String, row: u16, n: &mut u32) {
    let _ = write!(out, "\x1b[{row};1H");
    for _ in 0..SEGS_PER_ROW {
        push_truecolor(out, *n);
        push_filler(out, SEG_COLS, *n);
        *n = n.wrapping_add(1);
    }
}

/// Append one row of emoji at default colours.
///
/// Default colours are load-bearing: with a truecolor SGR active,
/// `style.has_style_extras()` is true, `write_unicode_bulk` falls back to the
/// per-character path, and each emoji then lands a HashMap entry instead of a
/// dense-ring codepoint — which would move this workload off the very ring it
/// exists to populate.
fn push_emoji_row(out: &mut String, row: u16, n: &mut u32) {
    let _ = write!(out, "\x1b[{row};1H");
    for _ in 0..EMOJI_PER_ROW {
        let cp = EMOJI_BASE + (*n % EMOJI_SPAN);
        out.push(char::from_u32(cp).expect("emoticon block is valid scalar values"));
        *n = n.wrapping_add(1);
    }
}

/// What a workload must be verified to have reached before it is timed.
enum Target {
    /// `RgbColorRing::clear_range` over full-width (160-column) spans, EL-0.
    RgbRangeWide,
    /// `RgbColorRing::clear_range` over short (8-24 column) spans, ECH + EL-1.
    RgbRangeShort,
    /// `ComplexCharRing::clear_range`, DECERA.
    ComplexRange,
    /// `CellExtras::clear_row` + `RgbColorRing::clear_row`, EL-2.
    RowLane,
}

struct Workload {
    name: &'static str,
    target: Target,
    corpus: Vec<u8>,
}

/// btop / htop / fzf: a full-screen 24-bit repaint, row by row, forever.
///
/// Each row is `CUP` + `EL-0` + a coloured rewrite, which is the canonical
/// ncurses-style repaint: park the cursor at column 1, clear what was there,
/// draw the new line. The EL-0 at column 0 covers the whole row but goes through
/// `CellExtras::clear_range(row, 0, 160)`, NOT `clear_row` — the operation picks
/// the lane, not the span.
///
/// The `\x1b[0m` before each row's EL is deliberate: with a truecolor background
/// still active the erase would take `fill_bce_rgb_range` and REPOPULATE the ring
/// it just cleared, which is a real behaviour (BCE) but would double the ring
/// traffic per erase and blur what is being priced. Reset first, so each EL-0 is
/// a pure `clear_range`.
fn corpus_truecolor_repaint() -> Vec<u8> {
    let mut out = String::with_capacity(CORPUS_BYTES + 1024);
    let mut n: u32 = 0;
    while out.len() < CORPUS_BYTES {
        for row in 1..=ROWS {
            let _ = write!(out, "\x1b[{row};1H\x1b[0m\x1b[K");
            push_painted_row(&mut out, row, &mut n);
        }
    }
    out.into_bytes()
}

/// The same dashboard between full repaints: only the fields whose VALUES
/// changed are rewritten — CUP to the field, ECH its width, draw the new value.
///
/// This is the short-span lane. `RgbColorRing::clear_range` runs ~5x as often as
/// in `truecolor_repaint` over spans an order of magnitude shorter, so the
/// per-call setup (the `min`, the `ring_row` derivation, two plane length checks)
/// is a much larger share of the call than the bytes it clears. A slice `fill`
/// can lose here while winning on wide spans, which is precisely the asymmetry
/// that made the ORIGINAL shape of the hyperlink-limit fix unshippable (2.77 %
/// faster on one path, 8.07 % slower on another), so this lane exists to stop
/// the same mistake being repeated on these two.
///
/// One full-screen paint opens each cycle so the ring is saturated before the
/// deltas start, then `DELTA_PASSES_PER_CYCLE` passes of field updates — a real
/// dashboard repaints wholesale rarely (a resize, a tab switch) and repaints
/// values constantly. `\x1b[1K` (EL-1, the other `clear_range` caller) rides
/// along once per row for coverage of the from-start-of-line bound.
fn corpus_truecolor_field_update() -> Vec<u8> {
    let mut out = String::with_capacity(CORPUS_BYTES + 4096);
    let mut n: u32 = 0;
    while out.len() < CORPUS_BYTES {
        // Full repaint: saturates the RGB ring (this is the state the deltas
        // below erase into).
        for row in 1..=ROWS {
            push_painted_row(&mut out, row, &mut n);
        }
        out.push_str("\x1b[0m");
        // Field deltas: four fields per row, walking down the screen.
        for _ in 0..DELTA_PASSES_PER_CYCLE {
            for row in 1..=ROWS {
                for (i, width) in ECH_WIDTHS.iter().enumerate() {
                    let col = 8 + (i as u16) * 34;
                    let _ = write!(out, "\x1b[{row};{col}H\x1b[{width}X");
                    push_truecolor(&mut out, n);
                    push_filler(&mut out, *width, n);
                    out.push_str("\x1b[0m");
                    n = n.wrapping_add(1);
                }
                // EL-1 at a fixed column: the erase-from-start-of-line bound,
                // whose span is [0, cursor+1) rather than [cursor, cols).
                let _ = write!(out, "\x1b[{row};24H\x1b[1K");
            }
        }
    }
    out.into_bytes()
}

/// An emoji canvas with DECERA pane clears — the only realistic shape that
/// reaches `ComplexCharRing::clear_range` at all.
///
/// Per frame: clear the four quadrant panes (`CSI t;l;b;r $ z`), then repaint
/// part of the canvas. A pane-based TUI that erases its regions before drawing
/// them is ordinary; what is NOT ordinary is reaching this function from
/// anything else, which is why the corpus leans on DECERA rather than dressing
/// up an EL/ED workload that would quietly miss the target entirely.
///
/// Every fifth frame repaints all `ROWS` rows, so the ring is re-saturated
/// periodically and the erases keep landing on live codepoints rather than on
/// slots the previous erase already zeroed.
fn corpus_emoji_pane_clear() -> Vec<u8> {
    let mut out = String::with_capacity(CORPUS_BYTES + 4096);
    let mut n: u32 = 0;
    let mut frame: u32 = 0;
    let half_row = ROWS / 2;
    let half_col = COLS / 2;
    while out.len() < CORPUS_BYTES {
        // Four quadrant DECERAs: rows [1, 25] / [26, 50] x cols [1, 80] / [81, 160].
        for (top, bottom) in [(1, half_row), (half_row + 1, ROWS)] {
            for (left, right) in [(1, half_col), (half_col + 1, COLS)] {
                let _ = write!(out, "\x1b[{top};{left};{bottom};{right}$z");
            }
        }
        let rows_this_frame = if frame.is_multiple_of(5) { ROWS } else { 10 };
        for row in 1..=rows_this_frame {
            push_emoji_row(&mut out, row, &mut n);
        }
        frame = frame.wrapping_add(1);
    }
    out.into_bytes()
}

/// `\r\x1b[2K` + rewrite: the prompt / progress-bar redraw.
///
/// Every shell redraws its prompt line this way on each keystroke, and every
/// progress bar (cargo, pip, tqdm, curl) redraws its line this way on each tick.
/// EL-2 is a DIFFERENT function from EL-0 — `CellExtras::clear_row`, which
/// clears the RGB ring row with one `fill` and then `retain`s over the ENTIRE
/// grid-wide extras map to drop that row's keys. The map is what makes the
/// retain cost anything, so the screen behind the status line carries
/// `UNDERLINE_ROWS` rows of SGR 58 coloured underlines: those defeat
/// `set_range_uniform`'s RGB-only ring fast path and land 1_600 real map entries,
/// the way a lint/diff pane with wavy underlines does.
///
/// The status line itself is painted in truecolor, so its ring row is genuinely
/// populated when the next EL-2 clears it.
fn corpus_status_line_redraw() -> Vec<u8> {
    let mut out = String::with_capacity(CORPUS_BYTES + 4096);
    let mut n: u32 = 0;
    while out.len() < CORPUS_BYTES {
        // Screen setup: underline-coloured rows (map entries) then truecolor
        // rows (ring entries), so both stores are populated behind the redraws.
        for row in 1..=UNDERLINE_ROWS {
            let color = 17 + u32::from(row) % 215;
            let _ = write!(out, "\x1b[{row};1H\x1b[4m\x1b[58;5;{color}m");
            push_filler(&mut out, COLS, n);
            n = n.wrapping_add(1);
        }
        out.push_str("\x1b[0m");
        for row in (UNDERLINE_ROWS + 1)..ROWS {
            push_painted_row(&mut out, row, &mut n);
        }
        // The redraw loop: park on the last row, erase the whole line, rewrite.
        for _ in 0..REDRAWS_PER_CYCLE {
            let _ = write!(out, "\x1b[{ROWS};1H\x1b[0m\x1b[2K");
            push_truecolor(&mut out, n);
            push_filler(&mut out, PAINT_COLS, n);
            n = n.wrapping_add(1);
        }
    }
    out.into_bytes()
}

fn workloads() -> Vec<Workload> {
    vec![
        Workload {
            name: "truecolor_repaint",
            target: Target::RgbRangeWide,
            corpus: corpus_truecolor_repaint(),
        },
        Workload {
            name: "truecolor_field_update",
            target: Target::RgbRangeShort,
            corpus: corpus_truecolor_field_update(),
        },
        Workload {
            name: "emoji_pane_clear",
            target: Target::ComplexRange,
            corpus: corpus_emoji_pane_clear(),
        },
        Workload {
            name: "status_line_redraw",
            target: Target::RowLane,
            corpus: corpus_status_line_redraw(),
        },
    ]
}

/// Count non-overlapping-by-start occurrences of `needle` in `hay`.
///
/// Used on the CORPUS BYTES, not on engine state: it pins how many times the
/// reaching escape is actually in the stream, so an innocent edit to a corpus
/// builder that drops the erase (or renames it to a shape that takes another
/// lane) fails the guard instead of quietly measuring a plain repaint.
fn count_seq(hay: &[u8], needle: &[u8]) -> usize {
    if needle.is_empty() || hay.len() < needle.len() {
        return 0;
    }
    hay.windows(needle.len()).filter(|w| *w == needle).count()
}

/// (cells resolving a truecolor fg or bg, cells resolving a complex codepoint).
///
/// These accessors consult the dense rings FIRST and the extras map second, so
/// on the three workloads whose map is empty they are an exact ring census; on
/// `status_line_redraw` they are an upper bound on the ring, which is why that
/// workload's guard bounds the MAP separately rather than leaning on this.
fn ring_population(term: &aterm_core::terminal::Terminal) -> (usize, usize) {
    let extras = term.grid().extras();
    let mut rgb_cells = 0usize;
    let mut complex_cells = 0usize;
    for row in 0..ROWS {
        for col in 0..COLS {
            if extras.fg_rgb_for(row, col).is_some() || extras.bg_rgb_for(row, col).is_some() {
                rgb_cells += 1;
            }
            if extras.complex_codepoint_for(row, col).is_some() {
                complex_cells += 1;
            }
        }
    }
    (rgb_cells, complex_cells)
}

/// LANE WITNESS for `RgbColorRing::clear_range`: EL-0 issued from column 2 must
/// wipe the ring from column 2 rightward and LEAVE COLUMNS 0-1 INTACT.
///
/// The surviving prefix is the whole point. "The colour is gone" is equally true
/// of `clear_row`, of `CellExtras::clear()` (ESC[2J) and of a plain overwrite;
/// only a column-bounded clear can take column 2 and spare column 0. Same
/// argument for ECH, which is `clear_range(row, col, col + n)`: the cells past
/// `col + n` must survive.
fn witness_rgb_clear_range(name: &str) {
    let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
    term.process(b"\x1b[1;1H\x1b[38;2;10;20;30m\x1b[48;2;40;50;60mabcdefgh");
    let before = term.grid().extras().fg_rgb_for(0, 4).is_some();
    assert!(
        before,
        "{name}: a truecolor ASCII run did not populate the RGB ring — the bulk \
         path (`set_range_uniform` -> `set_rgb_ring_range`) has moved, and this \
         workload's erases would be clearing an empty plane"
    );

    // EL-0 from column 2 (CUP is 1-indexed): clear_range(0, 2, COLS).
    term.process(b"\x1b[0m\x1b[1;3H\x1b[K");
    let extras = term.grid().extras();
    assert!(
        extras.fg_rgb_for(0, 0).is_some(),
        "{name}: EL-0 from column 2 also wiped column 0 — that is the clear_ROW \
         or `CellExtras::clear()` lane, not `RgbColorRing::clear_range`"
    );
    assert!(
        extras.fg_rgb_for(0, 4).is_none() && extras.bg_rgb_for(0, 4).is_none(),
        "{name}: EL-0 left the RGB ring populated inside the erased span — \
         `CellExtras::clear_range` is no longer reached from `erase_to_end_of_line`"
    );

    // ECH: the bounded form, whose right edge must also hold.
    term.process(b"\x1b[38;2;10;20;30m\x1b[2;1Habcdefghijklmnop\x1b[0m\x1b[2;3H\x1b[4X");
    let extras = term.grid().extras();
    assert!(
        extras.fg_rgb_for(1, 0).is_some() && extras.fg_rgb_for(1, 7).is_some(),
        "{name}: ECH cleared outside [col, col + n) — the span bound is gone"
    );
    assert!(
        extras.fg_rgb_for(1, 3).is_none(),
        "{name}: ECH did not clear the RGB ring inside its span — \
         `Grid::erase_chars` no longer calls `CellExtras::clear_range`"
    );
}

/// LANE WITNESS for `ComplexCharRing::clear_range`: a DECERA whose left edge is
/// column 3 must wipe the codepoints from column 2 (0-indexed) rightward within
/// the rect and leave column 0 holding its emoji.
///
/// This is also the standing proof of the header's second finding — the same
/// screen is hit with EL-2 first, which must NOT disturb the complex ring.
fn witness_complex_clear_range(name: &str) {
    let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
    let mut row = String::from("\x1b[1;1H");
    for i in 0..8u32 {
        row.push(char::from_u32(EMOJI_BASE + i).expect("valid emoji"));
    }
    term.process(row.as_bytes());
    assert!(
        term.grid().extras().complex_codepoint_for(0, 0).is_some()
            && term.grid().extras().complex_codepoint_for(0, 6).is_some(),
        "{name}: emoji did not land in the complex-char ring — the non-BMP write \
         path has moved off `set_complex_char_*` and this workload's DECERAs \
         would clear nothing"
    );

    // EL-2 on that row: clears the RGB ring row and the map, NEVER the complex
    // ring (`CellExtras::clear_row` touches `rgb_ring` only).
    term.process(b"\x1b[1;1H\x1b[2K");
    assert!(
        term.grid().extras().complex_codepoint_for(0, 6).is_some(),
        "{name}: EL-2 cleared the COMPLEX ring. That is new behaviour — this \
         file's claim that only the rect family reaches \
         `ComplexCharRing::clear_range` no longer holds, and the workload map at \
         the top of this file must be re-derived"
    );

    // DECERA over row 1, columns 3..6 (1-indexed) -> clear_range(0, 2, 6).
    term.process(b"\x1b[1;3;1;6$z");
    let extras = term.grid().extras();
    assert!(
        extras.complex_codepoint_for(0, 0).is_some(),
        "{name}: DECERA wiped outside its rect — this is not the bounded \
         `ComplexCharRing::clear_range` lane"
    );
    assert!(
        extras.complex_codepoint_for(0, 2).is_none()
            && extras.complex_codepoint_for(0, 4).is_none(),
        "{name}: DECERA did not clear the complex-char ring inside its rect — \
         `CellExtras::clear_rect` no longer reaches `ComplexCharRing::clear_range`"
    );
}

/// LANE WITNESS for the `clear_row` lane: EL-2 must take the WHOLE row —
/// column 0 included, which is exactly what EL-0-from-column-2 could not do —
/// out of BOTH the RGB ring and the extras map, while the row above survives.
fn witness_clear_row(name: &str) {
    let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
    // Row 1: map-resident extras (SGR 58 defeats the RGB-only ring fast path).
    // Row 2: ring-resident truecolor plus its own map entries.
    term.process(b"\x1b[1;1H\x1b[4m\x1b[58;5;120mabcdefgh\x1b[0m");
    term.process(b"\x1b[2;1H\x1b[4m\x1b[58;5;120m\x1b[38;2;10;20;30mabcdefgh\x1b[0m");
    let before = term.grid().extras().len();
    assert!(
        before >= 16,
        "{name}: SGR 58 no longer lands extras-map entries (map holds {before}), \
         so `CellExtras::clear_row`'s `retain` would have nothing to walk and \
         this workload would price an empty map"
    );

    term.process(b"\x1b[2;1H\x1b[2K");
    let extras = term.grid().extras();
    assert!(
        extras.fg_rgb_for(1, 0).is_none() && extras.fg_rgb_for(1, 7).is_none(),
        "{name}: EL-2 left RGB ring entries on the erased row — \
         `RgbColorRing::clear_row` is no longer reached"
    );
    let after = extras.len();
    assert!(
        after < before,
        "{name}: EL-2 removed no extras-map entries ({before} -> {after}) — \
         `CellExtras::clear_row`'s `retain` is no longer reached"
    );
    assert!(
        extras
            .get(aterm_grid::extra::CellCoord::new(0, 0))
            .is_some(),
        "{name}: EL-2 on row 2 also dropped row 1's extras — that is a whole-map \
         clear, not the single-row lane this workload prices"
    );
}

/// Prove the workload reaches the code it claims to, before it is timed.
///
/// Three independent checks, because any one of them alone is satisfiable by a
/// benchmark that measures nothing: the corpus really contains the reaching
/// escapes (byte count, bounded from both sides), the engine really builds the
/// state those escapes are supposed to erase (live ring/map census, bounded from
/// both sides), and the escape shape really lands in the target function (lane
/// witness, on a throwaway engine).
fn verify_reaches_target(w: &Workload) {
    let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
    let mut peak_rgb = 0usize;
    let mut peak_complex = 0usize;
    let mut peak_map = 0usize;
    for chunk in w.corpus.chunks(SAMPLE_CHUNK) {
        term.process(chunk);
        let (rgb_cells, complex_cells) = ring_population(&term);
        peak_rgb = peak_rgb.max(rgb_cells);
        peak_complex = peak_complex.max(complex_cells);
        peak_map = peak_map.max(term.grid().extras().len());
    }
    let cells = usize::from(ROWS) * usize::from(COLS);

    match w.target {
        Target::RgbRangeWide => {
            witness_rgb_clear_range(w.name);
            let el0 = count_seq(&w.corpus, b"\x1b[K");
            assert!(
                (2_000..8_000).contains(&el0),
                "{}: {el0} EL-0 escapes per MiB, outside the expected 2k..8k — \
                 the repaint shape changed and the erase rate this workload \
                 prices is not what its header claims",
                w.name
            );
            // BOTH bounds. A lower bound alone is satisfied by a screen that is
            // never erased (the ring only ever grows); an upper bound alone is
            // satisfied by an empty ring, i.e. by measuring nothing at all.
            assert!(
                peak_rgb >= cells * 3 / 4 && peak_rgb <= cells,
                "{}: RGB ring peaked at {peak_rgb} of {cells} cells, not the \
                 saturated screen this workload erases — is the truecolor bulk \
                 path still reaching `set_rgb_ring_range`?",
                w.name
            );
            assert_eq!(
                peak_map, 0,
                "{}: extras MAP is non-empty ({peak_map} entries). This workload \
                 must keep truecolor in the ring alone, or its erases are paying \
                 for a map walk that belongs to `status_line_redraw`",
                w.name
            );
        }
        Target::RgbRangeShort => {
            witness_rgb_clear_range(w.name);
            let ech: usize = ECH_WIDTHS
                .iter()
                .map(|width| count_seq(&w.corpus, format!("\x1b[{width}X").as_bytes()))
                .sum();
            let el1 = count_seq(&w.corpus, b"\x1b[1K");
            assert!(
                (8_000..40_000).contains(&ech),
                "{}: {ech} ECH escapes per MiB, outside the expected 8k..40k — \
                 the short-span lane is the point of this workload",
                w.name
            );
            assert!(
                el1 >= 1_000,
                "{}: only {el1} EL-1 escapes — the erase-from-start-of-line bound \
                 is no longer covered",
                w.name
            );
            assert!(
                peak_rgb >= cells * 3 / 4 && peak_rgb <= cells,
                "{}: RGB ring peaked at {peak_rgb} of {cells} cells; the field \
                 deltas must erase into a saturated ring, not an empty one",
                w.name
            );
            assert_eq!(
                peak_map, 0,
                "{}: extras MAP is non-empty ({peak_map} entries) — see \
                 `truecolor_repaint`",
                w.name
            );
        }
        Target::ComplexRange => {
            witness_complex_clear_range(w.name);
            let decera = count_seq(&w.corpus, b"$z");
            assert!(
                (200..4_000).contains(&decera),
                "{}: {decera} DECERA escapes per MiB, outside the expected \
                 200..4k — nothing else in the engine reaches \
                 `ComplexCharRing::clear_range`, so this count IS the workload",
                w.name
            );
            // Width-2 glyphs store their codepoint in the lead column only, so
            // the ceiling is half the grid; require most of it, and require the
            // count to stay under that ceiling (a higher number would mean the
            // emoji stopped being width 2 and the ring is being filled twice).
            let ceiling = cells / 2;
            assert!(
                peak_complex >= ceiling * 3 / 4 && peak_complex <= ceiling,
                "{}: complex-char ring peaked at {peak_complex} codepoints, \
                 outside the expected {} ..= {ceiling} — the DECERAs must land on \
                 a populated ring",
                w.name,
                ceiling * 3 / 4
            );
            assert_eq!(
                peak_rgb, 0,
                "{}: {peak_rgb} cells carry truecolor. This workload must never \
                 allocate the RGB ring, so that its rect erases price the COMPLEX \
                 ring alone",
                w.name
            );
            assert_eq!(
                peak_map, 0,
                "{}: extras MAP is non-empty ({peak_map} entries) — emoji have \
                 left the dense ring for the HashMap",
                w.name
            );
        }
        Target::RowLane => {
            witness_clear_row(w.name);
            let el2 = count_seq(&w.corpus, b"\x1b[2K");
            assert!(
                (2_000..20_000).contains(&el2),
                "{}: {el2} EL-2 escapes per MiB, outside the expected 2k..20k — \
                 the redraw rate this workload prices has changed",
                w.name
            );
            assert_eq!(
                count_seq(&w.corpus, b"\x1b[K"),
                0,
                "{}: an EL-0 crept into the corpus. This workload exists to \
                 isolate the clear_ROW lane from `clear_range`; mixing them makes \
                 its number unattributable",
                w.name
            );
            // BOTH bounds again: `>= 1_000` alone would be satisfied by a map so
            // large the retain dwarfs everything (a stress test, not a redraw),
            // and `<= 4_000` alone by an empty map, where the retain early-outs
            // on `data.is_empty()` and the workload measures nothing.
            let expected_map = usize::from(UNDERLINE_ROWS) * usize::from(COLS);
            assert!(
                peak_map >= expected_map * 3 / 4 && peak_map <= expected_map * 5 / 4,
                "{}: extras map peaked at {peak_map} entries, not the ~{expected_map} \
                 the SGR 58 rows should hold — `CellExtras::clear_row`'s `retain` \
                 walks this map, so its size IS the cost being priced",
                w.name
            );
            assert!(
                peak_rgb >= cells / 2,
                "{}: only {peak_rgb} of {cells} cells carry truecolor; the status \
                 row must be erased out of a populated `RgbColorRing`",
                w.name
            );
        }
    }
}

fn extras_erase(c: &mut Criterion) {
    let mut group = c.benchmark_group("extras_erase");
    for w in workloads() {
        verify_reaches_target(&w);
        group.throughput(Throughput::Bytes(w.corpus.len() as u64));
        group.bench_with_input(BenchmarkId::from_parameter(w.name), &w, |b, w| {
            b.iter(|| {
                // Fresh engine per iteration, as the sibling benches do: the
                // rings and the extras map are rebuilt from empty every time, so
                // one iteration is one honest "a TUI repaints at you for a MiB".
                let mut term = aterm_core::terminal::Terminal::new(ROWS, COLS);
                term.process(black_box(&w.corpus));
                black_box(&term);
            });
        });
    }
    group.finish();
}

criterion_group!(benches, extras_erase);
criterion_main!(benches);
