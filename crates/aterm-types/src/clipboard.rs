// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Clipboard types for OSC 52.

/// Clipboard selection target for OSC 52, one per character of the selection parameter.
/// Most terminals support only 'c' and 'p'; every target named is set to the same content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClipboardSelection {
    /// System clipboard ('c')
    Clipboard,
    /// Primary selection ('p') - X11 style mouse selection
    Primary,
    /// Secondary selection ('q')
    Secondary,
    /// Select ('s')
    Select,
    /// Cut buffers 0-7 ('0'-'7')
    CutBuffer(u8),
}

impl ClipboardSelection {
    /// Parse a selection character.
    #[inline]
    #[must_use]
    pub const fn from_char(c: char) -> Option<Self> {
        // Explicit digit arms, not `c as u8 - b'0'`: the Trust model checker refutes the
        // guarded subtraction.
        match c {
            'c' => Some(ClipboardSelection::Clipboard),
            'p' => Some(ClipboardSelection::Primary),
            'q' => Some(ClipboardSelection::Secondary),
            's' => Some(ClipboardSelection::Select),
            '0' => Some(ClipboardSelection::CutBuffer(0)),
            '1' => Some(ClipboardSelection::CutBuffer(1)),
            '2' => Some(ClipboardSelection::CutBuffer(2)),
            '3' => Some(ClipboardSelection::CutBuffer(3)),
            '4' => Some(ClipboardSelection::CutBuffer(4)),
            '5' => Some(ClipboardSelection::CutBuffer(5)),
            '6' => Some(ClipboardSelection::CutBuffer(6)),
            '7' => Some(ClipboardSelection::CutBuffer(7)),
            _ => None,
        }
    }

    /// Convert to selection character.
    #[inline]
    #[must_use]
    pub const fn to_char(self) -> char {
        match self {
            ClipboardSelection::Clipboard => 'c',
            ClipboardSelection::Primary => 'p',
            ClipboardSelection::Secondary => 'q',
            ClipboardSelection::Select => 's',
            ClipboardSelection::CutBuffer(n) => {
                // Clamp by branch and a saturating add that cannot saturate: no panic path
                // for the Trust model checker to refute.
                let n = if n < 8 { n } else { 7 };
                b'0'.saturating_add(n) as char
            }
        }
    }
}

/// Clipboard operation requested by OSC 52.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardOperation {
    /// Set the selection targets to the decoded text.
    Set {
        /// Selection targets (e.g., clipboard, primary)
        selections: Vec<ClipboardSelection>,
        /// The text content to set
        content: String,
    },
    /// Query clipboard content; answered with an OSC 52 response only when host policy
    /// allows clipboard reads.
    Query {
        /// Selection targets to query
        selections: Vec<ClipboardSelection>,
    },
    /// Clear clipboard content.
    Clear {
        /// Selection targets to clear
        selections: Vec<ClipboardSelection>,
    },
}
