// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal constructors — `new`, `with_size`, `with_scrollback`, etc.
//!
//! Extracted from `mod.rs` to reduce file size (#4553).

use crate::grid::{Grid, StyleId};
use crate::parser::Parser;
use crate::platform::FontDescriptor;
use crate::scrollback::Scrollback;

#[cfg(feature = "sixel")]
use super::grouped_state::SixelState;
use super::grouped_state::{
    BiDiGroupState, ClipboardState, ColorState, CursorSaveState, DcsState, Iterm2State, MarksState,
    NotificationState, SemanticState, ShellIntegrationState, TitleState,
};
use super::transient_state::TransientState;
use super::types::{CurrentStyle, TerminalModes, TerminalSize};
use super::{CharacterSetState, KittyKeyboardState, Terminal, XtermKeyboardState};

impl Terminal {
    /// Create a new terminal with the given dimensions.
    #[must_use]
    pub fn new(rows: u16, cols: u16) -> Self {
        Self::with_size(TerminalSize::new(rows, cols))
    }

    /// Create a new terminal with the given size.
    #[must_use]
    pub(crate) fn with_size(size: TerminalSize) -> Self {
        Self::with_grid(Grid::new(size.rows(), size.cols()))
    }

    /// Internal constructor: create terminal with pre-built grid.
    ///
    /// All public constructors delegate to this method to avoid field
    /// initialization duplication. See #1648.
    pub(crate) fn with_grid(grid: Grid) -> Self {
        let mut terminal = Self {
            grid,
            parser: Parser::new(),
            modes: TerminalModes::new(),
            style: CurrentStyle::default(),
            current_style_id: StyleId::DEFAULT,
            charset: CharacterSetState::new(),
            alt_grid: None,
            cursor_save: CursorSaveState::new(),
            title: TitleState::new(),
            bell_callback: None,
            kitty_file_resolver: None,
            last_bell_time: None,
            bell_total: 0,
            cursor_style_callback: None,
            default_cursor_style: aterm_types::CursorStyle::default(),
            buffer_activation_callback: None,
            notifications: NotificationState::new(),
            clipboard: ClipboardState::new(),
            iterm2: Iterm2State::new(),
            transient: TransientState::new(),
            watchers: super::observe::WatcherSet::default(),
            row_text_scratch: Vec::new(),
            current_working_directory: None,
            color: ColorState::new(),
            font: FontDescriptor::default(),
            bidi_state: BiDiGroupState::new(),
            dcs: DcsState::new(),
            shell: ShellIntegrationState::new(),
            marks_state: MarksState::new(),
            semantic: SemanticState::new(),
            taskbar_progress: None,
            kitty_keyboard: KittyKeyboardState::new(),
            xterm_keyboard: XtermKeyboardState::new(),
            #[cfg(feature = "sixel")]
            sixel: SixelState::new(),
            window_callback: None,
            text_sizing_callback: None,
            text_selection: crate::selection::TextSelection::new(),
            secure_keyboard_entry: false,
            vi: crate::vi_mode::ViMode::new(),
            sync_timeout_duration: std::time::Duration::from_secs(1),
            clipboard_auth: super::clipboard_auth::ClipboardAuth::new(),
            shell_integration_auth: super::shell_integration_auth::ShellIntegrationAuth::new(),
            hyperlink_auth: super::hyperlink_auth::HyperlinkAuth::new(),
            dcs_auth: super::dcs_auth::DcsAuth::new(),
            policy_engine: None,
            damage_epoch: 0,
            damage_epoch_counted: false,
            search_index: None,
            search_index_rebuilds: 0,
            budgeted_search: None,
            absolute_row_revision: 0,
            repaint_blink_epoch: 0,
        };

        terminal.sync_bidi_resolver_from_config();

        // Sync `clipboard_auth` from the `modes.allow_osc52_*` mirror bits
        // set by `TerminalModes::new()`. Post-#7782 both flags default to
        // `false` (fail-closed) — the capability gate stays revoked until
        // the host explicitly calls `authorize_clipboard_access(...)` after
        // wiring its clipboard callback. Routing the initial mode bit
        // through `authorize_*` here keeps a single source of truth for
        // the capability state (#7874, #7878 CF-004/CF-005, #7782).
        if terminal.modes.allow_osc52_set {
            terminal.clipboard_auth.authorize_write();
        }
        if terminal.modes.allow_osc52_query {
            terminal.clipboard_auth.authorize_query();
        }

        terminal
    }

    /// Create a terminal with tiered scrollback.
    #[must_use]
    pub fn with_scrollback(
        rows: u16,
        cols: u16,
        ring_buffer_size: usize,
        scrollback: Scrollback,
    ) -> Self {
        Self::with_grid(Grid::with_tiered_scrollback(
            rows,
            cols,
            ring_buffer_size,
            scrollback,
        ))
    }

    /// Create a terminal from a restored grid.
    ///
    /// Used by checkpoint restore to recreate terminal state.
    #[must_use]
    #[allow(
        dead_code,
        reason = "checkpoint-restore constructor consumed by the checkpoint test-support layer"
    )]
    pub(crate) fn from_grid(grid: Grid) -> Self {
        Self::with_grid(grid)
    }

    /// Create a terminal from a restored grid and scrollback.
    ///
    /// Used by checkpoint restore to recreate terminal state with scrollback history.
    #[must_use]
    #[allow(
        dead_code,
        reason = "checkpoint-restore constructor consumed by the checkpoint test-support layer"
    )]
    pub(crate) fn from_grid_and_scrollback(mut grid: Grid, scrollback: Scrollback) -> Self {
        grid.attach_scrollback(scrollback);
        Self::with_grid(grid)
    }
}
