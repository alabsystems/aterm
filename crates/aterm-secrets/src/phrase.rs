// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The join phrase (`docs/DESIGN-fluid-swarm-2026-10-28.md` §3.3.4, "The phrase itself"): three
//! words from a compiled list of 2048, 33 bits. A member shows it on its own screen and a person
//! types it on the joining Mac, where [`crate::pake`] runs CPace over it. Typing is case-blind,
//! any run of characters that are not ASCII letters separates two words, and a word's first 4
//! letters are enough, because no two words share them.
//!
//! The phrase enters CPace as its `PRS` in the canonical form: the three full words, lower case,
//! one space apart ([`Phrase::canonical`]). Parsing, picking and laying out that form run in
//! constant time in the words: a lookup reads all 2048 words and keeps its match by a mask, and
//! the canonical bytes are placed by masks into a fixed buffer, so no branch, loop bound or index
//! depends on which words they are. Only [`Phrase::words`], what the member's sheet draws,
//! indexes the list by a word. A typed token's length is the typist's and is not hidden.

mod words;

use crate::{Error, Result};
use words::WORDS;
use zeroize::Zeroize;

/// How many words the list holds: 2^11, so each word carries 11 bits.
pub const WORD_COUNT: usize = 2048;

/// The fewest letters that name a word by its start.
pub const PREFIX_LETTERS: usize = 4;

/// The longest word.
const WORD_MAX: usize = 8;

/// The longest canonical phrase: three 8-letter words and two spaces.
pub(crate) const CANONICAL_MAX: usize = 3 * WORD_MAX + 2;

/// 0xff when `a == b`, else 0, with no branch. Both must be below 2^32. The mask leaves through
/// `black_box`, so the optimizer cannot see it is all ones or zero and branch on it instead.
#[inline]
pub(crate) fn mask_eq(a: usize, b: usize) -> u8 {
    let diff = std::hint::black_box((a ^ b) as u64);
    // `diff | -diff` has its top bit set exactly when `diff` is not 0.
    let nonzero = ((diff | diff.wrapping_neg()) >> 63) as u8;
    std::hint::black_box(nonzero.wrapping_sub(1))
}

/// 0xff when `a < b`, else 0, with no branch. Both must be below 2^62. The mask leaves through
/// `black_box`, as [`mask_eq`]'s does.
#[inline]
pub(crate) fn mask_lt(a: usize, b: usize) -> u8 {
    let diff = std::hint::black_box(a as i64 - b as i64);
    std::hint::black_box((diff >> 63) as u8)
}

/// A join phrase: three word indices. It never prints its words (`Debug` shows none) and is
/// wiped when dropped.
pub struct Phrase {
    index: [u16; 3],
}

impl std::fmt::Debug for Phrase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Phrase(..)")
    }
}

impl Drop for Phrase {
    fn drop(&mut self) {
        self.index.zeroize();
    }
}

impl Phrase {
    /// A new phrase of three words drawn uniformly and independently.
    ///
    /// # Errors
    /// When the system's random source fails.
    pub fn generate() -> Result<Self> {
        let mut bytes = [0u8; 8];
        aterm_uds::rand::fill(&mut bytes[..5])
            .map_err(|why| Error::new(format!("no randomness for a join phrase: {why}")))?;
        let mut bits = u64::from_le_bytes(bytes);
        let phrase = Self {
            // Each index is 11 of the 40 random bits, so each word is uniform over the list.
            index: [
                (bits & 0x7ff) as u16,
                ((bits >> 11) & 0x7ff) as u16,
                ((bits >> 22) & 0x7ff) as u16,
            ],
        };
        bytes.zeroize();
        bits.zeroize();
        Ok(phrase)
    }

    /// The phrase a person typed. Letters are read case-blind, any run of other characters
    /// separates words, and each word is either a whole listed word or at least its first 4
    /// letters.
    ///
    /// # Errors
    /// When the text is not three listed words. The error names no word, because a near miss of
    /// the phrase is itself a secret.
    pub fn parse(typed: &str) -> Result<Self> {
        let mut index = [0u16; 3];
        let mut count = 0usize;
        let mut found = true;
        for token in typed
            .split(|c: char| !c.is_ascii_alphabetic())
            .filter(|token| !token.is_empty())
        {
            if count < 3 {
                let mut lower = [0u8; WORD_MAX];
                let named = (token.len() <= WORD_MAX)
                    .then(|| {
                        for (slot, byte) in lower.iter_mut().zip(token.bytes()) {
                            *slot = byte.to_ascii_lowercase();
                        }
                        lookup(&lower[..token.len()])
                    })
                    .flatten();
                lower.zeroize();
                match named {
                    Some(word) => index[count] = word,
                    None => found = false,
                }
            }
            count += 1;
        }
        if count != 3 || !found {
            index.zeroize();
            return Err(Error::new(
                "a join phrase is three words from the list, as the member's screen shows them",
            ));
        }
        Ok(Self { index })
    }

    /// The three words, for the member's sheet to draw. This is the one place that indexes the
    /// list by a word; nothing else may print them.
    #[must_use]
    pub fn words(&self) -> [&'static str; 3] {
        self.index.map(|i| WORDS[usize::from(i)])
    }

    /// CPace's `PRS`: the three full words, lower case, one space apart, placed by masks into a
    /// fixed buffer.
    pub(crate) fn canonical(&self) -> Canonical {
        let mut letters = [[0u8; WORD_MAX]; 3];
        let mut lens = [0usize; 3];
        for (k, &index) in self.index.iter().enumerate() {
            for (i, word) in WORDS.iter().enumerate() {
                let take = mask_eq(i, usize::from(index));
                for (slot, byte) in letters[k].iter_mut().zip(word.bytes()) {
                    *slot |= take & byte;
                }
                lens[k] |= usize::from(take) & word.len();
            }
        }
        let starts = [0, lens[0] + 1, lens[0] + lens[1] + 2];
        let mut bytes = [0u8; CANONICAL_MAX];
        for (p, out) in bytes.iter_mut().enumerate() {
            for k in 0..3 {
                for (c, &letter) in letters[k].iter().enumerate() {
                    *out |= mask_eq(p, starts[k] + c) & mask_lt(c, lens[k]) & letter;
                }
                if k < 2 {
                    *out |= mask_eq(p, starts[k] + lens[k]) & b' ';
                }
            }
        }
        let len = lens[0] + lens[1] + lens[2] + 2;
        letters.zeroize();
        lens.zeroize();
        Canonical { bytes, len }
    }
}

/// The index of the one word `token` names, reading every word: `token` is the word, or has at
/// least [`PREFIX_LETTERS`] letters and starts it. Which word matched is kept by a mask. Only
/// the token's length, which the typist chose, steers a branch inside the scan, and only
/// whether the token names a word at all, which a refusal shows anyway, steers one after it.
fn lookup(token: &[u8]) -> Option<u16> {
    let mut found = 0u16;
    let mut hits = 0u16;
    for (i, word) in WORDS.iter().enumerate() {
        let word = word.as_bytes();
        let shape = token.len() <= word.len()
            && (token.len() == word.len() || token.len() >= PREFIX_LETTERS);
        let mut diff = 0u8;
        for (a, b) in token.iter().zip(word) {
            diff |= a ^ b;
        }
        // 1 when every compared byte matched, from bit 8 of `diff - 1`.
        let same = (u16::from(std::hint::black_box(diff)).wrapping_sub(1) >> 8) & 1;
        let hit = std::hint::black_box(u16::from(shape) & same);
        found |= hit.wrapping_neg() & (i as u16);
        hits += hit;
    }
    // At most one word starts with any 4 letters, so `hits` is 0 or 1.
    (hits == 1).then_some(found)
}

/// A canonical phrase in a fixed buffer, so its layout never copies by a secret length. `len`
/// is secret too: read it only through masks.
pub(crate) struct Canonical {
    pub(crate) bytes: [u8; CANONICAL_MAX],
    pub(crate) len: usize,
}

impl Drop for Canonical {
    fn drop(&mut self) {
        self.bytes.zeroize();
        self.len.zeroize();
    }
}

impl Canonical {
    /// The bytes, for tests that compare with a plain encoding.
    #[cfg(test)]
    pub(crate) fn as_slice(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn canonical_text(phrase: &Phrase) -> String {
        String::from_utf8(phrase.canonical().as_slice().to_vec()).expect("ASCII")
    }

    #[test]
    fn the_list_is_bip39_english_byte_for_byte() {
        let mut file = String::new();
        for word in WORDS {
            file.push_str(word);
            file.push('\n');
        }
        assert_eq!(
            aterm_codec::hex::encode_lower(&aterm_digest::Sha256::digest(file.as_bytes())),
            "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda"
        );
        assert_eq!(WORDS.len(), WORD_COUNT);
    }

    #[test]
    fn every_word_is_short_lower_case_and_alone_in_its_first_four_letters() {
        let mut starts = std::collections::BTreeSet::new();
        for word in WORDS {
            assert!((3..=WORD_MAX).contains(&word.len()), "{word}");
            assert!(word.bytes().all(|b| b.is_ascii_lowercase()), "{word}");
            let start = &word[..word.len().min(PREFIX_LETTERS)];
            assert!(starts.insert(start), "two words start {start}");
        }
        // A 3-letter word is also no other word's first 3 letters followed by nothing: "act" and
        // "action" differ in their fourth letter, which "act" lacks.
        assert_eq!(lookup(b"act"), Some(19));
        assert_eq!(lookup(b"acti"), Some(20));
    }

    #[test]
    fn typing_is_case_blind_any_separator_works_and_four_letters_are_enough() {
        let expected = "maple river ocean";
        for typed in [
            "maple river ocean",
            "MAPLE River oCeAn",
            "  maple-river.ocean\n",
            "mapl rive ocea",
            "maple,  river_ocean!",
            "maple\triver\u{a0}ocean",
            "Mapl/RIVE/ocea",
        ] {
            let phrase = Phrase::parse(typed).expect(typed);
            assert_eq!(canonical_text(&phrase), expected, "{typed:?}");
            assert_eq!(phrase.words(), ["maple", "river", "ocean"]);
        }
    }

    #[test]
    fn what_is_not_three_listed_words_is_refused_without_naming_a_word() {
        for typed in [
            "",
            "maple river",
            "maple river ocean maple",
            "map river ocean",
            "maplex river ocean",
            "maples river ocean",
            "maple river océan",
            "zzzz river ocean",
            "abandonedx river ocean",
        ] {
            let error = Phrase::parse(typed).expect_err(typed).to_string();
            for word in ["maple", "river", "ocean", "map", "zzzz"] {
                assert!(!error.contains(word), "{error}");
            }
        }
    }

    #[test]
    fn every_word_round_trips_through_the_canonical_form() {
        for (i, word) in WORDS.iter().enumerate() {
            let j = (i * 7 + 3) % WORD_COUNT;
            let k = (i * 13 + 1000) % WORD_COUNT;
            let phrase = Phrase::parse(&format!("{word} {} {}", WORDS[j], WORDS[k])).expect(word);
            assert_eq!(phrase.index, [i as u16, j as u16, k as u16]);
            assert_eq!(
                canonical_text(&phrase),
                format!("{word} {} {}", WORDS[j], WORDS[k])
            );
            let short: String = word.chars().take(PREFIX_LETTERS).collect();
            assert_eq!(
                Phrase::parse(&format!("{short} {} {}", WORDS[j], WORDS[k]))
                    .expect(word)
                    .index,
                phrase.index
            );
            // Every start from 4 letters up names the word.
            for n in PREFIX_LETTERS..word.len() {
                assert_eq!(
                    lookup(&word.as_bytes()[..n]),
                    Some(i as u16),
                    "{word} at {n}"
                );
            }
        }
    }

    #[test]
    fn a_generated_phrase_parses_back_from_its_words_and_never_prints_them() {
        for _ in 0..64 {
            let phrase = Phrase::generate().expect("CSPRNG");
            let typed = phrase.words().join(" ");
            assert_eq!(
                Phrase::parse(&typed).expect("generated").index,
                phrase.index
            );
            assert_eq!(canonical_text(&phrase), typed);
            assert_eq!(format!("{phrase:?}"), "Phrase(..)");
        }
    }

    #[test]
    fn a_generated_phrase_draws_each_word_from_its_own_bits() {
        // 33 random bits: in 128 phrases, each of the 33 index bits must be seen both set and
        // clear, and no two words may always match. A fair draw fails either with odds below
        // 2^-120.
        let phrases: Vec<Phrase> = (0..128)
            .map(|_| Phrase::generate().expect("CSPRNG"))
            .collect();
        for k in 0..3 {
            for bit in 0..11 {
                let set = phrases
                    .iter()
                    .filter(|p| p.index[k] >> bit & 1 == 1)
                    .count();
                assert!(
                    0 < set && set < phrases.len(),
                    "word {k} bit {bit} never varies"
                );
            }
        }
        for (a, b) in [(0, 1), (0, 2), (1, 2)] {
            assert!(
                phrases.iter().any(|p| p.index[a] != p.index[b]),
                "words {a} and {b} always match"
            );
        }
    }

    #[test]
    fn the_masks_answer_as_comparisons_do() {
        for a in 0..40 {
            for b in 0..40 {
                assert_eq!(mask_eq(a, b), if a == b { 0xff } else { 0 }, "{a} == {b}");
                assert_eq!(mask_lt(a, b), if a < b { 0xff } else { 0 }, "{a} < {b}");
            }
        }
        assert_eq!(mask_eq(0, 1 << 31), 0);
        assert_eq!(mask_lt(1 << 40, 3), 0);
    }
}
