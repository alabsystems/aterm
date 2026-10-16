// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! Shell integration types (re-exported from `aterm-types`) and the terminal's caps on
//! them. Most of the API is in [`super::shell_api`].

pub use aterm_types::{
    Annotation, BlockState, CommandMark, OutputBlock, TerminalMark, current_time_ms,
};

/// Maximum completed command marks (OSC 133). FIFO eviction when exceeded.
pub(super) const COMMAND_MARKS_MAX: usize = 1000;

/// Maximum completed output blocks. FIFO eviction.
pub(super) const OUTPUT_BLOCKS_MAX: usize = 1000;

/// Maximum number of user-created marks (OSC 1337 SetMark); oldest evicted
/// first.
pub(super) const TERMINAL_MARKS_MAX: usize = 1000;

/// Maximum number of annotations (OSC 1337 AddAnnotation); oldest evicted
/// first.
pub(super) const ANNOTATIONS_MAX: usize = 1000;

// `ShellState` lives in `domain` (a leaf module) to avoid a dependency cycle.
pub use crate::domain::ShellState;
