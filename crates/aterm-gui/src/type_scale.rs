// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The chrome type scale: one named 5-step scale for every text size the own-rendered
//! chrome draws.
//!
//! TOTALITY BY CONSTRUCTION: a [`DrawPrim::Text`](crate::widget::DrawPrim) is
//! only constructible through [`crate::widget::text_prim`], whose size
//! parameter is [`StepPx`] — and `StepPx`'s field is private to this module,
//! so the ONLY way to mint one is [`TypeStep::px`] / [`TypeStep::px_clamped`].
//! Every chrome text site therefore maps to a named step; an orphan multiplier
//! cannot compile. The funnel itself is [`crate::widget::text_prim`].

/// A named step of the chrome type scale, largest to smallest. Each chrome surface passes
/// its own base size (the overlay cards use the terminal `font_px`).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TypeStep {
    /// The wordmark.
    Display,
    /// Overlay titles ("Settings", "Commands") and the tray's headline values.
    Title,
    /// Row labels and primary content (the 1.0 anchor).
    Body,
    /// Values, buttons, menu options, field text.
    Secondary,
    /// Hints, footers, section headers, chips, counts, captions.
    Caption,
}

impl TypeStep {
    /// The step's multiplier on the chrome base size: the one place a text size factor is
    /// written.
    pub(crate) const fn factor(self) -> f32 {
        match self {
            TypeStep::Display => 1.6,
            TypeStep::Title => 1.15,
            TypeStep::Body => 1.0,
            TypeStep::Secondary => 0.9,
            TypeStep::Caption => 0.8,
        }
    }

    /// The step's size at chrome base `base` px, as the proof-carrying
    /// [`StepPx`] the text funnel requires.
    pub(crate) fn px(self, base: f32) -> StepPx {
        StepPx(base * self.factor())
    }

    /// [`Self::px`] clamped into `[lo, hi]`, for text that must also fit a box; inverted
    /// bounds (degenerate geometry) pin to `hi.max(0)`, as `settings::fit` does.
    pub(crate) fn px_clamped(self, base: f32, lo: f32, hi: f32) -> StepPx {
        let v = base * self.factor();
        StepPx(if hi <= lo {
            hi.max(0.0)
        } else {
            v.clamp(lo, hi)
        })
    }
}

/// A text size that came off the named type scale: the field is private, so only
/// [`TypeStep`] mints one.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct StepPx(f32);

impl StepPx {
    /// The size in px (for width measurement and layout arithmetic).
    pub(crate) const fn get(self) -> f32 {
        self.0
    }

    /// Apply a platform/user native-text scale while preserving the proof token.
    /// Invalid scales collapse to 1 rather than leaking NaN geometry.
    pub(crate) fn scaled(self, scale: f32) -> Self {
        let scale = if scale.is_finite() && scale > 0.0 {
            scale
        } else {
            1.0
        };
        Self(self.0 * scale)
    }
}

#[cfg(test)]
#[path = "type_scale_tests.rs"]
mod tests;
