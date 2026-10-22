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

/// Mutable references to the grouped state a terminal reset clears. A new
/// resettable group adds a field here and a call in `ResetGroups::reset()`.
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
        // Marks and annotations point into the erased grid; the next_*_id counters stay
        // so IDs stay unique.
        self.marks_state.marks.clear();
        self.marks_state.annotations.clear();
        *self.taskbar_progress = None;
        // A program's state report does not outlive a full reset (OSC 7501).
        self.program_state.reset();
    }
}

/// The modes a full reset leaves: power-on, except the host's policy. No escape sequence
/// sets these, so none may clear them: a program's reset must not lift a fail-closed
/// authorization (`allow_*`, the shell-integration nonce requirement), re-enable an input
/// capability the host declared absent (kitty) or claim one it never declared
/// (win32-input), or flip host display state (CJK ambiguous width, the announced colour
/// scheme, reconciled after the reset). The one list for both reset paths.
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

/// Reset the state both reset paths share. Core fields are passed one by one for split
/// borrows, grouped state in [`ResetGroups`]. A new resettable field goes in its group's
/// `reset()`, or in `TransientState` for ungrouped scalars.
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
    // Read before the modes reset: it says what `alt_grid` holds.
    let was_alternate_screen = modes.alternate_screen;
    *modes = reset_modes(modes);
    style.reset();
    charset.reset();
    // On the alternate screen `alt_grid` holds the main grid (with its scrollback): swap it
    // back so the main grid is reset, not dropped. On the main screen it holds the
    // inactive alt buffer, which `*alt_grid = None` below drops.
    if was_alternate_screen {
        if let Some(main_grid) = alt_grid.take() {
            *grid = main_grid;
        }
    }
    // No resize undo may hand rows back into a replaced screen (the erase below moving
    // `content_gen` drops it too).
    grid.drop_resize_undo();
    grid.reset_scroll_region();
    grid.reset_horizontal_margins();
    grid.erase_scrollback();
    // RIS resets SGR first, so cursor template is default (no BCE bg).
    grid.set_cursor_template(crate::grid::Cell::EMPTY, None);
    grid.erase_screen();
    // RIS clears DECDWL/DECDHL by its own spec text, not as a side effect of the erase.
    grid.clear_line_attributes();
    grid.set_cursor(0, 0);
    grid.reset_tab_stops();
    // Every host-cached coordinate is stale; `Terminal::reset()` drains this flag itself.
    grid.invalidate_host_coordinates();
    *alt_grid = None;
    cursor_save.reset();
    // Replies already generated stay queued for the PTY peer.
    transient.reset();
    color.reset();
    // So the platform leaves secure input mode.
    *secure_keyboard_entry = false;
    groups.reset();
}

impl Terminal {
    /// Reset the terminal to its initial state, keeping the host's policy. Also resets the
    /// parser, which the RIS handler cannot do from inside parser dispatch.
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
        // A reset leaves the alternate screen.
        self.holder_fence.note_alt_exit();
        // The host's default cursor style is host preference, not app state: it survives.
        self.modes.cursor_style = self.default_cursor_style;
        // Mode 12 mirrors the style's blink bit (see `set_cursor_blink`).
        self.modes.cursor_blink = self.modes.cursor_style.blinks();
        // So is the host's alternate scroll (1007).
        self.modes.alternate_scroll = self.host_alternate_scroll;
        // The BiDi cache may hold resolutions of pre-reset content.
        self.invalidate_bidi_all();
        // Selection anchors point at content that no longer exists.
        self.text_selection.clear();
        // The alt buffer was dropped without an alt->main switch through `post_process`,
        // so nothing else retires the parked selection over the erased grid.
        self.parked_text_selection.clear();
        // This path skips `post_process`, so publish the coordinate change here and drain
        // the grid's reset signals, or the next batch reports this reset again. The
        // counters themselves survive.
        let _ = self.grid.take_absolute_row_update();
        let _ = self.grid.take_selection_row_update();
        let _ = self.grid.take_content_scroll_delta();
        let _ = self.grid.take_coordinates_invalidated();
        let _ = self.grid.take_selection_damage();
        // The band record drains with the flag. `invalidate()` publishes no band batch on
        // purpose: a band-translating host sees an unexplained epoch step and discards.
        let _ = self.grid.take_row_band_moves();
        self.content_scroll_state.invalidate();
        // Every input of the encoder fold moved outside `process()`: republish the mirror.
        self.refresh_mode_mirror();
        // `transient.reset()` bumped the reset generation, and this path never reaches
        // `process_at`: wipe the alt-screen archive here.
        self.alt_archive_after_host_reset();
        // Colours are back on the baseline and 2031 is cleared: re-latch the
        // announced scheme (no push can fire).
        let _ = self.reconcile_color_scheme();
    }
}
