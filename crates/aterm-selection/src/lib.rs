// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

#![deny(unsafe_op_in_unsafe_fn)]

//! Selection system for terminal text.
//!
//! This crate provides two selection systems:
//!
//! ## Smart Selection (Pattern-based)
//!
//! Intelligent selection rules that recognize and select semantic text units
//! like URLs, file paths, email addresses, IP addresses, git hashes, and
//! quoted strings.
//!
//! - **Pattern-based rules**: Use regex patterns to identify semantic text units
//! - **Built-in rules**: Pre-configured rules for common patterns (URLs, paths, etc.)
//! - **Extensible**: Add custom rules for application-specific patterns
//! - **Priority-based**: Rules are matched in priority order
//!
//! ### Example
//!
//! ```
//! use aterm_selection::SmartSelection;
//!
//! let smart = SmartSelection::with_builtin_rules();
//!
//! // Given a line of text and a cursor column
//! let line = "Check out https://example.com for more info";
//! let bounds = smart.word_boundaries_at(line, 15); // cursor is on the URL
//!
//! if let Some((start, end)) = bounds {
//!     assert_eq!(&line[start..end], "https://example.com");
//! }
//! ```
//!
//! ## Text Selection (Mouse-based)
//!
//! State machine for mouse-based text selection, implementing the TLA+ spec
//! in `tla/Selection.tla`. Supports:
//!
//! - **Simple selection**: Character-by-character (single click + drag)
//! - **Block selection**: Rectangular selection (Alt + click + drag)
//! - **Semantic selection**: Word/URL selection (double-click)
//! - **Line selection**: Full line selection (triple-click)
//!
//! ### Example
//!
//! ```
//! use aterm_selection::{SelectionSide, SelectionType, TextSelection};
//!
//! let mut sel = TextSelection::new();
//!
//! // Start selection on mouse down
//! sel.start_selection(0, 5, SelectionSide::Left, SelectionType::Simple);
//!
//! // Update on mouse drag
//! sel.update_selection(0, 15, SelectionSide::Right);
//!
//! // Complete on mouse up
//! sel.complete_selection();
//!
//! // Check if a cell is selected
//! assert!(sel.contains(0, 10));
//! ```

#![deny(missing_docs)]
#![deny(clippy::all)]

mod builtin_patterns;
mod ffi_types;
mod rules;
mod text_selection;

#[cfg(test)]
pub(crate) use builtin_patterns::BuiltinRules;
pub use ffi_types::{
    AtermSelectionBounds, AtermSelectionError, AtermSelectionKind, AtermSelectionMatch,
    AtermSelectionState, AtermSelectionType, AtermSmartSelection, build_selection_match,
};
pub use rules::SmartSelection;
#[cfg(test)]
pub(crate) use rules::{RulePriority, SelectionRule};
pub use rules::{SelectionMatch, SelectionRuleKind};

pub use text_selection::SelectionState;
pub use text_selection::{
    SelectionAnchor, SelectionProjection, SelectionSide, SelectionType, TextSelection,
};

#[cfg(test)]
mod tests;
