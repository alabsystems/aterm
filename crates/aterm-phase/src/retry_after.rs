// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! THE WAIT A RATE LIMIT'S OWN WORDS NAME (v6 §14.17.5.4, slice 12c): the `retry-after` an API's
//! 429 states in the words a program draws at its wall, read so the harness's first try waits that
//! long instead of the outage ladder's first rung ([`retry_after`]).

use std::time::Duration;

/// The phrases an API's error names its own wait by, as a program draws the error's words: the
/// Responses API's (and the lab stub's) `Please try again in 5s.`, `try again in 1m30s`, a
/// Gemini error's `Please retry in 33.35s.`, and `retry after 30 seconds`. Each is followed by a
/// number ([`span`]); `try again in a moment` and `try again later` name none, and neither does
/// the Anthropic API's 429 body (its wait is a `retry-after` header Claude Code never draws).
pub const RETRY_AFTER_WORDS: [&str; 3] = ["try again in ", "retry in ", "retry after "];

/// agy's dump of a Gemini error's `RetryInfo` (`retryDelay:47.218363629s`), read with the spaces
/// the program's wrap put in it dropped.
pub const RETRY_DELAY_WORD: &str = "retrydelay:";

/// The wait `message` (a wall's words as the screen draws them) names for its next try: the
/// first [`RETRY_AFTER_WORDS`] phrase followed by a span, else agy's [`RETRY_DELAY_WORD`],
/// rounded up to whole seconds and at least one. `None` where no phrase is followed by a number.
#[must_use]
pub fn retry_after(message: &str) -> Option<Duration> {
    let lower = message.to_lowercase();
    let squeezed: String = lower.chars().filter(|c| !c.is_whitespace()).collect();
    let named = |text: &str, words: &[String]| {
        words.iter().find_map(|w| {
            text.match_indices(w.as_str())
                .find_map(|(at, w)| span(&text[at + w.len()..]))
        })
    };
    let spaced: Vec<String> = RETRY_AFTER_WORDS.iter().map(|w| (*w).to_string()).collect();
    let tight: Vec<String> = RETRY_AFTER_WORDS
        .iter()
        .map(|w| w.replace(' ', ""))
        .collect();
    // As drawn first; then with every space dropped, so a phrase or a span a program's wrap cut
    // across rows (`try again in 1` / `50s.`) still reads.
    named(&lower, &spaced)
        .or_else(|| named(&squeezed, &tight))
        .or_else(|| {
            squeezed
                .match_indices(RETRY_DELAY_WORD)
                .last()
                .and_then(|(at, w)| span(&squeezed[at + w.len()..]))
        })
}

/// The span `text` opens with: Go's form (`5s`, `1m30s`, `1.5s`, `250ms`, `2h`) or a number and a
/// word (`30 seconds`, `2 minutes`, `1 hour`), rounded up to whole seconds and at least one.
fn span(text: &str) -> Option<Duration> {
    let mut rest = text;
    let mut millis: u128 = 0;
    let mut parts = 0;
    loop {
        let (value, after) = number(rest)?;
        let after_space = after.trim_start_matches(' ');
        let (unit_ms, tail) = unit(after).or_else(|| word_unit(after_space))?;
        millis = millis.checked_add(value.checked_mul(unit_ms)? / 1000)?;
        parts += 1;
        rest = tail;
        if number(rest).is_none() || parts == 4 {
            break;
        }
    }
    let secs = u64::try_from(millis.div_ceil(1000)).ok()?.max(1);
    Some(Duration::from_secs(secs))
}

/// A decimal number `text` opens with, in thousandths, and what follows it.
fn number(text: &str) -> Option<(u128, &str)> {
    let whole: &str = &text[..text.bytes().take_while(u8::is_ascii_digit).count()];
    if whole.is_empty() {
        return None;
    }
    let after = &text[whole.len()..];
    let (frac, after) = match after.strip_prefix('.') {
        Some(f) => {
            let n = f.bytes().take_while(u8::is_ascii_digit).count();
            (&f[..n], &f[n..])
        }
        None => ("", after),
    };
    let mut thousandths: u128 = whole.parse::<u128>().ok()?.checked_mul(1000)?;
    let mut scale = 100;
    for d in frac.bytes().take(3) {
        thousandths = thousandths.checked_add(u128::from(d - b'0') * scale)?;
        scale /= 10;
    }
    if frac.bytes().skip(3).any(|d| d != b'0') {
        thousandths = thousandths.checked_add(1)?;
    }
    Some((thousandths, after))
}

/// Go's unit right after the number (`ms`, `s`, `m`, `h`), in milliseconds, and what follows it.
fn unit(text: &str) -> Option<(u128, &str)> {
    let ends = |rest: &str| !rest.starts_with(|c: char| c.is_ascii_alphabetic());
    for (u, ms) in [("ms", 1), ("h", 3_600_000), ("m", 60_000), ("s", 1000)] {
        if let Some(rest) = text.strip_prefix(u)
            && (ends(rest) || number(rest).is_some())
        {
            return Some((ms, rest));
        }
    }
    None
}

/// A unit word after the number (`seconds`, `sec`, `minutes`, `min`, `hours`), in milliseconds,
/// and what follows it.
fn word_unit(text: &str) -> Option<(u128, &str)> {
    let len = text.bytes().take_while(u8::is_ascii_alphabetic).count();
    let ms = match &text[..len] {
        "second" | "seconds" | "sec" | "secs" => 1000,
        "minute" | "minutes" | "min" | "mins" => 60_000,
        "hour" | "hours" => 3_600_000,
        _ => return None,
    };
    Some((ms, &text[len..]))
}

#[cfg(test)]
#[path = "retry_after_tests.rs"]
mod tests;
