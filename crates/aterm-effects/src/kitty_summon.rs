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
        let bytes = self.buf.as_bytes();
        let mut idx = bytes.len();
        let mut chars = 0usize;
        while idx > 0 && chars < BUF_CAP {
            idx -= 1;
            if (bytes[idx] & 0xC0) != 0x80 {
                chars += 1;
            }
        }
        if idx > 0 {
            self.buf.drain(..idx);
        }
    }

    /// Feed one committed printed keystroke; reports what the lexicon word it completed owes.
    #[inline]
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
        if ch.is_ascii() {
            if !(ch.is_ascii_alphanumeric() || ch == '_') {
                return TypedHit::NONE;
            }
        } else if !(aterm_lexicon::is_token_char(ch) || aterm_lexicon::is_no_space_script(ch)) {
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

    #[inline]
    fn trailing_scan_start(buf: &str, last: char) -> usize {
        #[inline]
        const fn is_ascii_tok(b: u8) -> bool {
            b.is_ascii_alphanumeric() || b == b'_'
        }

        if buf.is_ascii() {
            let bytes = buf.as_bytes();
            if bytes.is_empty() {
                return 0;
            }
            let mut k = bytes.len() - 1;
            while k > 0 {
                let b = bytes[k - 1];
                if is_ascii_tok(b) {
                    k -= 1;
                } else if b == b'\'' || b == b'-' {
                    k -= 1;
                    if k > 0 && is_ascii_tok(bytes[k - 1]) {
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
#[path = "kitty_summon_tests.rs"]
mod tests;
