// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Deterministic fuzz tests for scrollback byte-level decoders.

use crate::lz4;
use crate::{Line, deserialize_lines};

const ITERS: u32 = 10_000;

struct Lcg(u64);

impl Lcg {
    fn new(seed: u64) -> Self {
        Self(seed)
    }

    #[inline]
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        (self.0 >> 33) as u32
    }

    fn bytes(&mut self, max_len: u32) -> Vec<u8> {
        let len = (self.next_u32() % max_len) as usize;
        (0..len).map(|_| (self.next_u32() & 0xFF) as u8).collect()
    }
}

#[test]
fn fuzz_lz4_decompress_never_panics() {
    let mut rng = Lcg::new(0x9E37_79B9_7F4A_7C15);
    for i in 0..ITERS {
        let mut input = rng.bytes(96);

        if input.len() >= 4 {
            match i % 6 {
                0 => {
                    let claim = (input.len() as u32).saturating_sub(4);
                    input[..4].copy_from_slice(&claim.to_le_bytes());
                }
                1 => input[..4].copy_from_slice(&0x00FF_FFFFu32.to_le_bytes()),
                2 => input[..4].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes()),
                3 => input[..4].copy_from_slice(&0u32.to_le_bytes()),
                _ => {}
            }
        }

        if let Ok(out) = lz4::decompress_size_prepended(&input) {
            let claim = u32::from_le_bytes([input[0], input[1], input[2], input[3]]);
            assert_eq!(out.len(), claim as usize, "{input:02x?}");
        }
    }

    for case in [
        &b""[..],
        &b"\x00"[..],
        &b"\x00\x00\x00"[..],     // < 4 bytes (InputTooShort)
        &b"\x00\x00\x00\x00"[..], // zero prefix, empty body (Ok empty)
        &[0x04, 0x00, 0x00, 0x00, 0xFF, 0xFF, 0xFF, 0xFF], // garbage body
        &[0x01, 0x00, 0x00, 0x00], // claims 1 byte, no body
    ] {
        let result = lz4::decompress_size_prepended(case);
        if case.len() < 4 {
            assert_eq!(result, Err(lz4::Lz4Error::InputTooShort), "{case:02x?}");
        } else if let Ok(out) = result {
            let claim = u32::from_le_bytes([case[0], case[1], case[2], case[3]]);
            assert_eq!(out.len(), claim as usize, "{case:02x?}");
        }
    }
}

#[test]
fn fuzz_line_deserialize_never_panics() {
    let mut rng = Lcg::new(0xD1B5_4A32_D192_ED03);
    for i in 0..ITERS {
        let mut input = rng.bytes(128);
        if !input.is_empty() {
            input[0] = match i % 5 {
                0 => 0,
                1 => 1,
                2 => 2,
                3 => 3,
                _ => 255,
            };
        }
        let _ = std::hint::black_box(Line::deserialize(&input));
    }
}

#[test]
fn fuzz_deserialize_lines_never_panics() {
    let mut rng = Lcg::new(0x2545_F491_4F6C_DD1D);
    for i in 0..ITERS {
        let mut input = rng.bytes(160);

        if input.len() >= 4 {
            let count: u32 = match i % 4 {
                0 => 0xFFFF_FFFF,
                1 => rng.next_u32() % 8,
                2 => 0,
                _ => rng.next_u32(),
            };
            input[..4].copy_from_slice(&count.to_le_bytes());

            let mut p = 4;
            while p < input.len() {
                input[p] = (rng.next_u32() % 4) as u8; // version 0..3
                p += 1 + (rng.next_u32() % 12) as usize;
            }
        }

        let decoded = std::hint::black_box(deserialize_lines(&input));
        let max_possible = input.len().saturating_sub(4) / 5;
        assert!(
            decoded.len() <= max_possible,
            "decoded {} lines from {} bytes (max {max_possible})",
            decoded.len(),
            input.len()
        );
    }

    for case in [
        &b""[..],
        &b"\x00\x00\x00"[..],            // < 4 bytes -> empty
        &b"\x00\x00\x00\x00"[..],        // count 0 -> empty
        &[0xFF, 0xFF, 0xFF, 0xFF],       // huge count, no records
        &[0x01, 0x00, 0x00, 0x00, 0x00], // claims 1 line, truncated record
    ] {
        let _ = std::hint::black_box(deserialize_lines(case));
    }
}
