// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The v2 engine's state words (`crate::state_words`): its custody (the caret, the echo ledger,
//! the queued events, the hand's lift and run, the withheld rows and the wrap, lift, reflow and
//! revive records), then each producer's own words. The frame's status, fingerprint and cadence
//! are output and are not hashed.

use super::{CaretSeam, CompanionImpulse, Engine, Event, ribbon, witness};
use crate::state_words::{Fold, Words, sound_cue_words};
use aterm_time::Instant;

impl Engine {
    /// The custody records, then the spine, ribbon, stardust, meteor and witness.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.flag("rk.engaged", self.engaged);
        w.flag("rk.reduced_motion", self.reduced_motion);
        w.group("rk.caret_seam", |f| caret_seam_words(f, self.caret_seam));
        w.cell("rk.caret", self.caret);
        w.flag("rk.caret_known", self.caret_known);
        w.cell_opt("rk.caret_seen", self.caret_seen);
        w.list("rk.echo", self.echo.slots, |f, slot| {
            f.opt("slot", slot, |f, (at, cells): (Instant, u16)| {
                f.at("at", at);
                f.put("cells", u64::from(cells));
            });
        });
        w.put("rk.bridged", u64::from(self.bridged));
        w.cell("rk.erased", self.erased);
        w.opt("rk.mend", self.mend, |f, m| {
            f.at("at", m.at);
            f.f32("disp", m.disp);
            f.put("count", u64::from(m.count));
        });
        w.cell_opt("rk.pane", self.pane);
        w.cell_opt("rk.pane_rows", self.pane_rows);
        w.list(
            "rk.events",
            &self.events,
            |f, &(ev, at): &(Event, Instant)| {
                event_words(f, ev);
                f.at("at", at);
            },
        );
        w.opt(
            "rk.pre_move",
            self.pre_move,
            |f, (n, cell): (usize, Option<(u16, u16)>)| {
                f.put("n", n as u64);
                f.cell_opt("cell", cell);
            },
        );
        w.list("rk.earned", &self.earned, |f, &(cell, at)| {
            f.cell("cell", cell);
            f.at("at", at);
        });
        w.list("rk.pending_cues", &self.pending_cues, |f, cue| {
            sound_cue_words(f, cue);
        });
        w.opt("rk.companion", self.companion, impulse_words);
        w.opt(
            "rk.party",
            self.party,
            |f, (at, n, ring): (Instant, u8, bool)| {
                f.at("at", at);
                f.put("n", u64::from(n));
                f.flag("ring", ring);
            },
        );
        w.at_opt("rk.paint_at", self.paint_at);
        w.put("rk.retired", self.retired);
        w.put("rk.followed", self.followed);
        w.put("rk.follow_missed", self.follow_missed);
        w.cell_opt("rk.hand_lift", self.hand_lift);
        w.opt("rk.hand_run", self.hand_run, |f, r: witness::FollowRun| {
            f.put("row", u64::from(r.row));
            f.put("cohort", u64::from(r.cohort));
            f.put("lo", u64::from(r.lo));
            f.put("hi", u64::from(r.hi));
            f.put("dr", r.dr as u16 as u64);
        });
        w.list(
            "rk.withheld",
            &self.withheld[..self.withheld_n.min(self.withheld.len())],
            |f, &row| f.put("row", u64::from(row)),
        );
        w.put("rk.grid_rows", u64::from(self.grid_rows));
        w.opt(
            "rk.flush_clock",
            self.flush_clock,
            |f, (at, n): (Instant, Option<usize>)| {
                f.at("at", at);
                f.put(
                    "n",
                    match n {
                        Some(n) => n as u64,
                        None => u64::MAX,
                    },
                );
            },
        );
        w.opt(
            "rk.soft_wrap",
            self.soft_wrap,
            |f, (wrap, at): (ribbon::SoftWrap, Option<Instant>)| {
                ribbon::soft_wrap_words(f, wrap);
                f.at_opt("at", at);
            },
        );
        w.opt(
            "rk.lift",
            self.lift,
            |f, (lift, at): (ribbon::Lift, Option<Instant>)| {
                ribbon::lift_words(f, lift);
                f.at_opt("at", at);
            },
        );
        w.opt(
            "rk.reflow",
            self.reflow,
            |f, (wrap, cell): (ribbon::SoftWrap, (u16, u16))| {
                ribbon::soft_wrap_words(f, wrap);
                f.cell("cell", cell);
            },
        );
        w.opt("rk.revive", self.revive, ribbon::revive_words);
        w.flag("rk.hidden", self.hidden);

        self.spine.state_words(w);
        self.ribbon.state_words(w);
        self.stardust.state_words(w);
        self.meteor.state_words(w);
        self.witness.state_words(w);
    }

    /// The bed's ink table the ribbon draws by ([`ribbon::Ribbon::bed_ink_lut`]).
    #[inline]
    #[must_use]
    pub const fn bed_ink_lut(&self) -> &ribbon::BedInkLut {
        self.ribbon.bed_ink_lut()
    }
}

/// The caret seam's words, which the glow's own seam copy hashes through too.
#[inline]
pub(crate) fn caret_seam_words(f: &mut Fold, s: CaretSeam) {
    f.at_opt("flare_at", s.flare_at);
    f.f32("field_t", s.field_t);
    f.f32("paint", s.paint);
}

#[inline]
fn impulse_words(f: &mut Fold, i: CompanionImpulse) {
    match i {
        CompanionImpulse::Meteor { dir, t0, t_flight } => {
            f.put("impulse", 0);
            f.put("dir", dir as u64);
            f.at("t0", t0);
            f.dur("t_flight", t_flight);
        }
        CompanionImpulse::Land => f.put("impulse", 1),
        CompanionImpulse::Wince => f.put("impulse", 2),
        CompanionImpulse::Delight => f.put("impulse", 3),
    }
}

#[inline]
fn event_words(f: &mut Fold, ev: Event) {
    match ev {
        Event::Typed {
            cells,
            shifted,
            class,
        } => {
            f.put("event", 0);
            f.put("cells", u64::from(cells));
            f.flag("shifted", shifted);
            f.put("class", class as u64);
        }
        Event::Erase => f.put("event", 1),
        Event::Kill { cells, scope } => {
            f.put("event", 2);
            f.put("cells", u64::from(cells));
            f.put("scope", scope as u64);
        }
        Event::Move {
            from,
            to,
            licence,
            dir,
        } => {
            f.put("event", 3);
            f.cell("from", from);
            f.cell("to", to);
            f.put("licence", licence as u64);
            f.put("dir", dir as u64);
        }
        Event::Return => f.put("event", 4),
        Event::ComposerNewline => f.put("event", 5),
        Event::CancelComposerNewline => f.put("event", 6),
        Event::Focus(on) => {
            f.put("event", 7);
            f.flag("on", on);
        }
        Event::ReducedMotion(on) => {
            f.put("event", 8);
            f.flag("on", on);
        }
        Event::Sweep { row, col0, col1 } => {
            f.put("event", 9);
            f.put("row", u64::from(row));
            f.put("col0", u64::from(col0));
            f.put("col1", u64::from(col1));
        }
        Event::Rewrite { row, col, cells } => {
            f.put("event", 10);
            f.cell("at", (row, col));
            f.put("cells", u64::from(cells));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::Duration;

    fn words(e: &Engine, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        e.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_engine_custody_field_moves_the_engine_words() {
        let t0 = Instant::now();
        let mut e = Engine::new();
        e.set_engaged(true);
        e.on_event(
            Event::Typed {
                cells: 1,
                shifted: false,
                class: super::super::TypedClass::Glyph,
            },
            t0 + Duration::from_millis(3),
        );
        let base = words(&e, t0);
        assert_eq!(base, words(&e.clone(), t0));

        let mut banked = e.clone();
        banked.echo.bank(t0 + Duration::from_millis(9), 2);
        assert_ne!(words(&banked, t0), base, "the echo ledger is hashed");
        let mut queued = e.clone();
        queued
            .events
            .push((Event::Erase, t0 + Duration::from_millis(9)));
        assert_ne!(words(&queued, t0), base, "a queued event is hashed");
        let mut withheld = e.clone();
        withheld.withheld[0] = 7;
        withheld.withheld_n = 1;
        assert_ne!(words(&withheld, t0), base, "a withheld row is hashed");
        let mut followed = e.clone();
        followed.followed += 1;
        assert_ne!(words(&followed, t0), base, "the follow count is hashed");
        let mut stale = e.clone();
        stale.withheld[3] = 9;
        stale.fp ^= 1;
        assert_eq!(
            words(&stale, t0),
            base,
            "rows past the withheld count and the frame's fingerprint are not state"
        );
    }
}
