// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `unicode-width`: aterm's first-party facade for the crates.io package of
//! the same name, patched in (`[patch.crates-io]`) under codespan-reporting,
//! its only consumer (naga's diagnostics renderer). It answers from
//! `aterm_grapheme`'s width tables, the ones aterm's terminal grid uses.
//!
//! # What the consumer calls
//!
//! One method, once: `codespan-reporting-0.13.1/src/term/renderer.rs:859`
//! measures each character of a source line with `ch.width().unwrap_or(0)`
//! to align the carets under a naga shader error. Only
//! [`UnicodeWidthChar::width`] is provided: upstream's `width_cjk` (behind
//! its `cjk` feature, which is accepted here and ignored) and
//! `UnicodeWidthStr` with its sequence rules (emoji ZWJ ligatures, `"\r\n"`)
//! have no caller, and a new one fails to compile rather than answer from
//! tables that disagree more.
//!
//! # The contract kept, and where the tables differ
//!
//! Upstream's per-character contract holds exactly: `None` for the C0
//! controls, DEL and the C1 controls; `Some(1)` for printable ASCII;
//! `Some(0)`, `Some(1)` or `Some(2)` otherwise. The tables are not
//! upstream's, and differ (measured 2026-10-15 against unicode-width 0.2.2
//! over every scalar value): 56,741 of 1,112,064, of which 56,398 are the
//! unassigned U+323B0..U+3FFFD (upstream 2, aterm 1) and 343 are other code
//! points, such as a lone regional indicator (aterm 2), U+00AD SOFT HYPHEN
//! (aterm 1) and Unicode 17 additions. The cost is a caret misplaced under
//! such a character in a naga error message, for shaders aterm compiles and
//! that never fail.
//!
//! The crate retires with naga (the gpu-wgpu unit of
//! docs/AUDIT-third-party-2026-10-15.md).

#![warn(missing_docs)]

/// The C0 controls, DEL and the C1 controls: upstream's `None`.
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
