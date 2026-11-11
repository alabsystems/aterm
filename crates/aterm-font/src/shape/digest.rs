// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/set_digest.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! A glyph-set digest: three 64-bit masks over the glyph id at shifts 4, 0 and 9.

use crate::GlyphId;

#[derive(Clone, Copy, Default, Debug)]
pub(crate) struct Digest {
    masks: [u64; 3],
}

const SHIFTS: [u32; 3] = [4, 0, 9];

#[inline]
const fn mask_for(g: GlyphId, shift: u32) -> u64 {
    1u64 << ((g.0 as u64 >> shift) & 63)
}

impl Digest {
    #[inline]
    pub(crate) const fn new() -> Self {
        Self { masks: [0; 3] }
    }

    #[inline]
    pub(crate) fn add(&mut self, g: GlyphId) {
        for (mask, &shift) in self.masks.iter_mut().zip(&SHIFTS) {
            *mask |= mask_for(g, shift);
        }
    }

    #[inline]
    pub(crate) fn add_range(&mut self, a: GlyphId, b: GlyphId) {
        for (mask, &shift) in self.masks.iter_mut().zip(&SHIFTS) {
            if *mask == u64::MAX {
                continue;
            }
            let span = (u64::from(b.0) >> shift).wrapping_sub(u64::from(a.0) >> shift);
            if span >= 63 {
                *mask = u64::MAX;
            } else {
                let ma = mask_for(a, shift);
                let mb = mask_for(b, shift);
                *mask |= mb
                    .wrapping_add(mb.wrapping_sub(ma))
                    .wrapping_sub(u64::from(mb < ma));
            }
        }
    }

    #[inline]
    pub(crate) const fn may_have(&self, other: &Digest) -> bool {
        (self.masks[0] & other.masks[0]) != 0
            && (self.masks[1] & other.masks[1]) != 0
            && (self.masks[2] & other.masks[2]) != 0
    }

    #[inline]
    pub(crate) const fn may_have_glyph(&self, g: GlyphId) -> bool {
        (self.masks[0] & mask_for(g, SHIFTS[0])) != 0
            && (self.masks[1] & mask_for(g, SHIFTS[1])) != 0
            && (self.masks[2] & mask_for(g, SHIFTS[2])) != 0
    }
}

#[cfg(test)]
#[path = "digest_tests.rs"]
mod tests;
