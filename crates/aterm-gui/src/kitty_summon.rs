// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPED-WORD detector: the companion reacts to words you TYPE at the prompt,
//! in every language the lexicon knows.
//!
//! Two families are recognized, both straight out of [`aterm_lexicon`]:
//!
//! * [`Class::Feline`] — typing `kitty` (or `gato`, `chat`, `neko`, `kucing`,
//!   `猫`, …) summons a kitty cameo IN the terminal, the input-path twin of the
//!   Settings overlay's §L.4 cameo, and DELIGHTS the cursor companion.
//! * [`Class::Profanity`] — typing a complete curse in any listed language
//!   makes the companion WINCE. The screen scanner already reacts to profanity
//!   it can SEE (`word_decorations`'s `CurseCue`); this is the same reaction
//!   owed to a word that is merely typed, including into a no-echo prompt.
//!
//! MULTILINGUAL BY CONSTRUCTION, AND WORD-BOUNDED. Until 2026-07-26 this module
//! hardcoded `WORD: [char; 5] = ['k','i','t','t','y']` and completed on a
//! case-folded SUBSTRING suffix, so `sKitty` summoned. Both properties are now
//! gone deliberately:
//!
//! * English-only detection could never satisfy the multilingual requirement,
//!   and re-deriving per-language surfaces here would fork the vocabulary away
//!   from the one compiled list the screen scanner uses.
//! * Substring matching is actively dangerous on a multilingual vocabulary —
//!   that is precisely the bug in `27724514` ("typing `pick` fired an f-bomb —
//!   and 11 other languages had the same bug"). The lexicon's own word-boundary
//!   matching, `ambiguous` gating, and possessive/clitic morphology are the
//!   tested defence, so the detector defers to them wholesale.
//!
//! The detector is a bounded rolling window of the most recent PRINTED
//! keystrokes delivered to one window's front session, fed exclusively from
//! the committed key-press path ([`crate::App::input`]) — NEVER from screen
//! content. `cat`ing a file full of "kitty", a program printing the word, or
//! pasting it cannot fire here: PTY output never reaches the detector,
//! `InputEvent::Text` is IME-commit only, and `InputEvent::Paste` is a
//! different dispatch arm entirely. On-screen occurrences remain the ambient
//! word-cats' domain (`word_decorations`); this module only ever sees keys.
//! Like every effect on the input path the detector is Source-agnostic — a
//! controller typing "kitty" fires exactly like a human (the
//! indistinguishability invariant forbids branching on `Source`).
//!
//! A plain Backspace pops one letter, making the run backspace-TOLERANT:
//! `kitx⌫ty` still fires, because fixing a typo is still typing the word.
//! Every key that edits or moves beyond one glyph (Enter, Tab, Escape,
//! Delete, navigation, kill/nav chords, raw controller byte sequences)
//! CLEARS the window: a word assembled across an editing boundary was never
//! typed as a word. A pop alone can never fire by construction — only
//! [`TypedKittySummon::note_char`] checks for completion, mirroring the
//! Settings law that "backspacing alone never does".
//!
//! TWO TIERS for the feline family, deliberately split (owner, 2026-07-24:
//! "writing kitty is suppose to cause the toy kitty to appear. I just now typed
//! it, and it didn't appear! why! it should be 100% of the time"):
//!
//! * The CAMEO fires on EVERY completion. Typing the word is a direct request;
//!   answering it only sometimes reads as broken, not as restraint.
//! * The LEDGER row is what [`TYPED_SUMMON_COOLDOWN`] rate-limits, measured
//!   from the last RECORDED summon (a suppressed completion does not restamp
//!   the clock). That was always the documented concern: kitty-spam must not
//!   inflate the Kitty Log or force its writer batches. Nothing about it
//!   requires withholding the drawing.
//!
//! Profanity carries no ledger tier at all — the companion's wince is pure
//! expression and records nothing.
//!
//! A completion CONSUMES its letters either way — holding `y` re-triggers
//! nothing — the keystroke analogue of the Settings counter's count-increase
//! rule (deleting and retyping the whole word works; the tail alone never does).

use aterm_lexicon::{Class, Lexicon, Match, ScanOptions, ScanScratch};
use std::time::{Duration, Instant};

/// Minimum spacing between LEDGER-RECORDED summons, per window. ~30 s keeps a
/// deliberate second recorded summon reachable within one sitting while making
/// kitty-spam pointless; it matches the Kitty Log's own flush-debounce scale,
/// so spam cannot force ledger writer batches.
///
/// This bounds the RECORD ONLY. The cameo itself is never withheld — see the
/// two-tier note in the module docs.
pub(crate) const TYPED_SUMMON_COOLDOWN: Duration = Duration::from_secs(30);

/// What one keystroke did to the typed-feline detector.
///
/// Ordered so folding several outcomes (a multi-char IME commit) with `max`
/// keeps the strongest: recording implies showing.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug, Default)]
pub(crate) enum TypedSummon {
    /// No feline completion on this keystroke.
    #[default]
    None,
    /// A feline word completed inside the ledger cooldown: SHOW the cameo,
    /// record nothing.
    CameoOnly,
    /// A feline word completed and the ledger clock granted: show AND record.
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

/// Everything one keystroke (or one IME commit char) produced.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct TypedHit {
    /// The feline cameo/ledger verdict.
    pub(crate) summon: TypedSummon,
    /// A feline word completed — the companion's DELIGHT reaction is owed.
    pub(crate) feline: bool,
    /// A profanity word completed — the companion's WINCE reaction is owed.
    pub(crate) profanity: bool,
}

impl TypedHit {
    /// Fold a second outcome in, keeping the strongest of each independent
    /// axis. Used across the chars of one IME commit.
    pub(crate) fn merge(self, other: Self) -> Self {
        Self {
            summon: self.summon.max(other.summon),
            feline: self.feline || other.feline,
            profanity: self.profanity || other.profanity,
        }
    }
}

/// Rolling-window capacity in CHARS. Must comfortably exceed the longest
/// lexicon surface in any language (the compiled list's longest spaced form is
/// well under 20 chars) plus a few slots of backspace-tolerance history.
/// Deliberately small — the hot typing path pays one bounded scan of this
/// window, and after warmup nothing allocates.
const BUF_CAP: usize = 48;

/// Ident-namespace tag for SYNTHETIC typed-summon sightings (`b"typedKit"`
/// as big-endian ASCII): XOR'd with the App-wide summon sequence so every
/// granted summon presents a FRESH `(session, ident)` episode to the Kitty
/// Log's dedupe ring — and so summon idents live in their own namespace,
/// apart from the word renderer's position-bearing occurrence idents.
pub(crate) const TYPED_SUMMON_IDENT_TAG: u64 = 0x7479_7065_644B_6974;

/// Ident-namespace tag for SYNTHETIC favourite sightings (`b"favKitty"` as
/// big-endian ASCII): its own namespace beside the typed summon, sharing the
/// App-wide summon sequence so every press is a FRESH `(session, ident)`
/// episode. The favourite path bypasses the dedupe ring outright, so this tag
/// exists to keep the ledger's episode identities honest and collision-free,
/// not to defeat a suppression.
pub(crate) const FAVOURITE_IDENT_TAG: u64 = 0x6661_764B_6974_7479;

/// Per-window detector state: the rolling keystroke window (keyed to the
/// session it was typed into), the summon cooldown stamp, and the resident
/// scan buffers.
#[derive(Default)]
pub(crate) struct TypedKittySummon {
    /// Rolling window of recent printed keystrokes (≤ [`BUF_CAP`] chars).
    /// Stored RAW — the lexicon owns folding, and pre-folding here would
    /// defeat its morphology handling.
    buf: String,
    /// The session the window's letters were typed into. A switch clears the
    /// window — letters typed into different sessions never assemble one word.
    session: Option<u64>,
    /// Instant of the last GRANTED summon — the rate limiter. Deliberately
    /// NOT reset on session switch: flipping tabs must not defeat the
    /// cooldown (the cameo presents per window, so the limit is per window).
    last_summon: Option<Instant>,
    /// Resident scan buffers: after warmup the per-keystroke scan allocates
    /// nothing.
    chars: Vec<char>,
    matches: Vec<Match>,
    scratch: ScanScratch,
}

impl TypedKittySummon {
    /// Bind the window to `session`, clearing it on a switch.
    fn rekey(&mut self, session: u64) {
        if self.session != Some(session) {
            self.buf.clear();
            self.session = Some(session);
        }
    }

    /// Drop leading chars until the window holds at most [`BUF_CAP`].
    /// Trimming on a CHAR boundary keeps the buffer valid UTF-8.
    fn trim(&mut self) {
        while self.buf.chars().count() > BUF_CAP {
            let mut it = self.buf.chars();
            it.next();
            let rest = it.as_str().to_string();
            self.buf = rest;
        }
    }

    /// Feed one committed printed keystroke.
    ///
    /// Fires when this character COMPLETED a lexicon word — i.e. the scan
    /// finds a match whose end is the window's end. For the feline family the
    /// cameo is owed either way; the cooldown only decides whether the Kitty
    /// Log may record it. A completion consumes its letters regardless, so the
    /// tail can never re-trigger; a suppressed RECORD does not restamp the
    /// clock, so steady spam cannot starve the next legitimate ledger row.
    pub(crate) fn note_char(
        &mut self,
        now: Instant,
        session: u64,
        ch: char,
        lexicon: &Lexicon,
        opts: &ScanOptions,
    ) -> TypedHit {
        self.rekey(session);
        self.buf.push(ch);
        self.trim();
        let Self {
            buf,
            chars,
            matches,
            scratch,
            ..
        } = self;
        lexicon.scan_into_with_scratch(buf, opts, chars, matches, scratch);
        let end = self.chars.len();
        // A word COMPLETED on this keystroke iff a match ends at the window's
        // end. Anything earlier was already consumed or is mid-word context.
        let hit =
            self.matches
                .iter()
                .filter(|m| m.end == end)
                .fold(None::<Class>, |acc, m| match (acc, m.class) {
                    // Cross-class homographs resolve by the lexicon's own total
                    // order (profanity > feline), matching the screen scanner.
                    (Some(Class::Profanity), _) | (_, Class::Profanity) => Some(Class::Profanity),
                    (_, class) => Some(class),
                });
        let Some(class) = hit else {
            return TypedHit::default();
        };
        // Consume the completion BEFORE the cooldown check — the Settings
        // counter's count-increase rule in keystroke form: retyping only the
        // tail (or holding a letter) never completes again.
        self.buf.clear();
        match class {
            Class::Profanity => TypedHit {
                summon: TypedSummon::None,
                feline: false,
                profanity: true,
            },
            Class::Feline => {
                let summon = if self
                    .last_summon
                    .is_some_and(|at| now.saturating_duration_since(at) < TYPED_SUMMON_COOLDOWN)
                {
                    // Inside the ledger window: the cat still comes when called.
                    TypedSummon::CameoOnly
                } else {
                    self.last_summon = Some(now);
                    TypedSummon::CameoAndLog
                };
                TypedHit {
                    summon,
                    feline: true,
                    profanity: false,
                }
            }
            // Other classes (orca, emphasis) carry no companion reaction.
            _ => TypedHit::default(),
        }
    }

    /// A plain Backspace pops the most recent char (typo tolerance, bounded
    /// by [`BUF_CAP`] of history). Popping never fires — deletion cannot
    /// complete a word here, only [`Self::note_char`] checks for completion.
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

    fn lex() -> Lexicon {
        Lexicon::builtin().clone()
    }

    /// Feed a string one char at a time; count the LEDGER-RECORDED summons.
    fn feed(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).summon.records())
            .count()
    }

    /// Feed a string one char at a time; count the CAMEOS (either tier).
    fn feed_cameos(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| {
                d.note_char(now, session, c, &lx, &opts)
                    .summon
                    .shows_cameo()
            })
            .count()
    }

    /// Feed a string one char at a time; count PROFANITY completions.
    fn feed_curses(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).profanity)
            .count()
    }

    /// The core contract: the typed sequence fires exactly once, at the
    /// completing letter, and the consumed completion's tail re-triggers
    /// nothing (count-increase semantics — a held `y` is not a new word).
    #[test]
    fn typed_word_summons_once_at_completion() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        let lx = lex();
        let opts = ScanOptions::default();
        for c in ['k', 'i', 't', 't'] {
            assert!(
                !d.note_char(t, 7, c, &lx, &opts).summon.shows_cameo(),
                "no summon before the word completes"
            );
        }
        assert!(
            d.note_char(t, 7, 'y', &lx, &opts).summon.shows_cameo(),
            "the completing letter summons"
        );
        assert!(
            !d.note_char(t, 7, 'y', &lx, &opts).summon.shows_cameo(),
            "the completion was consumed — its tail cannot re-trigger"
        );
    }

    /// Case folding is the lexicon's, so `KiTTY` completes.
    #[test]
    fn completion_is_case_insensitive() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(feed(&mut d, t, 7, "KiTTY"), 1);
    }

    /// MULTILINGUAL (owner, 2026-07-26: "this must also work multilingual").
    /// Every listed language's feline word summons through the same detector —
    /// no per-language code here, the compiled lexicon is the vocabulary. Words
    /// from languages the user has NOT enabled are simply absent from the
    /// compiled build (`chat`/`neko`/`katze` are not in `Lexicon::builtin`'s
    /// default subset), so this fixture stays inside it; widening the config's
    /// language list widens the detector with zero code change here.
    #[test]
    fn feline_words_summon_in_every_listed_language() {
        let t = Instant::now();
        for word in ["kitty", "gato", "gatto", "kucing"] {
            let mut d = TypedKittySummon::default();
            assert_eq!(
                feed_cameos(&mut d, t, 7, word),
                1,
                "typing {word:?} summons the cameo"
            );
        }
    }

    /// WORD BOUNDARIES ARE THE POINT. The pre-2026-07-26 detector completed on
    /// a case-folded SUBSTRING suffix, so `hellokitty` summoned. The lexicon's
    /// boundary matching is what makes multilingual detection safe at all (see
    /// `innocent_words_containing_a_curse_never_fire`), and it applies to the
    /// feline family identically: a word is typed when it is typed as a word.
    #[test]
    fn a_feline_word_glued_to_another_is_not_the_word() {
        let t = Instant::now();
        let mut glued = TypedKittySummon::default();
        assert_eq!(
            feed_cameos(&mut glued, t, 7, "hellokitty"),
            0,
            "`kitty` welded onto a preceding token was not typed as a word"
        );
        let mut spaced = TypedKittySummon::default();
        assert_eq!(
            feed_cameos(&mut spaced, t, 7, "hello kitty"),
            1,
            "…and the ordinary spaced form still summons"
        );
    }

    /// The profanity family reacts in every listed language too, and carries
    /// NO ledger tier — the wince is pure expression.
    #[test]
    fn profanity_winces_multilingually_and_never_records() {
        let t = Instant::now();
        for word in ["fuck", "merde", "mierda"] {
            let mut d = TypedKittySummon::default();
            assert_eq!(
                feed_curses(&mut d, t, 7, word),
                1,
                "typing {word:?} winces the companion"
            );
            let mut ledger = TypedKittySummon::default();
            assert_eq!(
                feed(&mut ledger, t, 7, word),
                0,
                "{word:?} is not a kitty sighting"
            );
        }
    }

    /// THE `27724514` REGRESSION, now structural: word-boundary matching means
    /// an ordinary word that merely CONTAINS a curse never fires. Substring
    /// matching — what this detector used to do for `kitty` — is exactly what
    /// made typing `pick` fire an f-bomb in twelve languages.
    #[test]
    fn innocent_words_containing_a_curse_never_fire() {
        let t = Instant::now();
        for word in ["pick", "assemble", "class", "scunthorpe"] {
            let mut d = TypedKittySummon::default();
            assert_eq!(
                feed_curses(&mut d, t, 7, word),
                0,
                "{word:?} must not fire an f-bomb"
            );
        }
    }

    /// The documented rate limit, LEDGER TIER: a completion inside the cooldown
    /// records nothing and does NOT restamp the clock, so the next window still
    /// opens [`TYPED_SUMMON_COOLDOWN`] after the last RECORD — spam can never
    /// push the next legitimate ledger row further away.
    #[test]
    fn cooldown_suppresses_the_record_without_restamping() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(feed(&mut d, t, 7, "kitty "), 1, "the first summon records");
        let inside = t + TYPED_SUMMON_COOLDOWN - Duration::from_secs(1);
        assert_eq!(
            feed(&mut d, inside, 7, "kitty "),
            0,
            "a completion inside the cooldown records nothing"
        );
        let after = t + TYPED_SUMMON_COOLDOWN;
        assert_eq!(
            feed(&mut d, after, 7, "kitty "),
            1,
            "the window is measured from the RECORD, not the suppressed attempt"
        );
    }

    /// THE OWNER'S CONTRACT (2026-07-24: "it should be 100% of the time"): the
    /// cooldown bounds the ledger, never the drawing. Every completion — however
    /// fast they are typed — is owed a cameo.
    #[test]
    fn cooldown_never_withholds_the_cameo() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(
            feed_cameos(&mut d, t, 7, "kitty kitty kitty"),
            3,
            "every completion draws a cat"
        );
        let mut ledger = TypedKittySummon::default();
        assert_eq!(
            feed(&mut ledger, t, 7, "kitty kitty kitty"),
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

    /// `merge` keeps the strongest of each independent axis, so one IME commit
    /// carrying both families owes both reactions.
    #[test]
    fn merge_keeps_every_axis() {
        let feline = TypedHit {
            summon: TypedSummon::CameoAndLog,
            feline: true,
            profanity: false,
        };
        let curse = TypedHit {
            summon: TypedSummon::None,
            feline: false,
            profanity: true,
        };
        let both = feline.merge(curse);
        assert_eq!(both.summon, TypedSummon::CameoAndLog);
        assert!(both.feline && both.profanity);
    }

    /// Backspace tolerance as documented: fixing a typo mid-word keeps the
    /// run (`kitx⌫ty` summons), and a pop alone can never summon — deleting
    /// down to (or past) empty then typing a lone `y` completes nothing.
    #[test]
    fn backspace_pops_typos_and_never_summons() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        let lx = lex();
        let opts = ScanOptions::default();
        assert_eq!(feed(&mut d, t, 7, "kitx"), 0);
        d.note_backspace(7);
        assert!(!d.note_char(t, 7, 't', &lx, &opts).summon.shows_cameo());
        assert!(
            d.note_char(t, 7, 'y', &lx, &opts).summon.shows_cameo(),
            "the corrected word still summons"
        );

        let mut fresh = TypedKittySummon::default();
        assert_eq!(feed(&mut fresh, t, 7, "kitt"), 0);
        for _ in 0..BUF_CAP {
            fresh.note_backspace(7); // over-popping an emptied window is a no-op
        }
        assert!(
            !fresh.note_char(t, 7, 'y', &lx, &opts).summon.shows_cameo(),
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
        // The leftover `ty` is still in the window, so the next word needs a
        // delimiter to be a word — exactly as it would when actually typed.
        assert_eq!(feed(&mut d, t, 7, " kitty"), 1);
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
            feed(&mut d, t, 2, " kitty"),
            1,
            "the new session types the whole word and summons"
        );
    }

    /// The window stays bounded and valid UTF-8 under a long multi-byte run,
    /// and a word completing at the very end still fires after trimming.
    #[test]
    fn window_stays_bounded_and_utf8() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        let long = "é".repeat(BUF_CAP * 3);
        assert_eq!(feed(&mut d, t, 7, &long), 0);
        assert!(d.buf.chars().count() <= BUF_CAP, "the window stays bounded");
        assert_eq!(feed(&mut d, t, 7, " kitty"), 1, "and still completes");
    }

    /// The public constants ARE the documented contract: a ~30 s cooldown, a
    /// window that can hold any lexicon surface, and the `b"typedKit"` namespace.
    #[test]
    fn documented_constants_are_pinned() {
        assert_eq!(TYPED_SUMMON_COOLDOWN, Duration::from_secs(30));
        assert_eq!(TYPED_SUMMON_IDENT_TAG, u64::from_be_bytes(*b"typedKit"));
        assert_eq!(FAVOURITE_IDENT_TAG, u64::from_be_bytes(*b"favKitty"));
        assert_ne!(
            TYPED_SUMMON_IDENT_TAG, FAVOURITE_IDENT_TAG,
            "summons and favourites are distinct ident namespaces"
        );
        // The window must be able to hold the LONGEST surface the compiled
        // lexicon can match, or that word could never complete inside it.
        let longest = lex()
            .iter_spaced()
            .map(|(surface, _)| surface.chars().count())
            .max()
            .expect("the builtin lexicon is non-empty");
        assert!(
            BUF_CAP >= longest,
            "the window ({BUF_CAP}) must hold the longest lexicon surface ({longest})"
        );
    }
}
