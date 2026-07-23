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
    Annotation, BlockState, CommandMark, OutputBlock, ShellEvent, TerminalMark, current_time_ms,
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

/// Maximum number of semantic code blocks (OSC 1337 Block).
///
/// When exceeded, oldest closed blocks are evicted. Open blocks are not evicted.
// Surfaced for assertions only through the cfg(test/"testing") `terminal::testing`
// module; allow it to be unused in the default build.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "exposed only to the test-gated `testing` module")
)]
pub(super) const SEMANTIC_BLOCKS_MAX: usize = 256;

/// Maximum number of semantic buttons (OSC 1337 Button).
///
/// When exceeded, oldest buttons are evicted (FIFO).
// Surfaced for assertions only through the cfg(test/"testing") `terminal::testing`
// module; allow it to be unused in the default build.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "exposed only to the test-gated `testing` module")
)]
pub(super) const SEMANTIC_BUTTONS_MAX: usize = 512;

// ShellState lives in domain (leaf module) to break the
// semantic -> terminal dependency cycle. Re-exported here
// to preserve the existing API.
pub use crate::domain::ShellState;

// Re-export callback type from aterm-types (Part of #5663 Phase 2).
pub(super) use aterm_types::ShellCallback;
