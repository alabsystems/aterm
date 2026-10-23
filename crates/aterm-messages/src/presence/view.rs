// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-window view the frame path reads without allocating: the border
//! ([`Rim`]), the session it is lit for, the needs pill's model ([`Pill`]),
//! the change seed the repaint key folds ([`View::fp`]), the pill's own age
//! clock, and the story watermarks `status level=` reads.

use std::collections::HashMap;

use super::{ChipLevel, PresenceTones, RIM_BORDER_ALPHA, Rim, YOURS_BORDER_SCALE_Q4, pill::Pill};
use crate::Instant;

/// One window's presence, as the last refresh left it. Every field the frame
/// path reads is plain data; the pill is recomposed only on change.
#[derive(Clone, Debug, Default)]
pub struct View {
    /// The story watermarks, one per session shown in this window: the newest
    /// story seq the human has seen of that session here, moved only by a
    /// person's key in a calm session ([`super::drive::human_acted_on`]).
    /// `status level=story` reads the highest any window holds.
    pub watermarks: HashMap<u64, u64>,
    /// The border shown.
    pub rim: Rim,
    /// The session the border is lit for: the window's most pressing need's,
    /// or the front session the person holds. `None` with no border.
    pub pressing: Option<u64>,
    /// The projected pill. Reported needs can light the border without a pill.
    pub pill: Option<Pill>,
    /// Bumped on every change the painter can see; folded into [`Self::fp`].
    pub seed: u64,
    /// When the pill's age next moves (1 s under a minute, 60 s after);
    /// `None` without a pill or before its first tick. Armed only for a
    /// window someone can see the pill on.
    pub pill_due: Option<Instant>,
}

/// The border as the frame path draws it: the accent and the alphas an
/// inset border takes ([`View::rim_glow`]). Steady: no wash, no ripple.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RimGlow {
    /// The border's colour.
    pub accent: [u8; 3],
    /// The interior wash's alpha (0..255): always 0, nothing washes the
    /// glass.
    pub wash_a: u8,
    /// The inset border's alpha (0..255).
    pub border_a: u8,
    /// The border's thickness in 1/16 units of the host's size law (16 = the
    /// plain law); the yours outline's [`YOURS_BORDER_SCALE_Q4`] is one pixel
    /// at every size the law gives (the host floors it at one).
    pub border_scale_q4: u8,
}

impl View {
    /// The story watermark this window holds for `session` (0: never read).
    #[must_use]
    pub fn watermark(&self, session: u64) -> u64 {
        self.watermarks.get(&session).copied().unwrap_or(0)
    }

    /// Show `rim` lit for `pressing` with `pill`: `true` when anything the
    /// painter reads moved, which bumps the seed. A pill that moved restarts
    /// its age clock at `now`'s step; one that went clears it.
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

    /// The repaint-key term: exactly 0 on a quiet window (no border, no
    /// pill), else a nonzero fold of the seed. No clock is read and nothing
    /// allocates.
    #[must_use]
    pub fn fp(&self) -> u64 {
        if matches!(self.rim, Rim::None) && self.pill.is_none() {
            return 0;
        }
        self.seed.max(1)
    }

    /// The border as the frame path draws it, with no allocation; `None` on
    /// a quiet window. `tones` are read only when a border paints, so a quiet
    /// frame answers before the host derives them. The stuck and decide
    /// borders take the host's inset width at the drop target's alpha; the
    /// yours outline is one pixel of the theme's dim ink. No wash, ever.
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

/// The chip level the tab's own indicator bit spells (a native leaf's
/// out-of-band attention, a failed last outcome not yet seen): a
/// [`ChipLevel::Note`] when set, else `Off`. Never a need mark: `◆` and `◇`
/// come only from the published verdict ([`super::Slot::chip`]), which the
/// host folds over this.
#[must_use]
pub const fn chip_of_attention(attention: bool) -> ChipLevel {
    if attention {
        ChipLevel::Note
    } else {
        ChipLevel::Off
    }
}
