// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! The one table of characters no surface prints and no free text keeps: the chrome's sanitizer
//! (`aterm_messages::text`), the session log's free text (`aterm_slog::BoundedText`) and a
//! request's `why=` line ([`crate::control_verbs::request_why`]) all read it.

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
