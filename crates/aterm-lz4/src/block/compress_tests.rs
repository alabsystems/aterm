// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

use super::*;

#[test]
fn test_count_same_bytes() {
    let first: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    let second: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
    ];
    assert_eq!(count_same_bytes(first, &mut 0, second, 0), 16);

    let first: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0,
    ];
    let second: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1,
    ];
    assert_eq!(count_same_bytes(first, &mut 0, second, 0), 20);

    let first: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 0, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0,
    ];
    let second: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 1, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1,
    ];
    assert_eq!(count_same_bytes(first, &mut 0, second, 0), 22);

    let first: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0,
    ];
    let second: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1,
    ];
    assert_eq!(count_same_bytes(first, &mut 0, second, 0), 23);

    let first: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 5, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0,
    ];
    let second: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1,
    ];
    assert_eq!(count_same_bytes(first, &mut 0, second, 0), 22);

    let first: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 9, 5, 0, 0, 0, 0, 0, 0, 0,
        0, 0, 0, 0, 0,
    ];
    let second: &[u8] = &[
        1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 1, 2, 3, 4, 3, 4, 6, 1, 1, 1, 1, 1, 1, 1,
        1, 1, 1, 1, 1,
    ];
    assert_eq!(count_same_bytes(first, &mut 0, second, 0), 21);

    for diff_idx in 8..100 {
        let first: Vec<u8> = (0u8..255).cycle().take(100 + 12).collect();
        let mut second = first.clone();
        second[diff_idx] = 255;
        for start in 0..=diff_idx {
            let same_bytes = count_same_bytes(&first, &mut start.clone(), &second, start);
            assert_eq!(same_bytes, diff_idx - start);
        }
    }
}

#[test]
fn test_bug() {
    let input: &[u8] = &[
        10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18, 10, 12, 14, 16, 18,
    ];
    let _out = compress(input);
}

fn read_ext_len(bytes: &[u8], pos: &mut usize) -> usize {
    let mut extra = 0usize;
    loop {
        let b = bytes[*pos];
        *pos += 1;
        extra += b as usize;
        if b != 0xFF {
            return extra;
        }
    }
}

fn assert_conformant_block(compressed: &[u8], expected: &[u8]) {
    assert_eq!(
        crate::block::decompress(compressed, expected.len()).unwrap(),
        expected
    );
    let mut pos = 0usize;
    let mut decoded_pos = 0usize;
    loop {
        assert!(
            pos < compressed.len(),
            "block must end on a literal sequence"
        );
        let token = compressed[pos];
        pos += 1;
        let mut lit_len = (token >> 4) as usize;
        if lit_len == 15 {
            lit_len += read_ext_len(compressed, &mut pos);
        }
        assert!(pos + lit_len <= compressed.len());
        pos += lit_len;
        decoded_pos += lit_len;
        if pos == compressed.len() {
            if expected.len() >= LZ4_MIN_LENGTH {
                assert!(
                    lit_len >= crate::block::LAST_LITERALS,
                    "last sequence must carry >= {} literals, got {lit_len}",
                    crate::block::LAST_LITERALS
                );
            }
            break;
        }
        assert!(
            expected.len() - decoded_pos >= MFLIMIT,
            "match at {decoded_pos} starts fewer than {MFLIMIT} bytes before end {}",
            expected.len()
        );
        assert!(pos + 2 <= compressed.len());
        let offset = u16::from_le_bytes([compressed[pos], compressed[pos + 1]]) as usize;
        pos += 2;
        assert!(
            (1..=MAX_DISTANCE).contains(&offset) && offset <= decoded_pos,
            "invalid match offset {offset} at {decoded_pos}"
        );
        let mut match_len = MINMATCH + ((token & 0x0F) as usize);
        if (token & 0x0F) == 15 {
            match_len += read_ext_len(compressed, &mut pos);
        }
        decoded_pos += match_len;
    }
    assert_eq!(decoded_pos, expected.len());
}

#[test]
fn test_conformant_last_block() {
    let aaas: &[u8] = b"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    for n in 0..LZ4_MIN_LENGTH {
        let out = compress(&aaas[..n]);
        assert_eq!(out.len(), n + 1, "short literal block length for {n}");
        assert_eq!(out[0], (n as u8) << 4, "short literal token for {n}");
        assert_eq!(&out[1..], &aaas[..n], "short literal payload for {n}");
        assert_conformant_block(&out, &aaas[..n]);
    }
    for n in LZ4_MIN_LENGTH..=aaas.len() {
        let out = compress(&aaas[..n]);
        assert_conformant_block(&out, &aaas[..n]);
    }
}
