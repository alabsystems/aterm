// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Paragraph direction hint for BiDi text layout.

/// Paragraph direction hint for the Unicode Bidirectional Algorithm (`CSI n SPACE k`).
#[non_exhaustive]
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum ParagraphDirection {
    /// Default left-to-right direction (`SCP 0`), using first-strong detection under DECSET 2501.
    #[default]
    Auto = 0,
    /// Default right-to-left direction, using first-strong detection under DECSET 2501.
    AutoRtl = 1,
    /// Force left-to-right.
    Ltr = 2,
    /// Force right-to-left.
    Rtl = 3,
}
