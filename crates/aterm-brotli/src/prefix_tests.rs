// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

use super::*;

fn bits(bytes: &[u8]) -> Bits<&[u8]> {
    Bits::new(bytes)
}

/// A complete code decodes each symbol from the bits its canonical code assigns, a
/// second-level table included (lengths past 8 bits), and refuses an incomplete or
/// over-subscribed one.
#[test]
fn canonical_codes_decode_their_symbols_and_bad_ones_are_refused() {
    // Symbols 0..=9: lengths 1, 2, ..., 9, 9 — complete, the two longest past ROOT.
    let lengths = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 9];
    let code = Code::canonical(&lengths).unwrap();
    // Canonical codes: 0 -> "0", 1 -> "10", ..., 8 -> "111111110", 9 -> "111111111";
    // sent first bit first, least significant first in the stream.
    for (symbol, &len) in lengths.iter().enumerate() {
        let ones = if symbol == 9 { len } else { len - 1 };
        let mut stream = 0u32;
        for i in 0..u32::from(ones) {
            stream |= 1 << i;
        }
        let bytes = stream.to_le_bytes();
        let mut b = bits(&bytes);
        assert_eq!(code.decode(&mut b).unwrap(), symbol as u32, "{symbol}");
        assert_eq!(b.position(), u64::from(len), "{symbol} took its length");
    }
    assert!(Code::canonical(&[1, 2, 3]).is_err(), "incomplete");
    assert!(Code::canonical(&[1, 1, 1]).is_err(), "over-subscribed");
    assert!(Code::canonical(&[0, 0]).is_err(), "empty");
    assert!(Code::canonical(&[16, 1]).is_err(), "too long");
    let one = Code::canonical(&[0, 0, 3, 0]).unwrap();
    assert_eq!(
        one.decode(&mut bits(&[])).unwrap(),
        2,
        "one symbol takes no bits"
    );
}
