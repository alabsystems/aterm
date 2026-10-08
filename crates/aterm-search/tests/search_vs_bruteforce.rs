// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The verified search path ([`SearchIndex::search_with_positions`]) against a
//! brute-force substring scan, over generated lines (fixed-seed xorshift): the
//! same set of matched lines, and match columns that recover the query.

use aterm_search::SearchIndex;
use aterm_spec::rng::XorShift;

/// ASCII printable, so column index == byte index == char index. Half the
/// strings come from a five-letter alphabet so queries actually match.
fn text(rng: &mut XorShift, lo: usize, hi: usize, narrow: bool) -> String {
    let len = rng.range(lo, hi + 1);
    (0..len)
        .map(|_| {
            if narrow {
                char::from(*rng.pick(b"abc d"))
            } else {
                char::from(b' ' + rng.index(95) as u8)
            }
        })
        .collect()
}

/// For queries of length >= 3 (the trigram-indexed regime) the verified search
/// returns exactly the lines a brute-force scan finds, and every reported span
/// recovers the query from its source line.
#[test]
fn verified_search_matches_bruteforce() {
    let mut rng = XorShift::new(0x5EA2_C4B1_7E00_0001);
    for case in 0..512 {
        let narrow = case % 2 == 0;
        let lines: Vec<String> = (0..rng.index(12))
            .map(|_| text(&mut rng, 0, 24, narrow))
            .collect();
        let query = text(&mut rng, 3, 6, narrow);

        let mut index = SearchIndex::new();
        for (i, text) in lines.iter().enumerate() {
            index.index_line(i, text);
        }

        let matches = index.search_with_positions(&query);
        let mut got: Vec<usize> = matches.iter().map(|m| m.line).collect();
        got.sort_unstable();
        got.dedup();
        let expected: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, text)| text.contains(&query))
            .map(|(i, _)| i)
            .collect();
        assert_eq!(got, expected, "query {query:?} over {lines:?}");

        for m in matches {
            let line = &lines[m.line];
            assert!(m.start_col <= line.len() && m.end_col <= line.len());
            assert_eq!(&line[m.start_col..m.end_col], query.as_str());
        }
    }
}
