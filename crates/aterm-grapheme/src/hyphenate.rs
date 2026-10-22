// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! WHERE A WORD MAY TAKE A HYPHEN: its syllable breaks, as a typesetter
//! finds them — Liang's algorithm (TeX's `\patterns`) over the American
//! English patterns and exception list TeX ships (`data/hyph-en-us.tex`,
//! hyph-utf8's `ushyphmax`, embedded as published), with TeX's English
//! minimums: two letters before a hyphen and three after it.
//!
//! The one answer every wrap of aterm's chrome reads when a word is wider
//! than its whole box: "Bright-" / "ness", "AUTO-" / "MATIC", "notifica-" /
//! "tions". Broken wherever the box ended, it read "Brightn-" / "ess",
//! "AUTOMA-" / "TIC" and "Messag-" / "es.". The same on every platform and
//! in a headless test: no platform hyphenator is asked.
//!
//! Only a word of prose takes one ([`crate::TokenKind::Word`]): a path, a
//! version, a hash or a name written in mixed case (`macOS`) is an
//! identifier, every character of which is its own, and a hyphen put in it
//! would read as one of them.

use std::collections::HashMap;
use std::sync::OnceLock;

/// TeX's American English patterns and exceptions, as hyph-utf8 publishes
/// them (its notice kept in the file).
const PATTERNS: &str = include_str!("../data/hyph-en-us.tex");

/// The fewest letters before a hyphen (TeX's `\lefthyphenmin` for English).
const LEFT_MIN: usize = 2;
/// The fewest letters after a hyphen (TeX's `\righthyphenmin` for English).
const RIGHT_MIN: usize = 3;

/// The patterns, parsed once: each pattern's letters to the values between
/// them, and each exception's letters to its hyphen positions.
struct Patterns {
    values: HashMap<String, Vec<u8>>,
    /// The most letters a pattern has (the dots that anchor one included).
    longest: usize,
    exceptions: HashMap<String, Vec<usize>>,
}

/// The body of `\name{ … }` in the pattern file, comments dropped.
fn block<'a>(source: &'a str, name: &str) -> impl Iterator<Item = &'a str> {
    let body = source
        .find(name)
        .map(|at| &source[at + name.len()..])
        .and_then(|rest| rest.split_once('}'))
        .map_or("", |(body, _)| body);
    body.lines()
        .map(|line| line.split('%').next().unwrap_or_default())
        .flat_map(str::split_whitespace)
}

fn patterns() -> &'static Patterns {
    static PARSED: OnceLock<Patterns> = OnceLock::new();
    PARSED.get_or_init(|| {
        let mut values = HashMap::new();
        let mut longest = 0;
        for pattern in block(PATTERNS, "\\patterns{") {
            let mut letters = String::new();
            let mut between = vec![0_u8];
            for c in pattern.chars() {
                match c.to_digit(10) {
                    Some(digit) => {
                        if let Some(last) = between.last_mut() {
                            *last = u8::try_from(digit).unwrap_or(0);
                        }
                    }
                    None => {
                        letters.push(c);
                        between.push(0);
                    }
                }
            }
            longest = longest.max(letters.len());
            values.insert(letters, between);
        }
        let mut exceptions = HashMap::new();
        for word in block(PATTERNS, "\\hyphenation{") {
            let mut letters = String::new();
            let mut points = Vec::new();
            for c in word.chars() {
                if c == '-' {
                    points.push(letters.len());
                } else {
                    letters.push(c);
                }
            }
            exceptions.insert(letters, points);
        }
        Patterns {
            values,
            longest,
            exceptions,
        }
    })
}

/// WHERE `letters` — one run of ASCII letters, any case — MAY TAKE A
/// HYPHEN: the byte offsets (each a count of letters before it) of its
/// syllable breaks, in order, at least [`LEFT_MIN`] letters from its start
/// and [`RIGHT_MIN`] from its end. Liang's algorithm: every pattern found in
/// the word, dot-anchored at its ends, sets the values between its letters,
/// the greatest value at a place winning, and an odd value is a break; a
/// word on the exception list takes its listed breaks instead.
fn letter_run_points(letters: &str) -> Vec<usize> {
    let count = letters.len();
    if count < LEFT_MIN + RIGHT_MIN || !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Vec::new();
    }
    let lower = letters.to_ascii_lowercase();
    let allowed = |at: &usize| (LEFT_MIN..=count - RIGHT_MIN).contains(at);
    let table = patterns();
    if let Some(points) = table.exceptions.get(&lower) {
        return points.iter().copied().filter(allowed).collect();
    }
    let dotted = format!(".{lower}.");
    // The value before each byte of `dotted`, and one after its end.
    let mut values = vec![0_u8; dotted.len() + 1];
    for start in 0..dotted.len() {
        for end in start + 1..=dotted.len().min(start + table.longest) {
            if let Some(found) = table.values.get(&dotted[start..end]) {
                for (offset, value) in found.iter().enumerate() {
                    let slot = &mut values[start + offset];
                    *slot = (*slot).max(*value);
                }
            }
        }
    }
    // A break after `at` letters of the word is before byte `at + 1` of the
    // dotted word.
    (1..count)
        .filter(|at| values[at + 1] % 2 == 1)
        .filter(allowed)
        .collect()
}

/// WHERE A WORD OF PROSE MAY TAKE A HYPHEN: the byte offsets in `word` of
/// its syllable breaks, in order ([`syllable_points`]), so "built-in" and
/// "Couldn't" break only inside their letters, and punctuation around a
/// word is no letter of it: "Messages." breaks as "Mes-" / "sages.". None
/// for an identifier ([`crate::token_kind`]): a path, a version, a hash, a
/// name in mixed case, and none for a run of letters TeX's English patterns
/// do not cover (an accented or another script's letter in it).
#[must_use]
pub fn hyphen_points(word: &str) -> Vec<usize> {
    if crate::token_kind(word) != crate::TokenKind::Word {
        return Vec::new();
    }
    syllable_points(word)
}

/// The syllable breaks of every run of letters in `text`
/// ([`letter_run_points`]), as byte offsets in `text`, in order, whatever
/// the text around them: an identifier no line holds and that has no joint
/// left to break at breaks at one of these with no mark rather than
/// anywhere (`/Applica` / `tions`, not `/Applicati` / `ons`).
pub(crate) fn syllable_points(text: &str) -> Vec<usize> {
    let mut points = Vec::new();
    let mut run_start: Option<usize> = None;
    let push_run = |from: usize, to: usize, points: &mut Vec<usize>| {
        points.extend(
            letter_run_points(&text[from..to])
                .into_iter()
                .map(|at| from + at),
        );
    };
    for (at, c) in text.char_indices() {
        match (c.is_alphabetic(), run_start) {
            (true, None) => run_start = Some(at),
            (false, Some(from)) => {
                push_run(from, at, &mut points);
                run_start = None;
            }
            _ => {}
        }
    }
    if let Some(from) = run_start {
        push_run(from, text.len(), &mut points);
    }
    points
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `word` with a hyphen at each of its syllable breaks.
    fn hyphenated(word: &str) -> String {
        let mut out = String::new();
        let mut from = 0;
        for at in hyphen_points(word) {
            out.push_str(&word[from..at]);
            out.push('-');
            from = at;
        }
        out.push_str(&word[from..]);
        out
    }

    /// THE PATTERNS FIND A TYPESETTER'S BREAKS, two letters from a word's
    /// start and three from its end, in any case, the exception list
    /// winning; and a word too short for both minimums takes none.
    #[test]
    fn words_break_at_their_syllables() {
        for (word, broken) in [
            ("Brightness", "Bright-ness"),
            ("AUTOMATIC", "AU-TO-MATIC"),
            ("notifications", "no-ti-fi-ca-tions"),
            ("Messages.", "Mes-sages."),
            ("hyphenation", "hy-phen-ation"),
            ("table", "ta-ble"),
            ("associate", "as-so-ciate"),
            ("project", "project"),
            ("Choose", "Choose"),
            ("Theme", "Theme"),
            ("the", "the"),
            ("(default)", "(de-fault)"),
            ("built-in", "built-in"),
        ] {
            assert_eq!(hyphenated(word), broken, "{word:?}");
        }
    }

    /// AN IDENTIFIER TAKES NO HYPHEN: a path, a host, a version, a hash, a
    /// file's name, a flag and a name written in mixed case keep every
    /// character their own.
    #[test]
    fn identifiers_take_no_hyphen() {
        for identifier in [
            "alab.systems",
            "aterm.toml",
            "0.11.0+g20565c325",
            "ec73ab1c007d90ca",
            "/Applications/aterm.app",
            "~/.config/aterm",
            "--columns",
            "macOS",
            "MacBook-Pro-M4",
            "trust_verify",
        ] {
            assert!(
                hyphen_points(identifier).is_empty(),
                "{identifier:?}: {:?}",
                hyphen_points(identifier)
            );
        }
    }
}
