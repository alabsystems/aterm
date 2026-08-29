// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! ALLOCATION-COUNT gate for the agent-facing styled frame — the deterministic
//! half of `benches/styled_frame.rs`'s claim.
//!
//! `gather_styled_frame` runs with the terminal MUTEX HELD, so every allocation
//! it makes is one the engine waits for: the PTY drain, keystroke encode and the
//! next frame snapshot all queue behind an agent's `screen` poll. The retired
//! shape built one `String` PER CELL for the grapheme — 1,920 heap allocations
//! on a 24x80 screen, 10,000 on 50x200, and a blank cell allocated too, because
//! the grapheme of a written blank is a single space rather than the empty
//! string. A per-row grapheme buffer with a byte range per cell makes that one
//! allocation per row.
//!
//! Wall-clock says how much that is worth on this box; the COUNT is the part
//! that is a property of the code, so it is what gets pinned. Both arms are
//! measured here in one process: the retired per-cell sweep, spelled exactly as
//! the gather spelled it (`Terminal::cell_grapheme` once per cell), against the
//! shipping gather — so the gate can never go vacuous by measuring only the
//! side that already passes.
//!
//! An integration test is its own crate, so the counting global allocator here
//! affects no other test or bench (the `mem_budget` precedent in aterm-core).
//! Run: `cargo test -p aterm-gui --features bench-support --test styled_frame_alloc`.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicUsize, Ordering};

use aterm_gui::bench_support::{GatheredFrame, ScreenFill, painted_screen};

static COUNT: AtomicUsize = AtomicUsize::new(0);

/// System allocator that counts allocation CALLS (not bytes): the question here
/// is how many times the frame read enters the allocator while holding the
/// terminal lock, and a one-byte glyph costs a full trip like any other.
struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        COUNT.fetch_add(1, Ordering::Relaxed);
        // SAFETY: forwarding to the System allocator with the same layout.
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        // SAFETY: forwarding to the System allocator with the same ptr+layout.
        unsafe { System.dealloc(p, l) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

/// Allocation calls made while `f` runs, plus its result. The result is returned
/// rather than dropped inside, so a `Vec` that is freed cannot make its
/// allocations disappear from the count.
fn allocations<T>(f: impl FnOnce() -> T) -> (usize, T) {
    let before = COUNT.load(Ordering::Relaxed);
    let v = f();
    (COUNT.load(Ordering::Relaxed) - before, v)
}

/// The two arms, on one painted screen, at one grid size: `(per-cell sweep
/// allocations, gather allocations, cells whose grapheme is non-empty)`.
///
/// The third number is the retired shape's exact allocation floor: `String::new()`
/// does not touch the allocator, so the cells that cost one are precisely the
/// ones with a non-empty grapheme — every cell except a wide CONTINUATION.
fn arms(rows: u16, cols: u16, fill: ScreenFill) -> (usize, usize, usize) {
    let term = painted_screen(rows, cols, fill);
    let cells = usize::from(rows) * usize::from(cols);

    // THE RETIRED SHAPE, verbatim: `glyph: t.cell_grapheme(r, c).unwrap_or_default()`
    // once per cell. `with_capacity` keeps the comparison about the per-cell
    // `String`s rather than about the vector holding them.
    let (per_cell, glyphs) = allocations(|| {
        let mut out: Vec<String> = Vec::with_capacity(cells);
        for r in 0..usize::from(rows) {
            for c in 0..usize::from(cols) {
                out.push(term.cell_grapheme(r, c).unwrap_or_default());
            }
        }
        out
    });
    assert_eq!(glyphs.len(), cells, "the retired sweep read the whole grid");
    let painted = glyphs.iter().filter(|g| !g.is_empty()).count();

    let (gather, gathered) = allocations(|| GatheredFrame::gather(&term));
    assert_eq!(
        gathered.cells(),
        cells,
        "the gather must produce the full rows x cols frame, or the counts \
         below are of two different reads"
    );
    // Keep both alive across the measurement (see `allocations`).
    drop(glyphs);
    drop(gathered);
    (per_cell, gather, painted)
}

/// The GATE: the in-lock gather's allocation count scales with ROWS, not cells.
///
/// Two-sided. The lower side is the non-vacuity floor — the retired per-cell
/// sweep really does allocate once for every cell that has a glyph, so this is
/// not a gate that would pass on any implementation. The upper side is the
/// claim: the gather stays under a small multiple of the ROW count, which
/// cannot hold for anything that still allocates per cell.
///
/// Observed 2026-08-29 on this tree (allocator CALLS, per gather):
///
/// ```text
///   24x80  ascii   1921 -> 74     50x200 ascii  10001 -> 152
///   24x80  wide    1297 -> 98     50x200 wide    6701 -> 202
/// ```
///
/// The wide arm's per-cell number is below its cell count because a wide
/// CONTINUATION's grapheme is empty and `String::new()` allocates nothing; its
/// gather number is above the ASCII arm's because three-byte graphemes outgrow
/// the row buffer's one-byte-per-column reserve and grow it once or twice.
///
/// The ceiling is `4 * rows + 32`: the gather's own fixed overhead (the two
/// frame-level `Vec`s, the selection projection, the image scan) plus room per
/// row for the cells `Vec`, the grapheme buffer, the resolved render row and one
/// buffer growth. Its real slack is the ~30 FIXED allocations of headroom —
/// enough for jitter and for a new fixed-cost allocation, and deliberately NOT
/// enough for a fifth per-row one: the wide arm already measures exactly 4/row
/// (202 at 50 rows against a 232 ceiling), so a new per-row allocation lands at
/// ~252 and trips this gate, which is the conscious-ceiling-bump conversation
/// it exists to force. A return to per-CELL cannot fit at any grid size.
#[test]
fn gather_allocates_per_row_not_per_cell() {
    for (rows, cols) in [(24u16, 80u16), (50, 200)] {
        let cells = usize::from(rows) * usize::from(cols);
        let ceiling = 4 * usize::from(rows) + 32;
        for (fill, name) in [(ScreenFill::Ascii, "ascii"), (ScreenFill::Wide, "wide")] {
            let (per_cell, gather, painted) = arms(rows, cols, fill);
            println!(
                "{rows}x{cols} {name}: per-cell={per_cell} gather={gather} \
                 cells={cells} painted={painted} ceiling={ceiling}"
            );
            assert!(
                per_cell >= painted,
                "{rows}x{cols} {name}: the retired per-cell sweep allocated \
                 {per_cell} times for {painted} cells carrying a glyph — under \
                 one per glyph it is not the shape this gate is measured \
                 against, and the gate is vacuous"
            );
            assert!(
                painted * 2 > cells,
                "{rows}x{cols} {name}: only {painted} of {cells} cells carry a \
                 glyph — the fixture is mostly empty and neither arm is priced \
                 on a full screen"
            );
            assert!(
                gather <= ceiling,
                "{rows}x{cols} {name}: the gather allocated {gather} times \
                 (ceiling {ceiling} for {rows} rows) — a per-cell allocation is \
                 back on the path that holds the terminal lock"
            );
        }
    }
}
