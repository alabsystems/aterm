// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! First-party `unicode-width` facade wired via `[patch.crates-io]` for
//! `codespan-reporting` (`naga`'s diagnostics renderer), backed by
//! `aterm_grapheme`'s width tables.
//!
//! Provides [`UnicodeWidthChar::width`] (`None` for C0/DEL/C1 controls,
//! `Some(1)` for printable ASCII, `Some(0 | 1 | 2)` otherwise, with East
//! Asian Ambiguous characters counted narrow). `width_cjk` and
//! `UnicodeWidthStr` are omitted because no consumer in the graph calls them.

#![warn(missing_docs)]

/// The C0 controls, DEL and the C1 controls: upstream's `None`.
#[inline]
const fn is_control(c: char) -> bool {
    matches!(c, '\0'..='\u{1F}' | '\u{7F}'..='\u{9F}')
}

/// Displayed width of a `char` in terminal columns.
pub trait UnicodeWidthChar {
    /// Columns `self` occupies, East Asian Ambiguous characters counted
    /// narrow; `None` for a control character.
    fn width(self) -> Option<usize>;
}

impl UnicodeWidthChar for char {
    #[inline]
    fn width(self) -> Option<usize> {
        (!is_control(self)).then(|| aterm_grapheme::char_width(self))
    }
}
