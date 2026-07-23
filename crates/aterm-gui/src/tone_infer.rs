// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TONE-OF-TYPING tracker: the host seam between the committed key-press
//! path and the [`aterm_effects::tone`] classifier, producing the cached
//! [`Tone`] the render tick stamps onto every trail [`SoundEvent`] (the
//! synth's tone-melody tables).
//!
//! PROVENANCE — typed input only, never screen content. This is the
//! [`crate::kitty_summon`] discipline verbatim: the window is fed
//! exclusively from the committed key-press path (bare Character/Space,
//! committed IME `Text`), a plain Backspace pops one char, and every key
//! that edits or moves beyond one glyph CLEARS the window — after such a
//! boundary the buffer can no longer claim to be "the text before the
//! caret", so it forgets rather than guesses. PTY output, `cat`ing a mood
//! diary, program output, and pastes can never steer the melody; and like
//! every effect on the input path the tracker is Source-agnostic (a
//! controller types moods exactly like a human).
//!
//! THROTTLE — the classifier is cheap (<100 µs/line) but the typing path is
//! sacred, so inference runs at most once per [`INFER_KEYS`] keystrokes or
//! once the [`INFER_INTERVAL`] has passed since the last run, whichever
//! opens first; between runs the cached verdict is served for free. The
//! cache deliberately SURVIVES line breaks: a mood does not end at Enter,
//! and the next line re-infers as soon as it carries enough evidence. Below
//! `tone::MIN_NGRAMS` the classifier ABSTAINS ([`tone::ToneModel::classify_opt`]
//! returns `None`) and the cached mood is left untouched — an evidence-thin
//! window (a one-char line right after Enter) never snaps a classified mood
//! back to neutral.
//!
//! POLICY — the App gates the expensive classifier on tone inference being
//! ACTIVE ([`crate::App::tone_infer_active`]): the `tone_melody` knob is on,
//! trail sounds are configured audible, and the trail-audio worker is live.
//! A headless/muted build never runs the model and never loads its weights.
//! WINDOW MAINTENANCE, by contrast, is unconditional: the O(1) note_char /
//! note_break / note_backspace bookkeeping runs whether or not inference is
//! active, so the window always mirrors the text before the caret and a
//! later re-enable classifies a clean window instead of one spliced across
//! the editing that happened while it was off. Focus/reduced-motion/quiet
//! policies need no
//! re-check here: the tone only ever RIDES events the sound path already
//! admitted — silence stays silence, whatever the mood.

use std::time::{Duration, Instant};

use aterm_effects::tone::{self, Tone, ToneScratch};

/// Rolling window capacity in chars — enough of the current line to carry a
/// mood (the classifier mean-pools, so the tail of a long line is plenty),
/// small enough that the worst-case `remove(0)` shift is noise.
const BUF_CAP: usize = 160;

/// Re-infer after this many fed keystrokes…
const INFER_KEYS: u32 = 6;

/// …or once this long has passed since the last inference (slow typing gets
/// a fresh verdict per key; fast typing batches).
const INFER_INTERVAL: Duration = Duration::from_millis(500);

/// Per-window tone tracker: the bounded typed-line window (keyed to the
/// session it was typed into), the cached verdict, and the throttle clocks.
pub(crate) struct ToneTracker {
    /// Rolling window of recent printed keystrokes (≤ [`BUF_CAP`]).
    buf: Vec<char>,
    /// The session the window's chars were typed into; a switch clears it
    /// (two sessions' halves never assemble one mood).
    session: Option<u64>,
    /// Cached verdict — [`Tone::Technical`] (the melodic identity) until the
    /// classifier first speaks.
    tone: Tone,
    /// Keystrokes fed since the last inference.
    keys_since_infer: u32,
    /// Stamp of the last inference (None ⇒ never ran).
    last_infer: Option<Instant>,
    /// Fixed classifier scratch — allocated once, inference allocates zero.
    scratch: ToneScratch,
    /// Reused `&str` bridge for the char window (the classifier takes text).
    /// Amortized zero-allocation after warmup: a 160-char window is ≤ 640
    /// bytes, taken and returned around each inference.
    text_buf_slot: String,
    /// Total inferences run (test observability for the throttle proofs).
    #[cfg(test)]
    pub(crate) inferences: u64,
}

impl Default for ToneTracker {
    fn default() -> Self {
        Self {
            buf: Vec::new(),
            session: None,
            tone: Tone::Technical,
            keys_since_infer: 0,
            last_infer: None,
            scratch: ToneScratch::default(),
            text_buf_slot: String::new(),
            #[cfg(test)]
            inferences: 0,
        }
    }
}

impl ToneTracker {
    /// The cached verdict — what the render tick stamps onto trail cues.
    pub(crate) fn current(&self) -> Tone {
        self.tone
    }

    /// Bind the window to `session`, clearing it on a switch (the cached
    /// tone survives — switching tabs does not end a mood, it just needs
    /// fresh evidence to change it).
    fn rekey(&mut self, session: u64) {
        if self.session != Some(session) {
            self.buf.clear();
            self.session = Some(session);
        }
    }

    /// Feed one committed printed keystroke and maybe re-infer.
    ///
    /// WINDOW MAINTENANCE IS UNCONDITIONAL — the rekey and the bounded push
    /// run whether or not inference is active, so the window always mirrors
    /// the text before the caret. Only the expensive [`Self::infer`] (the
    /// classifier call) is gated on `infer_active`: an inactive build keeps
    /// the window coherent for free but never loads the weights or runs the
    /// model, so a later re-enable classifies a clean window rather than one
    /// spliced across the editing that happened while it was off. The unspent
    /// keystroke count carries across, so the first press after a re-enable is
    /// already "due" and re-infers promptly.
    pub(crate) fn note_char(&mut self, now: Instant, session: u64, ch: char, infer_active: bool) {
        self.rekey(session);
        if self.buf.len() == BUF_CAP {
            self.buf.remove(0);
        }
        self.buf.push(ch);
        self.keys_since_infer = self.keys_since_infer.saturating_add(1);
        if !infer_active {
            return;
        }
        let due = self.keys_since_infer >= INFER_KEYS
            || self
                .last_infer
                .is_none_or(|at| now.saturating_duration_since(at) >= INFER_INTERVAL);
        if due {
            self.infer(now);
        }
    }

    /// A plain Backspace pops one char (typo tolerance, like the summon
    /// detector). Deletion alone never re-infers — only new evidence can
    /// move the mood, mirroring "backspacing alone never summons".
    pub(crate) fn note_backspace(&mut self, session: u64) {
        self.rekey(session);
        self.buf.pop();
    }

    /// A word-breaking key (Enter/Tab/Escape/Delete/nav/kill chords, raw
    /// byte sequences): the window can no longer claim to be the text before
    /// the caret, so it clears. The cached tone persists — see the module
    /// docs.
    pub(crate) fn note_break(&mut self) {
        self.buf.clear();
    }

    /// Run the classifier over the window and cache the verdict. A missing/
    /// corrupt weight asset (builtin() == None) degrades to the neutral
    /// Technical rather than panicking an input-path frame.
    fn infer(&mut self, now: Instant) {
        self.keys_since_infer = 0;
        self.last_infer = Some(now);
        #[cfg(test)]
        {
            self.inferences += 1;
        }
        let Some(model) = tone::builtin() else {
            self.tone = Tone::Technical;
            return;
        };
        let mut text = std::mem::take(&mut self.text_buf_slot);
        text.clear();
        text.extend(self.buf.iter());
        // ABSTENTION LEAVES THE CACHE UNTOUCHED: below `tone::MIN_NGRAMS` the
        // model has no evidence and returns `None` — a one-char line after
        // Enter must NOT snap a classified mood back to neutral, so we keep
        // the prior verdict and only overwrite it when the window actually
        // speaks. A real neutral (`Some(Tone::Technical)`) still overwrites.
        if let Some(tone) = model.classify_opt(&text, &mut self.scratch) {
            self.tone = tone;
        }
        self.text_buf_slot = text;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feed(t: &mut ToneTracker, now: Instant, session: u64, s: &str) {
        for c in s.chars() {
            t.note_char(now, session, c, true);
        }
    }

    /// Feed keystrokes with inference INACTIVE — window maintenance still
    /// runs (the chars land, breaks clear), but the classifier never does.
    fn feed_inactive(t: &mut ToneTracker, now: Instant, session: u64, s: &str) {
        for c in s.chars() {
            t.note_char(now, session, c, false);
        }
    }

    /// A fresh tracker is neutral: the melody plays today's sound until the
    /// classifier has actually seen evidence.
    #[test]
    fn starts_neutral() {
        let t = ToneTracker::default();
        assert_eq!(t.current(), Tone::Technical);
    }

    /// The keystroke throttle: a same-instant burst re-infers once per
    /// [`INFER_KEYS`] keys (plus the immediate first run), never per key.
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

    /// The interval side of the throttle: with the interval elapsed, a
    /// single keystroke is enough to re-infer (slow typing gets fresh
    /// verdicts).
    #[test]
    fn interval_reopens_inference() {
        let mut t = ToneTracker::default();
        let start = Instant::now();
        feed(&mut t, start, 1, "abc");
        let before = t.inferences;
        t.note_char(start + INFER_INTERVAL, 1, 'd', true);
        assert_eq!(
            t.inferences,
            before + 1,
            "an elapsed interval must re-infer"
        );
    }

    /// The classifier actually steers the cache: a canonical frustrated line
    /// (drawn from the committed training distribution — this is a
    /// conformance pin on the shipped weights, not a generalization claim)
    /// flips the verdict, and a following technical command flips it back.
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

    /// Session switches clear the WINDOW but keep the cached tone — halves
    /// of two sessions never assemble one mood, yet flipping tabs does not
    /// snap the melody back to neutral.
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

    /// Backspace pops without ever re-inferring; the window stays bounded
    /// under adversarial length.
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

    /// T1 — an evidence-thin window LEAVES THE CACHED MOOD STANDING. Type an
    /// angry line (mood → Frustrated), press Enter, and after a pause type a
    /// single char: inference runs over a one-char window, the classifier
    /// ABSTAINS (below `MIN_NGRAMS`), and the prior mood must survive rather
    /// than snapping to neutral Technical.
    #[test]
    fn thin_window_after_a_classified_line_keeps_the_mood() {
        let mut t = ToneTracker::default();
        let start = Instant::now();
        feed(&mut t, start, 1, "why is this broken again ugh");
        assert_eq!(t.current(), Tone::Frustrated);
        t.note_break(); // Enter — window clears, mood persists…
        // …a pause (interval elapsed) then ONE char: inference is due and
        // runs over a 1-char window, which is below MIN_NGRAMS ⇒ abstains.
        t.note_char(start + INFER_INTERVAL, 1, 'x', true);
        assert_eq!(t.buf, vec!['x'], "the window is the single new char");
        assert_eq!(
            t.current(),
            Tone::Frustrated,
            "abstention must leave the cached mood untouched"
        );
    }

    /// T2 — breaks recorded while inference is INACTIVE keep the window
    /// coherent, so a re-enable never classifies a window spliced across an
    /// editing boundary. Type a line while active, deactivate, type more and
    /// press Enter while inactive (the break must still clear), then re-enable
    /// and type a fresh line: the classifier sees ONLY the post-break text.
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
