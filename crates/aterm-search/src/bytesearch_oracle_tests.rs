// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{Searcher, memchr, memchr2, memrchr};

mod naive {
    pub(super) fn memchr(n: u8, h: &[u8]) -> Option<usize> {
        h.iter().position(|&b| b == n)
    }
    pub(super) fn memchr2(a: u8, b: u8, h: &[u8]) -> Option<usize> {
        h.iter().position(|&x| x == a || x == b)
    }
    pub(super) fn memrchr(n: u8, h: &[u8]) -> Option<usize> {
        h.iter().rposition(|&b| b == n)
    }
    pub(super) mod memmem {
        pub(in super::super) fn find(h: &[u8], n: &[u8]) -> Option<usize> {
            (0..=h.len().checked_sub(n.len())?).find(|&i| &h[i..i + n.len()] == n)
        }
        pub(in super::super) fn rfind(h: &[u8], n: &[u8]) -> Option<usize> {
            (0..=h.len().checked_sub(n.len())?)
                .rev()
                .find(|&i| &h[i..i + n.len()] == n)
        }
    }
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    Searcher::new(needle).find_in(haystack)
}

fn rfind(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    Searcher::new(needle).rfind_in(haystack)
}

mod ours {
    pub(super) use super::{find, memchr, memchr2, memrchr, rfind};
}

const BINARY_NEEDLE: usize = if cfg!(debug_assertions) { 8 } else { 10 };
const BINARY_HAYSTACK: usize = if cfg!(debug_assertions) { 11 } else { 13 };
const TERNARY_NEEDLE: usize = if cfg!(debug_assertions) { 5 } else { 6 };
const TERNARY_HAYSTACK: usize = if cfg!(debug_assertions) { 7 } else { 8 };

fn strings(alphabet: &[u8], len: usize) -> impl Iterator<Item = Vec<u8>> + '_ {
    let total = alphabet.len().checked_pow(len as u32).expect("small");
    (0..total).map(move |mut n| {
        (0..len)
            .map(|_| {
                let byte = alphabet[n % alphabet.len()];
                n /= alphabet.len();
                byte
            })
            .collect()
    })
}

fn check_pair(haystack: &[u8], needle: &[u8]) {
    let searcher = Searcher::new(needle);
    let expected = (0..haystack.len())
        .filter(|&at| !needle.is_empty() && haystack.get(at..at + needle.len()) == Some(needle));
    assert!(
        searcher.matches_in::<false>(haystack).eq(expected.clone()),
        "overlapping forward {haystack:?} / {needle:?}"
    );
    assert!(
        searcher.matches_in::<true>(haystack).eq(expected.rev()),
        "overlapping backward {haystack:?} / {needle:?}"
    );
    assert_eq!(
        ours::find(haystack, needle),
        naive::memmem::find(haystack, needle),
        "find({:?}, {:?})",
        String::from_utf8_lossy(haystack),
        String::from_utf8_lossy(needle),
    );
    assert_eq!(
        ours::rfind(haystack, needle),
        naive::memmem::rfind(haystack, needle),
        "rfind({:?}, {:?})",
        String::from_utf8_lossy(haystack),
        String::from_utf8_lossy(needle),
    );
}

#[test]
fn a_warm_searcher_matches_the_reference() {
    let mut state: u64 = 0x1234_5678_9ABC_DEF0;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 32) as u32
    };
    for _ in 0..40_000 {
        let alphabet: &[u8] = match next() % 3 {
            0 => b"ab",
            1 => b"abcde",
            _ => &[0x00, 0x01, 0xFF, b'z'],
        };
        let h_len = (next() % 300) as usize;
        let haystack: Vec<u8> = (0..h_len)
            .map(|_| alphabet[(next() as usize) % alphabet.len()])
            .collect();
        let n_len = (next() % 12) as usize;
        let needle: Vec<u8> = if h_len > n_len && next() % 2 == 0 {
            let at = (next() as usize) % (h_len - n_len + 1);
            haystack[at..at + n_len].to_vec()
        } else {
            (0..n_len)
                .map(|_| alphabet[(next() as usize) % alphabet.len()])
                .collect()
        };
        let want_find = naive::memmem::find(&haystack, &needle);
        let want_rfind = naive::memmem::rfind(&haystack, &needle);
        let forward_first = Searcher::new(&needle);
        assert_eq!(forward_first.find_in(&haystack), want_find);
        assert_eq!(forward_first.rfind_in(&haystack), want_rfind);
        assert_eq!(forward_first.find_in(&haystack), want_find);
        let reverse_first = Searcher::new(&needle);
        assert_eq!(reverse_first.rfind_in(&haystack), want_rfind);
        assert_eq!(reverse_first.find_in(&haystack), want_find);
        assert_eq!(reverse_first.rfind_in(&haystack), want_rfind);
    }
}

#[test]
fn exhaustive_binary_alphabet() {
    let mut pairs = 0u64;
    for h_len in 0..=BINARY_HAYSTACK {
        for haystack in strings(b"ab", h_len) {
            for n_len in 0..=BINARY_NEEDLE.min(h_len + 1) {
                for needle in strings(b"ab", n_len) {
                    check_pair(&haystack, &needle);
                    pairs += 1;
                }
            }
        }
    }
    eprintln!("binary alphabet: {pairs} (haystack, needle) pairs");
    assert!(pairs > 1_000_000, "corpus too small: {pairs}");
}

#[test]
fn exhaustive_ternary_alphabet() {
    let mut pairs = 0u64;
    for h_len in 0..=TERNARY_HAYSTACK {
        for haystack in strings(b"abc", h_len) {
            for n_len in 0..=TERNARY_NEEDLE {
                for needle in strings(b"abc", n_len) {
                    check_pair(&haystack, &needle);
                    pairs += 1;
                }
            }
        }
    }
    eprintln!("ternary alphabet: {pairs} (haystack, needle) pairs");
    assert!(pairs > 1_000_000, "corpus too small: {pairs}");
}

#[test]
fn periodic_needles_match_the_reference() {
    let mut state: u64 = 0x2545_F491_4F6C_DD1D;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 32) as u32
    };
    for _ in 0..20_000 {
        let alphabet: &[u8] = match next() % 3 {
            0 => b"ab",
            1 => b"abc",
            _ => &[0x00, 0x01, 0xFF],
        };
        let h_len = (next() % 400) as usize;
        let haystack: Vec<u8> = (0..h_len)
            .map(|_| alphabet[(next() as usize) % alphabet.len()])
            .collect();
        let unit_len = 1 + (next() % 4) as usize;
        let unit: Vec<u8> = (0..unit_len)
            .map(|_| alphabet[(next() as usize) % alphabet.len()])
            .collect();
        let n_len = 1 + (next() % 40) as usize;
        let needle: Vec<u8> = unit.iter().cycle().take(n_len).copied().collect();
        check_pair(&haystack, &needle);
    }
}

#[test]
fn named_edges_match_the_reference() {
    let cases: &[(&[u8], &[u8])] = &[
        (b"", b""),
        (b"", b"a"),
        (b"a", b""),
        (b"a", b"a"),
        (b"ab", b"abc"),
        (b"aaaa", b"aa"),
        (b"abababa", b"aba"),
        (b"aaaaaaaaaaaaaaaa", b"aaaaaaaaaaaaaaaa"),
        (b"aaaaaaaaaaaaaaaa", b"aaaaaaaab"),
        ("héllo wörld".as_bytes(), "ö".as_bytes()),
        ("日本語テキスト".as_bytes(), "テキ".as_bytes()),
        (&[0x00, 0x01, 0x00, 0x01], &[0x01, 0x00]),
        (&[0xFF; 40], &[0xFF; 40]),
        (&[0xFF; 40], &[0xFF; 41]),
    ];
    for (haystack, needle) in cases {
        check_pair(haystack, needle);
    }
}

#[test]
fn byte_scanning_matches_the_reference() {
    for len in 0..=200usize {
        let filler = vec![b'.'; len];
        assert_eq!(ours::memchr(b'!', &filler), naive::memchr(b'!', &filler));
        assert_eq!(
            ours::memchr2(b'!', b'?', &filler),
            naive::memchr2(b'!', b'?', &filler)
        );
        for at in 0..len {
            let mut haystack = filler.clone();
            haystack[at] = b'!';
            assert_eq!(
                ours::memchr(b'!', &haystack),
                naive::memchr(b'!', &haystack),
                "len {len} at {at}"
            );
            assert_eq!(
                ours::memchr2(b'?', b'!', &haystack),
                naive::memchr2(b'?', b'!', &haystack),
                "len {len} at {at}"
            );
            assert_eq!(
                ours::memrchr(b'!', &haystack),
                naive::memrchr(b'!', &haystack),
                "len {len} at {at}"
            );
        }
        for a in 0..len {
            for b in 0..len {
                if a == b {
                    continue;
                }
                let mut haystack = filler.clone();
                haystack[a] = b'x';
                haystack[b] = b'y';
                assert_eq!(
                    ours::memchr2(b'x', b'y', &haystack),
                    naive::memchr2(b'x', b'y', &haystack),
                    "len {len}, x at {a}, y at {b}"
                );
            }
            if len > 24 {
                break; // the quadratic sweep only needs the short lengths
            }
        }
    }
}

#[test]
fn every_byte_value_is_findable() {
    for needle in 0u16..=255 {
        let needle = needle as u8;
        for len in [1usize, 7, 8, 9, 15, 16, 17, 33, 63, 64, 65, 129] {
            for at in 0..len {
                let mut haystack = vec![needle.wrapping_add(1); len];
                haystack[at] = needle;
                assert_eq!(
                    ours::memchr(needle, &haystack),
                    naive::memchr(needle, &haystack),
                    "byte {needle:#04x} len {len} at {at}"
                );
                assert_eq!(
                    ours::memrchr(needle, &haystack),
                    naive::memrchr(needle, &haystack),
                    "byte {needle:#04x} len {len} at {at}"
                );
            }
        }
    }
}

#[test]
fn realistic_haystacks_match_the_reference() {
    let mut state: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut next = move || {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        (state >> 32) as u32
    };
    let mut haystacks: Vec<Vec<u8>> = vec![
        b"error: cannot find value `foo` in this scope".to_vec(),
        vec![b' '; 4096],
        (0..4096).map(|i| (i % 251) as u8).collect(),
        "héllo wörld — a line with multi-byte text "
            .repeat(64)
            .into_bytes(),
    ];
    for _ in 0..200 {
        let len = (next() % 4096) as usize;
        haystacks.push((0..len).map(|_| (next() & 0xFF) as u8).collect());
        let len = (next() % 512) as usize;
        haystacks.push((0..len).map(|_| b'a' + (next() % 2) as u8).collect());
    }
    for haystack in &haystacks {
        for n_len in [1usize, 2, 3, 5, 8, 13, 32, 64] {
            for _ in 0..8 {
                if haystack.len() > n_len {
                    let at = (next() as usize) % (haystack.len() - n_len + 1);
                    check_pair(haystack, &haystack[at..at + n_len]);
                }
                let needle: Vec<u8> = (0..n_len).map(|_| (next() & 0xFF) as u8).collect();
                check_pair(haystack, &needle);
                let runs: Vec<u8> = vec![b'a'; n_len];
                check_pair(haystack, &runs);
            }
        }
    }
}
