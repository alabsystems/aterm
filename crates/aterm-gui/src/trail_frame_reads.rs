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
mod tests {
    use super::*;
    use crate::app_render::CursorFxInputs;
    use aterm_effects::state_words::Word;
    use std::time::Duration;

    /// A headless App typing three keys on row 2 under the rainbow kitty, then resting: its
    /// window's reads at the end, against `t0`.
    fn typed(t0: Instant) -> (App, Instant) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.config.cursor_trail = Some(true);
        app.config.cursor_trail_style = Some("rainbow kitty".into());
        app.config.motion = Some("full".into());
        app.config.trail_sounds = Some(false);
        app.windows.get_mut(&wid).expect("window").focused = true;
        let frame = |app: &mut App, at: Instant, col: u16| {
            let mut fx = CursorFxInputs::sample_for_test(at);
            fx.cur = Some((2, col));
            assert!(app.cursor_frame(wid, fx).is_some(), "a presented tick");
        };
        frame(&mut app, t0, 3);
        let mut now = t0;
        for (i, col) in [4_u16, 5, 6].into_iter().enumerate() {
            let key_at = t0 + Duration::from_millis(100 * (i as u64 + 1));
            let ws = app.windows.get_mut(&wid).expect("window");
            ws.fx.cursor_mut().glow.note_typed_cells(key_at, 1);
            ws.fx.cursor_mut().trail.note_typed(key_at);
            now = key_at + Duration::from_millis(16);
            frame(&mut app, now, col);
        }
        (app, now)
    }

    #[test]
    fn every_window_reads_its_pipelines_words_and_no_light_arm() {
        let t0 = Instant::now();
        let (app, now) = typed(t0);
        let reads = app.trail_frame_reads(t0, now);
        assert_eq!(reads.len(), app.windows.len(), "one read per window");
        let read = &reads[0];
        assert_eq!(read.window, WindowId(0));
        assert!(read.arms.is_empty(), "no light arm draws on a dark frame");
        assert_eq!(read.light, Light::OFF, "the dark frame's light");
        assert!(!read.latch.on, "the dark frame's latch");
        assert_eq!(read.raw, Some(false), "the dark page's raw light");
        assert_eq!(
            read.live,
            aterm_effects::rainbow_kitty::prism_glass::LIVE,
            "no level forced"
        );
        assert!(
            read.light_words.words().is_empty(),
            "the dark frame has no light state"
        );
        assert_eq!(
            read.words.get("rk.engaged"),
            Some(1),
            "the rainbow kitty's engine owns the window's trail"
        );
        assert!(
            read.words.get("ribbon.cells.n").is_some_and(|n| n > 0),
            "the typed keys laid a ribbon"
        );
        assert_eq!(
            read.ledger.words(),
            ws_ledger(&app, t0),
            "the ink ledger's products are the pipeline's"
        );
        assert_eq!(
            read.words.get("ink.notes"),
            None,
            "and not among the state words"
        );
        let ws = &app.windows[&WindowId(0)];
        assert_eq!(read.bed_ink.key(), ws.fx.bed_ink_lut().key());
        assert!(
            read.bed_ink.key().is_some(),
            "the frame synced the bed's table"
        );
        assert!(
            read.glow_deadline.is_some_and(|d| d > now),
            "a live trail has a next change"
        );
        assert_eq!(
            read.pet_deadline,
            ws.fx.companion().next_change_deadline(now),
            "the pet's next step is the window's own"
        );
        assert_eq!(
            read.words_deadline, ws.deco_anim_until,
            "the word latch is the window's own"
        );
        let again = app.trail_frame_reads(t0, now);
        assert_eq!(
            again[0].words.words(),
            read.words.words(),
            "a read changes nothing"
        );
    }

    /// The ledger words of `app`'s first window's pipeline, against `t0`.
    fn ws_ledger(app: &App, t0: Instant) -> Vec<Word> {
        app.windows[&WindowId(0)].fx.ledger_words(t0).into_words()
    }

    /// The `shed` words of `app`'s first window, against `t0`.
    fn shed(app: &App, t0: Instant) -> Vec<Word> {
        app.trail_frame_reads(t0, t0)[0]
            .words
            .words()
            .iter()
            .copied()
            .filter(|&(n, _)| n.starts_with("shed."))
            .collect()
    }

    /// A headless App whose latch read `costs`, one present every 16 ms frame after `at`.
    fn fed(at: Instant, costs: &[u64]) -> App {
        let mut app = App::headless_for_test();
        let interval = Duration::from_millis(16);
        for (i, &ns) in (1_u32..).zip(costs) {
            app.note_present_cost(ns, interval, at + interval * i);
        }
        app
    }

    /// A named change to a fresh App's shed latch, given the dump's `t0`.
    type Case = (&'static str, fn(&mut App, Instant));

    #[test]
    fn the_shed_latch_is_in_every_windows_words() {
        let t0 = Instant::now();
        let base = shed(&App::headless_for_test(), t0);
        let cases: [Case; 6] = [
            ("the cost's running mean", |a, _| {
                a.present_cost_ema_ns = Some(4.0e6);
            }),
            ("the run toward a flip", |a, _| a.perf_run = 2),
            ("the shed", |a, _| a.perf_reduced = true),
            ("the last flip", |a, t0| a.perf_flip_at = Some(t0)),
            ("the envelope it flipped from", |a, _| {
                a.perf_env_at_flip = 0.5;
            }),
            ("the dwell", |a, _| a.perf_shed_dwell *= 2),
        ];
        for (what, set) in cases {
            let mut app = App::headless_for_test();
            set(&mut app, t0);
            assert_ne!(shed(&app, t0), base, "{what} is hashed");
        }

        let slow = [25_000_000; 3];
        let shedding = fed(t0, &slow);
        assert!(
            shedding.perf_reduced,
            "fixture: three presents over 1.5 frames shed"
        );
        let later = t0 + Duration::from_secs(5);
        assert_eq!(
            shed(&fed(later, &slow), later),
            shed(&shedding, t0),
            "the same costs fed 5 s later write the same words"
        );
        assert_ne!(
            shed(&fed(later, &slow), t0),
            shed(&shedding, t0),
            "the negative control: read against another start, the flip moves"
        );
    }

    #[test]
    fn a_window_read_moves_with_the_windows_trail() {
        let t0 = Instant::now();
        let (mut app, now) = typed(t0);
        let before = app.trail_frame_reads(t0, now);
        let later = now + Duration::from_millis(16);
        let mut fx = CursorFxInputs::sample_for_test(later);
        fx.cur = Some((2, 9));
        assert!(app.cursor_frame(WindowId(0), fx).is_some());
        let after = app.trail_frame_reads(t0, later);
        assert_ne!(
            after[0].words.words(),
            before[0].words.words(),
            "a caret jump moves the window's state words"
        );
    }
}
