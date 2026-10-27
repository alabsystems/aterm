// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The ribbon's state words (`crate::state_words`): its cells, cohorts, holds, clocks and the
//! custody it keeps across frames, partial custody included. The frame's plan, runs, vertices,
//! rails and keep-outs are output, and the bed's ink table a cache read through
//! [`Ribbon::bed_ink_lut`]; none of them is in the ribbon's words. The dump folds the table on
//! its own ([`BedInkLut::state_words`]).

use super::{
    BED_INK_LUT_LEN, BedInkLut, Cell, Cohort, LastWalk, Lift, ReleaseClock, ReleasePart, Relocate,
    Revive, RewrapStart, Ribbon, RunOrigin, Seam, ShortReanchorProof, SoftWrap, SplitOff, WalkPos,
};
use crate::state_words::{Fold, Words};
use aterm_time::Instant;

impl Ribbon {
    /// Every cell, cohort and hold, the clocks, and the custody records.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.put("ribbon.cells.n", self.cells.len() as u64);
        w.list("ribbon.cells", &self.cells, cell_words);
        w.put("ribbon.cohorts.n", self.cohorts.len() as u64);
        w.list("ribbon.cohorts", &self.cohorts, cohort_words);
        w.list("ribbon.holds", &self.holds, |f, &(row, level, at)| {
            f.put("row", u64::from(row));
            f.f32("level", level);
            f.at("at", at);
        });
        w.list(
            "ribbon.carried_holds",
            &self.carried_holds,
            |f, &(id, shape)| {
                f.put("cohort", u64::from(id));
                f.opt("shape", shape, |f, (level, at): (f32, Instant)| {
                    f.f32("level", level);
                    f.at("at", at);
                });
            },
        );
        w.list(
            "ribbon.release_parts",
            &self.release_parts,
            |f, p: &ReleasePart| {
                f.put("cohort", u64::from(p.cohort));
                f.at("stamp", p.stamp);
                f.put("cells", p.cells as u64);
            },
        );

        w.put("ribbon.next_cohort", u64::from(self.next_cohort));
        w.put("ribbon.next_phrase", u64::from(self.next_phrase));
        w.put(
            "ribbon.phrase_open",
            match self.phrase_open {
                Some(v) => u64::from(v),
                None => u64::MAX,
            },
        );
        w.at_opt("ribbon.ember_at", self.ember_at);
        w.opt("ribbon.rearm", self.rearm, rearm_words);
        w.at_opt("ribbon.translated_at", self.translated_at);
        w.at_opt("ribbon.last_key_at", self.last_key_at);
        w.f32("ribbon.ioi_ms", self.ioi_ms);
        w.at_opt("ribbon.curtain", self.curtain);
        w.flag("ribbon.reduced", self.reduced);

        w.cell_opt("ribbon.caret", self.caret);
        w.cell_opt("ribbon.pane", self.pane);
        w.cell_opt("ribbon.pane_rows", self.pane_rows);
        w.cell_opt("ribbon.attach", self.attach);
        w.cell_opt("ribbon.pen", self.pen);
        w.cell_opt("ribbon.last_space", self.last_space);
        w.cell_opt("ribbon.arrived", self.arrived);
        w.cell_opt("ribbon.prev_space", self.prev_space);
        w.cell_opt("ribbon.hand_floor", self.hand_floor);
        w.put("ribbon.pending_erase", u64::from(self.pending_erase));
        w.flag("ribbon.scroll_followed", self.scroll_followed);
        w.opt(
            "ribbon.short_reanchor",
            self.short_reanchor,
            |f, p: ShortReanchorProof| {
                f.cell("from", p.from);
                f.cell("to", p.to);
                f.at("at", p.at);
            },
        );
        w.opt("ribbon.relocate", self.relocate, |f, r: Relocate| {
            f.put("row", u64::from(r.row));
            f.put("w", u64::from(r.w));
            f.at("at", r.at);
            f.opt("walk", r.walk, walk_words);
        });
        w.opt("ribbon.soft_wrap", self.soft_wrap, soft_wrap_words);
        w.opt(
            "ribbon.carry",
            self.carry,
            |f, (wrap, at): (SoftWrap, Instant)| {
                soft_wrap_words(f, wrap);
                f.at("at", at);
            },
        );
        w.opt("ribbon.reflow", self.reflow, soft_wrap_words);
        w.opt("ribbon.lift", self.lift, lift_words);
        w.opt(
            "ribbon.rewrap_start",
            self.rewrap_start,
            |f, r: RewrapStart| {
                f.cell("from", r.from);
                f.cell("to", r.to);
                f.put("start", u64::from(r.start));
                f.u16_opt("width", r.width);
                f.u16_opt("space", r.space);
            },
        );
        w.opt("ribbon.revive", self.revive, revive_words);
        w.opt("ribbon.split_off", self.split_off, |f, s: SplitOff| {
            f.put("copy", u64::from(s.copy));
            f.put("dr", s.dr as u16 as u64);
            f.put("col0", u64::from(s.col0));
            f.put("col1", u64::from(s.col1));
        });
        w.opt("ribbon.last_walk", self.last_walk, |f, l: LastWalk| {
            walk_words(f, l.end);
            f.at("alive_at", l.alive_at);
        });
        w.opt("ribbon.fresh_line", self.fresh_line, |f, l| {
            f.at("since", l.since);
            f.at_opt("chord_at", l.chord_at);
            f.flag("awaiting_move", l.awaiting_move);
            f.u16_opt("left_row", l.left_row);
        });
        w.opt("ribbon.seam", self.seam, |f, s: Seam| {
            f.put("row", u64::from(s.row));
            f.at("at", s.at);
        });
    }

    /// The bed's ink table this ribbon draws by (a debug read for `lab trail-diff`'s dump and the
    /// host unreachability pin, §15.1 P0.9).
    #[inline]
    #[must_use]
    pub const fn bed_ink_lut(&self) -> &BedInkLut {
        &self.ink
    }
}

impl BedInkLut {
    /// The `(theme_fg, theme_bg, dark_theme, ribbon_tall)` the table was built for, `None` before
    /// the first sync.
    #[inline]
    #[must_use]
    pub const fn key(&self) -> Option<(u32, u32, bool, bool)> {
        self.key
    }

    /// The bed's ink at each walk position.
    #[inline]
    #[must_use]
    pub const fn lut(&self) -> &[u32; BED_INK_LUT_LEN] {
        &self.lut
    }

    /// What of each ink the page shows.
    #[inline]
    #[must_use]
    pub const fn seen(&self) -> &[f32; BED_INK_LUT_LEN] {
        &self.seen
    }

    /// The table's words: the key it was built for, each ink, and what of each the page shows
    /// (`lab trail-diff`'s dump folds them as its `bed` word).
    #[inline]
    pub fn state_words(&self, w: &mut impl Words) {
        w.opt("bed.key", self.key, |f, (fg, bg, dark, tall)| {
            f.put("fg", u64::from(fg));
            f.put("bg", u64::from(bg));
            f.flag("dark", dark);
            f.flag("tall", tall);
        });
        w.list("bed.lut", self.lut, |f, ink| f.put("ink", u64::from(ink)));
        w.list("bed.seen", self.seen, |f, seen| f.f32("seen", seen));
    }
}

/// One cell's every field.
#[inline]
fn cell_words(f: &mut Fold, c: &Cell) {
    f.cell("at", (c.row, c.col));
    f.put("cohort", u64::from(c.cohort));
    f.f32("t", c.t);
    f.at("born", c.born);
    f.at("attack_at", c.attack_at);
    f.f32("life_s", c.life_s);
    f.f32("cov0", c.cov0);
    f.flag("typing", c.typing);
    f.at_opt("retract_at", c.retract_at);
    f.at_opt("retire_at", c.retire_at);
    f.f32("birth_disp", c.birth_disp);
    f.f32("edge_cells", c.edge_cells);
    f.put("layer", c.layer as u64);
    f.opt("rearm", c.rearm, rearm_words);
    f.at_opt("released_at", c.released_at);
}

/// One cohort's every field, its release clock and partial custody included.
#[inline]
fn cohort_words(f: &mut Fold, c: &Cohort) {
    f.put("id", u64::from(c.id));
    f.put("row", u64::from(c.row));
    f.put("col0", u64::from(c.col0));
    f.put("col1", u64::from(c.col1));
    f.put("anchor_col", u64::from(c.anchor_col));
    f.f32("walk_t0", c.t0);
    f.f32("walk_d0", c.d0);
    f.at("born", c.born);
    f.at("alive_at", c.alive_at);
    f.flag("abandoned", c.abandoned);
    f.flag("wake", c.wake);
    f.u16_opt("retract_col", c.retract_col);
    f.flag("drain_right_first", c.drain_right_first);
    f.put("phase", c.phase as u64);
    f.flag("rejoinable", c.rejoinable);
    f.put("phrase", u64::from(c.phrase));
    f.f32("grace_s", c.grace_s);
    f.at_opt("flow", c.flow);
    f.f32("flow_drift", c.flow_drift);
    f.opt("release_clock", c.release_clock, |f, r: ReleaseClock| {
        f.at("alive_at", r.alive_at);
        f.u16_opt("retract_col", r.retract_col);
        f.put("phase", r.phase as u64);
        f.flag("rejoinable", r.rejoinable);
        f.put("cells", r.cells as u64);
    });
    f.at_opt("parts_revoked_at", c.parts_revoked_at);
    f.flag("line_killed", c.line_killed);
    f.at_opt("printed", c.printed);
}

#[inline]
fn rearm_words(f: &mut Fold, (at, level): (Instant, f32)) {
    f.at("at", at);
    f.f32("level", level);
}

#[inline]
fn walk_words(f: &mut Fold, walk: WalkPos) {
    f.f32("walk.t", walk.t);
    f.f32("walk.d", walk.d);
}

/// A soft-wrapped caret's words ([`SoftWrap`]), which the engine's custody carries too.
#[inline]
pub(crate) fn soft_wrap_words(f: &mut Fold, s: SoftWrap) {
    f.cell("origin", s.origin);
    f.cell("landing", s.landing);
    f.put("word_col0", u64::from(s.word_col0));
}

/// A lifted word's words ([`Lift`]), which the engine's custody carries too.
#[inline]
pub(crate) fn lift_words(f: &mut Fold, l: Lift) {
    f.cell("origin", l.origin);
    f.cell("landing", l.landing);
    f.cell("dst", l.dst);
}

/// A revive's words ([`Revive`]), which the engine's custody carries too.
#[inline]
pub(crate) fn revive_words(f: &mut Fold, r: Revive) {
    f.put("row", u64::from(r.row));
    f.put("col0", u64::from(r.col0));
    f.put("col1", u64::from(r.col1));
    run_origin_words(f, r.origin);
}

/// A run's walk origin ([`RunOrigin`]), which custody records outside the ribbon carry too.
#[inline]
pub(crate) fn run_origin_words(f: &mut Fold, o: RunOrigin) {
    f.put("origin.anchor_col", u64::from(o.anchor_col));
    f.f32("origin.t0", o.t0);
    f.f32("origin.d0", o.d0);
}

#[cfg(test)]
mod tests {
    use super::super::{Layer, Phase};
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::Duration;

    fn words(r: &Ribbon, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        r.state_words(&mut w);
        w.into_words()
    }

    fn laid(t0: Instant) -> Ribbon {
        let born = t0 + Duration::from_millis(8);
        let mut rib = Ribbon::new();
        rib.cells.push(Cell {
            row: 2,
            col: 5,
            cohort: 1,
            t: 0.25,
            born,
            attack_at: born,
            life_s: 1.0,
            cov0: 0.5,
            typing: true,
            retract_at: None,
            retire_at: None,
            birth_disp: 0.4,
            edge_cells: 1.0,
            layer: Layer::Base,
            rearm: None,
            released_at: None,
        });
        rib.cohorts.push(Cohort {
            id: 1,
            row: 2,
            col0: 5,
            col1: 6,
            anchor_col: 5,
            t0: 0.25,
            d0: 0.0,
            born,
            alive_at: born,
            abandoned: false,
            wake: false,
            retract_col: None,
            drain_right_first: false,
            phase: Phase::Laying,
            rejoinable: true,
            phrase: 0,
            grace_s: 1.0,
            flow: None,
            flow_drift: 0.0,
            release_clock: None,
            parts_revoked_at: None,
            line_killed: false,
            printed: None,
        });
        rib.holds.push((2, 0.5, born));
        rib.next_cohort = 2;
        rib
    }

    #[test]
    fn a_hashed_ribbon_field_moves_the_ribbon_words() {
        let t0 = Instant::now();
        let rib = laid(t0);
        let base = words(&rib, t0);
        assert_eq!(base, words(&rib.clone(), t0));

        let mut released = rib.clone();
        released.cells[0].released_at = Some(t0 + Duration::from_millis(30));
        assert_ne!(
            words(&released, t0),
            base,
            "a cell's partial custody is hashed"
        );
        let mut revoked = rib.clone();
        revoked.cohorts[0].parts_revoked_at = Some(t0 + Duration::from_millis(30));
        assert_ne!(
            words(&revoked, t0),
            base,
            "a cohort's private custody is hashed"
        );
        let mut eased = rib.clone();
        eased.holds[0].1 = 0.75;
        assert_ne!(words(&eased, t0), base, "a row's hold is hashed");
        let mut clocked = rib.clone();
        clocked.ember_at = Some(t0);
        assert_ne!(words(&clocked, t0), base, "the ember's clock is hashed");
        let mut planned = rib.clone();
        planned.slabs += 3;
        planned.ink_quads += 1;
        assert_eq!(
            words(&planned, t0),
            base,
            "the frame's plan is output, not state"
        );
    }

    #[test]
    fn every_bed_table_field_moves_the_bed_words() {
        let t0 = Instant::now();
        let words = |bed: &BedInkLut| {
            let mut w = StateWords::new(t0);
            bed.state_words(&mut w);
            w.into_words()
        };
        let cfg = super::super::Config::from_glow(
            &crate::cursor_glow::GlowConfig::sample_for_test(),
            false,
        );
        let bed = BedInkLut::synced(&cfg);
        let base = words(&bed);
        assert_eq!(
            base,
            words(&BedInkLut::synced(&cfg)),
            "the same table, the same words"
        );
        assert_ne!(words(&BedInkLut::EMPTY), base, "an unsynced table is apart");
        let (fg, bg, dark, tall) = bed.key.expect("a synced table has its key");
        let mut mutants = Vec::new();
        for (what, key) in [
            ("fg", (fg ^ 1, bg, dark, tall)),
            ("bg", (fg, bg ^ 1, dark, tall)),
            ("dark", (fg, bg, !dark, tall)),
            ("tall", (fg, bg, dark, !tall)),
        ] {
            let mut keyed = bed;
            keyed.key = Some(key);
            mutants.push((what, keyed));
        }
        let mut inked = bed;
        inked.lut[BED_INK_LUT_LEN - 1] ^= 1;
        mutants.push(("last ink", inked));
        let mut seen = bed;
        seen.seen[0] = f32::from_bits(seen.seen[0].to_bits() ^ 1);
        mutants.push(("first seen", seen));
        for (what, mutant) in mutants {
            assert_ne!(words(&mutant), base, "the table's {what} is hashed");
        }
    }
}
