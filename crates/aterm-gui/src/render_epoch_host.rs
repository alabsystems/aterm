// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The event loop's half of the render epoch (`crate::resize_epoch`): the look
//! a displaced screen is owed once the risk window has passed.
//!
//! The ledger registers the look when it books a resize ([`crate::resize_epoch::note_look`]);
//! the loop folds the earliest one under `DeadlineOwner::RenderEpoch` and runs
//! it here. A machine with no displaced screen registers nothing, so an idle
//! window never wakes for this (FL-1).
//!
//! LOCKS. A look never holds the terminal and a session's timeline together:
//! the timeline (a strict leaf) says whether a look is due, the terminal is
//! `try_lock`ed on its own for the probe, and the timeline is taken again to
//! judge. A terminal the PTY reader holds is asked again after 100 ms, at most
//! 20 times, then at the next risk window: no spin.

use std::time::{Duration, Instant};

impl crate::App {
    /// Look at every session whose epoch look is due and publish what moved.
    /// Cheap when nothing is owed: one atomic load.
    pub(crate) fn observe_render_epochs(&mut self) {
        let now_ms = crate::turn_ledger::now_ms();
        match crate::resize_epoch::next_look_ms() {
            Some(at) if at <= now_ms => {}
            _ => return,
        }
        // Reset the minimum: each ledger re-registers what it still owes as it
        // is looked at, and a registration that races this walk survives.
        crate::resize_epoch::take_looks();
        for session in self.pool.iter() {
            let timeline = &session.ctx.timeline;
            {
                let tl = timeline.lock().unwrap_or_else(|p| p.into_inner());
                if !tl.resizes().look_due(now_ms) {
                    if let Some(at) = tl.resizes().look_at_ms() {
                        crate::resize_epoch::note_look(at);
                    }
                    continue;
                }
            }
            let Ok(guard) = session.term.try_lock() else {
                timeline
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .defer_resize_look(now_ms);
                continue;
            };
            let probe = crate::resize_ledger::EngineProbe::sample(&guard);
            drop(guard);
            let moved = timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .evaluate_resizes(probe, now_ms);
            if moved && self.subscribers.any() {
                self.subscribers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .notify(session.id);
            }
        }
    }

    /// The instant the loop must wake for a render epoch's look, if one is owed.
    pub(crate) fn next_render_epoch_wake(&self) -> Option<Instant> {
        let at = crate::resize_epoch::next_look_ms()?;
        let now_ms = crate::turn_ledger::now_ms();
        Some(Instant::now() + Duration::from_millis(at.saturating_sub(now_ms)))
    }
}
