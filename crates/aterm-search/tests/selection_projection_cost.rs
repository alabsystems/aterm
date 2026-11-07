// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! One-shot stored-cell projection used by word selection.

use aterm_search::LineCells;

#[test]
#[ignore = "manual before/after wrapped-row padding measurement"]
fn wrapped_padding_cost() {
    const REPEATS: usize = 256;
    for gap in [0, 1, 4, 80, 4096] {
        for unicode in [false, true] {
            for reserved in [false, true] {
                let head = if unicode { "é" } else { "x" };
                let mut cells = LineCells::new();
                cells.push(0..head.len(), 0, 1);
                let mut next = LineCells::new();
                next.push(0..1, 0, 1);
                // Clone before counting; measure both existing capacity and growth.
                let mut inputs: Vec<_> = (0..REPEATS)
                    .map(|_| {
                        let mut text = String::with_capacity(if reserved {
                            head.len() + gap + 1
                        } else {
                            head.len()
                        });
                        text.push_str(head);
                        (text, cells.clone())
                    })
                    .collect();
                let allocated = crate::heap::allocated();
                let before = super::overlap_cost::instructions();
                for (text, cells) in &mut inputs {
                    cells.append_wrapped_row(text, "y", &next, 1 + gap);
                    std::hint::black_box((&*text, &*cells));
                }
                let retired = before
                    .zip(super::overlap_cost::instructions())
                    .map(|(a, b)| b - a);
                let bytes = crate::heap::allocated() - allocated;
                for (text, cells) in &inputs {
                    assert_eq!(text, &format!("{head}{}y", " ".repeat(gap)));
                    assert_eq!(
                        cells.columns_of(text, text.len() - 1, text.len()),
                        (gap + 1, gap + 2)
                    );
                }
                eprintln!(
                    "wrapped_padding gap={gap} unicode={unicode} reserved={reserved} repeats={REPEATS} instructions={retired:?} allocated_bytes={bytes}"
                );
            }
        }
    }
}

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
    for shape in [
        "identity",
        "ascii",
        "unicode",
        "alternating",
        "unicode-middle",
        "unicode-suffix",
        "combining-suffix",
        "control-suffix",
        "cjk",
        "combining",
        "prepend",
        "zwj",
    ] {
        for length in [64, 4096] {
            let text = match shape {
                "identity" | "ascii" => "x".repeat(length),
                "unicode" => format!("é{}", "x".repeat(length)),
                "unicode-middle" => {
                    format!("{}é{}", "x".repeat(length / 2), "x".repeat(length / 2))
                }
                "unicode-suffix" => format!("{}é", "x".repeat(length)),
                "combining-suffix" => format!("{}\u{301}", "x".repeat(length)),
                "control-suffix" => format!("{}\r\n", "x".repeat(length)),
                "cjk" => "日本".repeat(length / 2),
                "combining" => "e\u{301}".repeat(length / 2),
                "prepend" => "\u{600}a".repeat(length / 2),
                "zwj" => "👩🏽‍💻".repeat(length / 2),
                _ => "éx".repeat(length / 2),
            };
            for merged in [false, true] {
                if shape == "identity" && merged {
                    continue;
                }
                let mut cells = if shape == "identity" {
                    LineCells::identity(text.len())
                } else {
                    LineCells::new()
                };
                if shape != "identity" {
                    if merged {
                        cells.push(0..text.len(), 7, 3);
                    } else {
                        for (col, (byte, ch)) in text.char_indices().enumerate() {
                            cells.push(byte..byte + ch.len_utf8(), 2 * col, 2);
                        }
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
