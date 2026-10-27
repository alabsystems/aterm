// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Each window's trail frame reads for `lab trail-diff`'s dump (`docs/design/PRISM-GLASS.md`
//! §15.1 P0.3, §15.7.2): the window's effects pipeline's state words, its light report, the bed's
//! ink table, the glow's next change at the window's own present interval, the pet's next step
//! and the end of the host's word-decoration latch. The dump drives the headless App through the
//! production frame and reads these after each frame ([`crate::trail_rig`]). Debug builds of the
//! tests and, on Unix, of the `bench-support` feature.

use super::*;
use aterm_effects::rainbow_kitty::ribbon::BedInkLut;
use aterm_effects::state_words::{ArmsDrawn, StateWords};

/// One window's trail reads after a frame.
#[doc(hidden)]
pub struct WindowTrailReads {
    /// The window read.
    pub(crate) window: WindowId,
    /// Its pipeline's state words, every time relative to the dump's `t0`.
    pub words: StateWords,
    /// Which light arms drew its last frame.
    pub arms: ArmsDrawn,
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
    /// Every window's trail reads, in window order, each time relative to `t0` and the glow's
    /// and the pet's next changes asked at `now`.
    pub(crate) fn trail_frame_reads(&self, t0: Instant, now: Instant) -> Vec<WindowTrailReads> {
        self.windows
            .iter()
            .map(|(&window, ws)| WindowTrailReads {
                window,
                words: ws.fx.state_words(t0),
                arms: ws.fx.light_arms_drawn(),
                bed_ink: *ws.fx.bed_ink_lut(),
                glow_deadline: ws
                    .fx
                    .glow_next_change_deadline(now, ws.effect_present_interval()),
                pet_deadline: ws.fx.companion().next_change_deadline(now),
                words_deadline: ws.deco_anim_until,
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_render::CursorFxInputs;
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
        assert!(read.arms.is_empty(), "no light arm exists yet");
        assert_eq!(
            read.words.get("rk.engaged"),
            Some(1),
            "the rainbow kitty's engine owns the window's trail"
        );
        assert!(
            read.words.get("ribbon.cells.n").is_some_and(|n| n > 0),
            "the typed keys laid a ribbon"
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
