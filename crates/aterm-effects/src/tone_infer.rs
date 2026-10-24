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
        Self {
            buf: std::collections::VecDeque::new(),
            session: None,
            tone: Tone::Technical,
            keys_since_infer: 0,
            last_infer: None,
            infer_pending: false,
            scratch: ToneScratch::default(),
            inferences: 0,
        }
    }
}

impl ToneTracker {
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
        if !infer_active {
            return;
        }
        let due = self.keys_since_infer >= INFER_KEYS
            || self
                .last_infer
                .is_none_or(|at| now.saturating_duration_since(at) >= INFER_INTERVAL);
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
        let Some(model) = tone::builtin() else {
            self.tone = Tone::Technical;
            return;
        };
        if self.buf.len() < 3 {
            return;
        }
        if let Some(tone) = model.classify_chars_opt(self.buf.iter().copied(), &mut self.scratch) {
            self.tone = tone;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut ToneTracker, now: Instant, session: u64, s: &str) {
        for c in s.chars() {
            t.note_char(now, session, c, true);
            t.drain_pending_inference(now);
        }
    }

    #[test]
    fn the_key_path_marks_inference_due_and_only_the_drain_runs_it() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        for c in "why is this broken again ugh".chars() {
            t.note_char(now, 1, c, true);
        }
        assert_eq!(t.inferences, 0, "no frame ⇒ the classifier has not run");
        assert_eq!(t.current(), Tone::Technical);
        assert!(t.infer_pending, "…but the keystrokes marked one due");
        t.drain_pending_inference(now);
        assert_eq!(t.inferences, 1, "one drain runs exactly one inference");
        assert_eq!(t.current(), Tone::Frustrated);
        t.drain_pending_inference(now);
        assert_eq!(t.inferences, 1, "a drain with nothing pending is free");
    }

    fn feed_inactive(t: &mut ToneTracker, now: Instant, session: u64, s: &str) {
        for c in s.chars() {
            t.note_char(now, session, c, false);
        }
    }

    #[test]
    fn inference_is_throttled_by_keystrokes() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        feed(&mut t, now, 1, "why is this broken again ugh");
        let n = t.inferences;
        assert!(n >= 1, "at least one inference must have run");
        assert!(
            n <= 28 / INFER_KEYS as u64 + 1,
            "throttle failed: {n} inferences for 28 same-instant keys"
        );
    }

    #[test]
    fn interval_reopens_inference() {
        let mut t = ToneTracker::default();
        let start = Instant::now();
        feed(&mut t, start, 1, "abc");
        let before = t.inferences;
        t.note_char(start + INFER_INTERVAL, 1, 'd', true);
        t.drain_pending_inference(start + INFER_INTERVAL);
        assert_eq!(
            t.inferences,
            before + 1,
            "an elapsed interval must re-infer"
        );
    }

    #[test]
    fn session_switch_clears_window_but_keeps_mood() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        feed(&mut t, now, 1, "ㅋㅋㅋㅋ 너무 웃겨 ㅎㅎ");
        assert_eq!(t.current(), Tone::Playful);
        t.note_char(now, 2, 'a', true); // new session: window restarts
        assert_eq!(t.buf, vec!['a'], "the old session's chars must be gone");
        assert_eq!(t.current(), Tone::Playful, "the mood itself persists");
    }

    #[test]
    fn backspace_pops_and_window_stays_bounded() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        feed(&mut t, now, 1, "abcd");
        let before = t.inferences;
        t.note_backspace(1);
        assert_eq!(t.buf.len(), 3);
        assert_eq!(t.inferences, before, "deletion alone must not re-infer");
        for _ in 0..(BUF_CAP * 2) {
            t.note_char(now, 1, 'x', true);
        }
        assert!(t.buf.len() <= BUF_CAP);
    }

    #[test]
    fn thin_window_after_a_classified_line_keeps_the_mood() {
        let mut t = ToneTracker::default();
        let start = Instant::now();
        feed(&mut t, start, 1, "why is this broken again ugh");
        assert_eq!(t.current(), Tone::Frustrated);
        t.note_break(); // Enter — window clears, mood persists…
        t.note_char(start + INFER_INTERVAL, 1, 'x', true);
        t.drain_pending_inference(start + INFER_INTERVAL);
        assert_eq!(t.buf, vec!['x'], "the window is the single new char");
        assert_eq!(
            t.current(),
            Tone::Frustrated,
            "abstention must leave the cached mood untouched"
        );
    }

    #[test]
    fn effective_is_neutral_with_the_knob_off_even_with_a_cached_verdict() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        feed(&mut t, now, 1, "why is this broken again ugh");
        assert_eq!(t.current(), Tone::Frustrated);
        assert_eq!(t.effective(true), Tone::Frustrated);
        assert_eq!(t.effective(false), Tone::Technical);
        assert_eq!(t.current(), Tone::Frustrated, "the cache is not cleared");
    }

    #[test]
    fn breaks_while_inactive_keep_the_window_coherent() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        feed(&mut t, now, 1, "why is this broken again ugh");
        assert_eq!(t.current(), Tone::Frustrated);
        feed_inactive(&mut t, now, 1, "still poking at it");
        assert!(!t.buf.is_empty(), "the window tracks edits while inactive");
        t.note_break(); // Enter recorded while inactive — must clear.
        assert!(
            t.buf.is_empty(),
            "a break must clear the window even when inference is inactive"
        );
        feed(&mut t, now, 1, "git rebase -i HEAD~3 && cargo test");
        assert_eq!(
            t.current(),
            Tone::Technical,
            "the re-enabled window carries only post-break text"
        );
    }

    #[test]
    fn drain_serious_clears_the_window_and_pending_inference_while_keeping_cached_tone() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        feed(&mut t, now, 1, "why is this broken again ugh");
        assert_eq!(t.current(), Tone::Frustrated);
        let inferences_before = t.inferences;
        t.note_char(now + Duration::from_secs(1), 1, '!', true);
        assert!(t.infer_pending && t.window_chars() > 0);
        t.drain_serious();
        assert_eq!(t.window_chars(), 0);
        assert!(!t.infer_pending);
        t.drain_pending_inference(now + Duration::from_secs(1));
        assert_eq!(t.inferences, inferences_before);
        assert_eq!(t.current(), Tone::Frustrated);
    }
}
