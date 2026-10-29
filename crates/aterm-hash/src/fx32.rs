// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The 32-bit `rustc-hash` 2.1 FxHash arm, shared between 32-bit targets and
//! 64-bit host tests so both word-size arms run under standard `targo test`.

#[cfg(test)]
use std::hash::{BuildHasher, Hasher};

use super::read_u32_le;
#[cfg(test)]
use super::{PREVENT_TRIVIAL_ZERO_COLLAPSE, SEED1, SEED2, read_u64_le};

pub(crate) const K32: u32 = 0x93d7_65dd;
pub(crate) const ROTATE32: u32 = 15;

#[inline]
pub(crate) const fn add_to_hash_32(hash: u32, i: u32) -> u32 {
    hash.wrapping_add(i).wrapping_mul(K32)
}

#[inline]
pub(crate) const fn write_u64_32(hash: u32, i: u64) -> u32 {
    add_to_hash_32(add_to_hash_32(hash, i as u32), (i >> 32) as u32)
}

#[inline]
pub(crate) fn write_u128_32(hash: u32, i: u128) -> u32 {
    let b = i.to_le_bytes();
    let bs: &[u8] = &b;
    let mut h = add_to_hash_32(hash, read_u32_le(bs));
    if bs.len() >= 16 {
        h = add_to_hash_32(h, read_u32_le(&bs[4..8]));
        h = add_to_hash_32(h, read_u32_le(&bs[8..12]));
        h = add_to_hash_32(h, read_u32_le(&bs[12..16]));
    }
    h
}

#[inline]
pub(crate) const fn finish_32(hash: u32) -> u64 {
    hash.rotate_left(ROTATE32) as u64
}

#[inline]
pub(crate) const fn multiply_mix_32(x: u64, y: u64) -> u64 {
    let lx = x as u32;
    let ly = y as u32;
    let hx = (x >> 32) as u32;
    let hy = (y >> 32) as u32;

    let afull = (lx as u64) * (hy as u64);
    let bfull = (hx as u64) * (ly as u64);
    afull ^ bfull.rotate_right(32)
}

#[cfg(test)]
pub(crate) fn hash_bytes_32(bytes: &[u8]) -> u64 {
    let len = bytes.len();
    let mut s0 = SEED1;
    let mut s1 = SEED2;

    if len <= 16 {
        if len >= 8 {
            s0 ^= read_u64_le(&bytes[0..8]);
            s1 ^= read_u64_le(&bytes[len - 8..]);
        } else if len >= 4 {
            s0 ^= u64::from(read_u32_le(&bytes[0..4]));
            s1 ^= u64::from(read_u32_le(&bytes[len - 4..]));
        } else {
            match *bytes {
                [] => {}
                [a] => {
                    s0 ^= u64::from(a);
                    s1 ^= (u64::from(a) << 8) | u64::from(a);
                }
                [a, b] => {
                    s0 ^= u64::from(a);
                    s1 ^= (u64::from(b) << 8) | u64::from(b);
                }
                [a, b, c] => {
                    s0 ^= u64::from(a);
                    s1 ^= (u64::from(c) << 8) | u64::from(b);
                }
                _ => {}
            }
        }
    } else {
        let tail_lo = read_u64_le(&bytes[len - 16..len - 8]);
        let tail_hi = read_u64_le(&bytes[len - 8..len]);

        let mut off = 0;
        while off < len - 16 {
            let x = read_u64_le(&bytes[off..off + 8]);
            let y = read_u64_le(&bytes[off + 8..off + 16]);
            let t = multiply_mix_32(s0 ^ x, PREVENT_TRIVIAL_ZERO_COLLAPSE ^ y);
            s0 = s1;
            s1 = t;
            off += 16;
        }

        s0 ^= tail_lo;
        s1 ^= tail_hi;
    }

    multiply_mix_32(s0, s1) ^ (len as u64)
}

#[cfg(test)]
#[derive(Clone, Default)]
pub(crate) struct FxHasher32 {
    hash: u32,
}

#[cfg(test)]
impl Hasher for FxHasher32 {
    fn write(&mut self, bytes: &[u8]) {
        self.write_u64(hash_bytes_32(bytes));
    }

    fn write_u8(&mut self, i: u8) {
        self.hash = add_to_hash_32(self.hash, u32::from(i));
    }

    fn write_u16(&mut self, i: u16) {
        self.hash = add_to_hash_32(self.hash, u32::from(i));
    }

    fn write_u32(&mut self, i: u32) {
        self.hash = add_to_hash_32(self.hash, i);
    }

    fn write_u64(&mut self, i: u64) {
        self.hash = write_u64_32(self.hash, i);
    }

    fn write_u128(&mut self, i: u128) {
        self.hash = write_u128_32(self.hash, i);
    }

    fn write_usize(&mut self, i: usize) {
        self.hash = add_to_hash_32(self.hash, i as u32);
    }

    fn finish(&self) -> u64 {
        finish_32(self.hash)
    }
}

#[cfg(test)]
#[derive(Copy, Clone, Default)]
pub(crate) struct FxBuildHasher32;

#[cfg(test)]
impl BuildHasher for FxBuildHasher32 {
    type Hasher = FxHasher32;

    fn build_hasher(&self) -> FxHasher32 {
        FxHasher32::default()
    }
}
