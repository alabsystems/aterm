// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The light latch (`docs/design/PRISM-GLASS.md` §2.1, §15.2 1.2): whether a window's frame draws
//! by the light rules.
//!
//! A frame may be light only when the front session's CONFIGURED scheme is light
//! ([`scheme_light`]: the pair its config or tab theme names, which DECSCNM and OSC 10/11 never
//! move) and the live page is a light ground ([`raw_light`]). Leaving is immediate: a frame that
//! is not both is off. Entering is latched: a light frame is on at once when its key is fresh
//! (the window's first frame, a new front session or a moved configured pair, held pending across
//! the frames that are not light until the first one that is), when the page comes back within
//! [`LIGHT_ENTRY_HOLD`] of the last frame that was on, and otherwise once raw light has held that
//! long. [`LightLatch::admits`] runs on every frame, never behind a short-circuit.
//! `aterm_models_effects::light_latch_model` is its model, which
//! `tests/light_latch_conformance.rs` holds it to.

use std::time::Duration;

use aterm_time::Instant;

use crate::color_math::{relative_luminance_table, relative_luminance_with};

/// How long raw light must hold on a light scheme before it enters with no other licence.
pub const LIGHT_ENTRY_HOLD: Duration = Duration::from_millis(300);

/// What a window's frame is latched on: its front session, and that session's configured
/// `(fg, bg)` (`0x00RRGGBB`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct LatchKey {
    pub session: u64,
    pub fg: u32,
    pub bg: u32,
}

/// One frame's verdict.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct LatchOut {
    /// The frame draws by the light rules.
    pub on: bool,
    /// The key is not the one the latch saw on its last frame, or this is its first: a new front
    /// session, or a writer moved the configured pair.
    pub scheme_changed: bool,
    /// The last frame was on and this one is not.
    pub fell: bool,
}

/// Whether the live page `(fg, bg)` is a light ground: not dark by main's polarity (`dark_theme`,
/// the frame's [`aterm_render::theme_is_dark`] of `bg`), its ink darker than its page, and the
/// two at least 3:1 apart. A colour that is no resolved `0x00RRGGBB` (`COLOR_UNSET`) is never
/// light.
#[must_use]
#[inline]
pub const fn raw_light(fg: u32, bg: u32, dark_theme: bool) -> bool {
    if !resolved(fg) || !resolved(bg) || dark_theme {
        return false;
    }
    let table = relative_luminance_table();
    let (l_fg, l_bg) = (
        relative_luminance_with(table, fg),
        relative_luminance_with(table, bg),
    );
    l_fg < l_bg && (l_bg + 0.05) / (l_fg + 0.05) >= 3.0
}

/// Whether a configured background is a light scheme's: main's polarity,
/// [`aterm_render::theme_is_dark`]. A colour that is no resolved `0x00RRGGBB` is never light.
#[must_use]
#[inline]
pub fn scheme_light(bg: u32) -> bool {
    resolved(bg) && !aterm_render::theme_is_dark(bg)
}

/// `rgb` is a packed colour, not a sentinel in the high byte.
#[inline]
const fn resolved(rgb: u32) -> bool {
    rgb >> 24 == 0
}

/// One window's latch.
#[derive(Clone, Copy, Debug, Default)]
pub struct LightLatch {
    /// The last frame's key; `None` before the first frame.
    key: Option<LatchKey>,
    /// A key change no light frame has met yet.
    pending: bool,
    /// The first frame of the current run of light frames.
    raw_since: Option<Instant>,
    /// The last frame that was on.
    last_on: Option<Instant>,
    /// The last frame's verdict.
    out: LatchOut,
}

impl LightLatch {
    /// The verdict for the frame at `now` on `key`, whose live page reads `raw` ([`raw_light`]).
    #[inline]
    pub fn admits(&mut self, now: Instant, key: LatchKey, raw: bool) -> LatchOut {
        let scheme_changed = self.key.replace(key) != Some(key);
        let fresh = self.pending || scheme_changed;
        let on = if raw && scheme_light(key.bg) {
            self.pending = false;
            let since = *self.raw_since.get_or_insert(now);
            fresh
                || now.saturating_duration_since(since) >= LIGHT_ENTRY_HOLD
                || self
                    .last_on
                    .is_some_and(|t| since.saturating_duration_since(t) <= LIGHT_ENTRY_HOLD)
        } else {
            self.pending = fresh;
            self.raw_since = None;
            false
        };
        if on {
            self.last_on = Some(now);
        }
        self.out = LatchOut {
            on,
            scheme_changed,
            fell: self.out.on && !on,
        };
        self.out
    }

    /// The last frame's verdict; all false before the first frame.
    #[inline]
    #[must_use]
    pub const fn last(&self) -> LatchOut {
        self.out
    }
}

#[cfg(test)]
#[path = "light_latch_tests.rs"]
mod tests;
