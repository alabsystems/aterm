// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::super::line_codec::take_records_written;
use super::{Line, serialize_lines, serialize_pages};

#[test]
fn page_packing_writes_each_record_once_and_preserves_greedy_boundaries() {
    use crate::{CellAttrs, HyperlinkSpan, Rle, UnderlineColorSpan};
    use std::sync::Arc;
    let lines: Vec<_> = (0..32)
        .map(|i| {
            let text = "x".repeat(i % 7 * 13);
            let mut attrs = Rle::new();
            for _ in 0..text.len() {
                attrs.push(CellAttrs::new(0x01_112233, 0, 0));
            }
            let mut line = Line::with_hyperlinks(
                &text,
                attrs,
                vec![HyperlinkSpan::with_id(
                    0,
                    1,
                    Arc::from("https://example.test"),
                    Some(Arc::from("id")),
                )],
            );
            if i % 2 == 1 {
                line.set_underline_colors(vec![UnderlineColorSpan::new(0, 1, 0x01_112233)]);
            }
            line
        })
        .collect();
    let exact_pair = 4 + lines[0].serialized_size_estimate() + lines[1].serialized_size_estimate();
    for limit in [0, 4, 13, 18, 27, 64, 127, exact_pair - 1, exact_pair, 4096] {
        take_records_written();
        let pages = serialize_pages(&lines, limit);
        let written = take_records_written();
        eprintln!(
            "page_packing limit={limit} records={} writes={written}",
            lines.len()
        );
        assert_eq!(
            written,
            lines.len(),
            "each record has exactly one destination page"
        );
        let mut first = 0;
        for (count, bytes) in pages {
            let end = first + count;
            assert!(count > 0);
            assert!(bytes.len() <= limit || count == 1);
            assert_eq!(bytes, serialize_lines(&lines[first..end]));
            if let Some(next) = lines.get(end) {
                assert!(bytes.len().saturating_add(next.serialized_size_estimate()) > limit);
            }
            first = end;
        }
        assert_eq!(first, lines.len());
    }
    assert!(serialize_pages(&[], 0).is_empty());
}
