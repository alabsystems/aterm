// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Clipboard types for OSC 52 and OSC 1337 clipboard operations.
//!
//! Extracted from `aterm-core::terminal::types::clipboard` to break circular
//! dependencies (Part of #5663, #2341).

// ============================================================================
// OSC 52 Clipboard Types
// ============================================================================

/// Clipboard selection target for OSC 52.
///
/// OSC 52 specifies which clipboard/selection buffer to operate on.
/// The selection parameter is a sequence of characters indicating targets:
/// - 'c': Clipboard (system clipboard)
/// - 'p': Primary selection (X11 primary selection, usually from mouse selection)
/// - 'q': Secondary selection (rarely used)
/// - 's': Select (X11 selection)
/// - '0'-'7': Cut buffers 0-7 (historical, rarely used)
///
/// Most implementations only support 'c' (clipboard) and 'p' (primary).
/// When multiple targets are specified, they should all be set to the same content.
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
    pub fn from_char(c: char) -> Option<Self> {
        // Trust L0: explicit digit arms instead of `c as u8 - b'0'` — the
        // native model checker falsely refutes the (guarded) cast/subtraction.
        // Byte-identical mapping, no arithmetic obligations.
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
    pub fn to_char(self) -> char {
        match self {
            ClipboardSelection::Clipboard => 'c',
            ClipboardSelection::Primary => 'p',
            ClipboardSelection::Secondary => 'q',
            ClipboardSelection::Select => 's',
            ClipboardSelection::CutBuffer(n) => {
                // Trust L0: branch clamp (same as `n.min(7)`) plus a
                // saturating add that can never saturate (55 max), so there
                // is no panic path for the model checker to falsely refute.
                let n = if n < 8 { n } else { 7 };
                char::from(b'0'.saturating_add(n))
            }
        }
    }
}

/// Clipboard operation requested by OSC 52.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardOperation {
    /// Set clipboard content.
    ///
    /// Contains the selection targets and the decoded text content.
    Set {
        /// Selection targets (e.g., clipboard, primary)
        selections: Vec<ClipboardSelection>,
        /// The text content to set
        content: String,
    },
    /// Query clipboard content.
    ///
    /// When clipboard queries are enabled by host policy, the terminal may respond
    /// with the clipboard content via an OSC 52 response.
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
