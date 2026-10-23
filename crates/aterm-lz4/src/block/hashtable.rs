// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

use alloc::boxed::Box;

/// Hashes 4 bytes to 16 bits, so a table smaller than 2^16 entries indexes it by right shift.
#[inline]
const fn hash(sequence: u32) -> u32 {
    (sequence.wrapping_mul(2654435761_u32)) >> 16
}

/// Hashes the low 5 bytes of `sequence` to 16 bits.
#[cfg(target_pointer_width = "64")]
#[inline]
const fn hash5(sequence: usize) -> u32 {
    let primebytes = if cfg!(target_endian = "little") {
        889523592379_usize
    } else {
        11400714785074694791_usize
    };
    (((sequence << 24).wrapping_mul(primebytes)) >> 48) as u32
}

/// Maps a 16-bit hash to the last input position that produced it.
pub trait HashTable {
    fn get_at(&self, pos: usize) -> usize;
    fn put_at(&mut self, pos: usize, val: usize);
    #[inline]
    #[cfg(target_pointer_width = "64")]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash5(super::compress::get_batch_arch(input, pos)) as usize
    }
    #[inline]
    #[cfg(target_pointer_width = "32")]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash(super::compress::get_batch(input, pos)) as usize
    }
}

const HASHTABLE_SIZE_4K: usize = 4 * 1024;
const HASHTABLE_BIT_SHIFT_4K: usize = 4;

#[derive(Debug)]
#[repr(align(64))]
pub struct HashTable4KU16 {
    dict: Box<[u16; HASHTABLE_SIZE_4K]>,
}
impl HashTable4KU16 {
    #[inline]
    pub fn new() -> Self {
        let dict = alloc::boxed::Box::new([0; HASHTABLE_SIZE_4K]);
        Self { dict }
    }
}
impl HashTable for HashTable4KU16 {
    #[inline]
    fn get_at(&self, hash: usize) -> usize {
        // `hash < 2^16`, so `% HASHTABLE_SIZE_4K` is a mask that bounds the index across the trait boundary.
        let dict: &[u16; HASHTABLE_SIZE_4K] = core::ops::Deref::deref(&self.dict);
        dict[(hash >> HASHTABLE_BIT_SHIFT_4K) % HASHTABLE_SIZE_4K] as usize
    }
    #[inline]
    fn put_at(&mut self, hash: usize, val: usize) {
        self.dict[(hash >> HASHTABLE_BIT_SHIFT_4K) % HASHTABLE_SIZE_4K] = val as u16;
    }
    #[inline]
    fn get_hash_at(input: &[u8], pos: usize) -> usize {
        hash(super::get_batch(input, pos)) as usize
    }
}

#[derive(Debug)]
pub struct HashTable4K {
    dict: Box<[u32; HASHTABLE_SIZE_4K]>,
}
impl HashTable4K {
    #[inline]
    pub fn new() -> Self {
        let dict = alloc::boxed::Box::new([0; HASHTABLE_SIZE_4K]);
        Self { dict }
    }
}
impl HashTable for HashTable4K {
    #[inline]
    fn get_at(&self, hash: usize) -> usize {
        let dict: &[u32; HASHTABLE_SIZE_4K] = core::ops::Deref::deref(&self.dict);
        dict[(hash >> HASHTABLE_BIT_SHIFT_4K) % HASHTABLE_SIZE_4K] as usize
    }
    #[inline]
    fn put_at(&mut self, hash: usize, val: usize) {
        self.dict[(hash >> HASHTABLE_BIT_SHIFT_4K) % HASHTABLE_SIZE_4K] = val as u32;
    }
}

#[cfg(clean_islands)]
macro_rules! trust_island {
    ($($item:item)*) => {
        $($item)*
    };
}

#[cfg(not(clean_islands))]
macro_rules! trust_island {
    ($($tt:tt)*) => {};
}

trust_island! {
    clean {
        namespace Lz4HashTableBounds

        def tableSize4K : Nat := 4096

        theorem tableSize4K_eq : tableSize4K = 4096 := rfl

        end Lz4HashTableBounds
    }
}
