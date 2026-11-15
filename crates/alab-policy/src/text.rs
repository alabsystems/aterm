// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The text primitives every reader here shares with the screen readers that feed it: how a
//! turn's earlier messages join into one text ([`MESSAGE_GAP`], [`join_messages`]), the one table
//! of characters no surface prints ([`is_hidden_char`]), and the one digits-only number parse
//! ([`parse_uint`]).

/// What joins a turn's earlier messages into one text (a screen reader's `turn_said_before`): a
/// blank line, as a message's own paragraphs are joined.
pub const MESSAGE_GAP: &str = "\n\n";

/// A turn's earlier `messages` as one text ([`MESSAGE_GAP`]).
#[must_use]
pub fn join_messages(messages: &[String]) -> String {
    messages.join(MESSAGE_GAP)
}

/// A character no chrome surface prints: a control (C0 and C1, so every ASCII line break and tab), a noncharacter, or an invisible format character that re-orders or hides the text around it (bidi controls, zero-width and filler characters, separators, the tag block's invisible ASCII that an agent reading the text still reads).
#[inline]
#[must_use]
pub const fn is_hidden_char(c: char) -> bool {
    matches!(c, '\u{0}'..='\u{1f}' | '\u{7f}'..='\u{9f}')
        || matches!(
            c,
            '\u{00ad}'
                | '\u{034f}'
                | '\u{061c}'
                | '\u{115f}'..='\u{1160}'
                | '\u{17b4}'..='\u{17b5}'
                | '\u{180b}'..='\u{180f}'
                | '\u{200b}'
                | '\u{200e}'..='\u{200f}'
                | '\u{2028}'..='\u{202e}'
                | '\u{2060}'..='\u{206f}'
                | '\u{3164}'
                | '\u{fdd0}'..='\u{fdef}'
                | '\u{feff}'
                | '\u{ffa0}'
                | '\u{fff0}'..='\u{fffb}'
                | '\u{13430}'..='\u{1343f}'
                | '\u{1bca0}'..='\u{1bca3}'
                | '\u{1d173}'..='\u{1d17a}'
                | '\u{e0000}'..='\u{e007f}'
        )
        || (c as u32) & 0xfffe == 0xfffe
}

/// `s` as a number where it is ASCII digits alone: no sign, no space, no other character, and
/// never the empty string; `None` too where the digits overflow `T`.
#[inline]
pub fn parse_uint<T: std::str::FromStr>(s: &str) -> Option<T> {
    (!s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .then(|| s.parse().ok())
        .flatten()
}

#[cfg(test)]
#[path = "text_tests.rs"]
mod tests;
