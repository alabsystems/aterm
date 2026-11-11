// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::{BlockBuf, SLACK, Window};

fn naive(all: &mut Vec<u8>, offset: usize, len: usize) {
    for _ in 0..len {
        all.push(all[all.len() - offset]);
    }
}

#[test]
fn matches_agree_with_a_byte_by_byte_copy_across_the_ring() {
    let cap = 13;
    for fed in [0usize, 5, 13, 14, 30, 41] {
        let history: Vec<u8> = (0..fed).map(|i| (i * 7 + 3) as u8).collect();
        let mut window = Window::default();
        window.reset(cap);
        for piece in history.chunks(4) {
            window.push(piece);
        }
        assert_eq!(window.len(), fed.min(cap));
        let block: Vec<u8> = (0..6).map(|i| 200 + i).collect();
        for offset in 1..=block.len() + fed.min(cap) {
            for len in 1..=70 {
                let mut out = BlockBuf::default();
                out.start(block.len() + len);
                out.push(&block).unwrap();
                assert!(out.has_room(len));
                out.copy_match(&window, offset, len).unwrap();
                let mut all = history.clone();
                all.extend_from_slice(&block);
                naive(&mut all, offset, len);
                assert_eq!(
                    out.as_slice(),
                    &all[fed..],
                    "fed {fed} offset {offset} len {len}"
                );
            }
        }
    }
}

#[test]
fn overlapping_matches_inside_the_block() {
    let seed: Vec<u8> = (0..40).map(|i| (i * 37 + 11) as u8).collect();
    for offset in 1..=40 {
        for len in [3, 15, 16, 17, 31, 32, 33, 100, 1000] {
            let mut out = BlockBuf::default();
            out.start(seed.len() + len);
            out.push(&seed).unwrap();
            out.copy_match(&Window::default(), offset, len).unwrap();
            let mut all = seed.clone();
            naive(&mut all, offset, len);
            assert_eq!(out.as_slice(), &all[..], "offset {offset} len {len}");
        }
    }
}

#[test]
fn literals_copy_short_and_long() {
    let lits: Vec<u8> = (0..100u8).chain(std::iter::repeat_n(0, SLACK)).collect();
    let mut out = BlockBuf::default();
    out.start(100);
    out.copy_literals(&lits, 0, 3).unwrap();
    out.copy_literals(&lits, 3, 40).unwrap();
    out.copy_literals(&lits, 43, 16).unwrap();
    out.copy_literals(&lits, 59, 41).unwrap();
    assert_eq!(out.as_slice(), &lits[..100]);
}

#[test]
fn offsets_past_the_history_are_refused() {
    let mut window = Window::default();
    window.reset(8);
    window.push(b"abc");
    let mut out = BlockBuf::default();
    out.start(16);
    out.push(b"xy").unwrap();
    assert!(out.copy_match(&window, 5, 3).is_ok());
    assert_eq!(out.as_slice(), b"xyabc");
    let mut out = BlockBuf::default();
    out.start(16);
    out.push(b"xy").unwrap();
    assert!(out.copy_match(&window, 6, 3).is_err());
    let mut empty = Window::default();
    empty.reset(8);
    let mut out = BlockBuf::default();
    out.start(16);
    assert!(out.copy_match(&empty, 1, 3).is_err());
    assert!(out.copy_match(&empty, 0, 3).is_err());
}

#[test]
fn a_push_larger_than_the_window_keeps_its_tail() {
    let mut window = Window::default();
    window.reset(4);
    window.push(b"0123456789");
    let mut out = BlockBuf::default();
    out.start(4);
    out.copy_match(&window, 4, 4).unwrap();
    assert_eq!(out.as_slice(), b"6789");
}

#[test]
fn the_block_maximum_holds() {
    let mut out = BlockBuf::default();
    out.start(4);
    assert!(out.push(b"abcde").is_err());
    assert!(out.fill_with(5).is_err());
    out.push(b"abcd").unwrap();
    assert!(!out.has_room(1));
}
