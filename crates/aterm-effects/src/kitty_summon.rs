// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed-word detector: the companion reacts to lexicon words typed at the prompt, in every
//! language [`aterm_lexicon`] knows.
//!
//! * [`Class::Feline`] (`kitty`, `gato`, `猫`, …) records a Kitty Log sighting, at most one
//!   per [`TYPED_SUMMON_COOLDOWN`]. It draws nothing itself: the echoed word gets the screen
//!   scanner's ambient word-cat.
//! * [`Class::Profanity`] makes the companion wince, as `word_decorations`'s `CurseCue` does
//!   for a curse on screen; this also covers a curse typed into a no-echo prompt.
//! * [`Class::Canine`] (`dog`, `perro`, `子犬`, …) summons the dog cameo
//!   ([`crate::dog_cameo`]), only after [`DOG_SUMMON_KEYS`] printed keystrokes.
//!
//! Matching is the lexicon's own (word boundaries, `ambiguous` gating, morphology), never a
//! substring test: substrings of a multilingual vocabulary fire on innocent words (`pick`).
//!
//! The detector is fed only from the host's committed key presses, never screen content: PTY
//! output, paste and a program printing the word cannot fire it. It does not branch on
//! `Source`, so a controller typing `kitty` fires like a human.

use aterm_lexicon::{Class, Lexicon, Match, ScanOptions, ScanScratch};
use aterm_time::Instant;
use std::time::Duration;

/// Minimum spacing between Kitty Log rows from typed summons, per window. 30 s matches the Kitty
/// Log's flush debounce, so spam cannot force writer batches. The completion itself (and its
/// ambient cat) is never withheld.
pub const TYPED_SUMMON_COOLDOWN: Duration = Duration::from_secs(30);

/// Printed keystrokes this window's detector must see before a canine word summons the dog.
/// Counted on the committed-key path only, over the window's lifetime; session switches, breaks
/// and backspaces never reset it.
pub const DOG_SUMMON_KEYS: u64 = 500;

/// Everything one keystroke (or one IME commit char) produced: four independent flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TypedHit {
    /// A feline word completed. Nothing on the input path draws for it; the echoed word's
    /// ambient cat is the answer.
    pub feline: bool,
    /// A feline word completed and the Kitty Log's cooldown granted the row.
    /// Never set without [`Self::feline`].
    pub records: bool,
    /// A profanity word completed: the companion's wince is owed.
    pub profanity: bool,
    /// A canine word completed past the [`DOG_SUMMON_KEYS`] gate: the dog cameo is owed. Below
    /// the gate the completion is consumed silently.
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

/// Rolling-window capacity in chars: must exceed the longest lexicon surface, plus backspace
/// history. Small because the typing path scans it on every key.
const BUF_CAP: usize = 48;

/// Ident-namespace tag for typed-summon sightings (`b"typedKit"` as big-endian ASCII). The host
/// XORs it with its summon sequence (`App::kitty_summon_seq`) so each recorded summon is a fresh
/// `(session, ident)` episode for the Kitty Log's dedupe ring, apart from the word renderer's
/// occurrence idents.
pub const TYPED_SUMMON_IDENT_TAG: u64 = 0x7479_7065_644B_6974;

/// Ident-namespace tag for favourite sightings (`b"favKitty"` as big-endian ASCII), sharing the
/// summon sequence. The favourite path bypasses the dedupe ring; the tag keeps ledger episode
/// identities collision-free.
pub const FAVOURITE_IDENT_TAG: u64 = 0x6661_764B_6974_7479;

/// Ident-namespace tag for typed dog summons (`b"typedDog"` as big-endian ASCII), XOR'd with
/// the same summon sequence. Never recorded; it seeds the dog cameo's breed/coat roll.
pub const DOG_SUMMON_IDENT_TAG: u64 = 0x7479_7065_6444_6f67;

/// Per-window detector state: the rolling keystroke window (keyed to the
/// session it was typed into), the summon cooldown stamp, and the resident
/// scan buffers.
#[derive(Default)]
pub struct TypedKittySummon {
    /// Rolling window of recent printed keystrokes (≤ [`BUF_CAP`] chars), stored raw: the
    /// lexicon owns folding, and pre-folding would defeat its morphology handling.
    buf: String,
    /// The session the window's letters were typed into. A switch clears the
    /// window; letters typed into different sessions never form one word.
    session: Option<u64>,
    /// Instant of the last recorded summon. Not reset on session switch, so flipping tabs
    /// cannot defeat the per-window cooldown.
    last_summon: Option<Instant>,
    /// Resident scan buffers: after warmup the per-keystroke scan allocates
    /// nothing.
    chars: Vec<char>,
    matches: Vec<Match>,
    scratch: ScanScratch,
    /// Lifetime count of printed keystrokes fed to this detector: the [`DOG_SUMMON_KEYS`]
    /// odometer. Not persisted.
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
        if self.buf.len() <= BUF_CAP {
            return;
        }
        if self.buf.is_ascii() {
            let split = self.buf.len() - BUF_CAP;
            self.buf.drain(..split);
            return;
        }
        if let Some((split, _)) = self.buf.char_indices().rev().nth(BUF_CAP - 1)
            && split > 0
        {
            self.buf.drain(..split);
        }
    }

    /// Feed one committed printed keystroke; reports what the lexicon word it completed owes.
    ///
    /// A completion consumes its letters, so its tail never re-fires. For a feline word the
    /// cooldown decides only whether the Kitty Log records it, and a suppressed record does not
    /// restamp the clock.
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
        let start = Self::trailing_scan_start(&self.buf, ch);
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
    /// context. Walks `buf.char_indices().rev()` directly in O(token_len) with
    /// zero scratch storage.
    ///
    /// Token segmentation mirrors the scanner's `token_end`, backwards: a
    /// maximal run of token chars where an interior joiner (`'`, `’`, `-`)
    /// counts only with token chars on both sides.
    fn trailing_scan_start(buf: &str, last: char) -> usize {
        let mut rev = buf.char_indices().rev().peekable();
        if rev.next().is_none() {
            return 0;
        }
        if aterm_lexicon::is_no_space_script(last) {
            while let Some(&(_, c)) = rev.peek()
                && aterm_lexicon::is_no_space_script(c)
            {
                rev.next();
            }
        } else {
            while let Some(&(_, c)) = rev.peek() {
                if aterm_lexicon::is_token_char(c) {
                    rev.next();
                } else if aterm_lexicon::is_interior_joiner(c) {
                    // Consume the joiner; if the char before it is a token
                    // char, the joiner is interior (consume that token char
                    // too and keep walking). Otherwise the joiner was already
                    // the first of the two left-context chars.
                    rev.next();
                    if let Some(&(_, c2)) = rev.peek()
                        && aterm_lexicon::is_token_char(c2)
                    {
                        rev.next();
                    } else {
                        return rev.next().map_or(0, |(idx, _)| idx);
                    }
                } else {
                    break;
                }
            }
        }
        // Two chars of left context for `left_suppresses` (`c == '.'` looks one
        // further back than the adjacent char).
        let _ = rev.next();
        rev.next().map_or(0, |(idx, _)| idx)
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

    /// Serious Mode's drain: clear the live typing window without dropping the
    /// warmed-up scan buffers, the Kitty Log cooldown (`last_summon`), or the
    /// canine keystroke odometer (`keys_typed`). Idempotent.
    pub fn drain_serious(&mut self) {
        self.buf.clear();
        self.session = None;
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

    /// The window must hold the longest surface the compiled lexicon can
    /// match, or that word could never complete inside it.
    #[test]
    fn the_window_holds_the_longest_lexicon_surface() {
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
