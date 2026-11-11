// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tone-of-typing tracker caching the [`Tone`](crate::tone::Tone) stamped onto trail sound events.

use aterm_time::Instant;
use std::time::Duration;

use crate::tone::{self, Tone, ToneScratch};

const BUF_CAP: usize = 160;

const INFER_KEYS: u32 = 6;

const INFER_INTERVAL: Duration = Duration::from_millis(500);

/// Per-window tone tracker: the bounded typed-line window (keyed to the session it was typed into),
/// the cached verdict, and the throttle clocks.
pub struct ToneTracker {
    buf: std::collections::VecDeque<char>,
    session: Option<u64>,
    tone: Tone,
    keys_since_infer: u32,
    last_infer: Option<Instant>,
    infer_pending: bool,
    scratch: ToneScratch,
    /// Total inferences run.
    pub inferences: u64,
}

impl Default for ToneTracker {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl ToneTracker {
    /// Construct a fresh tone tracker on the neutral [`Tone::Technical`] baseline.
    #[inline]
    #[must_use]
    pub const fn new() -> Self {
        Self {
            buf: std::collections::VecDeque::new(),
            session: None,
            tone: Tone::Technical,
            keys_since_infer: 0,
            last_infer: None,
            infer_pending: false,
            scratch: ToneScratch::new(),
            inferences: 0,
        }
    }

    /// The cached verdict before the `tone_melody` knob, for introspection.
    #[inline]
    #[must_use]
    pub const fn current(&self) -> Tone {
        self.tone
    }

    /// The tone the sound path stamps onto every trail [`crate::trail_sound::SoundEvent`].
    #[inline]
    #[must_use]
    pub const fn effective(&self, tone_melody: bool) -> Tone {
        if tone_melody {
            self.tone
        } else {
            Tone::Technical
        }
    }

    /// Chars currently in the window.
    #[inline]
    #[must_use]
    pub fn window_chars(&self) -> usize {
        self.buf.len()
    }

    #[inline]
    fn rekey(&mut self, session: u64) {
        if self.session != Some(session) {
            self.buf.clear();
            self.session = Some(session);
        }
    }

    /// Feed one committed printed keystroke and mark a re-inference due when the throttle opens.
    /// Window maintenance is unconditional; only marking is gated on `infer_active`.
    #[inline]
    pub fn note_char(&mut self, now: Instant, session: u64, ch: char, infer_active: bool) {
        self.rekey(session);
        if self.buf.len() == BUF_CAP {
            self.buf.pop_front();
        }
        self.buf.push_back(ch);
        self.keys_since_infer = self.keys_since_infer.saturating_add(1);
        if !infer_active || self.infer_pending {
            return;
        }
        let due = self.keys_since_infer >= INFER_KEYS
            || !matches!(
                self.last_infer,
                Some(at) if now.saturating_duration_since(at) < INFER_INTERVAL
            );
        if due {
            self.infer_pending = true;
        }
    }

    /// Run the inference a keystroke marked due, if any.
    #[inline]
    pub fn drain_pending_inference(&mut self, now: Instant) {
        if self.infer_pending {
            self.infer_pending = false;
            self.infer(now);
        }
    }

    /// A plain Backspace pops one char.
    #[inline]
    pub fn note_backspace(&mut self, session: u64) {
        self.rekey(session);
        self.buf.pop_back();
    }

    /// A word-breaking key (Enter/Tab/Escape/Delete/nav/kill chords, raw byte sequences) clears the
    /// window; the cached tone persists.
    #[inline]
    pub fn note_break(&mut self) {
        self.buf.clear();
    }

    /// Serious Mode's drain: clear the partial typing window and any owed re-inference while
    /// keeping the cached tone, throttle timestamp, and warmed-up scratch buffers intact.
    #[inline]
    pub fn drain_serious(&mut self) {
        self.buf.clear();
        self.infer_pending = false;
    }

    #[inline]
    fn infer(&mut self, now: Instant) {
        self.keys_since_infer = 0;
        self.last_infer = Some(now);
        self.inferences = self.inferences.saturating_add(1);
        if self.buf.len() < 3 {
            return;
        }
        let Some(model) = tone::builtin() else {
            self.tone = Tone::Technical;
            return;
        };
        let (s0, s1) = self.buf.as_slices();
        if let Some(tone) = model.classify_char_slices_opt([s0, s1], &mut self.scratch) {
            self.tone = tone;
        }
    }
}

#[cfg(test)]
#[path = "tone_infer_tests.rs"]
mod tests;
