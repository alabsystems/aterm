// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each window's trail frame reads for `lab trail-diff`'s dump (`docs/design/PRISM-GLASS.md`
//! §15.1 P0.3, §15.7.2): the window's effects pipeline's state words with the App's load-shed
//! latch beside them, its ink ledger's per-frame products (§15.9's preamble), its light report,
//! frame light and light words, its light latch, the bed's
//! ink table, the glow's next change at the window's own present interval, the pet's next step
//! and the end of the host's word-decoration latch. The dump drives the headless App through the
//! production frame and reads these after each frame ([`crate::trail_rig`]). Debug builds of the
//! tests and, on Unix, of the `bench-support` feature.
//!
//! The shed latch is the App's, one for every window, so each window's words carry it as the
//! `shed` group: the present cost's running mean, the run toward a flip, whether it sheds, when
//! it last flipped (relative to the dump's `t0`) with the envelope it flipped from, and the dwell
//! the next clear waits out.

use super::*;
use aterm_effects::cursor_glow::{Light, Live};
use aterm_effects::host::light_latch::LatchOut;
use aterm_effects::rainbow_kitty::ribbon::BedInkLut;
use aterm_effects::state_words::{ArmsDrawn, StateWords, Words};

/// One window's trail reads after a frame.
#[doc(hidden)]
pub struct WindowTrailReads {
    /// The window read.
    pub(crate) window: WindowId,
    /// Its pipeline's state words, then the App's shed latch's, every time relative to the
    /// dump's `t0`.
    pub words: StateWords,
    /// Its pipeline's ink ledger's per-frame products (`EffectsPipeline::ledger_words`), every
    /// time relative to the dump's `t0`: compared as a stream, not as state.
    pub ledger: StateWords,
    /// Which light arms drew its last frame.
    pub arms: ArmsDrawn,
    /// The light the frame at `now` ran under (`EffectsPipeline::frame_light`).
    pub light: Light,
    /// The window's light latch as its last frame left it (`TrailGround::last`).
    pub latch: LatchOut,
    /// The level a frame the latch admits lights at (`TrailGround::live`).
    pub live: Live,
    /// The last grounded frame's raw light (`TrailGround::seen`); `None` before the first.
    pub raw: Option<bool>,
    /// Its pipeline's light-only state (`EffectsPipeline::light_words`), every time relative to
    /// the dump's `t0`: never compared.
    pub light_words: StateWords,
    /// The bed's ink table its trail draws by.
    pub bed_ink: BedInkLut,
    /// The glow's next visible change after `now` at the window's present interval.
    pub glow_deadline: Option<Instant>,
    /// The pet's next visible step after `now` (`CompanionOwner::next_change_deadline`).
    pub pet_deadline: Option<Instant>,
    /// The instant the host's word-decoration latch keeps frames coming until.
    pub words_deadline: Option<Instant>,
}

impl App {
    /// Every window's trail reads, in window order, each time relative to `t0`, and the frame
    /// light and the glow's and the pet's next changes asked at `now`.
    pub(crate) fn trail_frame_reads(&self, t0: Instant, now: Instant) -> Vec<WindowTrailReads> {
        self.windows
            .iter()
            .map(|(&window, ws)| WindowTrailReads {
                window,
                words: {
                    let mut words = ws.fx.state_words(t0);
                    self.shed_words(&mut words);
                    words
                },
                ledger: ws.fx.ledger_words(t0),
                arms: ws.fx.light_arms_drawn(),
                light: ws.fx.frame_light(now),
                latch: ws.trail_ground.last(),
                live: ws.trail_ground.live(),
                raw: ws.trail_ground.seen().map(|(_, raw)| raw),
                light_words: ws.fx.light_words(t0),
                bed_ink: *ws.fx.bed_ink_lut(),
                glow_deadline: ws
                    .fx
                    .glow_next_change_deadline(now, ws.effect_present_interval()),
                pet_deadline: ws.fx.companion().next_change_deadline(now),
                words_deadline: ws.deco_anim_until,
            })
            .collect()
    }

    /// The load-shed latch ([`App::note_present_cost`]) as the `shed` group.
    fn shed_words(&self, w: &mut impl Words) {
        w.opt("shed.cost_ema", self.present_cost_ema_ns, |f, ns| {
            f.f64("ns", ns);
        });
        w.put("shed.run", u64::from(self.perf_run));
        w.flag("shed.reduced", self.perf_reduced);
        w.at_opt("shed.flip_at", self.perf_flip_at);
        w.f32("shed.env_at_flip", self.perf_env_at_flip);
        w.dur("shed.dwell", self.perf_shed_dwell);
    }
}

#[cfg(test)]
#[path = "trail_frame_reads_tests.rs"]
mod tests;
