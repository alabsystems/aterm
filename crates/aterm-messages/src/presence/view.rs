// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The per-window VIEW: what the frame path reads, allocation-free — the
//! level, the rim, the row's words, the change seed the repaint key folds
//! ([`View::fp`]), the edge ripple and the words' own clock.

use std::collections::HashMap;

use super::{
    ChipLevel, HOLD_WASH_ALPHA, Level, PresenceTones, RIM_BORDER_ALPHA, RIPPLE, RIPPLE_STEPS,
    RIPPLE_WASH_PEAK, Rim, Words,
};
use crate::{Duration, Instant};

/// One window's presence, as the last refresh left it. Every field the frame
/// path reads is plain data; the words are recomposed only on change.
#[derive(Clone, Debug)]
pub struct View {
    /// The story watermarks, one per SESSION shown in this window: the newest
    /// story seq the human has seen of that session here (moved by the fold
    /// law, never by focus alone or a timer). Per session, not per window:
    /// reading one tab's story must not fold another's, and a story read once
    /// must not return after the human reads a second tab.
    pub watermarks: HashMap<u64, u64>,
    /// The row count COMMITTED to this window's geometry (0 or 1) — moved
    /// only by the host's re-grid.
    pub rows: u16,
    /// The level shown.
    pub level: Level,
    /// The rim shown.
    pub rim: Rim,
    /// The row's words, when the row exists.
    pub words: Option<Words>,
    /// Bumped on every change the painter can see; folded into [`Self::fp`].
    /// Never 0 once anything has shown, but `fp` maps a quiet window to 0.
    pub seed: u64,
    /// A turn submit's edge ripple: when it started, `None` when none runs
    /// (and always `None` under reduced motion — amplitude 0 means the ripple
    /// never STARTS, so the frame is the steady image).
    pub ripple_at: Option<Instant>,
    /// The running ripple is a CHOICE PULSE: the harness answered a question
    /// box in the session this window's FRONT tab shows. It paints in the
    /// story tone and paints on a window with NO rim — the one ripple a quiet
    /// window shows. Reset with `ripple_at`.
    pub ripple_chose: bool,
    /// When the words' `since` figures next move — the row's own text clock
    /// ([`words_step`]: 1 s while they print seconds, 60 s after); `None`
    /// until the first tick of a row. The host recomposes a window only when
    /// this is due, so another owner's wakes never re-read presence facts at
    /// their own rate (audit 2026-09-24).
    pub words_due: Option<Instant>,
    /// Since when the committed row has wanted NO row (its words went away)
    /// while [`FOLD_QUIET`](super::FOLD_QUIET) holds its fold back — the row
    /// stays committed and paints blank meanwhile, and the wire reports it
    /// with no words (`band=""`, `sentence=""`), which is what the human sees.
    /// `None` when no fold is pending; a want that returns clears it.
    pub fold_since: Option<Instant>,
    /// The pending fold is a person's READ (the fold law's keystroke, or a
    /// switch to a tab whose session wants no row): it does not wait out
    /// [`FOLD_QUIET`](super::FOLD_QUIET) from the want's drop, only the row's
    /// own minimum life from its birth ([`Self::born_at`]). Sticky until the
    /// fold commits or the want returns, so a read under a hold folds as the
    /// hold lifts rather than waiting a fresh quiet.
    pub fold_read: bool,
    /// When the committed row was BORN (its last 0 -> 1 re-grid); `None`
    /// before it ever was. A read folds a row only once it has stood
    /// [`FOLD_QUIET`](super::FOLD_QUIET): a story told while a person types
    /// is born, and their next key would otherwise fold it milliseconds later
    /// — the incident's net-zero pair, with the key's echo between.
    pub born_at: Option<Instant>,
    /// The session this window's front tab showed at the last projection: a
    /// different one now is a tab switch, a person's act ([`Self::fold_read`]).
    pub front: Option<u64>,
}

impl Default for View {
    fn default() -> Self {
        Self {
            watermarks: HashMap::new(),
            rows: 0,
            level: Level::Quiet,
            rim: Rim::None,
            words: None,
            seed: 0,
            ripple_at: None,
            ripple_chose: false,
            words_due: None,
            fold_since: None,
            fold_read: false,
            born_at: None,
            front: None,
        }
    }
}

/// The rim as the frame path draws it: the accent and the three alphas an
/// inset border and its wash take ([`View::rim_glow`]).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RimGlow {
    /// The border's and the wash's colour.
    pub accent: [u8; 3],
    /// The interior wash's alpha (0..255).
    pub wash_a: u8,
    /// The inset border's alpha (0..255).
    pub border_a: u8,
    /// The border's thickness in 1/16 units of the host's size law (16 = the
    /// plain law, 32 = doubled under a hold).
    pub border_scale_q4: u8,
}

impl View {
    /// The story watermark this window holds for `session` (0: never read).
    #[must_use]
    pub fn watermark(&self, session: u64) -> u64 {
        self.watermarks.get(&session).copied().unwrap_or(0)
    }

    /// Show `level`, `rim` and `words` (the row's words, `None` without a
    /// row): `true` when anything the painter reads moved, which bumps the
    /// seed. New words re-arm their own clock ([`words_step`]): a minutes
    /// row's +60 s must not hold back a seconds row that replaced it. The
    /// earlier of the two wins, so a figure already due is never pushed back.
    pub fn show(&mut self, level: Level, rim: Rim, words: Option<Words>, now: Instant) -> bool {
        if self.level == level && self.rim == rim && self.words == words {
            return false;
        }
        if self.words != words {
            let next = words.as_ref().map(|w| now + words_step(w));
            self.words_due = match (self.words_due, next) {
                (Some(old), Some(new)) => Some(old.min(new)),
                (_, next) => next,
            };
        }
        self.level = level;
        self.rim = rim;
        self.words = words;
        self.seed = self.seed.wrapping_add(1).max(1);
        true
    }

    /// The ripple's step at `now`: `Some(0..RIPPLE_STEPS)` while it runs,
    /// `None` after.
    #[must_use]
    pub fn ripple_step(&self, now: Instant) -> Option<u32> {
        let t0 = self.ripple_at?;
        let elapsed = now.saturating_duration_since(t0);
        if elapsed >= RIPPLE {
            return None;
        }
        let step = elapsed.as_millis() * u128::from(RIPPLE_STEPS) / RIPPLE.as_millis();
        // `elapsed < RIPPLE`, so `step < RIPPLE_STEPS`: the conversion is exact.
        Some(u32::try_from(step).map_or(RIPPLE_STEPS - 1, |s| s.min(RIPPLE_STEPS - 1)))
    }

    /// The repaint-key term: **exactly 0 on a quiet window** (no rim, no row,
    /// no ripple), else a nonzero fold of the seed (floored at 1, so the fold
    /// is never 0) and the ripple step (0 = none, else `step + 1`). No clock is
    /// read unless a ripple is live, and no allocation ever.
    #[must_use]
    pub fn fp(&self, now: Instant) -> u64 {
        if self.rows == 0 && matches!(self.rim, Rim::None) && self.ripple_at.is_none() {
            return 0;
        }
        let step = self
            .ripple_at
            .and_then(|_| self.ripple_step(now))
            .map_or(0, |s| u64::from(s) + 1);
        (self.seed.max(1) << 8) | step
    }

    /// The next instant this view's PIXELS change on their own — only while a
    /// ripple runs. Steady rims and rows have no deadline (the change-driven
    /// law); the row's `since` text ticks through the host's deadline fold.
    #[must_use]
    pub fn ripple_deadline(&self, now: Instant) -> Option<Instant> {
        let t0 = self.ripple_at?;
        let end = t0 + RIPPLE;
        if now >= end {
            return Some(now);
        }
        let step = self.ripple_step(now).unwrap_or(0);
        Some((t0 + RIPPLE * (step + 1) / RIPPLE_STEPS).min(end))
    }

    /// The rim at `now` as the frame path draws it — plain field reads and
    /// arithmetic, no allocation; `None` on a quiet window. `rim_on` is the
    /// host's rim toggle (a CHOICE PULSE is a rim flash, so it too needs it);
    /// `tones` are read only when a rim or a pulse paints, so a quiet frame
    /// answers before the host derives them.
    #[must_use]
    pub fn rim_glow(
        &self,
        now: Instant,
        rim_on: bool,
        tones: impl FnOnce() -> PresenceTones,
    ) -> Option<RimGlow> {
        let step = self.ripple_step(now);
        let pulse = self.ripple_chose && step.is_some() && rim_on;
        if matches!(self.rim, Rim::None) && !pulse {
            return None;
        }
        let tones = tones();
        let (accent, mut wash_a, scale) = match self.rim {
            Rim::None => (tones.story, 0u8, 16u8),
            Rim::Drive => (tones.drive, 0u8, 16u8),
            Rim::Wait => (tones.wait, 0, 16),
            Rim::Stop { hold: false } => (tones.stop, 0, 16),
            Rim::Stop { hold: true } => (tones.stop, HOLD_WASH_ALPHA, 32),
        };
        // The pulse speaks in the story tone whatever rim it crosses: the
        // rim's own colour returns with the steady frame after it.
        let accent = if pulse { tones.story } else { accent };
        let mut border_a = RIM_BORDER_ALPHA;
        if let Some(step) = step {
            // One edge flash, decaying over nine frames: the border to full and
            // a wash that fades out — the pixels change, the fact does not.
            border_a = 255;
            let remaining = RIPPLE_STEPS.saturating_sub(step);
            // At most `RIPPLE_WASH_PEAK` (24): the conversion is exact.
            let ripple =
                u8::try_from(RIPPLE_WASH_PEAK * remaining / RIPPLE_STEPS).unwrap_or(u8::MAX);
            wash_a = wash_a.max(ripple);
        }
        Some(RimGlow {
            accent,
            wash_a,
            border_a,
            border_scale_q4: scale,
        })
    }
}

/// How often a row's words move: every second while any `since` clause
/// prints SECONDS (`12s`, `3m12s`, `since 3m12s`, `held 1m00s, resumed 40s
/// ago`), once a minute when it prints only minutes, hours or days.
#[must_use]
pub fn words_step(words: &Words) -> Duration {
    if words.since.iter().any(|c| prints_seconds(c)) {
        Duration::from_secs(1)
    } else {
        Duration::from_secs(60)
    }
}

/// Whether a `since` clause prints a seconds figure (`12s`, `3m12s`, `since
/// 40s`, `held 1m00s, resumed 5s ago`) — a token ending in `s` right after a
/// digit; `3 turns`, `2h05m` and `1d 22h` do not.
#[must_use]
pub(crate) fn prints_seconds(clause: &str) -> bool {
    clause
        .split(|c: char| !c.is_ascii_alphanumeric())
        .any(|tok| {
            tok.strip_suffix('s')
                .is_some_and(|head| head.chars().last().is_some_and(|c| c.is_ascii_digit()))
        })
}

/// The chip level the host's existing indicator bits spell: `Wait`.
#[must_use]
pub const fn chip_of_attention(attention: bool) -> ChipLevel {
    if attention {
        ChipLevel::Wait
    } else {
        ChipLevel::Off
    }
}
