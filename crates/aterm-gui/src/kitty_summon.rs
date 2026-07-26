// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPED-"kitty" summon detector: typing the word `kitty` at the terminal
//! prompt summons a kitty cameo IN the terminal — the input-path twin of the
//! Settings overlay's §L.4 cameo (`settings::KittyPop`, where an occurrence-
//! COUNT INCREASE of "kitty" in the landing comment / sidebar search summons
//! a cat).
//!
//! The detector is a bounded rolling window of the most recent PRINTED
//! keystrokes delivered to one window's front session, fed exclusively from
//! the committed key-press path ([`crate::App::input`]) — NEVER from screen
//! content. `cat`ing a file full of "kitty", a program printing the word, or
//! pasting it cannot summon here: PTY output never reaches the detector,
//! `InputEvent::Text` is IME-commit only, and `InputEvent::Paste` is a
//! different dispatch arm entirely. On-screen occurrences remain the ambient
//! word-cats' domain (`word_decorations`); this module only ever sees keys.
//! Like every effect on the input path the detector is Source-agnostic — a
//! controller typing "kitty" summons exactly like a human (the
//! indistinguishability invariant forbids branching on `Source`).
//!
//! Completion is CASE-INSENSITIVE and substring-shaped (`sKitty` completes —
//! the Settings counter equally counts occurrences anywhere in its field). A
//! plain Backspace pops one letter, making the run backspace-TOLERANT:
//! `kitx⌫ty` still summons, because fixing a typo is still typing the word.
//! Every key that edits or moves beyond one glyph (Enter, Tab, Escape,
//! Delete, navigation, kill/nav chords, raw controller byte sequences)
//! CLEARS the window: a word assembled across an editing boundary was never
//! typed as a word. A pop alone can never summon by construction — only
//! [`TypedKittySummon::note_char`] checks for completion, mirroring the
//! Settings law that "backspacing alone never does".
//!
//! TWO TIERS, deliberately split (owner, 2026-07-24: "writing kitty is suppose
//! to cause the toy kitty to appear. I just now typed it, and it didn't appear!
//! why! it should be 100% of the time"):
//!
//! * The CAMEO fires on EVERY completion. Typing the word is a direct request;
//!   answering it only sometimes reads as broken, not as restraint. A message
//!   containing "kitty" twice used to yield at most one cat, and a deliberate
//!   re-test within 30 s was silently dead — exactly the report above.
//! * The LEDGER row is what [`TYPED_SUMMON_COOLDOWN`] rate-limits, measured
//!   from the last RECORDED summon (a suppressed completion does not restamp
//!   the clock). That was always the documented concern: kitty-spam must not
//!   inflate the Kitty Log or force its writer batches. Nothing about it
//!   requires withholding the drawing.
//!
//! Re-arming the cameo is intrinsically strobe-free, so the visual tier needs
//! no clock of its own: `CursorCat` enters `FadeIn` only from `Hidden`/
//! `FadeOut`, so `kittykittykitty` EXTENDS one hello instead of restarting it.
//!
//! A completion CONSUMES its letters either way — holding `y` re-triggers
//! nothing — the keystroke analogue of the Settings counter's count-increase
//! rule (deleting and retyping the whole word works; the tail alone never does).

use std::time::{Duration, Instant};

/// Minimum spacing between LEDGER-RECORDED summons, per window. ~30 s keeps a
/// deliberate second recorded summon reachable within one sitting while making
/// kitty-spam pointless; it matches the Kitty Log's own flush-debounce scale,
/// so spam cannot force ledger writer batches.
///
/// This bounds the RECORD ONLY. The cameo itself is never withheld — see the
/// two-tier note in the module docs.
pub(crate) const TYPED_SUMMON_COOLDOWN: Duration = Duration::from_secs(30);

/// What one keystroke did to the typed-"kitty" detector.
///
/// Ordered so folding several outcomes (a multi-char IME commit) with `max`
/// keeps the strongest: recording implies showing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub(crate) enum TypedSummon {
    /// No completion on this keystroke.
    #[default]
    None,
    /// "kitty" completed inside the ledger cooldown: SHOW the cameo, record nothing.
    CameoOnly,
    /// "kitty" completed and the ledger clock granted: show AND record.
    CameoAndLog,
}

impl TypedSummon {
    /// Whether the cameo should be presented (either completion tier).
    pub(crate) fn shows_cameo(self) -> bool {
        self != Self::None
    }

    /// Whether the Kitty Log may record this summon.
    pub(crate) fn records(self) -> bool {
        self == Self::CameoAndLog
    }
}

/// Rolling-window capacity in chars: the word plus a few slots of
/// backspace-tolerance history. Deliberately tiny — the hot typing path pays
/// one bounded suffix compare, and after warmup nothing allocates.
const BUF_CAP: usize = 8;

/// The word, lowercase (each keystroke folds through `char::to_lowercase`).
const WORD: [char; 5] = ['k', 'i', 't', 't', 'y'];

/// Ident-namespace tag for SYNTHETIC typed-summon sightings (`b"typedKit"`
/// as big-endian ASCII): XOR'd with the App-wide summon sequence so every
/// granted summon presents a FRESH `(session, ident)` episode to the Kitty
/// Log's dedupe ring — and so summon idents live in their own namespace,
/// apart from the word renderer's position-bearing occurrence idents.
pub(crate) const TYPED_SUMMON_IDENT_TAG: u64 = 0x7479_7065_644B_6974;

/// Per-window detector state: the rolling keystroke window (keyed to the
/// session it was typed into) and the summon cooldown stamp.
#[derive(Default)]
pub(crate) struct TypedKittySummon {
    /// Rolling LOWERCASE window of recent printed keystrokes (≤ [`BUF_CAP`]).
    buf: Vec<char>,
    /// The session the window's letters were typed into. A switch clears the
    /// window — letters typed into different sessions never assemble one word.
    session: Option<u64>,
    /// Instant of the last GRANTED summon — the rate limiter. Deliberately
    /// NOT reset on session switch: flipping tabs must not defeat the
    /// cooldown (the cameo presents per window, so the limit is per window).
    last_summon: Option<Instant>,
}

impl TypedKittySummon {
    /// Bind the window to `session`, clearing it on a switch.
    fn rekey(&mut self, session: u64) {
        if self.session != Some(session) {
            self.buf.clear();
            self.session = Some(session);
        }
    }

    /// Feed one committed printed keystroke.
    ///
    /// Returns [`TypedSummon::CameoOnly`] or [`TypedSummon::CameoAndLog`] when
    /// this letter COMPLETED "kitty" (case-insensitive suffix) — the cameo is
    /// owed either way; the cooldown only decides whether the Kitty Log may
    /// record it. A completion consumes its letters regardless, so the tail can
    /// never re-trigger; a suppressed RECORD does not restamp the clock, so
    /// steady spam cannot starve the next legitimate ledger row.
    pub(crate) fn note_char(&mut self, now: Instant, session: u64, ch: char) -> TypedSummon {
        self.rekey(session);
        for folded in ch.to_lowercase() {
            if self.buf.len() == BUF_CAP {
                self.buf.remove(0);
            }
            self.buf.push(folded);
        }
        let len = self.buf.len();
        if len < WORD.len() || self.buf[len - WORD.len()..] != WORD {
            return TypedSummon::None;
        }
        // Consume the completion BEFORE the cooldown check — the Settings
        // counter's count-increase rule in keystroke form: retyping only the
        // tail (or holding `y`) never completes again.
        self.buf.clear();
        if self
            .last_summon
            .is_some_and(|at| now.saturating_duration_since(at) < TYPED_SUMMON_COOLDOWN)
        {
            // Inside the ledger window: the cat still comes when called.
            return TypedSummon::CameoOnly;
        }
        self.last_summon = Some(now);
        TypedSummon::CameoAndLog
    }

    /// A plain Backspace pops the most recent letter (typo tolerance, bounded
    /// by [`BUF_CAP`] of history). Popping never summons — deletion cannot
    /// complete a word here, only [`Self::note_char`] checks the suffix.
    pub(crate) fn note_backspace(&mut self, session: u64) {
        self.rekey(session);
        self.buf.pop();
    }

    /// A word-breaking key (Enter/Tab/Escape/Delete/nav/kill chords, raw byte
    /// sequences) clears the window: the run must be typed as one word.
    pub(crate) fn note_break(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Feed a string one char at a time; count the LEDGER-RECORDED summons.
    fn feed(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        s.chars()
            .filter(|&c| d.note_char(now, session, c).records())
            .count()
    }

    /// Feed a string one char at a time; count the CAMEOS (either tier).
    fn feed_cameos(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        s.chars()
            .filter(|&c| d.note_char(now, session, c).shows_cameo())
            .count()
    }

    /// The core contract: the typed sequence summons exactly once, at the
    /// completing letter, and the consumed completion's tail re-triggers
    /// nothing (count-increase semantics — a held `y` is not a new word).
    #[test]
    fn typed_word_summons_once_at_completion() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        for c in ['k', 'i', 't', 't'] {
            assert!(
                !d.note_char(t, 7, c).shows_cameo(),
                "no summon before the word completes"
            );
        }
        assert!(
            d.note_char(t, 7, 'y').shows_cameo(),
            "the completing letter summons"
        );
        assert!(
            !d.note_char(t, 7, 'y').shows_cameo(),
            "the completion was consumed — its tail cannot re-trigger"
        );
    }

    /// Case folding and substring shape mirror the Settings occurrence
    /// counter: `sKiTTY` completes (counted anywhere, any case).
    #[test]
    fn completion_is_case_insensitive_and_substring_shaped() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(feed(&mut d, t, 7, "sKiTT"), 0);
        assert!(d.note_char(t, 7, 'Y').shows_cameo());
    }

    /// The documented rate limit, LEDGER TIER: a completion inside the cooldown
    /// records nothing and does NOT restamp the clock, so the next window still
    /// opens [`TYPED_SUMMON_COOLDOWN`] after the last RECORD — spam can never
    /// push the next legitimate ledger row further away.
    #[test]
    fn cooldown_suppresses_the_record_without_restamping() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(feed(&mut d, t, 7, "kitty"), 1, "the first summon records");
        let inside = t + TYPED_SUMMON_COOLDOWN - Duration::from_secs(1);
        assert_eq!(
            feed(&mut d, inside, 7, "kitty"),
            0,
            "a completion inside the cooldown records nothing"
        );
        let after = t + TYPED_SUMMON_COOLDOWN;
        assert_eq!(
            feed(&mut d, after, 7, "kitty"),
            1,
            "the window is measured from the RECORD, not the suppressed attempt"
        );
    }

    /// THE OWNER'S CONTRACT (2026-07-24: "it should be 100% of the time"): the
    /// cooldown bounds the ledger, never the drawing. Every completion — however
    /// fast they are typed — is owed a cameo.
    ///
    /// This is the regression that produced the report: one message containing
    /// "kitty" twice yielded at most one cat, and a deliberate re-test inside the
    /// 30 s window was silently dead.
    #[test]
    fn cooldown_never_withholds_the_cameo() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        // Three completions back-to-back at the SAME instant: maximally inside
        // the cooldown, so every one after the first is ledger-suppressed.
        assert_eq!(
            feed_cameos(&mut d, t, 7, "kittykittykitty"),
            3,
            "every completion draws a cat"
        );

        // ...while the ledger still sees exactly one, from a fresh detector.
        let mut ledger = TypedKittySummon::default();
        assert_eq!(
            feed(&mut ledger, t, 7, "kittykittykitty"),
            1,
            "the ledger tier still rate-limits to one record"
        );
    }

    /// The two tiers are exactly the two completion outcomes, and `records`
    /// implies `shows_cameo` — the ordering the IME fold relies on.
    #[test]
    fn cameo_and_log_tiers_are_ordered() {
        assert!(!TypedSummon::None.shows_cameo());
        assert!(!TypedSummon::None.records());
        assert!(TypedSummon::CameoOnly.shows_cameo());
        assert!(!TypedSummon::CameoOnly.records());
        assert!(TypedSummon::CameoAndLog.shows_cameo());
        assert!(TypedSummon::CameoAndLog.records());
        assert!(TypedSummon::CameoAndLog > TypedSummon::CameoOnly);
        assert!(TypedSummon::CameoOnly > TypedSummon::None);
    }

    /// Backspace tolerance as documented: fixing a typo mid-word keeps the
    /// run (`kitx⌫ty` summons), and a pop alone can never summon — deleting
    /// down to (or past) empty then typing a lone `y` completes nothing.
    #[test]
    fn backspace_pops_typos_and_never_summons() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(feed(&mut d, t, 7, "kitx"), 0);
        d.note_backspace(7);
        assert!(!d.note_char(t, 7, 't').shows_cameo());
        assert!(
            d.note_char(t, 7, 'y').shows_cameo(),
            "the corrected word still summons"
        );

        let mut fresh = TypedKittySummon::default();
        assert_eq!(feed(&mut fresh, t, 7, "kitt"), 0);
        for _ in 0..BUF_CAP {
            fresh.note_backspace(7); // over-popping an emptied window is a no-op
        }
        assert!(
            !fresh.note_char(t, 7, 'y').shows_cameo(),
            "deletion cleared the run — a lone tail letter completes nothing"
        );
    }

    /// Word-breaking keys clear the run: `kit` ⏎ `ty` was never the typed
    /// word; the next contiguous `kitty` summons normally.
    #[test]
    fn break_clears_the_run() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(feed(&mut d, t, 7, "kit"), 0);
        d.note_break();
        assert_eq!(
            feed(&mut d, t, 7, "ty"),
            0,
            "the run did not survive the break"
        );
        assert_eq!(feed(&mut d, t, 7, "kitty"), 1);
    }

    /// Letters typed into DIFFERENT sessions never assemble one word: the
    /// window is keyed to the session it was typed into and clears on switch.
    #[test]
    fn session_switch_clears_the_window() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(feed(&mut d, t, 1, "kit"), 0);
        assert_eq!(
            feed(&mut d, t, 2, "ty"),
            0,
            "a tab switch mid-word breaks the run"
        );
        assert_eq!(
            feed(&mut d, t, 2, "kitty"),
            1,
            "the new session types the whole word and summons"
        );
    }

    /// The public constants ARE the documented contract: a ~30 s cooldown, a
    /// window that can hold the word, and the `b"typedKit"` ident namespace.
    #[test]
    fn documented_constants_are_pinned() {
        assert_eq!(TYPED_SUMMON_COOLDOWN, Duration::from_secs(30));
        assert!(BUF_CAP >= WORD.len());
        assert_eq!(TYPED_SUMMON_IDENT_TAG, u64::from_be_bytes(*b"typedKit"));
    }
}
