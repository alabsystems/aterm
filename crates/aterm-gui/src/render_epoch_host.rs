// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The event loop's half of the render epoch.

use std::time::{Duration, Instant};

impl crate::App {
    /// Look at every session whose epoch look is due and publish what moved.
    pub(crate) fn observe_render_epochs(&mut self) {
        let now_ms = crate::turn_ledger::now_ms();
        match crate::resize_epoch::next_look_ms() {
            Some(at) if at <= now_ms => {}
            _ => return,
        }
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
            let _ = timeline
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .evaluate_resizes(probe, now_ms);
        }
    }

    /// The instant the loop must wake for a render epoch's look, if one is owed.
    pub(crate) fn next_render_epoch_wake(&self) -> Option<Instant> {
        let at = crate::resize_epoch::next_look_ms()?;
        let now_ms = crate::turn_ledger::now_ms();
        Some(Instant::now() + Duration::from_millis(at.saturating_sub(now_ms)))
    }
}
