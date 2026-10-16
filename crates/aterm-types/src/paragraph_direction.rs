// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Paragraph direction hint for BiDi text layout, here to break the cycle between
//! `aterm-core` (which stores it in `TerminalModes`) and `aterm-bidi` (which resolves it).

/// Hint for the Unicode Bidirectional Algorithm's paragraph direction, set via SCP
/// (`CSI n SPACE k`). First-strong autodetection is a separate switch, DECSET 2501
/// (`TerminalModes::bidi_autodetection`, default off), per the Terminal WG: the `Auto*`
/// variants name the default direction, used directly while 2501 is reset and as the
/// fallback for a line with no strong character while it is set.
#[non_exhaustive]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParagraphDirection {
    /// The terminal's default direction, LTR (SCP 0). First-strong detection
    /// applies only while DECSET 2501 is set.
    #[default]
    Auto = 0,
    /// The terminal's default direction, RTL. First-strong detection applies
    /// only while DECSET 2501 is set.
    AutoRtl = 1,
    /// Force left-to-right.
    Ltr = 2,
    /// Force right-to-left.
    Rtl = 3,
}
