// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The trick flash's state words (`crate::state_words`): the noted word's slot (what was typed,
//! where the hands were and, once a rescan found it, where it is), when the last flash went on,
//! the host's declared rescan and the per-pane `base_y` memo. A pane's id is the process's count
//! of terminals, not the flash's state, so each is hashed only by presence.

use super::{Located, Slot, TrickFlash};
use crate::state_words::{Fold, Words};

impl TrickFlash {
    /// The slot, the last start, the declaration and the memo.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.opt("slot", self.slot, slot_words);
        w.at_opt("last_start", self.last_start);
        w.opt(
            "declared",
            self.declared,
            |f, (scope, base_y): (Option<u64>, i64)| {
                f.flag("scope", scope.is_some());
                f.put("base_y", base_y as u64);
            },
        );
        w.list(
            "base_y",
            self.base_y.iter().take(usize::from(self.base_y_len)),
            |f, &(pane, base_y)| {
                f.flag("pane", pane.is_some());
                f.put("base_y", base_y as u64);
            },
        );
    }
}

/// The noted word: its key, the hands, its scalars (the first `len`) and where it was found.
#[inline]
fn slot_words(f: &mut Fold, s: Slot) {
    let n = usize::from(s.len).min(s.chars.len());
    f.at("at", s.at);
    f.flag("pane", s.pane.is_some());
    f.cell("caret", s.caret);
    f.opt("base_y", s.base_y, |f, y| f.put("v", y as u64));
    f.list("chars", s.chars.iter().take(n), |f, &c| {
        f.put("c", u64::from(u32::from(c)));
    });
    f.put("back_chars", u64::from(s.back_chars));
    f.flag("rtl", s.rtl);
    f.put("hue_seed", s.hue_seed);
    f.opt("located", s.located, |f, loc| located_words(f, &loc, n));
    f.at_opt("revoked_at", s.revoked_at);
}

/// Where the word was found: its envelope's zero, its row, the lead cells of its `n` scalars and
/// the colours the emitter tints from.
#[inline]
fn located_words(f: &mut Fold, l: &Located, n: usize) {
    f.at("since", l.since);
    f.put("row", u64::from(l.row));
    f.opt("base_y", l.base_y, |f, y| f.put("v", y as u64));
    f.put("grid_cols", u64::from(l.grid_cols));
    f.flag("mirrored", l.mirrored);
    f.list(
        "cells",
        l.cols.iter().zip(l.fg.iter()).take(n),
        |f, (&col, &fg)| {
            f.put("col", u64::from(col));
            f.put("fg", rgb_word(fg));
        },
    );
    f.put("bg", rgb_word(l.bg));
    f.f32("sat", l.sat);
    f.f32("val", l.val);
    f.f32("strength", l.strength);
}

#[inline]
const fn rgb_word([r, g, b]: [u8; 3]) -> u64 {
    (r as u64) << 16 | (g as u64) << 8 | b as u64
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::{Duration, Instant};

    fn words(t: &TrickFlash, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        t.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_noted_word_its_revoke_and_a_declaration_move_the_flash_words() {
        let t0 = Instant::now();
        let base = words(&TrickFlash::new(), t0);
        assert_eq!(
            base,
            words(&TrickFlash::new(), t0),
            "an idle flash's words are fixed"
        );

        let note = |pane| {
            let mut flash = TrickFlash::new();
            flash.note(t0, Some(pane), Some(pane), Some((2, 5)), "sit", 0);
            flash
        };
        let noted = words(&note(38), t0);
        assert_ne!(noted, base, "a noted word is hashed");
        assert_eq!(
            words(&note(41), t0),
            noted,
            "a pane's id is the process's count of terminals, not state"
        );
        let mut other = TrickFlash::new();
        other.note(t0, Some(38), Some(38), Some((2, 5)), "down", 0);
        assert_ne!(
            words(&other, t0),
            noted,
            "the noted word's scalars are hashed"
        );
        let mut revoked = note(38);
        if let Some(slot) = revoked.slot.as_mut() {
            slot.revoked_at = Some(t0 + Duration::from_millis(40));
        }
        assert_ne!(words(&revoked, t0), noted, "a revoke's instant is hashed");
        let mut declared = TrickFlash::new();
        declared.declare_base_y(Some(38), Some(12));
        assert_ne!(
            words(&declared, t0),
            base,
            "the host's declaration is hashed"
        );
    }
}
