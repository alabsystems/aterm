// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Text from a host or the LAN, made safe to print on the owner's terminal ([`shown`]).

/// `text` safe to print to a terminal: each control character and each bidirectional
/// formatting character is shown as its `\u{…}` escape rather than sent.
#[must_use]
pub fn shown(text: &str) -> String {
    let inert = |c: char| {
        c.is_control()
            || matches!(
                c,
                '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
            )
    };
    if !text.chars().any(inert) {
        return text.to_owned();
    }
    text.chars()
        .map(|c| {
            if inert(c) {
                c.escape_unicode().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
