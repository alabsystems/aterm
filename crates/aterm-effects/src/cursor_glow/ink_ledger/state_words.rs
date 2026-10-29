// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ink ledger's words (`crate::state_words`): the frame's account (the lines and the pair it
//! keeps legible) is state; its per-frame products (the rails it noted off the under stream, its
//! dims and its notes, the print of every cut) are the ledger's own words, which `lab
//! trail-diff`'s dump prints as its `ledger` line and compares as a stream, exempt where an arm
//! touched the under stream they were read off (`docs/design/PRISM-GLASS.md` §15.9's preamble).
//! The admit passes' working buffers are scratch and are not hashed.

use super::{Dim, InkLedger};
use crate::state_words::Words;

impl InkLedger {
    /// The account's keys: whether it is on, its grid, its lines, the caret, the stride and the
    /// pair.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.flag("ink.on", self.on);
        w.group("ink.grid", |f| {
            f.f32("x", self.grid.0);
            f.f32("y", self.grid.1);
            f.f32("cw", self.grid.2);
            f.f32("ch", self.grid.3);
        });
        w.list("ink.lines", self.lines, |f, line| {
            f.opt("line", line, |f, (row, first, end): (i32, i32, i32)| {
                f.put("row", row as u32 as u64);
                f.put("first", first as u32 as u64);
                f.put("end", end as u32 as u64);
            });
        });
        w.opt("ink.caret", self.caret, |f, (x, y): (i32, i32)| {
            f.put("x", x as u32 as u64);
            f.put("y", y as u32 as u64);
        });
        w.put("ink.stride", self.stride as u64);
        w.put(
            "ink.pair",
            u64::from(self.pair.0) << 32 | u64::from(self.pair.1),
        );
    }

    /// The frame's products: the rails, the dims and the notes.
    #[inline]
    pub(crate) fn ledger_words(&self, w: &mut impl Words) {
        w.list("ink.rails", &self.rails, |f, &(from, to, foot)| {
            f.put("from", u64::from(from));
            f.put("to", u64::from(to));
            f.flag("foot", foot);
        });
        w.list(
            "ink.rail_ramps",
            &self.rail_ramps,
            |f, &(from, to, foot)| {
                f.put("from", u64::from(from));
                f.put("to", u64::from(to));
                f.flag("foot", foot);
            },
        );
        w.list("ink.dims", &self.dims, |f, d: &Dim| {
            f.f32("x0", d.x0);
            f.f32("y0", d.y0);
            f.f32("x1", d.x1);
            f.f32("y1", d.y1);
            f.f32("share", d.share);
        });
        w.put("ink.notes", self.print);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::Instant;

    fn words(l: &InkLedger, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        l.state_words(&mut w);
        w.into_words()
    }

    fn products(l: &InkLedger, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        l.ledger_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_ink_ledger_field_moves_the_ledger_words() {
        let t0 = Instant::now();
        let mut ledger = InkLedger::EMPTY;
        ledger.note_rail(2, 5);
        let (state, base) = (words(&ledger, t0), products(&ledger, t0));
        assert_eq!(base, products(&ledger, t0));

        let mut railed = InkLedger::EMPTY;
        railed.note_rail(2, 6);
        let mut footed = InkLedger::EMPTY;
        footed.note_foot_rail(2, 5);
        let mut dimmed = InkLedger::EMPTY;
        dimmed.note_rail(2, 5);
        dimmed.dims.push(Dim {
            x0: 0.0,
            y0: 0.0,
            x1: 15.0,
            y1: 28.0,
            share: 0.5,
        });
        let mut noted = InkLedger::EMPTY;
        noted.note_rail(2, 5);
        noted.print ^= 1;
        for (what, l) in [
            ("a rail", &railed),
            ("a rail's foot mark", &footed),
            ("a dim", &dimmed),
            ("the cut notes", &noted),
        ] {
            assert_ne!(products(l, t0), base, "{what} is in the ledger's words");
            assert_eq!(words(l, t0), state, "{what} is not state");
        }
        let mut scratch = InkLedger::EMPTY;
        scratch.note_rail(2, 5);
        scratch.reached.push(3);
        assert_eq!(
            products(&scratch, t0),
            base,
            "the admit scratch is not hashed"
        );
        assert_eq!(words(&scratch, t0), state, "nor state");
        let mut moved = InkLedger::EMPTY;
        moved.note_rail(2, 5);
        moved.caret = Some((3, 4));
        assert_ne!(words(&moved, t0), state, "the account is state");
        assert_eq!(products(&moved, t0), base, "and not the ledger's");
    }
}
