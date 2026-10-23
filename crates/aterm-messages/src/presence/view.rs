// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Per-window presence view for border glow, needs pill, and story watermarks.

use std::collections::HashMap;

use super::{ChipLevel, PresenceTones, RIM_BORDER_ALPHA, Rim, YOURS_BORDER_SCALE_Q4, pill::Pill};
use crate::Instant;

/// One window's presence state from the latest refresh.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// Newest story sequence seen per session in this window.
    pub watermarks: HashMap<u64, u64>,
    /// The border shown.
    pub rim: Rim,
    /// Session the border is lit for, or `None` without a border.
    pub pressing: Option<u64>,
    /// Projected needs pill, if any.
    pub pill: Option<Pill>,
    /// Monotonic counter bumped on visible changes and folded into [`Self::fp`].
    pub seed: u64,
    /// Next deadline when the pill's displayed age advances.
    pub pill_due: Option<Instant>,
}

/// Inset border colour, alphas, and thickness scale ([`View::rim_glow`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RimGlow {
    /// The border's colour.
    pub accent: [u8; 3],
    /// Interior wash alpha (`0..=255`, always `0`).
    pub wash_a: u8,
    /// The inset border's alpha (`0..=255`).
    pub border_a: u8,
    /// Border thickness in 1/16 units of the host's size law.
    pub border_scale_q4: u8,
}

impl View {
    /// The story watermark this window holds for `session` (`0` if never read).
    #[must_use]
    pub fn watermark(&self, session: u64) -> u64 {
        self.watermarks.get(&session).copied().unwrap_or(0)
    }

    /// Update `rim`, `pressing`, and `pill`, returning `true` and bumping [`Self::seed`] if anything changed.
    pub fn show(&mut self, rim: Rim, pressing: Option<u64>, pill: Option<Pill>) -> bool {
        if self.rim == rim && self.pressing == pressing && self.pill == pill {
            return false;
        }
        if self.pill != pill {
            self.pill_due = None;
        }
        self.rim = rim;
        self.pressing = pressing;
        self.pill = pill;
        self.seed = self.seed.wrapping_add(1).max(1);
        true
    }

    /// Repaint-key fingerprint (`0` on a quiet window, nonzero [`Self::seed`] otherwise).
    #[must_use]
    pub fn fp(&self) -> u64 {
        if matches!(self.rim, Rim::None) && self.pill.is_none() {
            return 0;
        }
        self.seed.max(1)
    }

    /// Border glow parameters for the frame path, or `None` on a quiet window.
    #[must_use]
    pub fn rim_glow(&self, tones: impl FnOnce() -> PresenceTones) -> Option<RimGlow> {
        if matches!(self.rim, Rim::None) {
            return None;
        }
        let tones = tones();
        let (accent, scale) = match self.rim {
            Rim::None | Rim::Stuck => (tones.stop, 16),
            Rim::Decide => (tones.wait, 16),
            Rim::Yours => (tones.yours, YOURS_BORDER_SCALE_Q4),
        };
        Some(RimGlow {
            accent,
            wash_a: 0,
            border_a: RIM_BORDER_ALPHA,
            border_scale_q4: scale,
        })
    }
}

/// Map a tab's attention indicator bit to [`ChipLevel::Note`] or [`ChipLevel::Off`].
#[must_use]
pub const fn chip_of_attention(attention: bool) -> ChipLevel {
    if attention {
        ChipLevel::Note
    } else {
        ChipLevel::Off
    }
}
