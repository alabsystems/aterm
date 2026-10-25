// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! English syllable hyphenation points using Liang's TeX algorithm.

use std::collections::HashMap;
use std::sync::OnceLock;

/// TeX's American English patterns and exceptions, as hyph-utf8 publishes them.
const PATTERNS: &str = include_str!("../data/hyph-en-us.tex");

/// The fewest letters before a hyphen (TeX's `\lefthyphenmin` for English).
const LEFT_MIN: usize = 2;
/// The fewest letters after a hyphen (TeX's `\righthyphenmin` for English).
const RIGHT_MIN: usize = 3;
/// The fewest letters after a hyphen in a measure too narrow for any [`RIGHT_MIN`] break, as a
/// narrow newspaper column sets it ("record-" / "ed", never the word cut where no syllable
/// ends: "recor-" / "ded").
const NARROW_RIGHT_MIN: usize = 2;

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

/// Byte offsets of syllable breaks in `letters` (one run of ASCII letters, any case), at least
/// [`LEFT_MIN`] letters from its start and `right_min` from its end.
fn letter_run_points(letters: &str, right_min: usize) -> Vec<usize> {
    let count = letters.len();
    if count < LEFT_MIN + right_min || !letters.bytes().all(|b| b.is_ascii_alphabetic()) {
        return Vec::new();
    }
    let lower = letters.to_ascii_lowercase();
    let allowed = |at: &usize| (LEFT_MIN..=count - right_min).contains(at);
    let table = patterns();
    if let Some(points) = table.exceptions.get(&lower) {
        return points.iter().copied().filter(allowed).collect();
    }
    let dotted = format!(".{lower}.");
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
    (1..count)
        .filter(|at| values[at + 1] % 2 == 1)
        .filter(allowed)
        .collect()
}

/// Returns the byte offsets of syllable hyphenation points in a prose `word`.
#[must_use]
pub fn hyphen_points(word: &str) -> Vec<usize> {
    if crate::token_kind(word) != crate::TokenKind::Word {
        return Vec::new();
    }
    syllable_points(word)
}

/// The syllable hyphenation points of a prose `word` that a measure too narrow for every
/// [`hyphen_points`] break may still take: the same syllables, down to [`NARROW_RIGHT_MIN`]
/// letters after the hyphen ("record-ed", "mend-ed", "on-ly").
pub(crate) fn narrow_hyphen_points(word: &str) -> Vec<usize> {
    if crate::token_kind(word) != crate::TokenKind::Word {
        return Vec::new();
    }
    syllable_points_leaving(word, NARROW_RIGHT_MIN)
}

/// Returns the byte offsets of syllable breaks across all letter runs in `text`.
pub(crate) fn syllable_points(text: &str) -> Vec<usize> {
    syllable_points_leaving(text, RIGHT_MIN)
}

/// [`syllable_points`], each with at least `right_min` letters of its run after it.
fn syllable_points_leaving(text: &str, right_min: usize) -> Vec<usize> {
    let mut points = Vec::new();
    let mut run_start: Option<usize> = None;
    let push_run = |from: usize, to: usize, points: &mut Vec<usize>| {
        points.extend(
            letter_run_points(&text[from..to], right_min)
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
