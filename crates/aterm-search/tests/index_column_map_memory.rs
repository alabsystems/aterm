// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use aterm_search::TerminalSearch;

use crate::heap::net;

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
                "cjk" => {
                    for _ in 0..100 {
                        let c = char::from_u32(0x4E00 + (lcg(&mut state) % 3000) as u32);
                        row.push(c.unwrap_or('中'));
                    }
                }
                "decomposed" => {
                    for _ in 0..100 {
                        row.push(char::from(b'a' + (lcg(&mut state) % 26) as u8));
                        row.push('\u{301}');
                    }
                }
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

/// Net heap a cold index over `shape`'s rows holds once built, and the rows' text bytes.
fn held(shape: &str) -> (f64, f64) {
    let rows = rows(shape);
    let text: usize = rows.iter().map(String::len).sum();
    let before = net();
    let mut index = TerminalSearch::with_capacity_and_max(rows.len(), 100_000);
    index.index_visible_content(0, &rows);
    let held = net() - before;
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
    assert!(
        agent <= 1.5 * ascii,
        "one glyph makes an agent row's index {:.1}x the plain row's",
        agent / ascii
    );
}
