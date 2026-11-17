// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A window's quiet, attention and wake state.
//!
//! [`Rest::stir`] restarts the quiet period and records a wake if the window was resting.
//! [`Rest::settle`] applies the configured period; `None` keeps the window awake.
//! [`Rest::deck_moves`] holds deck motion while resting and until a reading lands after a wake.
//! A touch attends the window for [`ATTENTION`]; output alone does not attend it.
//! Unless a page on screen reads them, a focused window that rests or is unattended no longer
//! keeps the shared readings on the focused cadence, and they slow once no other window on
//! screen is awake and attended either (`App::vitals_audience`,
//! `crate::vitals_host::attends_readings`; [`crate::vitals_host::BACKGROUND_PERIOD`] says what
//! they slow to, and an open stats lens keeps its own cadence, `Audience::cadence`). The touch
//! that attends an awake window again, where it is focused, the touch focuses it or it is on
//! screen, brings the next reading forward wherever aterm has a focused window, and so does the
//! focus coming to an awake window that is still attended (`App::touch_rest`).
//!
//! This state owns timestamps and creates no timer. The event loop chooses when to call it.

use std::time::{Duration, Instant};

use aterm_window::event::WindowEvent;

/// How long after a person's last touch a window is attended ([`Rest::settle_attended`]).
pub(crate) const ATTENTION: Duration = Duration::from_secs(40);

/// One window's rest: when it was last stirred, whether it rests now, when it last woke (or a
/// touch of it while focused ended a lapse of its attention), when a person last touched it and
/// whether it is attended now.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Rest {
    stirred_at: Instant,
    resting: bool,
    woke_at: Option<Instant>,
    touched_at: Instant,
    attended: bool,
    /// The inner size, `(width, height)` in device pixels, aterm last asked the window for on
    /// behalf of a control client or a program ([`Self::own_resize`]).
    own_size: Option<(u32, u32)>,
}

impl Rest {
    /// A window awake since `now`.
    pub(crate) const fn new(now: Instant) -> Self {
        Self {
            stirred_at: now,
            resting: false,
            woke_at: None,
            touched_at: now,
            attended: true,
            own_size: None,
        }
    }

    /// aterm itself asks the window for `size` (width, height, device pixels) on behalf of a
    /// control client or a program: the `Resized` that reports that size is no person's touch
    /// ([`Self::own`]).
    pub(crate) const fn own_resize(&mut self, size: (u32, u32)) {
        self.own_size = Some(size);
    }

    /// Whether `event` reports a change aterm itself made for a control client or a program: a
    /// `Resized` to the size last given to [`Self::own_resize`]. A `Resized` to any other size
    /// is somebody else's, and forgets that size: a person's drag away from it and back
    /// touches at every step.
    pub(crate) fn own(&mut self, event: &WindowEvent) -> bool {
        let WindowEvent::Resized(size) = event else {
            return false;
        };
        let own = self.own_size == Some((size.width, size.height));
        if !own {
            self.own_size = None;
        }
        own
    }

    /// A person's touch at `now`: the window is attended and its attention starts over. True
    /// when its attention had lapsed. Where it had and the window is `focused` (or the touch
    /// focuses it), the deck then holds still until a reading lands after `now`, as after a
    /// wake ([`Self::deck_moves`]). An unfocused window's deck draws still anyway
    /// (`App::live_facts` reads the focus), so its touch adds no hold of its own, and the focus
    /// that follows moves the deck as it moves an attended window's; a wake's hold
    /// ([`Self::stir`]) stands.
    pub(crate) const fn touch(&mut self, now: Instant, focused: bool) -> bool {
        self.touched_at = now;
        let lapsed = !std::mem::replace(&mut self.attended, true);
        if lapsed && focused {
            self.woke_at = Some(now);
        }
        lapsed
    }

    /// Whether [`Self::settle_attended`] at `now` under `within` would settle the window
    /// attended, settling nothing.
    pub(crate) fn attended_at(&self, now: Instant, within: Option<Duration>) -> bool {
        within.is_none_or(|within| now.saturating_duration_since(self.touched_at) < within)
    }

    /// Settles whether the window is attended at `now`: touched less than `within` ago; `None`
    /// is always attended. True when that changed.
    pub(crate) fn settle_attended(&mut self, now: Instant, within: Option<Duration>) -> bool {
        let attended = self.attended_at(now, within);
        attended != std::mem::replace(&mut self.attended, attended)
    }

    /// Whether a person touched the window within its attention, as last settled.
    pub(crate) const fn attended(&self) -> bool {
        self.attended
    }

    /// A person's event or visible output at `now`: the window is awake and its quiet starts
    /// over. True when it was resting (it woke at `now`); the deck then holds still until a
    /// reading lands after `now` ([`Self::deck_moves`]), whether or not the window is focused,
    /// so the focus that follows an unfocused window's wake moves its deck only from such a
    /// reading.
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
    /// rests or is unattended, and after a wake ([`Self::stir`]) or a focused window's touch
    /// that ended a lapse ([`Self::touch`]) only once a reading has landed since. A focused
    /// window's wake while attended, or such a touch, brings that reading forward
    /// (`App::vitals_ask_now`), and its frame redraws the deck anyway, so the deck's motion comes
    /// back there rather than in the frame of the key or output that woke the window.
    pub(crate) fn deck_moves(&self, landed: Option<Instant>) -> bool {
        !self.resting
            && self.attended
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

#[cfg(all(test, unix))]
#[path = "window_rest_attended_tests.rs"]
mod attended_tests;
