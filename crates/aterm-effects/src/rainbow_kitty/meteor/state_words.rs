// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The meteor's state words (`crate::state_words`): the live pool, its landings and the sows
//! still owed to stardust. The station vertices and keep-outs are output and are not hashed.

use super::{Burst, Landing, LandingWalk, Meteor, Meteors, Sow, Spark};
use crate::state_words::{Fold, Words};

impl Meteors {
    /// Every meteor, landing and pending sow, and the pool's order clock.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.put("meteor.live.n", self.live.len() as u64);
        w.list("meteor.live", &self.live, meteor_words);
        w.put("meteor.landings.n", self.landings.len() as u64);
        w.list("meteor.landings", &self.landings, landing_words);
        w.list("meteor.sown", &self.sown, sow_words);
        w.put("meteor.ord", u64::from(self.ord));
        w.flag("meteor.reduced", self.reduced);
        w.cell_opt("meteor.hand_floor", self.hand_floor);
    }
}

#[inline]
fn meteor_words(f: &mut Fold, m: &Meteor) {
    f.f32("x0", m.x0);
    f.f32("y0", m.y0);
    f.f32("x1", m.x1);
    f.f32("y1", m.y1);
    f.at("t0", m.t0);
    f.dur("t_flight", m.t_flight);
    f.f32("w", m.w);
    f.f32("grade", m.grade);
    f.at_opt("retired_at", m.retired_at);
    f.flag("chained", m.chained);
    f.f32("t_land", m.t_land);
    f.opt("band", m.band, pair_words);
    f.f32("cells", m.cells);
    f.put("dir", m.dir as u64);
    f.put("seed", u64::from(m.seed));
    f.put("shed", u64::from(m.shed));
    f.flag("landed", m.landed);
    f.flag("ring", m.ring);
}

#[inline]
fn landing_words(f: &mut Fold, l: &Landing) {
    f.at("pin.at", l.pin.at);
    f.f32("pin.t", l.pin.t);
    f.opt("burst", l.burst, burst_words);
    f.flag("flash", l.flash);
    f.at("fan.at", l.fan.at);
    f.put("fan.n", u64::from(l.fan.n));
    f.f32("fan.reach", l.fan.reach);
    f.put("fan.seed", u64::from(l.fan.seed));
    f.flag("fan.hero", l.fan.hero);
    f.f32("x", l.x);
    f.f32("y", l.y);
    walk_words(f, l.walk);
    f.put("sparks", u64::from(l.sparks));
    f.list("spark", &l.spark, |f, s: &Spark| {
        f.f32("vx", s.vx);
        f.f32("vy", s.vy);
        f.f32("life", s.life);
        f.put("arm", u64::from(s.arm));
    });
    f.opt("sky", l.sky, |f, sky| f.put("row", u64::from(sky.row)));
}

#[inline]
fn burst_words(f: &mut Fold, b: Burst) {
    f.at("at", b.at);
    f.f32("scale", b.scale);
    f.f32("ms", b.ms);
    f.put("n", u64::from(b.n));
    f.list("dir", b.dir, pair_words);
    f.list("len", b.len, |f, len| f.f32("len", len));
    f.list("order", b.order, |f, i| f.put("order", u64::from(i)));
    f.list("jet_dir", b.jet_dir, pair_words);
}

#[inline]
fn sow_words(f: &mut Fold, s: &Sow) {
    match *s {
        Sow::Shed { at, spec } => {
            f.put("sow", 0);
            f.at("at", at);
            pair_words(f, spec.at_px);
            pair_words(f, spec.v_head);
            f.f32("tint_t", spec.tint_t);
            f.put("seed", u64::from(spec.seed));
            f.put("n", u64::from(spec.n));
        }
        Sow::Fan { at, spec } => {
            f.put("sow", 1);
            f.at("at", at);
            pair_words(f, spec.at_px);
            f.f32("reach", spec.reach);
            walk_words(f, spec.walk);
            f.put("seed", u64::from(spec.seed));
            f.put("n", u64::from(spec.n));
            f.flag("hero", spec.hero);
        }
        Sow::MiniFan { at, at_px, seed } => {
            f.put("sow", 2);
            f.at("at", at);
            pair_words(f, at_px);
            f.put("seed", u64::from(seed));
        }
    }
}

#[inline]
fn walk_words(f: &mut Fold, walk: LandingWalk) {
    f.f32("walk.t_land", walk.t_land);
    f.opt("walk.band", walk.band, pair_words);
}

#[inline]
fn pair_words(f: &mut Fold, (a, b): (f32, f32)) {
    f.f32("a", a);
    f.f32("b", b);
}

#[cfg(test)]
mod tests {
    use super::super::super::Dir;
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::{Duration, Instant};

    fn words(m: &Meteors, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        m.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_meteor_field_moves_the_meteor_words() {
        let t0 = Instant::now();
        let mut pool = Meteors::new();
        pool.live.push(Meteor {
            x0: 10.0,
            y0: 20.0,
            x1: 200.0,
            y1: 20.0,
            t0: t0 + Duration::from_millis(4),
            t_flight: Duration::from_millis(180),
            w: 6.0,
            grade: 0.5,
            retired_at: None,
            chained: false,
            t_land: 0.3,
            band: None,
            cells: 12.0,
            dir: Dir::Right,
            seed: 9,
            shed: 0,
            landed: false,
            ring: true,
        });
        let base = words(&pool, t0);
        assert_eq!(base, words(&pool.clone(), t0));

        let mut shed = pool.clone();
        shed.live[0].shed |= 1;
        assert_ne!(
            words(&shed, t0),
            base,
            "a meteor's shed stations are hashed"
        );
        let mut retired = pool.clone();
        retired.live[0].retired_at = Some(t0 + Duration::from_millis(50));
        assert_ne!(
            words(&retired, t0),
            base,
            "a meteor's retire clock is hashed"
        );
        let mut ordered = pool.clone();
        ordered.ord += 1;
        assert_ne!(
            words(&ordered, t0),
            base,
            "the pool's order clock is hashed"
        );
    }
}
