// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

#[test]
fn row_modes_and_directions_prepare_once_per_query() {
    let mut matcher = CaseInsensitiveMatcher::new("AABA");
    let before = crate::bytesearch::factorizations();
    for row in 0..128 {
        for text in ["x aABA y aaba z", "é aABA y aaba z"] {
            let map = ColumnMap::new(text, AmbiguousWidth::Single);
            let mut forward = Vec::new();
            let visit = matcher.visit_matches::<false>(row, text, &map, |found| {
                forward.push((found.start_col, found.end_col));
                true
            });
            assert!(visit.completed && visit.had_occurrence);
            assert_eq!(forward, [(2, 6), (9, 13)]);
            let mut backward = Vec::new();
            let visit = matcher.visit_matches::<true>(row, text, &map, |found| {
                backward.push((found.start_col, found.end_col));
                true
            });
            assert!(visit.completed && visit.had_occurrence);
            assert_eq!(backward, [(9, 13), (2, 6)]);
        }
    }
    let prepared = crate::bytesearch::factorizations() - before;
    assert!(prepared <= 4, "prepared {prepared} times for one query");
}
