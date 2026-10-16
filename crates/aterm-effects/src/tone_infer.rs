// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Tone-of-typing tracker: the seam between the host's committed key-press path
//! and the [`crate::tone`] classifier, caching the [`Tone`] the render tick
//! stamps onto every trail [`crate::trail_sound::SoundEvent`].
//!
//! Typed input only, never screen content (the [`crate::kitty_summon`]
//! discipline): the window is fed from the committed key-press path (bare
//! Character/Space, committed IME `Text`), a plain Backspace pops one char, and
//! any key that edits or moves further clears it, since the buffer can no longer
//! claim to be the text before the caret. PTY output and pastes never steer the
//! melody; like every input-path effect the tracker is Source-agnostic.
//!
//! A keystroke marks inference due after [`INFER_KEYS`] keystrokes, or once
//! [`INFER_INTERVAL`] has passed since the last run; it runs at the render tick,
//! never in the key handler. The cached verdict survives line breaks (a mood does
//! not end at Enter), and an abstention below `tone::MIN_NGRAMS` leaves it as is.
//!
//! The host runs the classifier only while tone inference is active
//! (`App::tone_infer_active`), so a headless or muted build never loads the
//! weights; window bookkeeping runs regardless, so a re-enable classifies a
//! clean window. Focus, reduced-motion and quiet policies need no check here:
//! the tone only rides events the sound path already admitted.

use aterm_time::Instant;
use std::time::Duration;

use crate::tone::{self, Tone, ToneScratch};

/// Window capacity in chars: the classifier mean-pools, so the tail of a long
/// line is plenty.
const BUF_CAP: usize = 160;

/// Re-infer after this many fed keystrokes…
const INFER_KEYS: u32 = 6;

/// …or once this long has passed since the last inference (slow typing gets
/// a fresh verdict per key; fast typing batches).
const INFER_INTERVAL: Duration = Duration::from_millis(500);

/// Per-window tone tracker: the bounded typed-line window (keyed to the
/// session it was typed into), the cached verdict, and the throttle clocks.
pub struct ToneTracker {
    /// Rolling window of recent printed keystrokes (≤ [`BUF_CAP`]).
    buf: std::collections::VecDeque<char>,
    /// The session the window's chars were typed into; a switch clears it.
    session: Option<u64>,
    /// Cached verdict; [`Tone::Technical`] until the classifier first speaks.
    tone: Tone,
    /// Keystrokes fed since the last inference.
    keys_since_infer: u32,
    /// Stamp of the last inference (None ⇒ never ran).
    last_infer: Option<Instant>,
    /// An inference marked due on the key path, run at the render tick by
    /// [`Self::drain_pending_inference`]: the classifier never runs inside the
    /// key handler, between the PTY write and the echo's wake dispatch.
    infer_pending: bool,
    /// Fixed classifier scratch; inference allocates nothing.
    scratch: ToneScratch,
    /// Reused `&str` bridge for the char window, taken and returned around each
    /// inference.
    text_buf_slot: String,
    /// Total inferences run. Ships so the `tone` control verb can tell "reads
    /// technical" from "never ran"; counted on the throttled path, never the key
    /// path.
    pub inferences: u64,
}

impl Default for ToneTracker {
    fn default() -> Self {
        Self {
            buf: std::collections::VecDeque::new(),
            session: None,
            tone: Tone::Technical,
            keys_since_infer: 0,
            last_infer: None,
            infer_pending: false,
            scratch: ToneScratch::default(),
            text_buf_slot: String::new(),
            inferences: 0,
        }
    }
}

impl ToneTracker {
    /// The cached verdict before the `tone_melody` knob, for introspection;
    /// [`Self::effective`] is what rides a sound event.
    pub fn current(&self) -> Tone {
        self.tone
    }

    /// The tone the sound path stamps onto every trail
    /// [`crate::trail_sound::SoundEvent`]: the cached verdict with the knob on,
    /// the neutral [`Tone::Technical`] with it off, even while a verdict from
    /// before the toggle is cached. Every sound seam takes its tone from here
    /// (pinned by `every_trail_sound_policy_takes_its_tone_from_the_tracker`).
    pub fn effective(&self, tone_melody: bool) -> Tone {
        if tone_melody {
            self.tone
        } else {
            Tone::Technical
        }
    }

    /// Chars currently in the window. The count only, never the text: the window
    /// holds keystrokes typed at a password prompt that the screen never echoed.
    pub fn window_chars(&self) -> usize {
        self.buf.len()
    }

    /// Bind the window to `session`, clearing it on a switch; the cached tone
    /// survives.
    fn rekey(&mut self, session: u64) {
        if self.session != Some(session) {
            self.buf.clear();
            self.session = Some(session);
        }
    }

    /// Feed one committed printed keystroke and mark a re-inference due when the
    /// throttle opens; [`Self::drain_pending_inference`] runs it at the next
    /// render tick. The key's own click reads the tracker before this feed.
    ///
    /// Window maintenance is unconditional; only marking is gated on
    /// `infer_active`. The keystroke count carries across an inactive spell, so
    /// the first press after a re-enable re-infers.
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

    /// Run the inference a keystroke marked due, if any. The render-tick sound
    /// drain calls this right before reading the tone, so a verdict trails its
    /// key by at most one frame.
    pub fn drain_pending_inference(&mut self, now: Instant) {
        if self.infer_pending {
            self.infer_pending = false;
            self.infer(now);
        }
    }

    /// A plain Backspace pops one char. Deletion alone never re-infers: only new
    /// evidence moves the mood.
    pub fn note_backspace(&mut self, session: u64) {
        self.rekey(session);
        self.buf.pop_back();
    }

    /// A word-breaking key (Enter/Tab/Escape/Delete/nav/kill chords, raw byte
    /// sequences) clears the window; the cached tone persists.
    pub fn note_break(&mut self) {
        self.buf.clear();
    }

    /// Run the classifier over the window and cache the verdict. A missing or
    /// corrupt weight asset degrades to Technical.
    fn infer(&mut self, now: Instant) {
        self.keys_since_infer = 0;
        self.last_infer = Some(now);
        self.inferences = self.inferences.saturating_add(1);
        let Some(model) = tone::builtin() else {
            self.tone = Tone::Technical;
            return;
        };
        let mut text = std::mem::take(&mut self.text_buf_slot);
        text.clear();
        text.extend(self.buf.iter());
        // An abstention (`None`, below `tone::MIN_NGRAMS`) keeps the prior
        // verdict; a real `Some(Tone::Technical)` still overwrites it.
        if let Some(tone) = model.classify_opt(&text, &mut self.scratch) {
            self.tone = tone;
        }
        self.text_buf_slot = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A live session draws a frame per key, so drain after each keystroke as
    /// the render tick does.
    fn feed(t: &mut ToneTracker, now: Instant, session: u64, s: &str) {
        for c in s.chars() {
            t.note_char(now, session, c, true);
            t.drain_pending_inference(now);
        }
    }

    /// The key path only marks inference due; the drain runs it.
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

    /// Feed keystrokes with inference inactive: the window fills, the classifier
    /// never runs.
    fn feed_inactive(t: &mut ToneTracker, now: Instant, session: u64, s: &str) {
        for c in s.chars() {
            t.note_char(now, session, c, false);
        }
    }

    #[test]
    fn starts_neutral() {
        let t = ToneTracker::default();
        assert_eq!(t.current(), Tone::Technical);
    }

    /// A same-instant burst re-infers once per [`INFER_KEYS`] keys (plus the
    /// first run), never per key.
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

    /// With [`INFER_INTERVAL`] elapsed, one keystroke re-infers.
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

    /// A canonical frustrated line (a pin on the shipped weights) flips the
    /// verdict, and a technical command flips it back.
    #[test]
    fn typed_mood_moves_the_cached_tone() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        feed(&mut t, now, 1, "why is this broken again ugh");
        assert_eq!(t.current(), Tone::Frustrated);
        t.note_break(); // Enter — window clears, mood persists…
        assert_eq!(t.current(), Tone::Frustrated);
        // …until the next line carries new evidence.
        feed(&mut t, now, 1, "git rebase -i HEAD~3 && cargo test");
        assert_eq!(t.current(), Tone::Technical);
    }

    /// A session switch clears the window but keeps the cached tone.
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

    /// Backspace never re-infers; the window stays bounded.
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

    /// An abstention over a one-char window after Enter keeps the classified
    /// mood instead of snapping to Technical.
    #[test]
    fn thin_window_after_a_classified_line_keeps_the_mood() {
        let mut t = ToneTracker::default();
        let start = Instant::now();
        feed(&mut t, start, 1, "why is this broken again ugh");
        assert_eq!(t.current(), Tone::Frustrated);
        t.note_break(); // Enter — window clears, mood persists…
        // …a pause, then one char: inference is due but abstains (below
        // MIN_NGRAMS).
        t.note_char(start + INFER_INTERVAL, 1, 'x', true);
        t.drain_pending_inference(start + INFER_INTERVAL);
        assert_eq!(t.buf, vec!['x'], "the window is the single new char");
        assert_eq!(
            t.current(),
            Tone::Frustrated,
            "abstention must leave the cached mood untouched"
        );
    }

    /// With the knob off `effective` is neutral, while `current` still shows the
    /// cached verdict.
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

    /// `window_chars` and `inferences` track the feed; the count moves only when
    /// the classifier ran.
    #[test]
    fn introspection_counters_track_the_window_and_the_classifier() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        assert_eq!((t.window_chars(), t.inferences), (0, 0));
        feed_inactive(&mut t, now, 1, "hello there");
        assert_eq!(t.window_chars(), 11, "the window fills while inactive…");
        assert_eq!(t.inferences, 0, "…but the classifier never ran");
        feed(&mut t, now, 1, "!");
        assert!(t.inferences >= 1, "an active feed runs the model");
        t.note_break();
        assert_eq!(t.window_chars(), 0, "a break empties the window");
    }

    /// A break recorded while inference is inactive still clears the window, so
    /// a re-enable classifies only post-break text.
    #[test]
    fn breaks_while_inactive_keep_the_window_coherent() {
        let mut t = ToneTracker::default();
        let now = Instant::now();
        feed(&mut t, now, 1, "why is this broken again ugh");
        assert_eq!(t.current(), Tone::Frustrated);
        // Inference goes inactive: chars still land, the Enter still clears.
        feed_inactive(&mut t, now, 1, "still poking at it");
        assert!(!t.buf.is_empty(), "the window tracks edits while inactive");
        t.note_break(); // Enter recorded while inactive — must clear.
        assert!(
            t.buf.is_empty(),
            "a break must clear the window even when inference is inactive"
        );
        // Re-enable: a clean line classifies without splicing stale text.
        feed(&mut t, now, 1, "git rebase -i HEAD~3 && cargo test");
        assert_eq!(
            t.current(),
            Tone::Technical,
            "the re-enabled window carries only post-break text"
        );
    }
}
