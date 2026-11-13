// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Event-loop clock scheduling for Kitty animation playback.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use crate::App;

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
        let Some((term, temporal_tx, parked)) = self.pool.get(session).map(|s| {
            (
                std::sync::Arc::clone(&s.term),
                s.temporal_tx.clone(),
                s.ctx.update_parked.is_parked(),
            )
        }) else {
            self.kitty_animations.next.remove(&session);
            return;
        };
        if parked {
            if let Some(Some(at)) = self.kitty_animations.next.get_mut(&session) {
                *at = (*at).max(now + LOCK_RETRY);
            }
            return;
        }
        let shown = self.is_visible_session(session);
        if !shown && self.kitty_animations.next.get(&session) == Some(&None) {
            return;
        }
        let Some(mut term) = crate::term_try_lock(&term) else {
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
        if !term.parser_is_ground() {
            let playing = term.kitty_animation_playing();
            drop(term);
            if playing {
                let retry = now + LOCK_RETRY;
                match self.kitty_animations.next.get_mut(&session) {
                    Some(Some(at)) => *at = (*at).max(retry),
                    _ => {
                        self.kitty_animations.next.insert(session, Some(retry));
                    }
                }
            } else {
                self.kitty_animations.next.remove(&session);
            }
            return;
        }
        let pre_host = term.host_ordinal();
        let step = term.advance_kitty_animations(now);
        if let Some(tx) = &temporal_tx
            && term.host_ordinal() != pre_host
        {
            crate::temporal::SpineMark::attach(tx.spine()).flush_host(tx, &term);
        }
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
#[path = "kitty_animation_tests.rs"]
mod tests;
