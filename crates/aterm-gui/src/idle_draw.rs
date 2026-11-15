// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! How a hidden window draws (`docs/ATERM_DESIGN.md` §7.4). The Apple GPU driver keeps about
//! 179 MiB of working memory for render passes until a second or two after a window's last one,
//! so a window nobody can see draws as little as it can. Its rules are pure functions of the
//! moment and the window's state:
//! - a window that cannot be on any glass (minimised, or its app hidden) presents nothing;
//! - one that is only covered or on another Space presents a changed frame at most once every
//!   [`COVERED_FRAME_PERIOD`], since a window share can still watch it;
//! - either way a capture, a recording, an assistive client, the frame the window is revealed
//!   with, the frame of a raise that gives it the focus or its own first frame is drawn at once
//!   ([`hidden_frame`]);
//! - its own clocks (the blink, the cursor effects, the Settings preview) plan no frame unless
//!   it is recorded ([`clocks_have_glass`]).

use std::time::{Duration, Instant};

/// The least time between two frames of a window that is covered or on another Space: the
/// stats deck's cadence for a window nobody uses (`vitals_host::BACKGROUND_PERIOD`), three times
/// the longest release of the driver's block measured, so the block is gone about four fifths of
/// the time while a window share still sees the window change.
pub(crate) const COVERED_FRAME_PERIOD: Duration = Duration::from_secs(5);

/// Whether a window's own clocks (its blink, its cursor effects and its Settings preview) may plan
/// frames: an OS window may (`has_window`) while it is not hidden (`occluded`), or while it is
/// recorded (`recorded`); a hidden one plans none, and plans again once it is visible.
pub(crate) const fn clocks_have_glass(has_window: bool, occluded: bool, recorded: bool) -> bool {
    has_window && (!occluded || recorded)
}

/// Where a window's frames can be seen, from its occlusion state.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Sight {
    /// Some of the window is on screen.
    Visible,
    /// Fully covered, on another Space, or on a display that sleeps: nobody sees it on screen,
    /// but a window share or a single-window recording still reads its frames.
    Covered,
    /// Minimised, or its app hidden: on no glass and in no window capture.
    OffGlass,
}

impl Sight {
    /// The sight of a window whose occlusion state is `occluded` and that is minimised or whose
    /// app is hidden (`off_glass`).
    pub(crate) const fn of(occluded: bool, off_glass: bool) -> Self {
        match (occluded, off_glass) {
            (false, _) => Self::Visible,
            (true, false) => Self::Covered,
            (true, true) => Self::OffGlass,
        }
    }

    /// The name `ctl trail status` reports as `sight=`.
    pub(crate) const fn name(self) -> &'static str {
        match self {
            Self::Visible => "visible",
            Self::Covered => "covered",
            Self::OffGlass => "off-glass",
        }
    }
}

/// What a hidden window does with its next frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum HiddenFrame {
    /// The frame draws as any visible window's does.
    Draws,
    /// A covered window's frame draws, and starts its next [`COVERED_FRAME_PERIOD`].
    Paced,
    /// The frame parks before any compose; a covered window owes it at the period's end.
    Parks,
}

/// A window seen as `sight`, whose present something waits on (`awaited`: a capture, a
/// recording, an assistive client, its first frame, its reveal or a raise that gave it the
/// focus) and whose last paced frame drew at `paced_at`, draws, paces or parks its frame at
/// `now`.
pub(crate) fn hidden_frame(
    sight: Sight,
    awaited: bool,
    paced_at: Option<Instant>,
    now: Instant,
) -> HiddenFrame {
    match sight {
        Sight::Visible => HiddenFrame::Draws,
        _ if awaited => HiddenFrame::Draws,
        Sight::OffGlass => HiddenFrame::Parks,
        Sight::Covered
            if paced_at
                .is_some_and(|at| now.saturating_duration_since(at) < COVERED_FRAME_PERIOD) =>
        {
            HiddenFrame::Parks
        }
        Sight::Covered => HiddenFrame::Paced,
    }
}

#[cfg(test)]
#[path = "idle_draw_rule_tests.rs"]
mod tests;
