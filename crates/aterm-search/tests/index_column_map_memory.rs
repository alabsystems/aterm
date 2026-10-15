// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates
//
// What an index of non-ASCII scrollback holds, measured as REAL net heap (a
// counting global allocator, as `index_release_memory.rs` does), against its
// text and against the same rows in ASCII.
//
// Every indexed line keeps a column map. One entry of three `usize` per
// grapheme, in a doubling `Vec`, made a non-ASCII row's index 10-25 times its
// text (r15 content-3: 100,000 rows of CJK held 370 MiB, of decomposed
// accents 663 MiB), and ONE box-drawing glyph in an ASCII agent row made the
// row cost 9 times the plain one. The bounds below sit between that and the
// run-length encoded map.
//
// ONE test function on purpose: the counting allocator is a single global
// counter, so a second #[test] running in parallel would pollute every
// reading.

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicI64, Ordering};

use aterm_search::TerminalSearch;

static NET: AtomicI64 = AtomicI64::new(0);

struct Counting;
unsafe impl GlobalAlloc for Counting {
    unsafe fn alloc(&self, l: Layout) -> *mut u8 {
        NET.fetch_add(l.size() as i64, Ordering::Relaxed);
        unsafe { System.alloc(l) }
    }
    unsafe fn dealloc(&self, p: *mut u8, l: Layout) {
        NET.fetch_sub(l.size() as i64, Ordering::Relaxed);
        unsafe { System.dealloc(p, l) }
    }
    unsafe fn realloc(&self, p: *mut u8, l: Layout, n: usize) -> *mut u8 {
        NET.fetch_add(n as i64 - l.size() as i64, Ordering::Relaxed);
        unsafe { System.realloc(p, l, n) }
    }
}

#[global_allocator]
static ALLOC: Counting = Counting;

const ROWS: usize = 20_000;

/// A deterministic stream of row content (no RNG crate, no clock).
fn lcg(state: &mut u64) -> u64 {
    *state = state
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    *state >> 33
}

fn rows(shape: &str) -> Vec<String> {
    let mut state = 0x5eed_u64;
    (0..ROWS)
        .map(|i| {
            let mut row = String::new();
            match shape {
                // 100 ideographs: a 200-column row of CJK.
                "cjk" => {
                    for _ in 0..100 {
                        let c = char::from_u32(0x4E00 + (lcg(&mut state) % 3000) as u32);
                        row.push(c.unwrap_or('中'));
                    }
                }
                // 100 letters, each with a combining acute: decomposed accents.
                "decomposed" => {
                    for _ in 0..100 {
                        row.push(char::from(b'a' + (lcg(&mut state) % 26) as u8));
                        row.push('\u{301}');
                    }
                }
                // An agent's tool-output row, with and without its one glyph.
                "agent" | "ascii" => {
                    row.push_str(if shape == "agent" {
                        "  \u{23bf}  "
                    } else {
                        "  |  "
                    });
                    row.push_str(&format!("{i:08} "));
                    for _ in 0..66 {
                        row.push(char::from(b'a' + (lcg(&mut state) % 26) as u8));
                    }
                }
                _ => unreachable!("unknown shape {shape}"),
            }
            row
        })
        .collect()
}

/// Net heap a cold index over `shape`'s rows holds once built (the GUI's cold
/// build: `with_capacity_and_max` then `index_visible_content`), and the
/// rows' text bytes.
fn held(shape: &str) -> (f64, f64) {
    let rows = rows(shape);
    let text: usize = rows.iter().map(String::len).sum();
    let before = NET.load(Ordering::Relaxed);
    let mut index = TerminalSearch::with_capacity_and_max(rows.len(), 100_000);
    index.index_visible_content(0, &rows);
    let held = NET.load(Ordering::Relaxed) - before;
    assert_eq!(index.indexed_line_count(), ROWS);
    drop(rows);
    let held = held as f64;
    eprintln!(
        "{shape:>10}: text {:6.2} MiB, index holds {:6.2} MiB ({:4.1}x the text, {:5.0} B/row)",
        text as f64 / 1_048_576.0,
        held / 1_048_576.0,
        held / text as f64,
        held / ROWS as f64,
    );
    drop(index);
    (held, text as f64)
}

#[test]
fn an_index_of_non_ascii_rows_holds_a_small_multiple_of_their_text() {
    let (cjk, cjk_text) = held("cjk");
    let (decomposed, decomposed_text) = held("decomposed");
    let (agent, _) = held("agent");
    let (ascii, _) = held("ascii");

    // origin/main: 12.9x and 11.6x.
    assert!(
        cjk <= 4.0 * cjk_text,
        "a CJK index holds {:.1}x its text",
        cjk / cjk_text
    );
    assert!(
        decomposed <= 3.0 * decomposed_text,
        "a decomposed-accent index holds {:.1}x its text",
        decomposed / decomposed_text
    );
    // origin/main: 8.9x.
    assert!(
        agent <= 1.5 * ascii,
        "one glyph makes an agent row's index {:.1}x the plain row's",
        agent / ascii
    );
}
