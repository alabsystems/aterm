// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Incremental column projection on matching and nonmatching rows.

use aterm_search::{BudgetedSearch, LineCells};

#[test]
#[ignore = "manual before/after dense incremental column projection measurement"]
fn dense_column_projection_cost() {
    const ROWS: usize = 16;
    const CHARS: usize = 512;
    for (mode, sensitive, regex) in [
        ("literal", true, false),
        ("folded", false, false),
        ("regex", false, true),
    ] {
        for unicode in [false, true] {
            let text = format!("{}{}", if unicode { "é" } else { "" }, "a".repeat(CHARS));
            let mut cells = LineCells::new();
            for (column, (byte, ch)) in text.char_indices().enumerate() {
                cells.push(byte..byte + ch.len_utf8(), column * 2, 2);
            }
            for width in [1, 16] {
                let query = if sensitive {
                    "a".repeat(width)
                } else {
                    "A".repeat(width)
                };
                for stored in [false, true] {
                    let rows = vec![text.clone(); ROWS];
                    let mut search =
                        BudgetedSearch::new(&query, sensitive, regex, 0, ROWS).unwrap();
                    let allocated = crate::heap::allocated();
                    let before = super::overlap_cost::instructions();
                    for row in rows {
                        if stored {
                            search.feed_row_cells(row, &cells);
                        } else {
                            search.feed_row_owned(row);
                        }
                    }
                    let feed = before
                        .zip(super::overlap_cost::instructions())
                        .map(|(a, b)| b - a);
                    let bytes = crate::heap::allocated() - allocated;
                    let result = search.results();
                    let before = super::overlap_cost::instructions();
                    drop(search);
                    let drop = before
                        .zip(super::overlap_cost::instructions())
                        .map(|(a, b)| b - a);
                    let retired = feed.zip(drop).map(|(a, b)| a + b);
                    let per_row = if regex {
                        CHARS / width
                    } else {
                        CHARS - width + 1
                    };
                    assert_eq!(result.matches.len(), ROWS * per_row);
                    let scale = if stored { 2 } else { 1 };
                    for (i, found) in result.matches.iter().enumerate() {
                        let offset = if regex {
                            (i % per_row) * width
                        } else {
                            i % per_row
                        };
                        let start = (usize::from(unicode) + offset) * scale;
                        assert_eq!(
                            (found.line, found.start_col, found.end_col),
                            (i / per_row, start, start + width * scale)
                        );
                    }
                    eprintln!(
                        "column_dense mode={mode} unicode={unicode} width={width} stored={stored} rows={ROWS} instructions={retired:?} allocated_bytes={bytes}"
                    );
                }
            }
        }
    }
}

#[test]
fn nonmatching_rows_do_not_allocate_columns_per_row() {
    let text = "éx".repeat(4096);
    let mut cells = LineCells::new();
    for (column, (byte, ch)) in text.char_indices().enumerate() {
        cells.push(byte..byte + ch.len_utf8(), column * 2, 2);
    }
    for (sensitive, regex) in [(true, false), (false, false), (false, true)] {
        for stored in [false, true] {
            let rows = vec![text.clone(); 16];
            let mut search =
                BudgetedSearch::new("NEEDLE", sensitive, regex, 0, rows.len()).unwrap();
            let before = crate::heap::allocated();
            for row in rows {
                if stored {
                    search.feed_row_cells(row, &cells);
                } else {
                    search.feed_row_owned(row);
                }
            }
            let allocated = crate::heap::allocated() - before;
            assert_eq!(search.result_count(), 0);
            // Allow query-sized scratch and a folded text buffer, not a map per row.
            assert!(
                allocated <= (text.len() * 4 + 64 * 1024) as u64,
                "{sensitive} {regex} {stored}: {allocated}"
            );
        }
    }
}

#[test]
#[ignore = "manual before/after incremental column projection measurement"]
fn incremental_column_projection_cost() {
    const ROWS: usize = 256;
    for (mode, sensitive, regex) in [
        ("literal", true, false),
        ("folded", false, false),
        ("regex", false, true),
    ] {
        let query = if sensitive {
            "aaaaaaaaaaaaaaab"
        } else {
            "AAAAAAAAAAAAAAAB"
        };
        for shape in ["ascii", "unicode", "alternating"] {
            for length in [64, 4096] {
                for hit in [false, true] {
                    let prefix = match shape {
                        "ascii" => "x".repeat(length),
                        "unicode" => format!("é{}", "x".repeat(length)),
                        _ => "éx".repeat(length / 2),
                    };
                    let text = if hit {
                        format!("{prefix}{}b", "a".repeat(15))
                    } else {
                        format!("{prefix}aab{}c", "a".repeat(15))
                    };
                    for stored in [false, true] {
                        let mut cells = LineCells::new();
                        if stored {
                            for (column, (byte, ch)) in text.char_indices().enumerate() {
                                cells.push(byte..byte + ch.len_utf8(), column * 2, 2);
                            }
                        }
                        let rows = vec![text.clone(); ROWS];
                        let mut search =
                            BudgetedSearch::new(query, sensitive, regex, 0, ROWS).unwrap();
                        let allocated = crate::heap::allocated();
                        let before = super::overlap_cost::instructions();
                        for row in rows {
                            if stored {
                                search.feed_row_cells(row, &cells);
                            } else {
                                search.feed_row_owned(row);
                            }
                        }
                        let feed = before
                            .zip(super::overlap_cost::instructions())
                            .map(|(a, b)| b - a);
                        let bytes = crate::heap::allocated() - allocated;
                        let result = search.results();
                        let before = super::overlap_cost::instructions();
                        drop(search);
                        let drop = before
                            .zip(super::overlap_cost::instructions())
                            .map(|(a, b)| b - a);
                        let retired = feed.zip(drop).map(|(a, b)| a + b);
                        assert_eq!(result.matches.len(), if hit { ROWS } else { 0 });
                        let start = if stored {
                            prefix.chars().count() * 2
                        } else {
                            aterm_search::display_columns(&prefix)
                        };
                        let width = if stored { 32 } else { 16 };
                        for (row, found) in result.matches.iter().enumerate() {
                            assert_eq!(
                                (found.line, found.start_col, found.end_col),
                                (row, start, start + width)
                            );
                        }
                        eprintln!(
                            "column_projection mode={mode} shape={shape} length={length} hit={hit} stored={stored} rows={ROWS} instructions={retired:?} allocated_bytes={bytes}"
                        );
                    }
                }
            }
        }
    }
}
