// Copyright (c) HarfBuzz developers
// Copyright (c) 2020 Yevhenii Reizner
// SPDX-License-Identifier: MIT
//
// Derived from rustybuzz 0.20.1 src/hb/set_digest.rs and modified by the
// aterm project in 2026.
// See ../../LICENSE-RUSTYBUZZ for the upstream license.

//! A glyph-set digest: three 64-bit masks over the glyph id at shifts 4, 0
//! and 9. A digest answers "may contain" with no false negatives, so a
//! lookup whose coverage digest misses every glyph in the buffer cannot
//! apply and is skipped. It only saves work: skipping changes no answer.

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
            // Upstream's arithmetic is C's: it wraps, so a range whose end is
            // below its start (a malformed font) fills the mask.
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
mod tests {
    use super::*;

    #[test]
    fn digest_has_no_false_negatives() {
        let mut d = Digest::new();
        for g in [2u16, 70, 300, 65_535] {
            d.add(GlyphId(g));
        }
        d.add_range(GlyphId(1000), GlyphId(1100));
        d.add_range(GlyphId(60), GlyphId(66));
        for g in [2u16, 70, 300, 65_535, 1000, 1050, 1100, 60, 63, 64, 66] {
            assert!(d.may_have_glyph(GlyphId(g)), "{g}");
        }
        // A glyph in no shift's mask is told apart.
        let mut one = Digest::new();
        one.add(GlyphId(5));
        assert!(!one.may_have_glyph(GlyphId(6)));
        let mut other = Digest::new();
        other.add(GlyphId(6));
        assert!(!one.may_have(&other));
        other.add(GlyphId(5));
        assert!(one.may_have(&other));
        // An inverted range fills the masks rather than wrapping into a gap.
        let mut inverted = Digest::new();
        inverted.add_range(GlyphId(500), GlyphId(10));
        assert!(inverted.may_have_glyph(GlyphId(123)));
    }
}
