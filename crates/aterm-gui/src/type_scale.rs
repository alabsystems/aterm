// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The chrome TYPE SCALE: one named 5-step scale for every text size the
//! own-rendered chrome (Settings / About / command palette / tray widgets)
//! draws, replacing the ~18 ad-hoc per-site multipliers (`px * 0.92`,
//! `px * 0.78`, `px * 1.1`, …) that had accreted across the painters.
//!
//! TOTALITY BY CONSTRUCTION: a [`DrawPrim::Text`](crate::widget::DrawPrim) is
//! only constructible through [`crate::widget::text_prim`], whose size
//! parameter is [`StepPx`] — and `StepPx`'s field is private to this module,
//! so the ONLY way to mint one is [`TypeStep::px`] / [`TypeStep::px_clamped`].
//! Every chrome text site therefore maps to a named step; an orphan multiplier
//! cannot compile. The funnel itself (no `DrawPrim::Text` construction outside
//! the one helper) is enforced by the `every_text_prim_goes_through_the_funnel`
//! test in `crate::widget`.

/// A named step of the chrome type scale, largest to smallest. The factor is
/// applied to the chrome's base size (the terminal `font_px` for the
/// Settings/Palette overlay cards; the NATIVE `BASE_PT × display scale` for the
/// About dialog — `crate::about`; [`crate::widget`]'s fixed tray base for the
/// compact tray).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum TypeStep {
    /// The wordmark ("aterm" in About). 1.6×.
    Display,
    /// Overlay titles ("Settings", "Commands") and the tray's headline values. 1.15×.
    Title,
    /// Row labels and primary content. 1.0×.
    Body,
    /// Values, buttons, menu options, field text — secondary emphasis. 0.9×.
    Secondary,
    /// Hints, footers, section headers, chips, counts, captions. 0.8×.
    Caption,
}

impl TypeStep {
    /// Every step, largest first (the scale's public contract; tested below).
    #[cfg(test)]
    pub(crate) const ALL: [TypeStep; 5] = [
        TypeStep::Display,
        TypeStep::Title,
        TypeStep::Body,
        TypeStep::Secondary,
        TypeStep::Caption,
    ];

    /// The step's multiplier on the chrome base size — the ONE place a text
    /// size factor may be written.
    pub(crate) fn factor(self) -> f32 {
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

    /// [`Self::px`] clamped into `[lo, hi]` — for the one site whose text must
    /// also FIT a box (the settings preview's terminal sample). Inverted bounds
    /// (degenerate geometry) pin to the achievable upper bound, floored at
    /// zero, mirroring `settings::fit`.
    pub(crate) fn px_clamped(self, base: f32, lo: f32, hi: f32) -> StepPx {
        let v = base * self.factor();
        StepPx(if hi <= lo {
            hi.max(0.0)
        } else {
            v.clamp(lo, hi)
        })
    }
}

/// A text size that provably came off the named type scale (the field is
/// private; only [`TypeStep`] mints values). Carries plain f32 px inside —
/// call [`Self::get`] for measurement/layout math.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct StepPx(f32);

impl StepPx {
    /// The size in px (for width measurement and layout arithmetic).
    pub(crate) fn get(self) -> f32 {
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
mod tests {
    use super::*;

    /// The scale is exactly five named steps, strictly descending, anchored at
    /// Body = 1.0 — the "one named 5-step type scale" contract.
    #[test]
    fn five_named_steps_strictly_descending() {
        assert_eq!(TypeStep::ALL.len(), 5);
        for w in TypeStep::ALL.windows(2) {
            assert!(
                w[0].factor() > w[1].factor(),
                "{:?} ({}) must be larger than {:?} ({})",
                w[0],
                w[0].factor(),
                w[1],
                w[1].factor()
            );
        }
        assert_eq!(
            TypeStep::Body.factor(),
            1.0,
            "Body anchors the scale at 1em"
        );
    }

    #[test]
    fn px_scales_linearly_and_clamps() {
        assert_eq!(TypeStep::Caption.px(10.0).get(), 8.0);
        assert_eq!(TypeStep::Display.px(10.0).get(), 16.0);
        // Clamp binds on both sides; inverted bounds pin to hi.max(0).
        assert_eq!(TypeStep::Caption.px_clamped(10.0, 9.0, 12.0).get(), 9.0);
        assert_eq!(TypeStep::Display.px_clamped(10.0, 2.0, 12.0).get(), 12.0);
        assert_eq!(TypeStep::Body.px_clamped(10.0, 8.0, 4.0).get(), 4.0);
        assert_eq!(TypeStep::Body.px_clamped(10.0, 8.0, -3.0).get(), 0.0);
        assert_eq!(TypeStep::Body.px(10.0).scaled(1.5).get(), 15.0);
        assert_eq!(TypeStep::Body.px(10.0).scaled(f32::NAN).get(), 10.0);
    }
}
