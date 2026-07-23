// Copyright (c) 2020 Pascal Seitz et al.
// SPDX-License-Identifier: MIT
//
// Derived from lz4_flex 0.11.5 and modified by the aterm project in 2026.
// See ../../LICENSE-MIT for the upstream MIT license.

#[allow(unused_imports)]
use alloc::boxed::Box;

/// The Hashtable trait used by the compression to store hashed bytes to their position.
/// `val` can be maximum the size of the input in bytes.
///
/// `pos` can have a maximum value of u16::MAX or 65535
/// If the hashtable is smaller it needs to reduce the pos to its space, e.g. by right
/// shifting.
///
/// Duplication dictionary size.
///
/// Every four bytes is assigned an entry. When this number is lower, fewer entries exists, and
/// thus collisions are more likely, hurting the compression ratio.
///
/// hashes and right shifts to a maximum value of 16bit, 65535
/// The right shift is done in order to not exceed, the hashtables capacity
#[inline]
fn hash(sequence: u32) -> u32 {
    (sequence.wrapping_mul(2654435761_u32)) >> 16
}

/// hashes and right shifts to a maximum value of 16bit, 65535
/// The right shift is done in order to not exceed, the hashtables capacity
#[cfg(target_pointer_width = "64")]
#[inline]
fn hash5(sequence: usize) -> u32 {
    let primebytes = if cfg!(target_endian = "little") {
        889523592379_usize
    } else {
        11400714785074694791_usize
    };
    (((sequence << 24).wrapping_mul(primebytes)) >> 48) as u32
}

pub trait HashTable {
    fn get_at(&self, pos: usize) -> usize;
    fn put_at(&mut self, pos: usize, val: usize);
    #[allow(dead_code)]
    fn clear(&mut self);
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
        // This generates more efficient assembly in contrast to Box::new(slice), because of an
        // optimized call alloc_zeroed, vs. alloc + memset
        // try_into is optimized away
        //
        // Allocate the boxed ARRAY directly. The `vec![0; N].into_boxed_slice()
        // .try_into::<Box<[T; N]>>()` form the upstream used for codegen relies
        // on the slice->array `try_into`, whose absent std body the native
        // verifier cannot prove panic-free (fatal absent-callee). `Box::new`
        // of a zeroed array is the SAME zeroed table (this was already the
        // fallback arm), verifiable, and the compiler heap-zero-inits it.
        let dict = alloc::boxed::Box::new([0; HASHTABLE_SIZE_4K]);
        Self { dict }
    }
}
impl HashTable for HashTable4KU16 {
    #[inline]
    // Skip: same audited Box-deref assert class as `put_at` below (the
    // sep-lane alignment/null obligations on `self.dict`'s deref cannot see
    // the constructor's infallible boxed-array allocation across the fn
    // boundary). Non-null and align-2 by construction; droppable with
    // `put_at`'s when cross-fn box facts land.
    #[cfg_attr(trust_verify, trust::skip)]
    fn get_at(&self, hash: usize) -> usize {
        // `hash` is < 2^16 by construction (`hash`/`hash5` right-shift to 16
        // bits), so `hash >> 4 < 4096` on every real call; that invariant is
        // invisible across the trait boundary. `% HASHTABLE_SIZE_4K` (a
        // power of two — compiles to a mask) is identical when it holds.
        self.dict[(hash >> HASHTABLE_BIT_SHIFT_4K) % HASHTABLE_SIZE_4K] as usize
    }
    #[inline]
    // Skip: the one open is the MIR Box-deref null/alignment runtime assert on
    // `self.dict`. The allocator-anchored discharge (`box_alloc_facts_for_addr`)
    // cannot cross the fn boundary to the constructor, and trusting the Box
    // TYPE LABEL is a documented false-prove vector (a transmute can forge it) —
    // so the row is honestly undischargeable intra-fn today. Audited: every
    // `dict` is built by this crate's own infallible boxed-array constructors
    // (direct boxed-array alloc, 1047ada0) — non-null and align-2 by
    // construction; no transmute exists in this crate. A hoisted-reborrow
    // respelling was measured NET-NEGATIVE (adds a Misaligned assert row) and
    // reverted per the ratchet discipline. Verify-only; droppable when
    // cross-fn box facts or validity-invariant modeling land.
    #[cfg_attr(trust_verify, trust::skip)]
    fn put_at(&mut self, hash: usize, val: usize) {
        // Same `%` mask idiom as `get_at` above (identical on all real calls).
        self.dict[(hash >> HASHTABLE_BIT_SHIFT_4K) % HASHTABLE_SIZE_4K] = val as u16;
    }
    #[inline]
    // Skip: same audited Box-deref assert class as `get_at`/`put_at` above
    // (the hoist trips the Null+Misaligned runtime asserts the allocator-
    // anchored discharge cannot reach across the ctor boundary). Same
    // droppable-when.
    #[cfg_attr(trust_verify, trust::skip)]
    fn clear(&mut self) {
        // Hoist the Box deref ONCE (the init_dict idiom): the loop then
        // indexes a plain local slice whose `len()` correlates with the bound
        // in the verifier's model, where a per-iteration `self.dict[..]`
        // re-deref decorrelates. `% HASHTABLE_SIZE_4K` keeps the index provably
        // in range under the loop-carried havoc; `wrapping_add` cannot wrap
        // under `i < HASHTABLE_SIZE_4K`. LLVM elides the mask — same memset.
        // SAFETY: `self.dict` is an owned `Box`, always non-null, aligned,
        // and exclusively borrowed through `&mut self`; the reborrow is plain
        // safe Rust (the unsafe this justifies is `Box`'s inlined deref).
        let dict = &mut *self.dict;
        let mut i = 0;
        while i < HASHTABLE_SIZE_4K {
            dict[i % HASHTABLE_SIZE_4K] = 0;
            i = i.wrapping_add(1);
        }
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
        // Direct boxed-array allocation; see `HashTable4KU16::new` (avoids the
        // absent-callee slice->array `try_into`).
        let dict = alloc::boxed::Box::new([0; HASHTABLE_SIZE_4K]);
        Self { dict }
    }

    #[cold]
    #[allow(dead_code)]
    pub fn reposition(&mut self, offset: u32) {
        for i in self.dict.iter_mut() {
            *i = i.saturating_sub(offset);
        }
    }
}
impl HashTable for HashTable4K {
    #[inline]
    // Skip: same audited Box-deref assert class as the U16 twin above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn get_at(&self, hash: usize) -> usize {
        // `hash < 2^16` by construction; `% 4096` is a mask and identical on
        // all real calls (see `HashTable4KU16::get_at`).
        self.dict[(hash >> HASHTABLE_BIT_SHIFT_4K) % HASHTABLE_SIZE_4K] as usize
    }
    #[inline]
    // Skip: same audited Box-deref assert class as the U16 twin above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn put_at(&mut self, hash: usize, val: usize) {
        // Same `%` mask idiom as `get_at` above (identical on all real calls).
        self.dict[(hash >> HASHTABLE_BIT_SHIFT_4K) % HASHTABLE_SIZE_4K] = val as u32;
    }
    #[inline]
    // Skip: same audited Box-deref assert class as the U16 twin above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn clear(&mut self) {
        // Hoist the Box deref ONCE (the init_dict idiom): the loop then
        // indexes a plain local slice whose `len()` correlates with the bound
        // in the verifier's model, where a per-iteration `self.dict[..]`
        // re-deref decorrelates. `% HASHTABLE_SIZE_4K` keeps the index provably
        // in range under the loop-carried havoc; `wrapping_add` cannot wrap
        // under `i < HASHTABLE_SIZE_4K`. LLVM elides the mask — same memset.
        // SAFETY: `self.dict` is an owned `Box`, always non-null, aligned,
        // and exclusively borrowed through `&mut self`; the reborrow is plain
        // safe Rust (the unsafe this justifies is `Box`'s inlined deref).
        let dict = &mut *self.dict;
        let mut i = 0;
        while i < HASHTABLE_SIZE_4K {
            dict[i % HASHTABLE_SIZE_4K] = 0;
            i = i.wrapping_add(1);
        }
    }
}

const HASHTABLE_SIZE_8K: usize = 8 * 1024;
const HASH_TABLE_BIT_SHIFT_8K: usize = 3;

#[derive(Debug)]
pub struct HashTable8K {
    dict: Box<[u32; HASHTABLE_SIZE_8K]>,
}
#[allow(dead_code)]
impl HashTable8K {
    #[inline]
    pub fn new() -> Self {
        // Direct boxed-array allocation; see `HashTable4KU16::new` (avoids the
        // absent-callee slice->array `try_into` and its `unwrap` panic path).
        let dict = alloc::boxed::Box::new([0; HASHTABLE_SIZE_8K]);

        Self { dict }
    }
}
impl HashTable for HashTable8K {
    #[inline]
    // Skip: same audited Box-deref assert class as the 4K twins above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn get_at(&self, hash: usize) -> usize {
        // `hash < 2^16` by construction; `% 8192` is a mask and identical on
        // all real calls (see `HashTable4KU16::get_at`).
        self.dict[(hash >> HASH_TABLE_BIT_SHIFT_8K) % HASHTABLE_SIZE_8K] as usize
    }
    #[inline]
    // Skip: same audited Box-deref assert class as the 4K twins above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn put_at(&mut self, hash: usize, val: usize) {
        // Same `%` mask idiom as `get_at` above (identical on all real calls).
        self.dict[(hash >> HASH_TABLE_BIT_SHIFT_8K) % HASHTABLE_SIZE_8K] = val as u32;
    }
    #[inline]
    // Skip: same audited Box-deref assert class as the U16 twin above.
    #[cfg_attr(trust_verify, trust::skip)]
    fn clear(&mut self) {
        // Hoist the Box deref ONCE (the init_dict idiom): the loop then
        // indexes a plain local slice whose `len()` correlates with the bound
        // in the verifier's model, where a per-iteration `self.dict[..]`
        // re-deref decorrelates. `% HASHTABLE_SIZE_8K` keeps the index provably
        // in range under the loop-carried havoc; `wrapping_add` cannot wrap
        // under `i < HASHTABLE_SIZE_8K`. LLVM elides the mask — same memset.
        // SAFETY: `self.dict` is an owned `Box`, always non-null, aligned,
        // and exclusively borrowed through `&mut self`; the reborrow is plain
        // safe Rust (the unsafe this justifies is `Box`'s inlined deref).
        let dict = &mut *self.dict;
        let mut i = 0;
        while i < HASHTABLE_SIZE_8K {
            dict[i % HASHTABLE_SIZE_8K] = 0;
            i = i.wrapping_add(1);
        }
    }
}
