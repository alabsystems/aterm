// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! TYPED-WORD detector: the companion reacts to words you TYPE at the prompt,
//! in every language the lexicon knows.
//!
//! Three families are recognized, all straight out of [`aterm_lexicon`]:
//!
//! * [`Class::Feline`] — typing `kitty` (or `gato`, `chat`, `neko`, `kucing`,
//!   `猫`, …) records a Kitty Log sighting. It draws NOTHING of its own: the
//!   word echoes onto the grid and the screen scanner answers it with the one
//!   ambient word-cat, centred on the word (owner, 2026-08-12: *"I want the cat
//!   just like in the main text"*).
//! * [`Class::Profanity`] — typing a complete curse in any listed language
//!   makes the companion WINCE. The screen scanner already reacts to profanity
//!   it can SEE (`word_decorations`'s `CurseCue`); this is the same reaction
//!   owed to a word that is merely typed, including into a no-echo prompt.
//! * [`Class::Canine`] — typing `dog` / `puppy` (or `perro`, `chiot`,
//!   `hund`, `子犬`, …) pops up a DOG next to the cursor — but only after the
//!   window has typed a lot ([`DOG_SUMMON_KEYS`] printed keystrokes). Every
//!   summon rolls a fresh breed from the authored roster
//!   ([`crate::dog_cameo`]); dogs carry no ledger tier at all.
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
//! the host's committed key-press path — NEVER from screen
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
//! * The COMPLETION fires on EVERY typed word and is CONSUMED there. Typing
//!   the word is a direct request; answering it only sometimes reads as broken,
//!   not as restraint. What answers it is the ambient word-cat.
//! * The LEDGER row is what [`TYPED_SUMMON_COOLDOWN`] rate-limits, measured
//!   from the last RECORDED summon (a suppressed completion does not restamp
//!   the clock). That was always the documented concern: kitty-spam must not
//!   inflate the Kitty Log or force its writer batches. Nothing about it
//!   requires withholding the drawing, and nothing here does.
//!
//! Profanity carries no ledger tier at all — the companion's wince is pure
//! expression and records nothing.
//!
//! A completion CONSUMES its letters either way — holding `y` re-triggers
//! nothing — the keystroke analogue of the Settings counter's count-increase
//! rule (deleting and retyping the whole word works; the tail alone never does).

use aterm_lexicon::{Class, Lexicon, Match, ScanOptions, ScanScratch};
use aterm_time::Instant;
use std::time::Duration;

/// Minimum spacing between LEDGER-RECORDED summons, per window. ~30 s keeps a
/// deliberate second recorded summon reachable within one sitting while making
/// kitty-spam pointless; it matches the Kitty Log's own flush-debounce scale,
/// so spam cannot force ledger writer batches.
///
/// This bounds the RECORD ONLY. The word's own cat — the ambient peek the
/// screen scanner grows — is never withheld; see the two-tier note in the
/// module docs.
pub const TYPED_SUMMON_COOLDOWN: Duration = Duration::from_secs(30);

/// THE DOG GATE (owner's design, 2026-08-10: "you have to type a lot first"):
/// canine completions summon only after this many PRINTED keystrokes have been
/// fed to this window's detector, lifetime-of-the-window. The kitty answers
/// its name from the first keystroke — it lives here; the dog has to hear a
/// LOT of typing before it trots over from wherever dogs wait. Counted on the
/// same committed-key path as the rolling window (PTY output, paste, and IME
/// non-commits can never inflate it), never reset by session switches,
/// breaks, or backspaces — deleting text does not un-type it.
pub const DOG_SUMMON_KEYS: u64 = 500;

/// Everything one keystroke (or one IME commit char) produced.
///
/// Four independent latches. [`Self::feline`] and [`Self::records`] are the two
/// feline tiers: the completion is unconditional, the ledger row is what the
/// cooldown limits, and `records` implies `feline`.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TypedHit {
    /// A feline word completed. Expression only — nothing on the input path
    /// draws for it; the echoed word's own ambient cat is the answer.
    pub feline: bool,
    /// A feline word completed AND the Kitty Log's cooldown granted the row.
    /// Never set without [`Self::feline`].
    pub records: bool,
    /// A profanity word completed — the companion's WINCE reaction is owed.
    pub profanity: bool,
    /// A canine word completed AND the window has typed a lot
    /// ([`DOG_SUMMON_KEYS`]) — the dog cameo is owed. Below the gate a canine
    /// completion is consumed silently (no tier, no ledger, no reaction): the
    /// dog simply has not arrived yet.
    pub canine: bool,
}

impl TypedHit {
    /// Fold a second outcome in, keeping the strongest of each independent
    /// axis. Used across the chars of one IME commit.
    pub fn merge(self, other: Self) -> Self {
        Self {
            feline: self.feline || other.feline,
            records: self.records || other.records,
            profanity: self.profanity || other.profanity,
            canine: self.canine || other.canine,
        }
    }
}

/// Rolling-window capacity in chars. Must exceed the longest lexicon surface in
/// any language (well under 20 chars) plus some backspace-tolerance history.
/// Small because the typing path scans it on every key; after warmup nothing
/// allocates.
const BUF_CAP: usize = 48;

/// Ident-namespace tag for synthetic typed-summon sightings (`b"typedKit"` as
/// big-endian ASCII), XOR'd with the App-wide summon sequence so every granted
/// summon is a fresh `(session, ident)` episode for the Kitty Log's dedupe
/// ring, in a namespace apart from the word renderer's occurrence idents.
///
/// scope-waiver: the DECLARATION is a compile-time namespace tag — a constant
/// cannot multiply. The App-wide sequence it is XOR'd with is
/// `App::kitty_summon_seq`, whose own cardinality is waived (and queued as a
/// ScopeClaim) at its declaration in `lib.rs`.
pub const TYPED_SUMMON_IDENT_TAG: u64 = 0x7479_7065_644B_6974;

/// Ident-namespace tag for synthetic favourite sightings (`b"favKitty"` as
/// big-endian ASCII), sharing the App-wide summon sequence so every press is a
/// fresh `(session, ident)` episode. The favourite path bypasses the dedupe
/// ring; the tag keeps ledger episode identities collision-free.
///
/// scope-waiver: same shape as [`TYPED_SUMMON_IDENT_TAG`] — a constant
/// namespace tag, not the sequence.
pub const FAVOURITE_IDENT_TAG: u64 = 0x6661_764B_6974_7479;

/// Ident-namespace tag for typed dog summons (`b"typedDog"` as big-endian
/// ASCII), XOR'd with the same summon sequence. Never recorded (the Kitty Log
/// is cats); it seeds the dog cameo's breed/coat roll, fresh per summon.
///
/// scope-waiver: same shape as [`TYPED_SUMMON_IDENT_TAG`] — a constant
/// namespace tag, not the sequence.
pub const DOG_SUMMON_IDENT_TAG: u64 = 0x7479_7065_6444_6f67;

/// Per-window detector state: the rolling keystroke window (keyed to the
/// session it was typed into), the summon cooldown stamp, and the resident
/// scan buffers.
#[derive(Default)]
pub struct TypedKittySummon {
    /// Rolling window of recent printed keystrokes (≤ [`BUF_CAP`] chars).
    /// Stored RAW — the lexicon owns folding, and pre-folding here would
    /// defeat its morphology handling.
    buf: String,
    /// The session the window's letters were typed into. A switch clears the
    /// window; letters typed into different sessions never form one word.
    session: Option<u64>,
    /// Instant of the last granted summon (the rate limiter). Not reset on
    /// session switch, so flipping tabs cannot defeat the per-window cooldown.
    last_summon: Option<Instant>,
    /// Resident scan buffers: after warmup the per-keystroke scan allocates
    /// nothing.
    chars: Vec<char>,
    matches: Vec<Match>,
    scratch: ScanScratch,
    /// Reused `(byte offset, char)` walk for [`Self::trailing_scan_start`].
    tail: Vec<(usize, char)>,
    /// Lifetime count of printed keystrokes fed to this window's detector: the
    /// [`DOG_SUMMON_KEYS`] odometer. Not reset by session switches, breaks or
    /// backspaces, and not persisted.
    keys_typed: u64,
}

impl TypedKittySummon {
    /// Bind the window to `session`, clearing it on a switch.
    fn rekey(&mut self, session: u64) {
        if self.session != Some(session) {
            self.buf.clear();
            self.session = Some(session);
        }
    }

    /// Drop leading chars until the window holds at most [`BUF_CAP`]. Trimming
    /// on a char boundary keeps the buffer valid UTF-8.
    fn trim(&mut self) {
        while self.buf.chars().count() > BUF_CAP {
            let first = self.buf.chars().next().map_or(0, char::len_utf8);
            self.buf.drain(..first);
        }
    }

    /// Feed one committed printed keystroke.
    ///
    /// Fires when this character COMPLETED a lexicon word — i.e. the scan
    /// finds a match whose end is the window's end. For the feline family the
    /// cooldown only decides whether the Kitty Log may record it. A completion
    /// consumes its letters regardless, so the tail can never re-trigger; a
    /// suppressed RECORD does not restamp the clock, so steady spam cannot
    /// starve the next legitimate ledger row.
    pub fn note_char(
        &mut self,
        now: Instant,
        session: u64,
        ch: char,
        lexicon: &Lexicon,
        opts: &ScanOptions,
    ) -> TypedHit {
        self.rekey(session);
        self.keys_typed = self.keys_typed.saturating_add(1);
        self.buf.push(ch);
        self.trim();
        // A completion is a match ending at the window's end, and the scanner
        // ends a token only on a token character (an interior joiner only when
        // a token character follows) and a no-space run only on a no-space
        // character, so a key that is neither completes nothing.
        if !(aterm_lexicon::is_token_char(ch) || aterm_lexicon::is_no_space_script(ch)) {
            return TypedHit::default();
        }
        // Scan only the trailing token / no-space run plus the two characters
        // of left context the scanner's code-adjacency guard reads, segmented
        // with the lexicon's own predicates (its `token_end`, walked
        // backwards). Exact for any surface length: earlier tokens could only
        // yield matches that do not end at the window end.
        let start = Self::trailing_scan_start(&self.buf, ch, &mut self.tail);
        let Self {
            buf,
            chars,
            matches,
            scratch,
            ..
        } = self;
        lexicon.scan_into_with_scratch(&buf[start..], opts, chars, matches, scratch);
        let end = self.chars.len();
        // A word completed on this keystroke iff a match ends at the window's
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
        // Consume the completion before the cooldown check: retyping only the
        // tail (or holding a letter) never completes again.
        self.buf.clear();
        match class {
            Class::Profanity => TypedHit {
                profanity: true,
                ..TypedHit::default()
            },
            Class::Feline => {
                // Inside the ledger window the completion still stands (the
                // ambient cat still comes); only the row is not written.
                let records = !self
                    .last_summon
                    .is_some_and(|at| now.saturating_duration_since(at) < TYPED_SUMMON_COOLDOWN);
                if records {
                    self.last_summon = Some(now);
                }
                TypedHit {
                    feline: true,
                    records,
                    ..TypedHit::default()
                }
            }
            // Below the [`DOG_SUMMON_KEYS`] gate the completion is still
            // consumed, and nothing tiers, records or reacts.
            Class::Canine => TypedHit {
                canine: self.keys_typed >= DOG_SUMMON_KEYS,
                ..TypedHit::default()
            },
            // Other classes (animal, emphasis) carry no companion reaction.
            _ => TypedHit::default(),
        }
    }

    /// Byte offset into `buf` where the incremental scan starts: the trailing
    /// token (when `last` is a token char) or trailing no-space run (when it is
    /// a no-space-script char), extended by two preceding chars of left
    /// context. `tail` is reused scratch for the backward walk.
    ///
    /// Token segmentation mirrors the scanner's `token_end`, backwards: a
    /// maximal run of token chars where an interior joiner (`'`, `’`, `-`)
    /// counts only with token chars on both sides.
    fn trailing_scan_start(buf: &str, last: char, tail: &mut Vec<(usize, char)>) -> usize {
        tail.clear();
        tail.extend(buf.char_indices());
        let n = tail.len();
        if n == 0 {
            return 0;
        }
        // Index (into `tail`) of the first char of the trailing token/run.
        let mut i = n - 1;
        if aterm_lexicon::is_no_space_script(last) {
            while i > 0 && aterm_lexicon::is_no_space_script(tail[i - 1].1) {
                i -= 1;
            }
        } else {
            loop {
                if i > 0 && aterm_lexicon::is_token_char(tail[i - 1].1) {
                    i -= 1;
                } else if i > 1
                    && aterm_lexicon::is_interior_joiner(tail[i - 1].1)
                    && aterm_lexicon::is_token_char(tail[i - 2].1)
                {
                    // `tail[i]` is a token char (we only arrive here from one),
                    // so the joiner is interior: consume it and keep walking.
                    i -= 2;
                } else {
                    break;
                }
            }
        }
        // Two chars of left context for `left_suppresses` (`c == '.'` looks one
        // further back than the adjacent char).
        tail[i.saturating_sub(2)].0
    }

    /// A plain Backspace pops the most recent char (typo tolerance, bounded
    /// by [`BUF_CAP`] of history). Popping never fires — deletion cannot
    /// complete a word here, only [`Self::note_char`] checks for completion.
    pub fn note_backspace(&mut self, session: u64) {
        self.rekey(session);
        self.buf.pop();
    }

    /// A word-breaking key (Enter/Tab/Escape/Delete/nav/kill chords, raw byte
    /// sequences) clears the window: the run must be typed as one word.
    pub fn note_break(&mut self) {
        self.buf.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lex() -> Lexicon {
        Lexicon::builtin().clone()
    }

    /// The incremental detector (token-gated, trailing-token scan) matches a
    /// whole-window rescan keystroke for keystroke: same completions, classes
    /// and clears. The corpus covers joiners and possessives (`cat's`,
    /// `cat-like`), code-adjacent context (`./cat`, `--cat`, `x.cat`),
    /// `scat`/`concatenate`, tokens longer than any surface, CJK against
    /// Latin, punctuation and spaces, and a long run that trims the window.
    #[test]
    fn incremental_scan_matches_a_whole_window_rescan_keystroke_for_keystroke() {
        let lexicon = lex();
        let opts = ScanOptions {
            allow_bare_cat: true,
            ..ScanOptions::default()
        };
        let corpus: &[&str] = &[
            "the cat sat on the mat and the kitten purred loudly",
            "a scat concatenate cats' kitty's cat-like meow-cat kitty",
            "./cat --cat x.cat a.cat. cat.b (cat) [kitty] {kitten} cat!",
            "xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxcat kitty",
            "猫 kitty 子猫 ねこ cat ねこねこ kitty猫 cat",
            "kot koty kotek chat chaton gato gatito katze kätzchen",
            "damn that cat, what the hell kitty?!  puppy dog doggo",
            "cat's cat-like cat-cat cat'-cat cat''s ca't c-a-t catcat",
        ];
        // Reference: a whole-window scan with the same hit/clear rule.
        struct Reference {
            buf: String,
            chars: Vec<char>,
            matches: Vec<Match>,
            scratch: ScanScratch,
        }
        impl Reference {
            fn note_char(
                &mut self,
                ch: char,
                lexicon: &Lexicon,
                opts: &ScanOptions,
            ) -> Option<Class> {
                self.buf.push(ch);
                while self.buf.chars().count() > BUF_CAP {
                    let mut it = self.buf.chars();
                    it.next();
                    self.buf = it.as_str().to_string();
                }
                lexicon.scan_into_with_scratch(
                    &self.buf,
                    opts,
                    &mut self.chars,
                    &mut self.matches,
                    &mut self.scratch,
                );
                let end = self.chars.len();
                let hit = self.matches.iter().filter(|m| m.end == end).fold(
                    None::<Class>,
                    |acc, m| match (acc, m.class) {
                        (Some(Class::Profanity), _) | (_, Class::Profanity) => {
                            Some(Class::Profanity)
                        }
                        (_, class) => Some(class),
                    },
                );
                if hit.is_some() {
                    self.buf.clear();
                }
                hit
            }
        }
        let now = Instant::now();
        for (line_no, line) in corpus.iter().enumerate() {
            let mut det = TypedKittySummon::default();
            let mut reference = Reference {
                buf: String::new(),
                chars: Vec::new(),
                matches: Vec::new(),
                scratch: ScanScratch::default(),
            };
            let mut fired = 0;
            for (k, ch) in line.chars().enumerate() {
                let expect = reference.note_char(ch, &lexicon, &opts);
                let got = det.note_char(now, 7, ch, &lexicon, &opts);
                // `TypedHit` surfaces three classes, and the canine reaction is
                // further gated on `keys_typed`, so compare completions via the
                // fact both sides expose: a completion clears the window.
                let got_completed = det.buf.is_empty() && !reference.buf.is_empty() || {
                    // both cleared, or neither
                    det.buf.is_empty() == reference.buf.is_empty() && expect.is_some()
                };
                assert_eq!(
                    det.buf, reference.buf,
                    "line {line_no} key {k} ({ch:?}): window state diverged (a missed or spurious completion)"
                );
                let got_class = if got.profanity {
                    Some(Class::Profanity)
                } else if got.feline {
                    Some(Class::Feline)
                } else {
                    None
                };
                let expect_class = match expect {
                    Some(Class::Profanity) => Some(Class::Profanity),
                    Some(Class::Feline) => Some(Class::Feline),
                    _ => None,
                };
                assert_eq!(
                    got_class, expect_class,
                    "line {line_no} key {k} ({ch:?}): reaction diverged from the whole-window reference"
                );
                let _ = got_completed;
                fired += usize::from(expect.is_some());
            }
            assert!(fired > 0, "line {line_no} exercised no completion");
        }
    }

    /// Feed a string one char at a time; count the ledger-recorded summons.
    fn feed(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).records)
            .count()
    }

    /// Feed a string one char at a time; count the completions (either tier).
    fn feed_completions(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).feline)
            .count()
    }

    /// Feed a string one char at a time; count profanity completions.
    fn feed_curses(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).profanity)
            .count()
    }

    /// The typed sequence fires exactly once, at the completing letter, and the
    /// consumed completion's tail re-triggers nothing (a held `y` is not a new
    /// word).
    #[test]
    fn typed_word_summons_once_at_completion() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        let lx = lex();
        let opts = ScanOptions::default();
        for c in ['k', 'i', 't', 't'] {
            assert!(
                !d.note_char(t, 7, c, &lx, &opts).feline,
                "no summon before the word completes"
            );
        }
        assert!(
            d.note_char(t, 7, 'y', &lx, &opts).feline,
            "the completing letter summons"
        );
        assert!(
            !d.note_char(t, 7, 'y', &lx, &opts).feline,
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

    /// Every listed language's feline word summons through the same detector,
    /// with no per-language code. Words from languages not in
    /// `Lexicon::builtin`'s default subset (`chat`/`neko`/`katze`) are absent
    /// from the build, so this fixture stays inside it.
    #[test]
    fn feline_words_summon_in_every_listed_language() {
        let t = Instant::now();
        for word in ["kitty", "gato", "gatto", "kucing"] {
            let mut d = TypedKittySummon::default();
            assert_eq!(
                feed_completions(&mut d, t, 7, word),
                1,
                "typing {word:?} completes the feline word"
            );
        }
    }

    /// Word boundaries apply to the feline family: `hellokitty` does not
    /// summon (see `innocent_words_containing_a_curse_never_fire`).
    #[test]
    fn a_feline_word_glued_to_another_is_not_the_word() {
        let t = Instant::now();
        let mut glued = TypedKittySummon::default();
        assert_eq!(
            feed_completions(&mut glued, t, 7, "hellokitty"),
            0,
            "`kitty` welded onto a preceding token was not typed as a word"
        );
        let mut spaced = TypedKittySummon::default();
        assert_eq!(
            feed_completions(&mut spaced, t, 7, "hello kitty"),
            1,
            "…and the ordinary spaced form still summons"
        );
    }

    /// The profanity family reacts in every listed language too, and carries
    /// no ledger tier.
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

    /// Word-boundary matching means an ordinary word that merely contains a
    /// curse (`pick`) never fires.
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

    /// Ledger tier rate limit: a completion inside the cooldown records nothing
    /// and does not restamp the clock, so the next window opens
    /// [`TYPED_SUMMON_COOLDOWN`] after the last record.
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

    /// The cooldown bounds the ledger, never the completion: every typed word
    /// completes, however fast they come.
    #[test]
    fn the_cooldown_never_withholds_a_completion() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(
            feed_completions(&mut d, t, 7, "kitty kitty kitty"),
            3,
            "every typed word completes"
        );
        let mut ledger = TypedKittySummon::default();
        assert_eq!(
            feed(&mut ledger, t, 7, "kitty kitty kitty"),
            1,
            "the ledger tier still rate-limits to one record"
        );
    }

    /// `records` never stands alone: a recorded row is always a completion,
    /// which the ledger drain relies on. Exercised over the real cooldown so
    /// both tiers come from the detector.
    #[test]
    fn a_recorded_summon_is_always_a_completion() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        let lx = lex();
        let opts = ScanOptions::default();
        let mut granted = 0;
        let mut completions = 0;
        for c in "kitty kitty".chars() {
            let hit = d.note_char(t, 7, c, &lx, &opts);
            assert!(
                hit.feline || !hit.records,
                "a row was recorded for a keystroke that completed nothing"
            );
            completions += usize::from(hit.feline);
            granted += usize::from(hit.records);
        }
        assert_eq!(
            (completions, granted),
            (2, 1),
            "PRECONDITION: this fixture really produces BOTH tiers — two \
             completions, one of them inside the cooldown"
        );
    }

    /// `merge` keeps the strongest of each independent axis, so one IME commit
    /// carrying several families owes every reaction.
    #[test]
    fn merge_keeps_every_axis() {
        let feline = TypedHit {
            feline: true,
            records: true,
            ..TypedHit::default()
        };
        let curse = TypedHit {
            profanity: true,
            ..TypedHit::default()
        };
        let dog = TypedHit {
            canine: true,
            ..TypedHit::default()
        };
        let all = feline.merge(curse).merge(dog);
        assert!(all.feline && all.records && all.profanity && all.canine);
    }

    /// Feed a string one char at a time; count canine hits (gate-passed dogs).
    fn feed_dogs(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).canine)
            .count()
    }

    /// Type `n` keystrokes of neutral filler (letters + spaces, never a
    /// lexicon word) to walk the odometer forward.
    fn type_filler(d: &mut TypedKittySummon, now: Instant, session: u64, n: u64) {
        let lx = lex();
        let opts = ScanOptions::default();
        let filler: Vec<char> = "qwzx ".chars().collect();
        for i in 0..n {
            let c = filler[(i % filler.len() as u64) as usize];
            assert_eq!(
                d.note_char(now, session, c, &lx, &opts),
                TypedHit::default(),
                "filler must never fire anything"
            );
        }
    }

    /// `dog` and `puppy` summon nothing until the window has typed
    /// [`DOG_SUMMON_KEYS`] keystrokes, then summon every time, with no ledger
    /// tier and no companion reaction flags.
    #[test]
    fn dogs_arrive_only_after_a_lot_of_typing() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        assert_eq!(
            feed_dogs(&mut d, t, 7, "dog "),
            0,
            "no dog fresh out of the box"
        );
        assert_eq!(feed_dogs(&mut d, t, 7, "puppy "), 0, "no puppy either");
        // "dog puppy " above already advanced the odometer by 11; filler walks
        // it to exactly one keystroke short of the gate.
        type_filler(&mut d, t, 7, DOG_SUMMON_KEYS - 11 - 4);
        assert_eq!(feed_dogs(&mut d, t, 7, "dog "), 0, "one short of the gate");
        assert_eq!(feed_dogs(&mut d, t, 7, "dog "), 1, "the gate opened — dog!");
        assert_eq!(
            feed_dogs(&mut d, t, 7, "puppy puppy "),
            2,
            "every completion summons once earned (no cooldown tier for dogs)"
        );
        let lx = lex();
        let opts = ScanOptions::default();
        let mut earned = TypedKittySummon::default();
        type_filler(&mut earned, t, 7, DOG_SUMMON_KEYS);
        for c in "dog".chars() {
            let hit = earned.note_char(t, 7, c, &lx, &opts);
            assert!(!hit.feline, "a dog is not a feline completion");
            assert!(!hit.feline && !hit.profanity, "a dog owes no cat reaction");
        }
    }

    /// The odometer is a lifetime count: session switches, breaks and
    /// backspaces clear the word window but never un-type the keystrokes.
    #[test]
    fn the_gate_odometer_survives_window_resets() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        type_filler(&mut d, t, 1, DOG_SUMMON_KEYS / 2);
        d.note_break();
        for _ in 0..8 {
            d.note_backspace(1);
        }
        type_filler(&mut d, t, 2, DOG_SUMMON_KEYS.div_ceil(2));
        assert_eq!(
            feed_dogs(&mut d, t, 2, " dog "),
            1,
            "typing across sessions/breaks still adds up to a lot"
        );
    }

    /// Multilingual like everything else here: once earned, the listed
    /// languages' dog words summon through the same detector.
    #[test]
    fn earned_dogs_answer_in_every_listed_language() {
        let t = Instant::now();
        for word in ["dog", "puppy", "perro", "cachorro", "hund", "inu"] {
            let mut d = TypedKittySummon::default();
            type_filler(&mut d, t, 7, DOG_SUMMON_KEYS);
            assert_eq!(
                feed_dogs(&mut d, t, 7, &format!("{word} ")),
                1,
                "typing {word:?} summons the dog once earned"
            );
        }
    }

    /// Fixing a typo mid-word keeps the run (`kitx⌫ty` summons), and a pop
    /// alone never summons: deleting to (or past) empty then typing `y`
    /// completes nothing.
    #[test]
    fn backspace_pops_typos_and_never_summons() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        let lx = lex();
        let opts = ScanOptions::default();
        assert_eq!(feed(&mut d, t, 7, "kitx"), 0);
        d.note_backspace(7);
        assert!(!d.note_char(t, 7, 't', &lx, &opts).feline);
        assert!(
            d.note_char(t, 7, 'y', &lx, &opts).feline,
            "the corrected word still summons"
        );

        let mut fresh = TypedKittySummon::default();
        assert_eq!(feed(&mut fresh, t, 7, "kitt"), 0);
        for _ in 0..BUF_CAP {
            fresh.note_backspace(7); // over-popping an emptied window is a no-op
        }
        assert!(
            !fresh.note_char(t, 7, 'y', &lx, &opts).feline,
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
        // delimiter, as it would when actually typed.
        assert_eq!(feed(&mut d, t, 7, " kitty"), 1);
    }

    /// Letters typed into different sessions never assemble one word: the
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

    /// The public constants match the documented contract: a ~30 s cooldown, a
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
        // The window must hold the longest surface the compiled lexicon can
        // match, or that word could never complete inside it.
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
