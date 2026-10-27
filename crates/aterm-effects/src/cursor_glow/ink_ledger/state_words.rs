// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ink ledger's state words (`crate::state_words`): the frame's account (the lines and the
//! pair it keeps legible), its rails, its dims and its notes (the print of every cut). The
//! admit passes' working buffers are scratch and are not hashed.

use super::{Dim, InkLedger};
use crate::state_words::Words;

impl InkLedger {
    /// The account's keys, the rails, the dims and the notes.
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

    #[test]
    fn a_hashed_ink_ledger_field_moves_the_ledger_words() {
        let t0 = Instant::now();
        let mut ledger = InkLedger::EMPTY;
        ledger.note_rail(2, 5);
        let base = words(&ledger, t0);
        assert_eq!(base, words(&ledger, t0));

        let mut railed = InkLedger::EMPTY;
        railed.note_rail(2, 6);
        assert_ne!(words(&railed, t0), base, "a rail is hashed");
        let mut footed = InkLedger::EMPTY;
        footed.note_foot_rail(2, 5);
        assert_ne!(words(&footed, t0), base, "a rail's foot mark is hashed");
        let mut dimmed = InkLedger::EMPTY;
        dimmed.note_rail(2, 5);
        dimmed.dims.push(Dim {
            x0: 0.0,
            y0: 0.0,
            x1: 15.0,
            y1: 28.0,
            share: 0.5,
        });
        assert_ne!(words(&dimmed, t0), base, "a dim is hashed");
        let mut noted = InkLedger::EMPTY;
        noted.note_rail(2, 5);
        noted.print ^= 1;
        assert_ne!(words(&noted, t0), base, "the cut notes are hashed");
        let mut scratch = InkLedger::EMPTY;
        scratch.note_rail(2, 5);
        scratch.reached.push(3);
        assert_eq!(words(&scratch, t0), base, "the admit scratch is not state");
    }
}
