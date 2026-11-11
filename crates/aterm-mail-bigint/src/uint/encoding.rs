// Copyright 2026 Andrew Yates
// Derived from RustCrypto crypto-bigint 0.5.5; Copyright RustCrypto Developers.
// Modifications: added aterm provenance notices; retained the upstream arithmetic.
// SPDX-License-Identifier: Apache-2.0 OR MIT

//! Const-friendly decoding operations for [`Uint`]

#[cfg(all(feature = "der", feature = "generic-array"))]
mod der;

#[cfg(feature = "rlp")]
mod rlp;

use super::Uint;
use crate::{Encoding, Limb, Word};

impl<const LIMBS: usize> Uint<LIMBS> {
    /// Create a new [`Uint`] from the provided big endian bytes.
    pub const fn from_be_slice(bytes: &[u8]) -> Self {
        assert!(
            bytes.len() == Limb::BYTES * LIMBS,
            "bytes are not the expected size"
        );

        let mut res = [Limb::ZERO; LIMBS];
        let mut buf = [0u8; Limb::BYTES];
        let mut i = 0;

        while i < LIMBS {
            let mut j = 0;
            while j < Limb::BYTES {
                buf[j] = bytes[i * Limb::BYTES + j];
                j += 1;
            }
            res[LIMBS - i - 1] = Limb(Word::from_be_bytes(buf));
            i += 1;
        }

        Uint::new(res)
    }

    /// Create a new [`Uint`] from the provided big endian hex string.
    pub const fn from_be_hex(hex: &str) -> Self {
        let bytes = hex.as_bytes();

        assert!(
            bytes.len() == Limb::BYTES * LIMBS * 2,
            "hex string is not the expected size"
        );

        let mut res = [Limb::ZERO; LIMBS];
        let mut buf = [0u8; Limb::BYTES];
        let mut i = 0;
        let mut err = 0;

        while i < LIMBS {
            let mut j = 0;
            while j < Limb::BYTES {
                let offset = (i * Limb::BYTES + j) * 2;
                let (result, byte_err) = decode_hex_byte([bytes[offset], bytes[offset + 1]]);
                err |= byte_err;
                buf[j] = result;
                j += 1;
            }
            res[LIMBS - i - 1] = Limb(Word::from_be_bytes(buf));
            i += 1;
        }

        assert!(err == 0, "invalid hex byte");

        Uint::new(res)
    }

    /// Create a new [`Uint`] from the provided little endian bytes.
    pub const fn from_le_slice(bytes: &[u8]) -> Self {
        assert!(
            bytes.len() == Limb::BYTES * LIMBS,
            "bytes are not the expected size"
        );

        let mut res = [Limb::ZERO; LIMBS];
        let mut buf = [0u8; Limb::BYTES];
        let mut i = 0;

        while i < LIMBS {
            let mut j = 0;
            while j < Limb::BYTES {
                buf[j] = bytes[i * Limb::BYTES + j];
                j += 1;
            }
            res[i] = Limb(Word::from_le_bytes(buf));
            i += 1;
        }

        Uint::new(res)
    }

    /// Create a new [`Uint`] from the provided little endian hex string.
    pub const fn from_le_hex(hex: &str) -> Self {
        let bytes = hex.as_bytes();

        assert!(
            bytes.len() == Limb::BYTES * LIMBS * 2,
            "bytes are not the expected size"
        );

        let mut res = [Limb::ZERO; LIMBS];
        let mut buf = [0u8; Limb::BYTES];
        let mut i = 0;
        let mut err = 0;

        while i < LIMBS {
            let mut j = 0;
            while j < Limb::BYTES {
                let offset = (i * Limb::BYTES + j) * 2;
                let (result, byte_err) = decode_hex_byte([bytes[offset], bytes[offset + 1]]);
                err |= byte_err;
                buf[j] = result;
                j += 1;
            }
            res[i] = Limb(Word::from_le_bytes(buf));
            i += 1;
        }

        assert!(err == 0, "invalid hex byte");

        Uint::new(res)
    }

    /// Serialize this [`Uint`] as big-endian, writing it into the provided
    /// byte slice.
    #[inline]
    pub(crate) fn write_be_bytes(&self, out: &mut [u8]) {
        debug_assert_eq!(out.len(), Limb::BYTES * LIMBS);

        for (src, dst) in self
            .limbs
            .iter()
            .rev()
            .cloned()
            .zip(out.as_chunks_mut::<{ Limb::BYTES }>().0.iter_mut())
        {
            dst.copy_from_slice(&src.to_be_bytes());
        }
    }

    /// Serialize this [`Uint`] as little-endian, writing it into the provided
    /// byte slice.
    #[inline]
    pub(crate) fn write_le_bytes(&self, out: &mut [u8]) {
        debug_assert_eq!(out.len(), Limb::BYTES * LIMBS);

        for (src, dst) in self
            .limbs
            .iter()
            .cloned()
            .zip(out.as_chunks_mut::<{ Limb::BYTES }>().0.iter_mut())
        {
            dst.copy_from_slice(&src.to_le_bytes());
        }
    }
}

/// Decode a single nibble of upper or lower hex
#[inline(always)]
const fn decode_nibble(src: u8) -> u16 {
    let byte = src as i16;
    let mut ret: i16 = -1;

    // 0-9  0x30-0x39
    // if (byte > 0x2f && byte < 0x3a) ret += byte - 0x30 + 1; // -47
    ret += (((0x2fi16 - byte) & (byte - 0x3a)) >> 8) & (byte - 47);
    // A-F  0x41-0x46
    // if (byte > 0x40 && byte < 0x47) ret += byte - 0x41 + 10 + 1; // -54
    ret += (((0x40i16 - byte) & (byte - 0x47)) >> 8) & (byte - 54);
    // a-f  0x61-0x66
    // if (byte > 0x60 && byte < 0x67) ret += byte - 0x61 + 10 + 1; // -86
    ret += (((0x60i16 - byte) & (byte - 0x67)) >> 8) & (byte - 86);

    ret as u16
}

/// Decode a single byte encoded as two hexadecimal characters.
/// Second element of the tuple is non-zero if the `bytes` values are not in the valid range
/// (0-9, a-z, A-Z).
#[inline(always)]
const fn decode_hex_byte(bytes: [u8; 2]) -> (u8, u16) {
    let hi = decode_nibble(bytes[0]);
    let lo = decode_nibble(bytes[1]);
    let byte = (hi << 4) | lo;
    let err = byte >> 8;
    let result = byte as u8;
    (result, err)
}

#[cfg(test)]
#[path = "encoding_tests.rs"]
mod tests;
