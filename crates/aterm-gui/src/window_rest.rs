// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A resting window: one that nobody has touched, and whose visible content has not changed, for
//! `idle_rest_secs` (ATERM-KI-1027-4, `docs/DESIGN-stats-deck-live-2026-10-24.md` §13).
//!
//! A resting window stops its classic caret blink on the visible phase, and its stats deck draws
//! as an unfocused window's deck does: no live-edge motion. aterm takes one stream of readings
//! for every reader, and the rest slows it to a reading every
//! [`crate::vitals_host::BACKGROUND_PERIOD`] only while no page reads it: a Tab Control or
//! Allocation page on screen keeps a focused window's cadence, and an open stats lens its own
//! (`App::vitals_audience`, `Audience::cadence`). Otherwise the GPU gets several quiet
//! seconds at a time, and the Apple driver frees the ~179 MB block it keeps for render passes a
//! second or two after the last one (`docs/measured/idle-memory-window-2026-10-27.md`).
//!
//! The first person's event in the window ([`stirs`]) or the first output its active tab shows
//! wakes it at once. Its deck moves again from the first reading that lands after the wake
//! ([`Rest::deck_moves`]), not on the wake's own frame: a deck that went from still to moving
//! there patched its dial and now box into the frame that shows the key's echo, which measured
//! about 3 ms more key-to-present than an awake window's
//! (`docs/measured/idle-memory-rest-2026-10-29.md`). Nothing wakes the loop to put a window to
//! rest: while the window draws something, that drawing's own deadline wakes the loop, which
//! then sees the rest is due; a window that draws nothing has nothing to stop.

use std::time::{Duration, Instant};

use aterm_window::event::WindowEvent;

/// One window's rest: when it was last stirred, whether it rests now, and when it last woke.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rest {
    stirred_at: Instant,
    resting: bool,
    woke_at: Option<Instant>,
}

impl Rest {
    /// A window awake since `now`.
    pub(crate) const fn new(now: Instant) -> Self {
        Self {
            stirred_at: now,
            resting: false,
            woke_at: None,
        }
    }

    /// A person's event or visible output at `now`: the window is awake and its quiet starts
    /// over. True when it was resting (it woke at `now`).
    pub(crate) const fn stir(&mut self, now: Instant) -> bool {
        self.stirred_at = now;
        let woke = std::mem::replace(&mut self.resting, false);
        if woke {
            self.woke_at = Some(now);
        }
        woke
    }

    /// Puts the window to rest once it has been quiet for `after` since its last stir; `None`
    /// never rests and wakes a resting window (a reload that turns rest off applies at once).
    /// True when that changed whether it rests.
    pub(crate) fn settle(&mut self, now: Instant, after: Option<Duration>) -> bool {
        let resting =
            after.is_some_and(|after| now.saturating_duration_since(self.stirred_at) >= after);
        resting != std::mem::replace(&mut self.resting, resting)
    }

    /// Whether the window rests now.
    pub(crate) const fn resting(&self) -> bool {
        self.resting
    }

    /// Whether the window's deck may move, given when the last reading `landed`: not while it
    /// rests, and after a wake only once a reading has landed since. The wake asks that reading
    /// at once, and its frame redraws the deck anyway, so the deck's motion comes back there
    /// rather than in the frame of the key or output that woke the window.
    pub(crate) fn deck_moves(&self, landed: Option<Instant>) -> bool {
        !self.resting
            && self
                .woke_at
                .is_none_or(|woke| landed.is_some_and(|at| at > woke))
    }
}

/// Whether `event` is a person's touch on the window, which wakes it: a key, a pointer move, a
/// click or scroll, the focus coming to it, a move or resize, a file dragged over it. A frame,
/// an occlusion change, the focus leaving, a theme change and the window's closing are not: a
/// frame never stirs its own window.
pub(crate) const fn stirs(event: &WindowEvent) -> bool {
    match event {
        WindowEvent::KeyboardInput { .. }
        | WindowEvent::ModifiersChanged(_)
        | WindowEvent::Ime(_)
        | WindowEvent::CursorMoved { .. }
        | WindowEvent::CursorEntered { .. }
        | WindowEvent::CursorLeft { .. }
        | WindowEvent::MouseInput { .. }
        | WindowEvent::MouseWheel { .. }
        | WindowEvent::AxisMotion { .. }
        | WindowEvent::Touch(_)
        | WindowEvent::Focused(true)
        | WindowEvent::Resized(_)
        | WindowEvent::ScaleFactorChanged { .. }
        | WindowEvent::Moved(_)
        | WindowEvent::DroppedFile(_)
        | WindowEvent::HoveredFile(_)
        | WindowEvent::HoveredFileCancelled => true,
        WindowEvent::RedrawRequested
        | WindowEvent::FrameTick { .. }
        | WindowEvent::Occluded(_)
        | WindowEvent::Focused(false)
        | WindowEvent::ThemeChanged(_)
        | WindowEvent::CloseRequested
        | WindowEvent::Destroyed => false,
    }
}

#[cfg(test)]
#[path = "window_rest_tests.rs"]
pub(crate) mod tests;
