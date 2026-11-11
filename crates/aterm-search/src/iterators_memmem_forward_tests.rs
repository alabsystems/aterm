// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

use crate::SearchIndex;

#[test]
fn resumed_literal_lines_match_scalar_reference_in_both_directions() {
    use crate::grapheme::byte_to_column;

    let lines = ["日x x", "x\u{301}x x", "xx", "no hit", "日", "x日xx", ""];
    for cached_maps in [true, false] {
        let mut index = SearchIndex::new();
        for (line, text) in lines.iter().enumerate() {
            index.index_line(line, text);
        }
        if !cached_maps {
            index.column_maps.clear();
        }
        for needle in ["x", "xx", "日", "x\u{301}", "absent"] {
            let mut expected = Vec::new();
            for (line, text) in lines.iter().enumerate() {
                for (at, _) in text.char_indices() {
                    if text[at..].starts_with(needle) {
                        expected.push((
                            line,
                            byte_to_column(text, at),
                            byte_to_column(text, at + needle.len()),
                        ));
                    }
                }
            }
            let forward: Vec<_> = index
                .search_from_line(needle, 0)
                .map(|m| (m.line, m.start_col, m.end_col))
                .collect();
            assert_eq!(forward, expected, "cached={cached_maps} needle={needle:?}");
            expected.reverse();
            let reverse: Vec<_> = index
                .search_before_line(needle, index.len())
                .map(|m| (m.line, m.start_col, m.end_col))
                .collect();
            assert_eq!(reverse, expected, "cached={cached_maps} needle={needle:?}");
        }
    }
}

#[test]
fn compressed_intersections_match_set_reference_at_every_boundary() {
    use super::CandidateSource;
    use crate::bitmap::SparseBitmap;
    for seed in 0..24u32 {
        let lists: Vec<_> = (0..4)
            .map(|i| {
                let mut list = SparseBitmap::new();
                for key in 0..256u32 {
                    if (key.wrapping_mul(17 + i) + seed) % (i + 2) != 0 {
                        list.insert(key.saturating_mul(16_383));
                    }
                }
                list.drop_below(seed.saturating_mul(16_383));
                list
            })
            .collect();
        for count in 1..=lists.len() {
            let reference: Vec<_> = lists[0]
                .to_vec()
                .into_iter()
                .filter(|key| lists[1..count].iter().all(|list| list.contains(*key)))
                .collect();
            for boundary in [0, 1, seed * 16_383, 100 * 16_383, u32::MAX] {
                let mut forward = CandidateSource::from_postings_forward(
                    99,
                    lists[..count].iter().collect(),
                    boundary,
                );
                let mut backward = CandidateSource::from_postings_backward(
                    99,
                    lists[..count].iter().collect(),
                    boundary,
                );
                let got: Vec<_> = std::iter::from_fn(|| forward.next_candidate()).collect();
                let want: Vec<_> = reference
                    .iter()
                    .filter(|&&v| v >= boundary)
                    .map(|&v| 99 + v as usize)
                    .collect();
                assert_eq!(got, want);
                let got: Vec<_> = std::iter::from_fn(|| backward.next_candidate()).collect();
                let want: Vec<_> = reference
                    .iter()
                    .rev()
                    .filter(|&&v| v < boundary)
                    .map(|&v| 99 + v as usize)
                    .collect();
                assert_eq!(got, want);
            }
        }
    }
}

#[test]
fn forward_literal_matches_equal_a_str_find_reference() {
    let lines = ["BANANA split", "aXaXa here", "café au café", "no hit"];
    let mut index = SearchIndex::new();
    for (i, text) in lines.iter().enumerate() {
        index.index_line(i, text);
    }
    for needle in ["ANA", "aX", "café", "X", "z"] {
        let mut reference: Vec<(usize, usize)> = Vec::new();
        for (line, text) in lines.iter().enumerate() {
            let mut start = 0;
            while let Some(rel) = text[start..].find(needle) {
                let byte = start + rel;
                let col = text[..byte].chars().count();
                reference.push((line, col));
                let step = text[byte..].chars().next().map_or(1, char::len_utf8);
                start = byte + step;
            }
        }
        let got: Vec<(usize, usize)> = index
            .search_with_positions(needle)
            .into_iter()
            .map(|m| (m.line, m.start_col))
            .collect();
        assert_eq!(got, reference, "needle {needle:?}");
    }
}
