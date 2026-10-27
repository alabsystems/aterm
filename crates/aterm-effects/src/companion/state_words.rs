// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The pet driver's state words (`crate::state_words`): its dress and rung, its per-session
//! latches, the hit rect and the clocks it paces the pet by, the frames it drew in, then the
//! brain's own words. A session's id is the process's count of terminals, not the pet's state,
//! so the latches and frames are hashed without it.

use super::{CompanionOwner, PixelFrame};
use crate::state_words::{Fold, Words};

impl CompanionOwner {
    /// The driver's state, then the brain's.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.flag("companion.enabled", self.enabled);
        w.put("companion.species", self.species as u64);
        w.put(
            "companion.pair",
            u64::from(self.pair.0) << 8 | u64::from(self.pair.1),
        );
        w.put("companion.arrival", self.arrival as u64);
        w.put("companion.rung", self.rung as u64);
        w.group("companion.latches", |f| {
            let l = &self.latches;
            f.opt("cmd_seq", l.cmd_seq, session_seq);
            f.opt("content_seq", l.content_seq, session_seq);
            f.opt("wrap_serial", l.wrap_serial, session_seq);
            f.opt(
                "brackets",
                l.brackets,
                |f, ((_session, alt), seq): ((u64, bool), u64)| {
                    f.flag("alt", alt);
                    f.put("seq", seq);
                },
            );
            f.flag("finished_caret", l.finished_caret.is_some());
            f.flag("hidden_rest", l.hidden_rest.is_some());
        });
        w.opt(
            "companion.pointer_px",
            self.pointer_px,
            |f, (x, y): (f32, f32)| {
                f.f32("x", x);
                f.f32("y", y);
            },
        );
        w.opt(
            "companion.hit_rect",
            self.hit_rect,
            |f, (x0, y0, x1, y1): (i32, i32, i32, i32)| {
                f.put("x0", x0 as u32 as u64);
                f.put("y0", y0 as u32 as u64);
                f.put("x1", x1 as u32 as u64);
                f.put("y1", y1 as u32 as u64);
            },
        );
        w.put("companion.last_alpha", u64::from(self.last_alpha));
        w.flag("companion.last_visible", self.last_visible);
        w.flag("companion.last_static", self.last_static);
        w.at_opt("companion.last_shed_tick", self.last_shed_tick);
        w.group("companion.geom", |f| {
            f.put("cell_w", u64::from(self.geom.cell_w));
            f.put("cell_h", u64::from(self.geom.cell_h));
            f.put("rows", u64::from(self.geom.rows));
            f.put("cols", u64::from(self.geom.cols));
        });
        w.opt(
            "companion.last_sync",
            self.last_sync,
            |f, ((coat, iris), arrival)| {
                f.put("coat", u64::from(coat));
                f.put("iris", u64::from(iris));
                f.put("arrival", arrival as u64);
            },
        );
        w.opt("companion.drawn_frame", self.drawn_frame, pixel_frame_words);
        w.opt(
            "companion.resight_frame",
            self.resight_frame,
            pixel_frame_words,
        );
        w.list("companion.own_glass", self.own_glass, |f, glass| {
            f.opt("glass", glass, |f, g| {
                f.put("key", u64::from(g.key));
                pixel_frame_words(f, g.frame);
                f.flag("offered", g.offered);
            });
        });
        w.put("companion.glass_key", u64::from(self.glass_key));
        w.flag("companion.resight_offered", self.resight_offered);
        w.opt(
            "companion.landed_taken",
            self.landed_taken,
            |f, (_session, n): (u64, u32)| {
                f.put("serial", u64::from(n));
            },
        );
        self.pet.state_words(w);
    }
}

#[inline]
fn session_seq(f: &mut Fold, (_session, seq): (u64, u64)) {
    f.put("seq", seq);
}

#[inline]
fn pixel_frame_words(f: &mut Fold, p: PixelFrame) {
    f.put("origin.x", p.origin.0 as u32 as u64);
    f.put("origin.y", p.origin.1 as u32 as u64);
    f.cell("cell", p.cell);
    f.put("glass.origin.x", p.glass.origin.0 as u32 as u64);
    f.put("glass.origin.y", p.glass.origin.1 as u32 as u64);
    f.put("glass.cols", u64::from(p.glass.cols));
    f.put("glass.rows", u64::from(p.glass.rows));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state_words::{StateWords, Word};
    use aterm_time::{Duration, Instant};

    fn words(c: &CompanionOwner, t0: Instant) -> Vec<Word> {
        let mut w = StateWords::new(t0);
        c.state_words(&mut w);
        w.into_words()
    }

    #[test]
    fn a_hashed_companion_field_moves_the_companion_words() {
        let t0 = Instant::now();
        let base = words(&CompanionOwner::new(), t0);
        assert_eq!(base, words(&CompanionOwner::new(), t0));

        let mut hit = CompanionOwner::new();
        hit.hit_rect = Some((10, 20, 40, 48));
        assert_ne!(words(&hit, t0), base, "the hit rect is hashed");
        let mut shed = CompanionOwner::new();
        shed.last_shed_tick = Some(t0 + Duration::from_millis(250));
        assert_ne!(words(&shed, t0), base, "the shed tick's clock is hashed");
        let mut latched = CompanionOwner::new();
        latched.latches.cmd_seq = Some((1, 2));
        assert_ne!(words(&latched, t0), base, "a session latch is hashed");
        assert!(
            base.iter().any(|&(name, _)| name == "pet.col"),
            "the brain's words are part of the driver's"
        );
    }
}
