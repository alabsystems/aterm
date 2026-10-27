// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The witness's state words (`crate::state_words`): every `Seen` record and the run, blank and
//! landing records it keeps between frames. The follow and shift passes' working lists are
//! scratch and are not hashed.

use super::{Blanked, BlindRecord, RestoreRun, Seen, Unit, Witness};
use crate::state_words::{Fold, Words};
use aterm_time::Instant;

impl Witness {
    /// Every record the witness keeps.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.put("witness.seen.n", self.seen.len() as u64);
        w.list("witness.seen", &self.seen, seen_words);
        w.list("witness.retired_runs", &self.retired_runs, run_words);
        w.list("witness.standing_runs", &self.standing_runs, run_words);
        w.list("witness.retired_cells", &self.retired_cells, stamp_words);
        w.list("witness.released_runs", &self.released_runs, run_words);
        w.list(
            "witness.standing_glyphs",
            &self.standing_glyphs,
            |f, &(row, cohort, col)| {
                f.put("row", u64::from(row));
                f.put("cohort", u64::from(cohort));
                f.put("col", u64::from(col));
            },
        );
        w.list("witness.shape_runs", &self.shape_runs, run_words);
        w.list("witness.fresh_released", &self.fresh_released, stamp_words);
        w.list("witness.counted_names", &self.counted_names, stamp_words);
        w.list("witness.run_ids", &self.run_ids, stamp_words);
        w.list("witness.blanked", &self.blanked, |f, b: &Blanked| {
            f.cell("at", (b.row, b.col));
            f.at("born", b.born);
            f.put("cohort", u64::from(b.cohort));
            unit_words(f, b.unit);
            f.flag("held", b.held);
            f.flag("counted", b.counted);
        });
        w.list(
            "witness.restore_runs",
            &self.restore_runs,
            |f, r: &RestoreRun| {
                f.put("row", u64::from(r.row));
                f.put("cohort", u64::from(r.cohort));
                f.at_opt("stamp", r.stamp);
                f.flag("exact", r.exact);
                f.flag("returned_ink", r.returned_ink);
            },
        );
        w.list("witness.follow_runs", &self.follow_runs, run_words);
        w.list("witness.blank_seen", &self.blank_seen, stamp_words);
        w.list(
            "witness.landed",
            &self.landed,
            |f, &(row, col, at, cohort): &(u16, u16, Instant, u32)| {
                f.cell("at", (row, col));
                f.at("stamp", at);
                f.put("cohort", u64::from(cohort));
            },
        );
        w.list(
            "witness.blind_changed",
            &self.blind_changed,
            |f, b: &BlindRecord| {
                f.put("row", u64::from(b.row));
                f.put("cohort", u64::from(b.cohort));
                f.put("col", u64::from(b.col));
                f.flag("changed", b.changed);
                f.flag("deferred", b.deferred);
            },
        );
        w.list("witness.deferred_runs", &self.deferred_runs, run_words);
        w.opt("witness.awaiting", self.awaiting, |f, run| {
            run_words(f, &run)
        });
        w.at_opt("witness.last_walk", self.last_walk);
        w.put("witness.scroll_carry", u64::from(self.scroll_carry));
        w.put("witness.caret_carry", self.caret_carry as u16 as u64);
    }
}

/// One `Seen` record's every field.
#[inline]
fn seen_words(f: &mut Fold, s: &Seen) {
    f.cell("at", (s.row, s.col));
    f.at("born", s.born);
    unit_words(f, s.unit);
    f.flag("live", s.live);
    f.flag("released", s.released);
    f.flag("restorable", s.restorable);
    f.flag("counted", s.counted);
    f.put("clear", u64::from(s.clear));
    f.put("twin", u64::from(s.twin));
    f.put("pending", u64::from(s.pending));
    f.list("pend_ms", s.pend_ms, |f, ms| f.put("ms", u64::from(ms)));
    f.flag("deferred", s.deferred);
}

#[inline]
fn unit_words(f: &mut Fold, u: Unit) {
    f.put("unit.ch", u64::from(u32::from(u.ch)));
    f.flag("unit.wide", u.wide);
    f.flag("unit.cont", u.cont);
}

#[inline]
fn run_words(f: &mut Fold, &(row, cohort): &(u16, u32)) {
    f.put("row", u64::from(row));
    f.put("cohort", u64::from(cohort));
}

#[inline]
fn stamp_words(f: &mut Fold, &(row, col, at): &(u16, u16, Instant)) {
    f.cell("at", (row, col));
    f.at("stamp", at);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::Duration;

    fn words(wit: &Witness, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        wit.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_witness_field_moves_the_witness_words() {
        let t0 = Instant::now();
        let mut wit = Witness::new();
        wit.seen.push(Seen {
            row: 4,
            col: 9,
            born: t0 + Duration::from_millis(12),
            unit: Unit {
                ch: 'a',
                wide: false,
                cont: false,
            },
            live: true,
            released: false,
            restorable: false,
            counted: true,
            clear: 0,
            twin: 0,
            pending: 0,
            pend_ms: [0; 4],
            deferred: false,
        });
        let base = words(&wit, t0);
        assert_eq!(base, words(&wit.clone(), t0));

        let mut released = wit.clone();
        released.seen[0].released = true;
        assert_ne!(
            words(&released, t0),
            base,
            "a Seen record's release is hashed"
        );
        let mut twinned = wit.clone();
        twinned.seen[0].pend_ms[2] = 40;
        assert_ne!(
            words(&twinned, t0),
            base,
            "a Seen record's twin clock is hashed"
        );
        let mut carried = wit.clone();
        carried.scroll_carry += 1;
        assert_ne!(words(&carried, t0), base, "the scroll carry is hashed");
        let mut armed = wit.clone();
        armed.follow_armed.push((9, Unit::BLANK, true, 1));
        assert_eq!(
            words(&armed, t0),
            base,
            "the follow pass's scratch is not state"
        );
    }
}
