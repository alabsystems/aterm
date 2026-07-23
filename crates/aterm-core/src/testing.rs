// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Test-only helpers for migrated in-crate tests.
//!
//! This module exposes the small subset of terminal/grid helpers that migrated
//! tests need after moving out of `aterm-core` itself. It is compiled only for
//! in-crate tests (`cfg(test)`).

pub use crate::grid::{Cell, CellFlags, LineDamageBounds, LineSize, PackedColor, StyleId};
pub use crate::terminal::Terminal;
pub use crate::terminal::mouse::FocusState;
pub use crate::terminal::testing::{
    ANNOTATIONS_MAX, COMMAND_MARKS_MAX, MAX_OSC52_QUERY_RESPONSE_BYTES, OUTPUT_BLOCKS_MAX,
    SEMANTIC_BLOCKS_MAX, SEMANTIC_BUTTONS_MAX, SemanticBlockEvent, SemanticButtonEvent,
    TERMINAL_MARKS_MAX,
};

// Constants for migrated terminal tests (Wave 2, #6813).
// Defined here rather than re-exported because the source modules are private.
// Values must match their definitions in terminal/callbacks/mod.rs (TITLE_STACK_MAX_DEPTH),
// terminal/mod.rs (MAX_RESPONSE_BUFFER_SIZE), handler_osc_1337.rs (USER_VARS_MAX).
/// Maximum title stack depth (`push_title` / `pop_title`).
pub const TITLE_STACK_MAX_DEPTH: usize = 10;
/// Maximum response buffer size (1 MiB).
pub const MAX_RESPONSE_BUFFER_SIZE: usize = 1024 * 1024;
/// Maximum number of user variables per terminal.
pub const USER_VARS_MAX: usize = 256;
/// Maximum byte length of a single user variable key.
pub const MAX_USER_VAR_KEY_BYTES: usize = 4096;
/// Maximum byte length of a single decoded user variable value (16 KiB).
pub const MAX_USER_VAR_VALUE_BYTES: usize = 16 * 1024;
/// Maximum size of clipboard copy capture (10 MiB).
pub const MAX_COPY_TO_CLIPBOARD_CAPTURE_BYTES: usize = 10 * 1024 * 1024;

/// Create a standard 24x80 terminal for testing.
///
/// Opts into `allow_palette_reconfigure` (#7937) and `allow_osc52_set`
/// (#7782) so the large fleet of pre-existing OSC 4 / OSC 21 palette-SET
/// and OSC 52 clipboard-SET tests keep working. Regression tests
/// verifying the fail-closed defaults construct a `Terminal::new`
/// directly (or toggle the bits back off via `modes_mut` /
/// `revoke_clipboard_access`).
#[must_use]
pub fn default_terminal() -> Terminal {
    use crate::terminal::ClipboardAccess;
    let mut term = Terminal::new(24, 80);
    term.modes_mut().allow_palette_reconfigure = true;
    // Route through `authorize_clipboard_access` so the capability gate
    // and the mirror bit on `modes` stay in lockstep (#7782).
    term.authorize_clipboard_access(ClipboardAccess::Write);
    term
}

/// Get a cell at the given position, panicking if out of bounds.
#[must_use]
pub fn cell_at(term: &Terminal, row: u16, col: u16) -> &Cell {
    term.grid()
        .cell(row, col)
        .unwrap_or_else(|| panic!("cell ({row}, {col}) should exist"))
}

/// Get cell flags at the given position.
#[must_use]
pub fn cell_flags(term: &Terminal, row: u16, col: u16) -> CellFlags {
    cell_at(term, row, col).flags()
}

/// Get a single line from the grid with trailing blanks trimmed.
#[must_use]
pub fn grid_line(term: &Terminal, row: usize) -> String {
    term.grid()
        .row_text(u16::try_from(row).expect("row fits in u16"))
        .map(|line| line.trim_end().to_string())
        .unwrap_or_default()
}

/// Drain the terminal response buffer into a byte vector.
#[must_use]
pub fn take_response(term: &mut Terminal) -> Vec<u8> {
    term.take_response().expect("expected terminal response")
}

/// Read the currently interned style identifier for the terminal state.
#[must_use]
pub fn current_style_id(term: &Terminal) -> StyleId {
    term.current_style_id()
}

/// Install a text-sizing callback (OSC 66) for integration tests.
pub fn set_text_sizing_callback<F>(term: &mut Terminal, callback: F)
where
    F: FnMut(aterm_types::TextSizingOperation) + Send + 'static,
{
    term.set_text_sizing_callback(callback);
}
