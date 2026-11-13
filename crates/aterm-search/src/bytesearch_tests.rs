// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Searcher, memchr};

#[test]
fn owned_query_keeps_ascii_and_exact_preparation_separate() {
    use super::{OwnedSearcher, factorizations};
    for query in ["AaBa", "aABaba", "AaAaAa", "ΣσAa"] {
        let owned = OwnedSearcher::new(query);
        let row = format!(
            "é{query}y{}z{}",
            query.to_ascii_lowercase(),
            query.to_ascii_uppercase()
        );
        let exact: Vec<_> = row
            .as_bytes()
            .windows(query.len())
            .enumerate()
            .filter_map(|(i, bytes)| (bytes == query.as_bytes()).then_some(i))
            .collect();
        let folded: Vec<_> = row
            .as_bytes()
            .windows(query.len())
            .enumerate()
            .filter_map(|(i, bytes)| bytes.eq_ignore_ascii_case(query.as_bytes()).then_some(i))
            .collect();
        let before = factorizations();
        for _ in 0..128 {
            assert_eq!(
                owned
                    .searcher()
                    .matches_in::<false>(row.as_bytes())
                    .collect::<Vec<_>>(),
                exact
            );
            assert_eq!(
                owned
                    .searcher_ascii()
                    .matches_in::<false>(row.as_bytes())
                    .collect::<Vec<_>>(),
                folded
            );
            assert_eq!(
                owned
                    .searcher()
                    .matches_in::<true>(row.as_bytes())
                    .collect::<Vec<_>>(),
                exact.iter().copied().rev().collect::<Vec<_>>()
            );
            assert_eq!(
                owned
                    .searcher_ascii()
                    .matches_in::<true>(row.as_bytes())
                    .collect::<Vec<_>>(),
                folded.iter().copied().rev().collect::<Vec<_>>()
            );
        }
        let prepared = factorizations() - before;
        assert!(prepared <= 4, "prepared {prepared} times for one query");
    }
}

#[test]
fn owned_query_preparation_is_shared_across_rows_and_directions() {
    use super::{FACTORIZATIONS, OwnedSearcher};
    for query in ["abababab".to_owned(), "界é界é".to_owned(), "a".repeat(4096)] {
        let owned = OwnedSearcher::new(&query);
        let row = format!("x{query}y{query}z");
        let want = [1, query.len() + 2];
        let before = FACTORIZATIONS.with(std::cell::Cell::get);
        for _ in 0..128 {
            let searcher = owned.searcher();
            assert_eq!(
                searcher
                    .matches_in::<false>(row.as_bytes())
                    .collect::<Vec<_>>(),
                want
            );
            assert_eq!(
                searcher
                    .matches_in::<true>(row.as_bytes())
                    .collect::<Vec<_>>(),
                [want[1], want[0]]
            );
        }
        assert_eq!(FACTORIZATIONS.with(std::cell::Cell::get) - before, 2);
    }
}

#[test]
fn ascii_folded_cursors_match_a_byte_reference_in_both_directions() {
    use aterm_spec::rng::XorShift;
    let mut rng = XorShift::new(0xA5C1_1F01_DE00_0001);
    let alphabet = b"aAbBcC .\0\xff";
    for case in 0..8192 {
        let len = rng.index(257);
        let haystack: Vec<_> = (0..len).map(|_| *rng.pick(alphabet)).collect();
        let needle_len = rng.index(33);
        let needle: Vec<_> = if case % 2 == 0 && needle_len < len {
            let at = rng.index(len - needle_len + 1);
            haystack[at..at + needle_len]
                .iter()
                .map(u8::to_ascii_uppercase)
                .collect()
        } else {
            (0..needle_len).map(|_| *rng.pick(alphabet)).collect()
        };
        let expected = (0..len).filter(|&at| {
            !needle.is_empty()
                && haystack
                    .get(at..at + needle.len())
                    .is_some_and(|part| part.eq_ignore_ascii_case(&needle))
        });
        let searcher = Searcher::new_ascii(&needle);
        assert!(searcher.matches_in::<false>(&haystack).eq(expected.clone()));
        assert!(searcher.matches_in::<true>(&haystack).eq(expected.rev()));
    }
    let haystack = b"aA".repeat(8192);
    let needle = vec![b'A'; 256];
    for backward in [false, true] {
        let before = super::BYTE_READS.with(std::cell::Cell::get);
        let searcher = Searcher::new_ascii(&needle);
        let expected = 0..=haystack.len() - needle.len();
        if backward {
            assert!(searcher.matches_in::<true>(&haystack).eq(expected.rev()));
        } else {
            assert!(searcher.matches_in::<false>(&haystack).eq(expected));
        }
        let reads = super::BYTE_READS.with(std::cell::Cell::get) - before;
        assert!(reads <= 20 * (haystack.len() + needle.len()));
    }
}

#[test]
fn reverse_two_byte_scans_match_a_reference_across_word_and_block_edges() {
    for len in 0..=200 {
        for first in [0, 1, b'a', b'A', 0xff] {
            let second = first.wrapping_add(1);
            let mut haystack = vec![first.wrapping_add(2); len];
            for at in 0..len {
                haystack[at] = if at % 2 == 0 { first } else { second };
                let expected = haystack.iter().rposition(|&b| b == first || b == second);
                assert_eq!(super::memrchr2(first, second, &haystack), expected);
                assert_eq!(super::memrchr2(second, first, &haystack), expected);
            }
            assert_eq!(
                super::memrchr2(first, second, &vec![first.wrapping_add(2); len]),
                None
            );
        }
    }
}

#[test]
fn dense_periodic_matches_reuse_prefix_facts_in_both_directions() {
    for unit in [&b"a"[..], &b"ab"[..], &b"abacaba"[..]] {
        let haystack = unit.repeat(8192);
        let needle = unit.repeat(256);
        for backward in [false, true] {
            let before = super::BYTE_READS.with(std::cell::Cell::get);
            let searcher = Searcher::new(&needle);
            let count = if backward {
                searcher.matches_in::<true>(&haystack).count()
            } else {
                searcher.matches_in::<false>(&haystack).count()
            };
            let reads = super::BYTE_READS.with(std::cell::Cell::get) - before;
            assert_eq!(count, 8192 - 256 + 1);
            assert!(
                reads <= 20 * (haystack.len() + needle.len()),
                "unit={unit:?} backward={backward}: {reads} byte reads"
            );
        }
    }
    // Negative control: discarding the known prefix recreates the repeated work.
    let haystack = vec![b'a'; 8192];
    let needle = vec![b'a'; 256];
    let searcher = Searcher::new(&needle);
    let before = super::BYTE_READS.with(std::cell::Cell::get);
    let mut cursor = searcher.matches_in::<false>(&haystack);
    while cursor.next().is_some() {
        cursor.state.memory = 0;
    }
    let reads = super::BYTE_READS.with(std::cell::Cell::get) - before;
    assert!(
        reads > 20 * (haystack.len() + needle.len()),
        "the bound must detect a discarded periodic prefix"
    );
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    Searcher::new(needle).find_in(haystack)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    Searcher::new(needle).rfind_in(haystack)
}

#[test]
fn a_one_byte_after_a_zero_byte_does_not_shift_the_answer() {
    let haystack = [0x00u8, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01];
    assert_eq!(memchr(0x00, &haystack), Some(0));
    assert_eq!(memchr(0x01, &haystack), Some(1));
    assert_eq!(super::memrchr(0x00, &haystack), Some(0));
    assert_eq!(super::memrchr(0x01, &haystack), Some(8));
}

#[test]
fn a_useless_skip_switches_itself_off_and_the_answer_is_unchanged() {
    let haystack = vec![b'a'; 4096];
    assert_eq!(find(&haystack, &[b'a'; 64]), Some(0));
    assert_eq!(rfind(&haystack, &[b'a'; 64]), Some(4032));
    let mut tail = vec![b'a'; 4096];
    let last = tail.len().saturating_sub(1);
    tail[last] = b'b';
    let needle: Vec<u8> = std::iter::repeat_n(b'a', 63).chain(*b"b").collect();
    assert_eq!(find(&tail, &needle), Some(4032));
    assert_eq!(rfind(&tail, &needle), Some(4032));
}
