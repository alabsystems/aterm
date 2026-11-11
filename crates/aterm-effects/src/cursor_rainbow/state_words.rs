// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The rainbow caret's state words (`crate::state_words`): its clocks, its energy and paint
//! latches, its flare and the settle probe's latched inputs. The frame fingerprints, the rim
//! memo and the dry-run memo and sink are output and caches, and are not hashed.

use super::{CursorRainbow, RainbowConfig, SettleProbe};
use crate::state_words::{Fold, Words, geom_words};

impl CursorRainbow {
    /// The animation state and the settle probe.
    #[inline]
    pub(crate) fn state_words(&self, w: &mut impl Words) {
        w.f32("rainbow.phase", self.phase);
        w.f32("rainbow.pulse", self.pulse);
        w.at_opt("rainbow.last", self.last);
        w.f32("rainbow.energy", self.energy);
        w.f32("rainbow.paint", self.paint);
        w.put("rainbow.flare_rung", u64::from(self.flare_rung));
        w.at_opt("rainbow.twinkle_at", self.twinkle_at);
        w.put("rainbow.twinkle_seq", u64::from(self.twinkle_seq));
        w.f32("rainbow.spin", self.spin);
        w.flag("rainbow.twinkling", self.twinkling);
        w.flag("rainbow.flaring", self.flaring);
        w.opt(
            "rainbow.settle_probe",
            self.settle_probe,
            settle_probe_words,
        );
    }
}

/// The settle probe's latched inputs, every field.
#[inline]
fn settle_probe_words(f: &mut Fold, p: SettleProbe) {
    f.cell_opt("cur", p.cur);
    f.f32("energy", p.energy);
    f.f32("energy_prev", p.energy_prev);
    f.opt("family", p.family, |f, (a, b): (f32, f32)| {
        f.f32("a", a);
        f.f32("b", b);
    });
    f.flag("blink_phase", p.blink_phase);
    f.flag("dark_theme", p.dark_theme);
    f.group("geom", |f| geom_words(f, &p.geom));
    f.group("cfg", |f| rainbow_cfg_words(f, &p.cfg));
    f.opt("paint_prev", p.paint_prev, |f, v| f.f32("v", v));
    f.f32("sample_dt", p.sample_dt);
}

#[inline]
fn rainbow_cfg_words(f: &mut Fold, c: &RainbowConfig) {
    f.flag("enabled", c.enabled);
    f.f32("intensity", c.intensity);
    f.flag("blinking", c.blinking);
    f.put(
        "base",
        match c.base {
            Some(v) => u64::from(v),
            None => u64::MAX,
        },
    );
    f.put(
        "head_rgb",
        match c.head_rgb {
            Some(v) => u64::from(v),
            None => u64::MAX,
        },
    );
    f.opt("paint", c.paint, |f, v| f.f32("v", v));
    f.put(
        "ground",
        match c.ground {
            Some(v) => u64::from(v),
            None => u64::MAX,
        },
    );
    f.at_opt("flare_at", c.flare_at);
}

#[cfg(test)]
#[path = "state_words_tests.rs"]
mod tests;
