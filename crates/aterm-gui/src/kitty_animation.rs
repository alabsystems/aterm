// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! KITTY ANIMATION PLAYBACK, the host's half: the loop's clock for the frames
//! the engine plays.
//!
//! The engine plays a Kitty graphics animation
//! ([`aterm_core::terminal::Terminal::advance_kitty_animations`]): given the
//! time, it shows every frame that is due and says when the next one is. This
//! module keeps that one instant per session and wakes the loop for it, so an
//! animation is a scheduled wake per frame and never a busy redraw, and a
//! session with nothing playing on screen folds nothing — an idle terminal
//! never wakes for it.
//!
//! A session's clock is looked at when its program writes
//! ([`App::tick_kitty_animations`] from the output wake: the bytes may start,
//! stop or feed an animation) and when its frame comes due
//! ([`App::service_kitty_animations`] on every wake, the way the other level
//! clocks are judged, so a busy loop never re-arms a past instant). A frame
//! that changed repaints every window showing the session. Kitty plays only
//! what it draws: a session no window shows keeps its animation PARKED, with
//! no wake, and the first wake that finds it shown again plays it on.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::App;

/// How long a due frame waits when its terminal is mid-batch: the reader holds
/// the lock for one chunk, and its output wake looks at the clock again.
const LOCK_RETRY: Duration = Duration::from_millis(4);

/// Each session's next Kitty animation frame: `Some(at)` due at `at`, `None`
/// parked (playing, but no window shows the session). A session with nothing
/// playing has no entry.
#[derive(Debug, Default)]
pub(crate) struct KittyAnimationClocks {
    next: HashMap<u64, Option<Instant>>,
}

impl KittyAnimationClocks {
    /// The soonest frame due: the loop's one wake for every animation.
    pub(crate) fn deadline(&self) -> Option<Instant> {
        self.next.values().flatten().min().copied()
    }

    /// The sessions to look at `now`: a frame due, or parked (to see whether
    /// a window shows them again).
    fn due(&self, now: Instant) -> Vec<u64> {
        let mut due: Vec<u64> = self
            .next
            .iter()
            .filter(|(_, at)| at.is_none_or(|at| at <= now))
            .map(|(&session, _)| session)
            .collect();
        due.sort_unstable();
        due
    }
}

impl App {
    /// Play `session`'s Kitty animations up to `now` and keep its next frame
    /// on the loop's clock; a frame that changed repaints the windows showing
    /// it. A busy terminal is retried shortly rather than waited for.
    pub(crate) fn tick_kitty_animations(&mut self, session: u64, now: Instant) {
        let Some(term) = self
            .pool
            .get(session)
            .map(|s| std::sync::Arc::clone(&s.term))
        else {
            self.kitty_animations.next.remove(&session);
            return;
        };
        let shown = self.is_visible_session(session);
        if !shown && self.kitty_animations.next.get(&session) == Some(&None) {
            return;
        }
        let Ok(mut term) = term.try_lock() else {
            if let Some(Some(at)) = self.kitty_animations.next.get_mut(&session) {
                *at = (*at).max(now + LOCK_RETRY);
            }
            return;
        };
        if !shown {
            let playing = term.kitty_animation_playing();
            drop(term);
            if playing {
                self.kitty_animations.next.insert(session, None);
            } else {
                self.kitty_animations.next.remove(&session);
            }
            return;
        }
        let step = term.advance_kitty_animations(now);
        drop(term);
        match step.next_frame_at {
            Some(at) => {
                self.kitty_animations.next.insert(session, Some(at));
            }
            None => {
                self.kitty_animations.next.remove(&session);
            }
        }
        if step.advanced {
            let _ = self.admit_content_redraws(session, now, false);
        }
    }

    /// Look at every session whose frame is due at `now`, and every parked one.
    pub(crate) fn service_kitty_animations(&mut self, now: Instant) {
        for session in self.kitty_animations.due(now) {
            self.tick_kitty_animations(session, now);
        }
    }

    /// When the loop must next wake for a Kitty animation frame, if ever.
    pub(crate) fn kitty_animation_deadline(&self) -> Option<Instant> {
        self.kitty_animations.deadline()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One Kitty graphics command.
    fn apc(control: &str, payload: &[u8]) -> Vec<u8> {
        let mut seq = format!("\x1b_G{control}").into_bytes();
        if !payload.is_empty() {
            seq.push(b';');
            seq.extend_from_slice(
                aterm_codec::base64::encode(payload)
                    .expect("encode")
                    .as_bytes(),
            );
        }
        seq.extend_from_slice(b"\x1b\\");
        seq
    }

    /// The host plays the engine's animation on the loop's clock: an idle
    /// terminal arms nothing; a running animation on screen arms exactly its
    /// next frame, and serving that wake shows the frame and arms the one
    /// after; a stopped one disarms. Before 2026-09-29 nothing played frames
    /// on a clock at all.
    #[test]
    fn a_playing_animation_arms_one_wake_per_frame_and_an_idle_one_none() {
        let mut app = App::headless_for_test();
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        app.tick_kitty_animations(0, t0);
        assert_eq!(app.kitty_animation_deadline(), None, "idle: no wake");

        let term = std::sync::Arc::clone(&app.pool.get(0).expect("session").term);
        let placed_frame = || {
            crate::term_lock(&term)
                .images_row(0)
                .first()
                .map(|(_, image)| image.image.bytes[0])
        };
        let at = aterm_core::terminal::ClockReading {
            monotonic: t0,
            wall_ms: None,
        };
        {
            let mut term = crate::term_lock(&term);
            term.set_cell_pixel_size(10, 20);
            for control in [
                "a=t,f=32,s=10,v=20,i=7,q=2",
                "a=f,f=32,s=10,v=20,i=7,z=30,q=2",
            ] {
                let pixel = if control.starts_with("a=t") { 1 } else { 2 };
                term.process_at(&apc(control, &[pixel; 800]), at);
            }
            term.process_at(&apc("a=a,i=7,r=1,z=30", b""), at);
            term.process_at(&apc("a=p,i=7,C=1,q=2", b""), at);
            term.process_at(&apc("a=a,i=7,s=3", b""), at);
        }
        app.tick_kitty_animations(0, ms(10));
        assert_eq!(
            app.kitty_animation_deadline(),
            Some(ms(30)),
            "the root's 30 ms gap, not now"
        );
        assert_eq!(placed_frame(), Some(1));

        app.service_kitty_animations(ms(20));
        assert_eq!(placed_frame(), Some(1), "not due yet");
        app.service_kitty_animations(ms(30));
        assert_eq!(placed_frame(), Some(2), "the due frame is shown");
        assert_eq!(
            app.kitty_animation_deadline(),
            Some(ms(60)),
            "and the one after it armed"
        );

        crate::term_lock(&term).process_at(&apc("a=a,i=7,s=1", b""), at);
        app.tick_kitty_animations(0, ms(40));
        assert_eq!(app.kitty_animation_deadline(), None, "stopped: no wake");
    }

    /// Kitty plays only what it draws: an animation in a tab no window shows
    /// is PARKED — no wake, its frame held — and the first look after the tab
    /// is shown again plays it on, arming its next frame.
    #[test]
    fn an_animation_in_a_hidden_tab_waits_until_the_tab_shows() {
        let mut app = App::headless_for_test();
        let window = crate::WindowId(0);
        let t0 = Instant::now();
        let ms = |n: u64| t0 + Duration::from_millis(n);
        let term = std::sync::Arc::clone(&app.pool.get(0).expect("session").term);
        let placed_frame = || {
            crate::term_lock(&term)
                .images_row(0)
                .first()
                .map(|(_, image)| image.image.bytes[0])
        };
        let at = aterm_core::terminal::ClockReading {
            monotonic: t0,
            wall_ms: None,
        };
        {
            let mut term = crate::term_lock(&term);
            term.set_cell_pixel_size(10, 20);
            term.process_at(&apc("a=t,f=32,s=10,v=20,i=7,q=2", &[1; 800]), at);
            term.process_at(&apc("a=f,f=32,s=10,v=20,i=7,z=30,q=2", &[2; 800]), at);
            term.process_at(&apc("a=a,i=7,r=1,z=30", b""), at);
            term.process_at(&apc("a=p,i=7,C=1,q=2", b""), at);
            term.process_at(&apc("a=a,i=7,s=3", b""), at);
        }
        app.push_stub_tab(window, crate::stub_session(1));
        app.tick_kitty_animations(0, ms(10));
        assert_eq!(app.kitty_animation_deadline(), None, "parked: no wake");
        app.service_kitty_animations(ms(100));
        assert_eq!(
            placed_frame(),
            Some(1),
            "a hidden animation holds its frame"
        );
        assert_eq!(app.kitty_animation_deadline(), None, "and stays parked");

        app.switch_tab_in(window, 0);
        app.service_kitty_animations(ms(110));
        assert_eq!(placed_frame(), Some(2), "shown again, it plays on");
        assert_eq!(app.kitty_animation_deadline(), Some(ms(140)));
    }

    /// The loop wakes for the soonest frame only, parked sessions arm nothing,
    /// and a parked session is always looked at (to be resumed once shown).
    #[test]
    fn the_clock_wakes_for_the_soonest_frame_and_never_for_a_parked_one() {
        let t0 = Instant::now();
        let ms = |n| t0 + Duration::from_millis(n);
        let mut clocks = KittyAnimationClocks::default();
        assert_eq!(clocks.deadline(), None, "nothing playing: no wake");
        clocks.next.insert(1, Some(ms(40)));
        clocks.next.insert(2, Some(ms(15)));
        clocks.next.insert(3, None);
        assert_eq!(clocks.deadline(), Some(ms(15)));
        assert_eq!(clocks.due(ms(10)), [3], "only the parked one before 15 ms");
        assert_eq!(clocks.due(ms(15)), [2, 3]);
        assert_eq!(clocks.due(ms(40)), [1, 2, 3]);
        clocks.next.remove(&2);
        clocks.next.remove(&1);
        assert_eq!(clocks.deadline(), None, "a parked animation arms no wake");
    }
}
