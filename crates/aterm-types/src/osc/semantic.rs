// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Semantic blocks and buttons (OSC 1337 Block/UpdateBlock/Button).

#[cfg(feature = "serde")]
use serde::{Deserialize, Serialize};

/// A semantic code block defined by OSC 1337 Block sequences.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticBlock {
    /// Unique identifier for this block.
    pub id: String,
    /// Absolute row where the block starts.
    pub start_row: u64,
    /// Column where the block starts.
    pub start_col: u16,
    /// Absolute row where the block ends (set when closed).
    pub end_row: Option<u64>,
    /// Column where the block ends (set when closed).
    pub end_col: Option<u16>,
    /// Whether the block is currently folded (collapsed to single line).
    pub folded: bool,
}

impl SemanticBlock {
    /// Create a new open block starting at the given position.
    pub fn new(id: String, start_row: u64, start_col: u16) -> Self {
        Self {
            id,
            start_row,
            start_col,
            end_row: None,
            end_col: None,
            folded: false,
        }
    }

    /// Close the block at the given position.
    pub fn close(&mut self, end_row: u64, end_col: u16) {
        self.end_row = Some(end_row);
        self.end_col = Some(end_col);
    }

    /// Check if the block is closed (has an end position).
    #[must_use]
    pub fn is_closed(&self) -> bool {
        self.end_row.is_some()
    }

    /// Set the folded state.
    pub fn set_folded(&mut self, folded: bool) {
        self.folded = folded;
    }
}

/// Type of semantic button.
#[non_exhaustive]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticButtonType {
    /// Copy button for a code block.
    Copy,
    /// Custom button that sends an escape sequence when clicked.
    Custom,
}

/// A semantic button attached to terminal content.
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticButton {
    /// Type of button (Copy or Custom).
    pub button_type: SemanticButtonType,
    /// Block ID this button is associated with (for Copy buttons).
    pub block_id: Option<String>,
    /// Custom button code (for Custom buttons).
    pub code: Option<u32>,
    /// SF Symbol icon name (for Custom buttons).
    pub icon: Option<String>,
    /// Absolute row position where the button was created.
    pub row: u64,
    /// Column position where the button was created.
    pub col: u16,
}

impl SemanticButton {
    /// Create a copy button for a block.
    pub fn copy(block_id: String, row: u64, col: u16) -> Self {
        Self {
            button_type: SemanticButtonType::Copy,
            block_id: Some(block_id),
            code: None,
            icon: None,
            row,
            col,
        }
    }

    /// Create a custom button with a code and optional icon.
    pub fn custom(code: u32, icon: Option<String>, row: u64, col: u16) -> Self {
        Self {
            button_type: SemanticButtonType::Custom,
            block_id: None,
            code: Some(code),
            icon,
            row,
            col,
        }
    }
}

/// Event types for semantic block operations.
#[non_exhaustive]
#[allow(missing_docs)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticBlockEvent {
    /// A block was opened (start received).
    Opened { id: String, row: u64, col: u16 },
    /// A block was closed (end received).
    Closed { id: String, row: u64, col: u16 },
    /// A block's fold state changed.
    FoldChanged { id: String, folded: bool },
}

/// Event types for semantic button operations.
#[non_exhaustive]
#[allow(missing_docs)]
#[cfg_attr(feature = "serde", derive(Serialize, Deserialize))]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SemanticButtonEvent {
    /// A copy button was created for a block.
    CopyCreated {
        block_id: String,
        row: u64,
        col: u16,
    },
    /// A custom button was created.
    CustomCreated {
        code: u32,
        icon: Option<String>,
        row: u64,
        col: u16,
    },
    /// All custom buttons were disabled.
    CustomDisabled,
}
