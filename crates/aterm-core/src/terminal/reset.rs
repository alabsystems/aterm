// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal reset. [`Terminal::reset()`] and RIS (`TerminalHandler::reset_terminal_state()`)
//! share [`reset_common_fields`].

use crate::grid::Grid;

use super::TaskbarProgress;
use super::grouped_state::SixelState;
use super::grouped_state::{
    CursorSaveState, DcsState, Iterm2State, MarksState, NotificationState, ShellIntegrationState,
    TitleState,
};
use super::transient_state::TransientState;
use super::types::{CurrentStyle, TerminalModes};
use super::{CharacterSetState, KittyKeyboardState, Terminal, XtermKeyboardState};

/// Mutable references to the grouped state cleared by a terminal reset.
pub(super) struct ResetGroups<'a> {
    pub(super) kitty_keyboard: &'a mut KittyKeyboardState,
    pub(super) xterm_keyboard: &'a mut XtermKeyboardState,
    pub(super) iterm2: &'a mut Iterm2State,
    pub(super) shell: &'a mut ShellIntegrationState,
    pub(super) sixel: &'a mut SixelState,
    pub(super) title: &'a mut TitleState,
    pub(super) dcs: &'a mut DcsState,
    pub(super) notifications: &'a mut NotificationState,
    pub(super) marks_state: &'a mut MarksState,
    pub(super) taskbar_progress: &'a mut Option<TaskbarProgress>,
    pub(super) program_state: &'a mut super::program_state::ProgramStateReport,
}

impl ResetGroups<'_> {
    #[inline]
    fn reset(&mut self) {
        self.kitty_keyboard.reset();
        self.xterm_keyboard.reset();
        self.iterm2.reset();
        self.shell.reset();
        self.title.reset();
        self.sixel.reset();
        self.dcs.reset();
        self.notifications.reset();
        self.marks_state.marks.clear();
        self.marks_state.annotations.clear();
        *self.taskbar_progress = None;
        self.program_state.reset();
    }
}

/// Reset terminal modes to power-on defaults while preserving host policy and capabilities.
#[inline]
fn reset_modes(prev: &TerminalModes) -> TerminalModes {
    TerminalModes {
        allow_osc52_query: prev.allow_osc52_query,
        allow_osc52_set: prev.allow_osc52_set,
        allow_window_ops: prev.allow_window_ops,
        allow_notifications: prev.allow_notifications,
        allow_palette_reconfigure: prev.allow_palette_reconfigure,
        require_shell_integration_nonce: prev.require_shell_integration_nonce,
        kitty_keyboard_enabled: prev.kitty_keyboard_enabled,
        win32_input_enabled: prev.win32_input_enabled,
        win32_input_openconsole: prev.win32_input_openconsole,
        ambiguous_width_double: prev.ambiguous_width_double,
        color_scheme: prev.color_scheme,
        ..TerminalModes::new()
    }
}

/// Reset the state shared by [`Terminal::reset`] and RIS (`TerminalHandler::reset_terminal_state`).
#[allow(
    clippy::too_many_arguments,
    reason = "reset needs split-borrow access to each field"
)]
pub(super) fn reset_common_fields(
    grid: &mut Grid,
    modes: &mut TerminalModes,
    style: &mut CurrentStyle,
    charset: &mut CharacterSetState,
    alt_grid: &mut Option<Grid>,
    cursor_save: &mut CursorSaveState,
    transient: &mut TransientState,
    color: &mut super::ColorState,
    secure_keyboard_entry: &mut bool,
    groups: &mut ResetGroups<'_>,
) {
    let was_alternate_screen = modes.alternate_screen;
    *modes = reset_modes(modes);
    style.reset();
    charset.reset();
    if was_alternate_screen {
        if let Some(main_grid) = alt_grid.take() {
            *grid = main_grid;
        }
    }
    grid.drop_resize_undo();
    grid.reset_scroll_region();
    grid.reset_horizontal_margins();
    grid.erase_scrollback();
    grid.set_cursor_template(crate::grid::Cell::EMPTY, None);
    grid.erase_screen();
    grid.clear_line_attributes();
    grid.set_cursor(0, 0);
    grid.reset_tab_stops();
    grid.invalidate_host_coordinates();
    *alt_grid = None;
    cursor_save.reset();
    transient.reset();
    color.reset();
    *secure_keyboard_entry = false;
    groups.reset();
}

impl Terminal {
    /// Reset the terminal and parser to initial state while preserving host policy.
    pub fn reset(&mut self) {
        self.parser.reset();
        let mut groups = ResetGroups {
            kitty_keyboard: &mut self.kitty_keyboard,
            xterm_keyboard: &mut self.xterm_keyboard,
            iterm2: &mut self.iterm2,
            shell: &mut self.shell,
            sixel: &mut self.sixel,
            title: &mut self.title,
            dcs: &mut self.dcs,
            notifications: &mut self.notifications,
            marks_state: &mut self.marks_state,
            taskbar_progress: &mut self.taskbar_progress,
            program_state: &mut self.program_state,
        };
        reset_common_fields(
            &mut self.grid,
            &mut self.modes,
            &mut self.style,
            &mut self.charset,
            &mut self.alt_grid,
            &mut self.cursor_save,
            &mut self.transient,
            &mut self.color,
            &mut self.secure_keyboard_entry,
            &mut groups,
        );
        self.holder_fence.note_alt_exit();
        self.modes.cursor_style = self.default_cursor_style;
        self.modes.cursor_blink = self.modes.cursor_style.blinks();
        self.modes.alternate_scroll = self.host_alternate_scroll;
        self.invalidate_bidi_all();
        self.text_selection.clear();
        self.parked_text_selection.clear();
        let _ = self.grid.take_absolute_row_update();
        let _ = self.grid.take_selection_row_update();
        let _ = self.grid.take_content_scroll_delta();
        let _ = self.grid.take_coordinates_invalidated();
        let _ = self.grid.take_selection_damage();
        let _ = self.grid.take_row_band_moves();
        self.content_scroll_state.invalidate();
        self.refresh_mode_mirror();
        self.alt_archive_after_host_reset();
        let _ = self.reconcile_color_scheme();
        self.host_journal.push(super::HostOp::Reset);
    }
}
