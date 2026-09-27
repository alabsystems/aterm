// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell integration types and constants.
//!
//! Pure data types now live in `aterm-types` crate. This module re-exports them
//! and keeps terminal-internal constants and state (Part of #5663, #2341).
//!
//! Most API surface is in [`super::shell_api`].

// Re-export pure data types from aterm-types.
pub use aterm_types::{
    Annotation, BlockState, CommandMark, OutputBlock, TerminalMark, current_time_ms,
};

/// Maximum completed command marks (OSC 133). FIFO eviction when exceeded.
pub(super) const COMMAND_MARKS_MAX: usize = 1000;

/// Maximum completed output blocks. FIFO eviction, matches COMMAND_MARKS_MAX.
pub(super) const OUTPUT_BLOCKS_MAX: usize = 1000;

/// Maximum number of user-created marks (OSC 1337 SetMark).
///
/// When exceeded, oldest marks are evicted (FIFO).
pub(super) const TERMINAL_MARKS_MAX: usize = 1000;

/// Maximum number of annotations (OSC 1337 AddAnnotation).
///
/// When exceeded, oldest annotations are evicted (FIFO).
pub(super) const ANNOTATIONS_MAX: usize = 1000;

// ShellState lives in domain (leaf module) to break the
// semantic -> terminal dependency cycle. Re-exported here
// to preserve the existing API.
pub use crate::domain::ShellState;
