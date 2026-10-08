// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates
//
//! Panic-freedom fuzz for the VT engine on hostile terminal output.
//!
//! A terminal must never panic on the bytes a program writes to it — a crash on
//! `curl evil | cat`, a crafted log file, or a corrupt stream is a denial of
//! service for a daily driver. This pins ROBUSTNESS across the full byte space
//! (malformed UTF-8, the whole `0x00..=0xFF` range) and the structured
//! escape-sequence space (CSI/OSC/DCS/APC/PM with extreme or random params,
//! bodies, intermediates, and terminators), interleaved with resizes.
//! It asserts `process()` never panics AND core invariants hold (cursor in
//! bounds, dimensions positive, every cell accessible) after arbitrary input.
//!
//! Deterministic LCG so any failure reproduces exactly.

use aterm_core::terminal::Terminal;

/// One step of a deterministic LCG (same constants as the lz4 decode fuzz).
#[inline]
fn next(state: &mut u64) -> u32 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (*state >> 33) as u32
}

/// Invariants that must hold no matter what bytes were fed.
fn check_invariants(term: &Terminal) {
    let (rows, cols) = (term.rows(), term.cols());
    assert!(
        rows > 0 && cols > 0,
        "dimensions must stay positive, got {rows}x{cols}"
    );
    let cur = term.cursor();
    assert!(
        cur.row < rows,
        "cursor row {} escaped bounds (rows {rows})",
        cur.row
    );
    assert!(
        cur.col <= cols,
        "cursor col {} escaped bounds (cols {cols})",
        cur.col
    );
    // Wire in the engine's own formal invariants (grid/invariants.rs): cursor
    // bounds, scroll-region, ring-buffer structure, and wide-char pairing.
    term.grid().assert_structural_invariants();
    term.grid().assert_wide_char_consistent();
}

/// Every cell must be accessible without panic (corruption would index-panic).
fn walk_all_cells(term: &Terminal) {
    // `Grid::cell(r, c)` is `row(r)` then `get(c)`; resolving the row once
    // per row visits the same cells.
    let (rows, cols) = (term.rows(), term.cols());
    for r in 0..rows {
        let Some(row) = term.grid().row(r) else {
            continue;
        };
        for c in 0..cols {
            if let Some(cell) = row.get(c) {
                let _ = cell.char();
            }
        }
    }
}

#[test]
fn process_arbitrary_bytes_never_panics() {
    // The widest net: uniform random bytes across the full 0x00..=0xFF range
    // (malformed UTF-8, lone continuation bytes, stray ESC/CSI introducers).
    let mut s = 0xC0FF_EE12_3456_789Au64;
    let mut term = Terminal::new(24, 80);
    let mut buf: Vec<u8> = Vec::with_capacity(64);
    for _ in 0..100_000u32 {
        buf.clear();
        let len = (next(&mut s) % 48) as usize;
        for _ in 0..len {
            buf.push((next(&mut s) & 0xFF) as u8);
        }
        term.process(&buf);
        check_invariants(&term);
    }
    walk_all_cells(&term);
}

#[test]
fn process_crafted_escape_sequences_never_panics() {
    // Bias hard toward valid-looking ESC/CSI/OSC/DCS/APC/PM shapes so the fuzz
    // drives deep into the parser's parameter/string handling, where an
    // unchecked index or overflow on an attacker-chosen parameter would panic.
    let mut s = 0x0BAD_C0DE_99C0_1A57u64;
    let mut term = Terminal::new(24, 80);
    // Every `resize()` below reflows all retained history; at the default
    // 10,000-line ring that was ~2/3 of the run. Same cap as the other resize
    // fuzzers, which also lets the ring reach capacity and wrap.
    term.set_scrollback_line_limit(Some(RESIZE_FUZZ_SCROLLBACK_LINES));
    let mut buf: Vec<u8> = Vec::with_capacity(256);
    for _ in 0..100_000u32 {
        buf.clear();
        let seqs = 1 + next(&mut s) % 5;
        for _ in 0..seqs {
            emit_escape(&mut s, &mut buf);
        }
        term.process(&buf);
        check_invariants(&term);
        // Occasionally resize to exercise reflow against pathological state.
        if next(&mut s).is_multiple_of(64) {
            let r = (1 + next(&mut s) % 60) as u16;
            let c = (1 + next(&mut s) % 160) as u16;
            term.resize(r, c);
            check_invariants(&term);
        }
    }
    walk_all_cells(&term);
}

#[test]
fn process_unicode_edges_never_panics() {
    // Valid + truncated multi-byte UTF-8, wide CJK, emoji / ZWJ / regional
    // indicators / skin tones, combining marks, VS15/VS16 — the width, grapheme
    // and wrap paths, on a small grid so column-boundary wrap fires constantly.
    let mut s = 0xFEED_FACE_7717_2390u64;
    let mut term = Terminal::new(10, 12);
    let samples: &[&[u8]] = &[
        b"\xe6\x97\xa5",
        b"\xe6\x9c\xac", // CJK 日本
        b"\xf0\x9f\x9a\x80",
        b"\xf0\x9f\x91\x8d",    // 🚀 👍
        "\u{1f3fd}".as_bytes(), // skin-tone modifier
        "\u{200d}".as_bytes(),  // ZWJ
        "\u{1f1fa}".as_bytes(), // regional indicator U
        "\u{1f1f8}".as_bytes(), // regional indicator S
        "\u{0301}".as_bytes(),  // combining acute
        "\u{fe0f}".as_bytes(),  // VS16
        "\u{fe0e}".as_bytes(),  // VS15
        b"\xf0",                // truncated 4-byte lead
        b"\xe6\x97",            // truncated 3-byte
        b"\x80",                // lone continuation
        b"\xff",                // invalid
    ];
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    for _ in 0..100_000u32 {
        buf.clear();
        let n = 1 + next(&mut s) % 8;
        for _ in 0..n {
            let pick = (next(&mut s) as usize) % samples.len();
            buf.extend_from_slice(samples[pick]);
            if next(&mut s).is_multiple_of(5) {
                buf.push(b'\n');
            }
            if next(&mut s).is_multiple_of(7) {
                buf.push(b'\r');
            }
        }
        term.process(&buf);
        check_invariants(&term);
    }
    walk_all_cells(&term);
}

/// Deeper invariants for the reflow stress: everything `check_invariants` pins,
/// plus scrollback consistency (`total_lines >= visible rows`, the spec's
/// `TotalLinesMinimum`) and the offset-0 precondition of the strict cell walk.
/// All O(1); the resize fuzzers run them after every step, and
/// [`walk_visible_cells_strict`] (which calls this first) adds the O(rows x cols)
/// cell walk where a row can have just been rebuilt.
fn check_invariants_reflow(term: &Terminal) {
    check_invariants(term);
    let (rows, cols) = (term.rows(), term.cols());
    let total = term.grid().total_lines();
    assert!(
        total >= rows as usize,
        "total_lines {total} < visible rows {rows} ({rows}x{cols})"
    );
    // The cell walk (`walk_visible_cells_strict`) is an oracle with A precondition, so state it. `Grid::cell`
    // resolves through `GridStorage::row_index`, the ring accessor — it serves only
    // ring-resident rows and returns `None` for a viewport scrolled onto lazy or
    // tiered history (pinned by `grid/tests/style_perf.rs`
    // `row_returns_none_when_scrolled_into_tiered_scrollback`). The real viewport
    // reader is `visible_row_view`, whose bound is `scrollback_lines()`. So "every
    // visible cell is accessible" is a sound test only at offset 0, which is where
    // these fuzzers always sit: nothing they emit moves the viewport, and the batch
    // prologue re-pins only when it was already nonzero. Assert it rather than
    // assume it — without this line a viewport change elsewhere reads out as
    // "reflow corrupted the grid".
    assert_eq!(
        term.grid().display_offset(),
        0,
        "the cell oracle (walk_visible_cells_strict) is ring-only and therefore offset-0-only"
    );
}

/// Every visible cell must be accessible (a corrupt row index/length would
/// panic or return None here), and reading its char must not panic. Only sound
/// at display offset 0 — [`check_invariants_reflow`] asserts that first.
///
/// Where it runs: in `reflow_wide_char_resize_never_panics`, after every step
/// (process and resize) and at the end. The per-step walk is the only strict
/// row-width check that follows a `process()`, and it matters most once the
/// capped ring ([`RESIZE_FUZZ_SCROLLBACK_LINES`]) wraps: `scroll_up` then
/// recycles ring rows, and a recycled row of stale width shows up right after
/// a `process()` step, where a later width-changing resize could rebuild it
/// before a resize-only walk saw it. Measured cost on the capped test (debug
/// build, CPU user seconds, box at load 40-95): 16.5-17.6 s with the per-step
/// walk vs 19.6-20.7 s without it, i.e. below this box's noise; reflow of the
/// retained history, not this walk, is the long pole.
fn walk_visible_cells_strict(term: &Terminal) {
    check_invariants_reflow(term);
    // `Grid::cell(r, c)` is `row(r)` then `get(c)`: the row is resolved once
    // per row, and a missing row fails at its first cell as before.
    let (rows, cols) = (term.rows(), term.cols());
    for r in 0..rows {
        let row = term.grid().row(r);
        for c in 0..cols {
            let Some(cell) = row.and_then(|row| row.get(c)) else {
                panic!("cell ({r},{c}) inaccessible on a {rows}x{cols} grid");
            };
            let _ = cell.char();
        }
    }
}

/// Read every visible line plus a sampled scrollback window. This reconstructs
/// reflowed rows + history lines (slicing wide graphemes), so corruption from a
/// resize surfaces here as a panic or out-of-range slice rather than silently.
///
/// History is sampled, not walked: the 16 lines just above the screen, about 16
/// lines strided across the full history depth, the oldest line, and one past
/// it (the `None` boundary). The previous contiguous 256-line walk never
/// reached the ring-cap or oldest-line boundaries of a deeper history.
fn probe_lines(term: &Terminal) {
    let rows = i32::from(term.rows());
    for r in 0..rows {
        let _ = term.get_line_text(r, None);
    }
    let depth = term.grid().history_line_count();
    let recent = 0..depth.min(16);
    let strided = (0..depth).step_by((depth / 16).max(1));
    let edges = [depth.saturating_sub(1), depth];
    for rev in recent.chain(strided).chain(edges) {
        let rev = i32::try_from(rev).expect("history depth fits i32");
        let _ = term.get_line_text(-1 - rev, None);
    }
}

/// History cap for the resize fuzzers. Below the default ring cap, so the
/// ring reaches capacity (head wrap, oldest-line eviction) within the run; it
/// also bounds the cost of each `resize()`, which reflows all retained history.
const RESIZE_FUZZ_SCROLLBACK_LINES: usize = 1_000;

/// Wide-grapheme + autowrap + style content that makes reflow do real work:
/// CJK / emoji (width 2), ASCII runs, combining marks, colour SGR, newlines.
fn emit_reflow_content(s: &mut u64, buf: &mut Vec<u8>) {
    const WIDE: &[&[u8]] = &[
        "日".as_bytes(),
        "本".as_bytes(),
        "語".as_bytes(),
        "界".as_bytes(),
        "🚀".as_bytes(),
        "👍".as_bytes(),
    ];
    match next(s) % 6 {
        0 => buf.extend_from_slice(WIDE[(next(s) as usize) % WIDE.len()]),
        1 => {
            let n = 1 + next(s) % 12;
            for _ in 0..n {
                buf.push((0x41 + (next(s) % 26)) as u8);
            }
        }
        2 => buf.extend_from_slice(b"\r\n"),
        3 => {
            buf.extend_from_slice(b"\x1b[38;5;");
            for d in (next(s) % 256).to_string().bytes() {
                buf.push(d);
            }
            buf.push(b'm');
        }
        4 => buf.push(b'\n'),
        _ => {
            // base char + combining acute — the grapheme-cluster path.
            buf.push((0x61 + (next(s) % 26)) as u8);
            buf.extend_from_slice("\u{0301}".as_bytes());
        }
    }
}

/// Resize dimensions biased toward small + extreme shapes (1×1, 1×N, N×1, tiny)
/// that stress reflow's row/column buffers, plus a general range.
fn next_dim_pair(s: &mut u64) -> (u16, u16) {
    match next(s) % 8 {
        0 => (1, 1),
        1 => (1, 1 + (next(s) % 200) as u16),
        2 => (1 + (next(s) % 80) as u16, 1),
        3 => (1 + (next(s) % 4) as u16, 1 + (next(s) % 8) as u16),
        _ => (1 + (next(s) % 60) as u16, 1 + (next(s) % 200) as u16),
    }
}

#[test]
fn reflow_wide_char_resize_never_panics() {
    // The gap the other fuzzers leave: WIDE graphemes driven through aggressive
    // resize/reflow oscillation. A wide char straddling a shrunk column boundary
    // is the classic reflow corruption surface; this asserts panic-freedom and
    // the wide-continuation pairing and line-reconstruction across every resize.
    let mut s = 0x5EED_1234_ABCD_9001u64;
    let mut term = Terminal::new(24, 80);
    term.set_scrollback_line_limit(Some(RESIZE_FUZZ_SCROLLBACK_LINES));
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    for _ in 0..30_000u32 {
        buf.clear();
        let chunks = 1 + next(&mut s) % 10;
        for _ in 0..chunks {
            emit_reflow_content(&mut s, &mut buf);
        }
        term.process(&buf);
        walk_visible_cells_strict(&term);
        // Resize ~1/3 of iterations to drive reflow against wide-char state.
        if next(&mut s).is_multiple_of(3) {
            let (r, c) = next_dim_pair(&mut s);
            term.resize(r, c);
            walk_visible_cells_strict(&term);
            probe_lines(&term);
        }
    }
    walk_visible_cells_strict(&term);
}

/// A WideCharConsistent case replayed from bytes: `ESC # 6` makes the row
/// DECDWL, 45 cells then U+A0E2 (a wide Yi syllable) put a pair at 45/46, and a
/// resize 112 -> 46 truncates the row through that pair. The head in the new
/// last column must be blanked, never left wide without its continuation.
#[test]
fn a_decdwl_row_resized_through_its_wide_pair_stays_consistent() {
    let mut term = Terminal::new(4, 112);
    let mut bytes = b"\x1b#6".to_vec();
    bytes.extend(std::iter::repeat_n(b'a', 45));
    bytes.extend("\u{A0E2}".as_bytes());
    term.process(&bytes);
    term.grid().assert_wide_char_consistent();
    assert!(
        term.grid().is_wide_continuation_at(0, 46),
        "the pair is intact before the resize"
    );
    term.resize(4, 46);
    term.grid().assert_wide_char_consistent();
}

#[test]
fn alt_screen_resize_never_panics() {
    // Resizing while in the alternate screen (vim / htop / less) is a real,
    // common scenario with a distinct buffer + resize path the other fuzzers
    // never enter. Toggle DEC 1049 (alt screen + cursor save/restore) around
    // wide-char content and aggressive resizes; assert panic-freedom + the
    // structural invariants (via check_invariants) throughout the switches.
    let mut s = 0xA17E_5C9E_2026_0601u64;
    let mut term = Terminal::new(24, 80);
    term.set_scrollback_line_limit(Some(RESIZE_FUZZ_SCROLLBACK_LINES));
    let mut buf: Vec<u8> = Vec::with_capacity(128);
    let mut in_alt = false;
    for _ in 0..40_000u32 {
        buf.clear();
        if next(&mut s).is_multiple_of(8) {
            buf.extend_from_slice(if in_alt {
                b"\x1b[?1049l"
            } else {
                b"\x1b[?1049h"
            });
            in_alt = !in_alt;
        }
        let chunks = 1 + next(&mut s) % 8;
        for _ in 0..chunks {
            emit_reflow_content(&mut s, &mut buf);
        }
        term.process(&buf);
        check_invariants(&term);
        if next(&mut s).is_multiple_of(3) {
            let (r, c) = next_dim_pair(&mut s);
            term.resize(r, c);
            check_invariants(&term);
            probe_lines(&term);
        }
    }
    walk_all_cells(&term);
}

/// Append one (mostly well-formed) escape sequence with randomized contents.
fn emit_escape(s: &mut u64, buf: &mut Vec<u8>) {
    buf.push(0x1b); // ESC
    match next(s) % 9 {
        0 => {
            // CSI: optional private marker, random params (`;`/`:`-separated),
            // random intermediates, a final byte in 0x40..=0x7e.
            buf.push(b'[');
            if next(s).is_multiple_of(4) {
                buf.push(b'?');
            }
            let np = next(s) % 6;
            for _ in 0..np {
                for d in next(s).to_string().bytes() {
                    buf.push(d);
                }
                buf.push(if next(s).is_multiple_of(4) {
                    b':'
                } else {
                    b';'
                });
            }
            let ni = next(s) % 3;
            for _ in 0..ni {
                buf.push((0x20 + (next(s) % 0x10)) as u8);
            }
            buf.push((0x40 + (next(s) % 0x3f)) as u8);
        }
        1 => {
            // OSC: numeric command ; random body ; random terminator (BEL/ST/none).
            buf.push(b']');
            for d in (next(s) % 1000).to_string().bytes() {
                buf.push(d);
            }
            buf.push(b';');
            let bl = next(s) % 40;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
            match next(s) % 3 {
                0 => buf.push(0x07),
                1 => buf.extend_from_slice(b"\x1b\\"),
                _ => {}
            }
        }
        2 => {
            // DCS ... ST
            buf.push(b'P');
            let bl = next(s) % 48;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
            buf.extend_from_slice(b"\x1b\\");
        }
        3 => {
            // ESC # n — DEC line sizing / alignment.
            buf.push(b'#');
            buf.push((0x30 + (next(s) % 0x10)) as u8);
        }
        4 => {
            // Charset designation: ESC ( ) * + <byte>.
            buf.push((0x28 + (next(s) % 4)) as u8);
            buf.push((next(s) & 0xFF) as u8);
        }
        5 => {
            // APC ... ST
            buf.push(b'_');
            let bl = next(s) % 24;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
            buf.extend_from_slice(b"\x1b\\");
        }
        6 => {
            // PM ... ST
            buf.push(b'^');
            let bl = next(s) % 24;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
            buf.extend_from_slice(b"\x1b\\");
        }
        7 => {
            // A single ESC Fe (0x40..=0x5f): RI, NEL, HTS, IND, etc.
            buf.push((0x40 + (next(s) % 0x20)) as u8);
        }
        _ => {
            // ESC followed by arbitrary garbage (resync stress).
            let bl = next(s) % 24;
            for _ in 0..bl {
                buf.push((next(s) & 0xFF) as u8);
            }
        }
    }
}

/// Resize / reflow robustness fuzz, with its own emitter, invariants and seed.
///
/// Drives realistic samples (long wrapping ASCII, wide CJK, emoji ZWJ
/// clusters, combining marks, tabs, SGR, home, a lone cell in pending-wrap)
/// and resizes about every other iteration through random dimensions,
/// including 1xN / Nx1, asserting after each resize that the dimensions match
/// and the cursor is in bounds, and at the end that every cell is accessible.
/// Deterministic LCG: any failure reproduces exactly.
mod resize_reflow {
    use aterm_core::terminal::Terminal;

    #[inline]
    fn next(s: &mut u64) -> u32 {
        *s = s
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (*s >> 33) as u32
    }

    fn emit_content(s: &mut u64, term: &mut Terminal) {
        const SAMPLES: &[&[u8]] = &[
            b"the quick brown fox jumps over the lazy dog 0123456789 ", // long -> wraps
            "\u{65e5}\u{672c}\u{8a9e}\u{306e}\u{30c6}\u{30ad}\u{30b9}\u{30c8}".as_bytes(), // wide CJK
            "\u{1f680}\u{1f600}\u{2764}\u{fe0f}\u{1f468}\u{200d}\u{1f469}\u{200d}\u{1f467}"
                .as_bytes(), // emoji + ZWJ
            "e\u{0301}n\u{0303}o\u{0308}a\u{030a}".as_bytes(), // combining marks
            b"\tTABBED\t",
            b"\x1b[31mred\x1b[1mbold\x1b[0m ",
            b"\r\n",
            b"\x1b[H", // home
            b"X",      // single cell at the current position (exercises pending-wrap)
        ];
        let n = 1 + next(s) % 10;
        for _ in 0..n {
            term.process(SAMPLES[(next(s) as usize) % SAMPLES.len()]);
        }
    }

    fn check_invariants(term: &Terminal, it: u32, r: u16, c: u16) {
        assert_eq!(
            term.rows(),
            r,
            "iter {it}: rows {} != resize target {r}",
            term.rows()
        );
        assert_eq!(
            term.cols(),
            c,
            "iter {it}: cols {} != resize target {c}",
            term.cols()
        );
        let cur = term.cursor();
        assert!(
            cur.row < r,
            "iter {it}: cursor row {} out of bounds (rows {r})",
            cur.row
        );
        assert!(
            cur.col <= c,
            "iter {it}: cursor col {} out of bounds (cols {c})",
            cur.col
        );
    }

    #[test]
    fn resize_reflow_keeps_engine_invariants() {
        let mut s: u64 = 0xD1CE_F00D_1234_5678;
        let mut term = Terminal::new(24, 80);
        for it in 0..30_000u32 {
            emit_content(&mut s, &mut term);
            // Resize ~1/2 the time through a wide range of dimensions, incl. very
            // narrow widths that force aggressive re-wrapping and degenerate sizes.
            if next(&mut s) & 1 == 0 {
                let r = 1 + (next(&mut s) % 50) as u16;
                let c = 1 + (next(&mut s) % 200) as u16;
                term.resize(r, c);
                check_invariants(&term, it, r, c);
            }
        }
        // Every cell of the final grid must be accessible without panic.
        let (rows, cols) = (term.rows(), term.cols());
        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = term.grid().cell(row, col) {
                    let _ = cell.char();
                }
            }
        }
    }

    /// The same schedule under the ConPTY seam policy (how a Windows session
    /// resizes), plus that policy's law: a rows-only grow appends blank rows at
    /// the bottom and reveals nothing, so it never changes the history depth,
    /// and it moves the counter with the rows it appends, so nothing renumbers.
    ///
    /// Fewer iterations than the native schedule: with no reveal, history
    /// accumulates (up to a few thousand lines), so each width change also
    /// rewraps history, the path this schedule exists to reach, at a much
    /// higher cost per iteration.
    #[test]
    fn resize_reflow_keeps_engine_invariants_under_the_conpty_policy() {
        use aterm_core::grid::ResizePolicy;
        let mut s: u64 = 0xD1CE_F00D_1234_5678;
        let mut term = Terminal::new(24, 80);
        for it in 0..6_000u32 {
            emit_content(&mut s, &mut term);
            if next(&mut s) & 1 == 0 {
                let r = 1 + (next(&mut s) % 50) as u16;
                // A third of the resizes keep the width: window-height drags are
                // the common ConPTY resize, and a random width almost never
                // repeats.
                let c = if next(&mut s).is_multiple_of(3) {
                    term.cols()
                } else {
                    1 + (next(&mut s) % 200) as u16
                };
                let rows_only_grow = c == term.cols() && r > term.rows();
                let history = term.grid().scrollback_lines();
                let keys = (
                    term.grid().oldest_absolute_row(),
                    term.grid().visible_to_absolute(0),
                    term.grid().history_renumber_epoch(),
                );
                term.resize_with_policy(r, c, ResizePolicy::ConPty);
                check_invariants(&term, it, r, c);
                if rows_only_grow {
                    assert_eq!(
                        term.grid().scrollback_lines(),
                        history,
                        "iter {it}: a ConPTY rows-only grow revealed history"
                    );
                    assert_eq!(
                        (
                            term.grid().oldest_absolute_row(),
                            term.grid().visible_to_absolute(0),
                            term.grid().history_renumber_epoch(),
                        ),
                        keys,
                        "iter {it}: a ConPTY rows-only grow moved an absolute key"
                    );
                }
            }
        }
        let (rows, cols) = (term.rows(), term.cols());
        for row in 0..rows {
            for col in 0..cols {
                if let Some(cell) = term.grid().cell(row, col) {
                    let _ = cell.char();
                }
            }
        }
    }
}

// Fresh-terminal batches: a fresh 24x80 terminal fed one multi-KiB batch,
// chunked soup, 1..=3 x 1..=3 grids, and the adversarial corpus.
// aterm-conformance's `vt` suite mounts the same file, so the soups also run
// against aterm-core[sixel]; the module's doc says why that takes two binaries.
// (This file is a module of `core_fuzz`, so `mod fresh;` finds
// `fuzz_process_never_panics/fresh.rs`.)
mod fresh;
