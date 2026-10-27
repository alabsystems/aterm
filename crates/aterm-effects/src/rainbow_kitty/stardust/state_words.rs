// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Stardust's state words (`crate::state_words`): the mint, the glint bucket, the stars, the
//! glints and the aurora's veil. The frame's probe, keep-outs, marks and paper-ink memo are inputs
//! and outputs, not state, and the sky index is the draw's scratch (each draw that files a star
//! opens it afresh): none is hashed.

use super::{Finish, Glint, Star, Stardust, Veil};
use crate::state_words::{Fold, Words};

impl Stardust {
    /// The mint, the bucket, every star, every glint, every veil, the cell height the next arm
    /// step is timed by, and the key clock the slipstream reads.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.put("stardust.minted", u64::from(self.minted));
        w.f32("stardust.budget.tokens", self.budget.tokens);
        w.at_opt("stardust.budget.at", self.budget.at);
        w.put("stardust.stars.n", self.stars.len() as u64);
        w.list("stardust.stars", &self.stars, star_words);
        w.list("stardust.glints", &self.glints, |f, g: &Glint| {
            f.put("col", u64::from(g.col));
            f.f32("f", g.f);
        });
        w.put("stardust.veil.n", self.veil.len() as u64);
        w.list("stardust.veil", &self.veil, veil_words);
        w.flag("stardust.reduced", self.reduced);
        w.f32("stardust.cell_h", self.cell_h);
        w.at_opt("stardust.last_typed", self.last_typed);
        w.f32("stardust.ioi_ms", self.ioi_ms);
    }
}

/// One veil's every field.
#[inline]
fn veil_words(f: &mut Fold, v: &Veil) {
    f.put("row", u64::from(v.row));
    f.put("col", u64::from(v.col));
    f.f32("t", v.t);
    f.f32("gain", v.gain);
    f.at("born", v.born);
    f.at("lit", v.lit);
    f.opt("finish", v.finish, finish_words);
}

#[inline]
fn finish_words(f: &mut Fold, fin: Finish) {
    f.at("at", fin.at);
    f.f32("span_s", fin.span_s);
}

/// One star's every field.
#[inline]
fn star_words(f: &mut Fold, s: &Star) {
    f.put("class", s.class as u64);
    f.put("lane", s.lane as u64);
    f.f32("x", s.x);
    f.f32("y", s.y);
    f.u16_opt("home_row", s.home_row);
    f.at("born", s.born);
    f.at("lit", s.lit);
    f.put("seed", u64::from(s.seed));
    f.put("tint", u64::from(s.tint));
    f.flag("gold", s.gold);
    f.f32("v0.x", s.v0.0);
    f.f32("v0.y", s.v0.1);
    f.f32("f", s.f);
    f.f32("stream", s.stream);
    f.f32("pull", s.pull);
    f.opt("finish", s.finish, finish_words);
}

#[cfg(test)]
mod tests {
    use super::super::{StarClass, StarLane};
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::{Duration, Instant};

    fn words(s: &Stardust, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        s.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_stardust_field_moves_the_stardust_words() {
        let t0 = Instant::now();
        let born = t0 + Duration::from_millis(5);
        let mut sky = Stardust::new();
        sky.stars.push(Star {
            class: StarClass::M2,
            lane: StarLane::Strike,
            x: 40.0,
            y: 12.0,
            home_row: Some(3),
            born,
            lit: born,
            seed: 7,
            tint: 0x00FF_00FF,
            gold: false,
            v0: (0.0, -0.003),
            f: 4.0,
            stream: 2.0,
            pull: 0.0,
            finish: None,
        });
        sky.minted = 1;
        let base = words(&sky, t0);
        assert_eq!(base, words(&sky.clone(), t0));

        let mut finished = sky.clone();
        finished.stars[0].finish = Some(Finish {
            at: born + Duration::from_millis(40),
            span_s: 0.04,
        });
        assert_ne!(words(&finished, t0), base, "a star's finish is hashed");
        let mut minted = sky.clone();
        minted.minted += 1;
        assert_ne!(words(&minted, t0), base, "the mint is hashed");
        let mut spent = sky.clone();
        spent.budget.empty();
        assert_ne!(words(&spent, t0), base, "the glint bucket is hashed");
        let veil = Veil {
            row: 4,
            col: 9,
            t: 0.3,
            gain: 0.8,
            born,
            lit: born,
            finish: None,
        };
        let mut veiled = sky.clone();
        veiled.veil.push(veil);
        let dealt = words(&veiled, t0);
        assert_ne!(dealt, base, "a dealt veil is hashed");
        let mut relit = veiled.clone();
        relit.veil[0].lit = born + Duration::from_millis(20);
        assert_ne!(words(&relit, t0), dealt, "a veil's last key is hashed");
        let mut retracting = veiled.clone();
        retracting.veil[0].finish_by(born + Duration::from_millis(30), 0.3);
        assert_ne!(words(&retracting, t0), dealt, "a veil's finish is hashed");
        let mut unmarked = sky.clone();
        unmarked.clear_marks();
        unmarked.mark_lights.push(1);
        assert_eq!(
            words(&unmarked, t0),
            base,
            "the painted marks are output, not state"
        );
    }
}
