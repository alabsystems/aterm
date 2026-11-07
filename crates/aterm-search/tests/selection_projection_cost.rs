// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One-shot stored-cell projection used by word selection.

use aterm_search::LineCells;

#[test]
fn one_shot_projection_does_not_allocate_a_column_map() {
    let text = "éx".repeat(4096);
    let mut cells = LineCells::new();
    for (col, (byte, ch)) in text.char_indices().enumerate() {
        cells.push(byte..byte + ch.len_utf8(), 2 * col, 2);
    }
    let before = crate::heap::allocated();
    let spans = [
        cells.columns_of(&text, 0, 3),
        cells.columns_of(&text, text.len() - 3, text.len()),
        cells.columns_of(&text, text.len(), text.len()),
    ];
    let allocated = crate::heap::allocated() - before;
    assert_eq!(spans, [(0, 4), (16380, 16384), (16384, 16384)]);
    assert_eq!(allocated, 0);
}

#[test]
#[ignore = "manual before/after one-shot selection projection measurement"]
fn selection_projection_cost() {
    const REPEATS: usize = 256;
    for shape in ["ascii", "unicode", "alternating"] {
        for length in [64, 4096] {
            let text = match shape {
                "ascii" => "x".repeat(length),
                "unicode" => format!("é{}", "x".repeat(length)),
                _ => "éx".repeat(length / 2),
            };
            for merged in [false, true] {
                let mut cells = LineCells::new();
                if merged {
                    cells.push(0..text.len(), 7, 3);
                } else {
                    for (col, (byte, ch)) in text.char_indices().enumerate() {
                        cells.push(byte..byte + ch.len_utf8(), 2 * col, 2);
                    }
                }
                for place in ["front", "middle", "end"] {
                    let start = match place {
                        "front" => 0,
                        "middle" => text.len() / 2,
                        _ => text.len().saturating_sub(2),
                    };
                    let end = start + 2;
                    let expected = cells.columns_of(&text, start, end);
                    let allocated = crate::heap::allocated();
                    let before = super::overlap_cost::instructions();
                    let mut result = (0, 0);
                    for _ in 0..REPEATS {
                        result = std::hint::black_box(cells.columns_of(
                            std::hint::black_box(&text),
                            start,
                            end,
                        ));
                    }
                    let retired = before
                        .zip(super::overlap_cost::instructions())
                        .map(|(a, b)| b - a);
                    let bytes = crate::heap::allocated() - allocated;
                    assert_eq!(result, expected);
                    eprintln!(
                        "selection_projection shape={shape} length={length} merged={merged} place={place} repeats={REPEATS} instructions={retired:?} allocated_bytes={bytes}"
                    );
                }
            }
        }
    }
}
