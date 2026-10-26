// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Typed-word detector for companion lexicon reactions.

use aterm_lexicon::{Class, Lexicon, Match, ScanOptions, ScanScratch};
use aterm_time::Instant;
use std::time::Duration;

/// Minimum spacing between Kitty Log rows from typed summons, per window.
pub const TYPED_SUMMON_COOLDOWN: Duration = Duration::from_secs(30);

/// Printed keystrokes this window's detector must see before a canine word summons the dog.
pub const DOG_SUMMON_KEYS: u64 = 500;

/// Everything one keystroke (or one IME commit char) produced: four independent flags.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TypedHit {
    /// A feline word completed.
    pub feline: bool,
    /// A feline word completed and the Kitty Log's cooldown granted the row.
    pub records: bool,
    /// A profanity word completed: the companion's wince is owed.
    pub profanity: bool,
    /// A canine word completed past the [`DOG_SUMMON_KEYS`] gate: the dog cameo is owed.
    pub canine: bool,
}

impl Default for TypedHit {
    #[inline]
    fn default() -> Self {
        Self::NONE
    }
}

impl TypedHit {
    /// No lexicon hit on any axis.
    pub const NONE: Self = Self {
        feline: false,
        records: false,
        profanity: false,
        canine: false,
    };

    /// Fold a second outcome in, keeping the strongest of each independent axis.
    #[inline]
    #[must_use]
    pub const fn merge(self, other: Self) -> Self {
        Self {
            feline: self.feline || other.feline,
            records: self.records || other.records,
            profanity: self.profanity || other.profanity,
            canine: self.canine || other.canine,
        }
    }
}

const BUF_CAP: usize = 48;

/// Ident-namespace tag for typed-summon sightings (`b"typedKit"` as big-endian ASCII).
pub const TYPED_SUMMON_IDENT_TAG: u64 = 0x7479_7065_644B_6974;

/// Ident-namespace tag for favourite sightings (`b"favKitty"` as big-endian ASCII).
pub const FAVOURITE_IDENT_TAG: u64 = 0x6661_764B_6974_7479;

/// Ident-namespace tag for typed dog summons (`b"typedDog"` as big-endian ASCII).
pub const DOG_SUMMON_IDENT_TAG: u64 = 0x7479_7065_6444_6f67;

/// Per-window detector state holding the rolling keystroke buffer, cooldown, and scan scratch.
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

impl Default for TypedKittySummon {
    #[inline]
    fn default() -> Self {
        Self::new()
    }
}

impl TypedKittySummon {
    /// Construct a fresh per-window detector with empty rolling buffers.
    #[inline]
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: String::new(),
            session: None,
            last_summon: None,
            chars: Vec::new(),
            matches: Vec::new(),
            scratch: ScanScratch::default(),
            keys_typed: 0,
        }
    }

    /// Bind the window to `session`, clearing it on a switch.
    #[inline]
    fn rekey(&mut self, session: u64) {
        if self.session != Some(session) {
            self.buf.clear();
            self.session = Some(session);
        }
    }

    /// Drop leading chars until the window holds at most [`BUF_CAP`]. Trimming
    /// on a char boundary keeps the buffer valid UTF-8.
    #[inline]
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
        if !(aterm_lexicon::is_token_char(ch) || aterm_lexicon::is_no_space_script(ch)) {
            return TypedHit::NONE;
        }
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
        let mut hit = None;
        let matches = self.matches.as_slice();
        let mut i = 0;
        while i < matches.len() {
            let m = &matches[i];
            if m.end == end {
                if m.class == Class::Profanity {
                    hit = Some(Class::Profanity);
                    break;
                }
                hit = Some(m.class);
            }
            i += 1;
        }
        let Some(class) = hit else {
            return TypedHit::NONE;
        };
        self.buf.clear();
        match class {
            Class::Profanity => TypedHit {
                profanity: true,
                ..TypedHit::NONE
            },
            Class::Feline => {
                let records = !matches!(
                    self.last_summon,
                    Some(at) if now.saturating_duration_since(at) < TYPED_SUMMON_COOLDOWN
                );
                if records {
                    self.last_summon = Some(now);
                }
                TypedHit {
                    feline: true,
                    records,
                    ..TypedHit::NONE
                }
            }
            Class::Canine => TypedHit {
                canine: self.keys_typed >= DOG_SUMMON_KEYS,
                ..TypedHit::NONE
            },
            _ => TypedHit::NONE,
        }
    }

    fn trailing_scan_start(buf: &str, last: char) -> usize {
        if buf.is_ascii() {
            let bytes = buf.as_bytes();
            if bytes.is_empty() {
                return 0;
            }
            let is_tok = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
            let mut k = bytes.len() - 1;
            while k > 0 {
                let b = bytes[k - 1];
                if is_tok(b) {
                    k -= 1;
                } else if b == b'\'' || b == b'-' {
                    k -= 1;
                    if k > 0 && is_tok(bytes[k - 1]) {
                        k -= 1;
                    } else {
                        return k.saturating_sub(1);
                    }
                } else {
                    break;
                }
            }
            return k.saturating_sub(2);
        }
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
                    rev.next();
                    if let Some(&(_, c2)) = rev.peek()
                        && aterm_lexicon::is_token_char(c2)
                    {
                        rev.next();
                    } else {
                        return match rev.next() {
                            Some((idx, _)) => idx,
                            None => 0,
                        };
                    }
                } else {
                    break;
                }
            }
        }
        let _ = rev.next();
        match rev.next() {
            Some((idx, _)) => idx,
            None => 0,
        }
    }

    /// Pop the most recent char on a plain Backspace.
    #[inline]
    pub fn note_backspace(&mut self, session: u64) {
        self.rekey(session);
        self.buf.pop();
    }

    /// Clear the window on a word-breaking key.
    #[inline]
    pub fn note_break(&mut self) {
        self.buf.clear();
    }

    /// Clear the live typing window for Serious Mode.
    #[inline]
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
                let got_completed = det.buf.is_empty() && !reference.buf.is_empty() || {
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

    fn feed(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).records)
            .count()
    }

    fn feed_completions(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).feline)
            .count()
    }

    fn feed_curses(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).profanity)
            .count()
    }

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

    fn feed_dogs(d: &mut TypedKittySummon, now: Instant, session: u64, s: &str) -> usize {
        let lx = lex();
        let opts = ScanOptions::default();
        s.chars()
            .filter(|&c| d.note_char(now, session, c, &lx, &opts).canine)
            .count()
    }

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
        assert_eq!(feed(&mut d, t, 7, " kitty"), 1);
    }

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

    #[test]
    fn window_stays_bounded_and_utf8() {
        let mut d = TypedKittySummon::default();
        let t = Instant::now();
        let long = "é".repeat(BUF_CAP * 3);
        assert_eq!(feed(&mut d, t, 7, &long), 0);
        assert!(d.buf.chars().count() <= BUF_CAP, "the window stays bounded");
        assert_eq!(feed(&mut d, t, 7, " kitty"), 1, "and still completes");
    }

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
