// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `unicode-width` facade wired via `[patch.crates-io]` for `codespan-reporting`.

#![warn(missing_docs)]

/// The C0 controls, DEL and the C1 controls: upstream's `None`.
#[inline]
const fn is_control(c: char) -> bool {
    matches!(c, '\0'..='\u{1F}' | '\u{7F}'..='\u{9F}')
}

/// Displayed width of a `char` in terminal columns.
pub trait UnicodeWidthChar {
    /// Columns `self` occupies (East Asian Ambiguous counted narrow); `None` for a control.
    fn width(self) -> Option<usize>;
}

impl UnicodeWidthChar for char {
    #[inline]
    fn width(self) -> Option<usize> {
        (!is_control(self)).then(|| aterm_grapheme::char_width(self))
    }
}
