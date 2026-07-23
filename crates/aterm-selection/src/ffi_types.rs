// Copyright 2026 Andrew Yates
// Author: Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! FFI shared types for selection APIs.
//!
//! These `#[repr(C)]` types back the selection FFI surface, but they belong to
//! the selection domain rather than the old in-tree `ffi/` module tree.

use std::ffi::{CString, c_char};

use super::rules::{SelectionMatch as CoreSelectionMatch, SelectionRuleKind, SmartSelection};
use super::text_selection::{SelectionState, SelectionType};

// Canonical definition in `aterm_ffi_types::ffi_error_types` (Part of #2584, #3353).
pub use aterm_ffi_types::AtermSelectionError;

/// Result of a smart selection match.
#[repr(C)]
pub struct AtermSelectionMatch {
    /// Start byte offset in the text.
    pub start: u32,
    /// End byte offset in the text (exclusive).
    pub end: u32,
    /// Rule name (null-terminated).
    pub rule_name: *mut c_char,
    /// Matched text (null-terminated).
    pub matched_text: *mut c_char,
    /// Kind of match (see `AtermSelectionKind`).
    pub kind: u8,
}

/// Kind of selection match.
#[repr(u8)]
pub enum AtermSelectionKind {
    /// URL (http, https, ftp, file, etc.)
    Url = 0,
    /// File path
    FilePath = 1,
    /// Email address
    Email = 2,
    /// IP address (IPv4 or IPv6)
    IpAddress = 3,
    /// Git hash
    GitHash = 4,
    /// Quoted string
    QuotedString = 5,
    /// UUID
    Uuid = 6,
    /// Semantic version
    SemVer = 7,
    /// Custom rule
    Custom = 8,
}

impl From<SelectionRuleKind> for AtermSelectionKind {
    fn from(kind: SelectionRuleKind) -> Self {
        match kind {
            SelectionRuleKind::Url => Self::Url,
            SelectionRuleKind::FilePath => Self::FilePath,
            SelectionRuleKind::Email => Self::Email,
            SelectionRuleKind::IpAddress => Self::IpAddress,
            SelectionRuleKind::GitHash => Self::GitHash,
            SelectionRuleKind::QuotedString => Self::QuotedString,
            SelectionRuleKind::Uuid => Self::Uuid,
            SelectionRuleKind::SemVer => Self::SemVer,
            SelectionRuleKind::Custom => Self::Custom,
        }
    }
}

/// Build a heap-allocated `AtermSelectionMatch` from a Rust `SelectionMatch`.
pub fn build_selection_match(
    selection_match: &CoreSelectionMatch,
) -> Result<*mut AtermSelectionMatch, AtermSelectionError> {
    let rule_name = CString::new(selection_match.rule_name())
        .map_err(|_| AtermSelectionError::ErrAllocationFailed)?;
    let matched_text = CString::new(selection_match.matched_text())
        .map_err(|_| AtermSelectionError::ErrAllocationFailed)?;

    let result = Box::new(AtermSelectionMatch {
        start: u32::try_from(selection_match.start()).unwrap_or(u32::MAX),
        end: u32::try_from(selection_match.end()).unwrap_or(u32::MAX),
        rule_name: rule_name.into_raw(),
        matched_text: matched_text.into_raw(),
        kind: AtermSelectionKind::from(selection_match.kind()) as u8,
    });

    Ok(Box::into_raw(result))
}

/// Selection type for text selection.
///
/// See `tla/Selection.tla` for the formal specification.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtermSelectionType {
    /// Character-by-character selection (single click + drag).
    Simple = 0,
    /// Rectangular block selection (Alt + click + drag).
    Block = 1,
    /// Semantic selection - words, URLs, etc. (double-click).
    Semantic = 2,
    /// Full line selection (triple-click).
    Lines = 3,
}

impl From<SelectionType> for AtermSelectionType {
    fn from(ty: SelectionType) -> Self {
        match ty {
            SelectionType::Simple => Self::Simple,
            SelectionType::Block => Self::Block,
            SelectionType::Semantic => Self::Semantic,
            SelectionType::Lines => Self::Lines,
            #[allow(unreachable_patterns)]
            _ => Self::Simple,
        }
    }
}

impl From<AtermSelectionType> for SelectionType {
    fn from(ty: AtermSelectionType) -> Self {
        match ty {
            AtermSelectionType::Simple => Self::Simple,
            AtermSelectionType::Block => Self::Block,
            AtermSelectionType::Semantic => Self::Semantic,
            AtermSelectionType::Lines => Self::Lines,
        }
    }
}

/// Selection state for text selection.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AtermSelectionState {
    /// No selection active.
    None = 0,
    /// Selection in progress (mouse button held down).
    InProgress = 1,
    /// Selection complete (mouse button released).
    Complete = 2,
}

impl From<SelectionState> for AtermSelectionState {
    fn from(state: SelectionState) -> Self {
        match state {
            SelectionState::None => Self::None,
            SelectionState::InProgress => Self::InProgress,
            SelectionState::Complete => Self::Complete,
        }
    }
}

/// Selection bounds returned by `aterm_terminal_selection_get_bounds`.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct AtermSelectionBounds {
    /// Current selection state.
    pub state: AtermSelectionState,
    /// Selection type (only valid when state != None).
    pub selection_type: AtermSelectionType,
    /// Start column (0-indexed).
    pub start_col: u16,
    /// Start row (0 = top visible, negative = scrollback).
    pub start_row: i32,
    /// End column (0-indexed).
    pub end_col: u16,
    /// End row (0 = top visible, negative = scrollback).
    pub end_row: i32,
}

impl Default for AtermSelectionBounds {
    fn default() -> Self {
        Self {
            state: AtermSelectionState::None,
            selection_type: AtermSelectionType::Simple,
            start_col: 0,
            start_row: 0,
            end_col: 0,
            end_row: 0,
        }
    }
}

/// Opaque handle for smart selection engine.
pub struct AtermSmartSelection(SmartSelection);

impl AtermSmartSelection {
    /// Create a new opaque handle wrapping a `SmartSelection`.
    pub fn new(inner: SmartSelection) -> Self {
        Self(inner)
    }

    /// Borrow the inner `SmartSelection`.
    pub fn inner(&self) -> &SmartSelection {
        &self.0
    }

    /// Mutably borrow the inner `SmartSelection`.
    pub fn inner_mut(&mut self) -> &mut SmartSelection {
        &mut self.0
    }
}
