// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal reset logic — `ResetGroups`, `reset_common_fields`, `Terminal::reset`.
//!
//! Both [`Terminal::reset()`] and `TerminalHandler::reset_terminal_state()` (ESC c / RIS)
//! share [`reset_common_fields`] as a single source of truth.

use crate::grid::Grid;

use super::TaskbarProgress;
#[cfg(feature = "sixel")]
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
    #[cfg(feature = "sixel")]
    pub(super) sixel: &'a mut SixelState,
    pub(super) title: &'a mut TitleState,
    pub(super) dcs: &'a mut DcsState,
    pub(super) notifications: &'a mut NotificationState,
    pub(super) marks_state: &'a mut MarksState,
    pub(super) taskbar_progress: &'a mut Option<TaskbarProgress>,
}

impl ResetGroups<'_> {
    fn reset(&mut self) {
        self.kitty_keyboard.reset();
        self.xterm_keyboard.reset();
        self.iterm2.reset();
        self.shell.reset();
        self.title.reset();
        #[cfg(feature = "sixel")]
        self.sixel.reset();
        self.dcs.reset();
        self.notifications.reset();
        // Clear marks and annotations — they reference positions in the now-erased
        // grid. Preserve next_*_id counters for ID uniqueness.
        self.marks_state.marks.clear();
        self.marks_state.annotations.clear();
        *self.taskbar_progress = None;
    }
}

/// The modes a full reset leaves: power-on, except the HOST's policy. No escape
/// sequence sets these, so none may clear them: a program's reset must not lift
/// a fail-closed authorization (`allow_*`, the shell-integration nonce
/// requirement), re-enable an input capability the host declared absent (kitty)
/// or claim one it never declared (win32-input), or flip host display state it
/// cannot set back (CJK ambiguous width, the OS appearance). The one list for
/// both reset paths, which had drifted when each kept its own.
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
        ambiguous_width_double: prev.ambiguous_width_double,
        color_scheme: prev.color_scheme,
        ..TerminalModes::new()
    }
}

/// Reset shared terminal state.
///
/// Core fields are passed individually for split borrows; grouped
/// protocol/image/title state is bundled in [`ResetGroups`]. A new resettable
/// field goes in its group's `reset()`, or in `TransientState` for ungrouped
/// scalars.
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
    // Capture before the modes reset below: it tells us what `alt_grid` holds.
    let was_alternate_screen = modes.alternate_screen;
    *modes = reset_modes(modes);
    style.reset();
    charset.reset();
    // If on the alternate screen, `grid` currently points at the alt grid and
    // `alt_grid` holds the main grid (with its scrollback).  Swap the main
    // grid back before resetting so we erase/reset the *main* grid rather than
    // dropping it.  On the main screen the slot instead holds the
    // persistent (inactive) alt buffer — leave the main grid in place and let
    // the `*alt_grid = None` below drop the alt buffer.
    if was_alternate_screen {
        if let Some(main_grid) = alt_grid.take() {
            *grid = main_grid;
        }
    }
    // A full reset replaces the screen: no resize undo may hand rows back into
    // it (the erase below moves `content_gen`, which drops it too; this says so).
    grid.drop_resize_undo();
    // Grid: reset scroll region and horizontal margins before erasing
    grid.reset_scroll_region();
    grid.reset_horizontal_margins();
    grid.erase_scrollback();
    // RIS resets SGR first, so cursor template is default (no BCE bg).
    grid.set_cursor_template(crate::grid::Cell::EMPTY, None);
    grid.erase_screen();
    // RIS must clear DECDWL/DECDHL line attributes. ED 2 already resets the
    // rows it erases; RIS owes the reset by its own spec text and must not
    // inherit it from whichever erase it calls.
    grid.clear_line_attributes();
    grid.set_cursor(0, 0);
    grid.reset_tab_stops();
    // RIS swaps the main grid back in, erases the screen, clears line
    // attributes and homes the cursor: every host-cached coordinate is stale.
    // `Terminal::reset()` drains this flag and reports it once itself.
    grid.invalidate_host_coordinates();
    // Alternate screen — already consumed by the swap above; ensure cleared.
    *alt_grid = None;
    cursor_save.reset();
    // Transient state (hyperlinks, underline, SGR stack, etc.); replies already
    // generated stay queued for the PTY peer.
    transient.reset();
    color.reset();
    // Secure keyboard entry: clear so platform exits secure input mode
    *secure_keyboard_entry = false;
    groups.reset();
}

impl Terminal {
    /// Reset terminal to initial state.
    ///
    /// Delegates to `reset_common_fields` for shared state, then resets
    /// parser state (which cannot be reset inside the RIS handler since
    /// we're already inside parser dispatch).
    pub fn reset(&mut self) {
        self.parser.reset();
        // Host-configured policy survives: `reset_common_fields` keeps it
        // (`reset_modes`).
        let mut groups = ResetGroups {
            kitty_keyboard: &mut self.kitty_keyboard,
            xterm_keyboard: &mut self.xterm_keyboard,
            iterm2: &mut self.iterm2,
            shell: &mut self.shell,
            #[cfg(feature = "sixel")]
            sixel: &mut self.sixel,
            title: &mut self.title,
            dcs: &mut self.dcs,
            notifications: &mut self.notifications,
            marks_state: &mut self.marks_state,
            taskbar_progress: &mut self.taskbar_progress,
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
        // Restore the host-preferred default cursor style: RIS wiped modes back to
        // CursorStyle::default(), but the host's configured default must survive a
        // reset (it's host preference, not app state, like the policy `reset_modes` keeps).
        self.modes.cursor_style = self.default_cursor_style;
        // Mode 12 mirrors the style's blink bit (see `set_cursor_blink`).
        self.modes.cursor_blink = self.modes.cursor_style.blinks();
        // Invalidate BiDi render cache — mode flags are reset but the cache
        // may hold stale resolutions from pre-reset content.
        self.invalidate_bidi_all();
        // Clear selection — anchors point at content that no longer exists.
        self.text_selection.clear();
        // `reset_common_fields` swapped the main grid back and dropped the alt
        // buffer: an alt->main switch that never reaches `post_process`, so nothing
        // else restores or retires the parked slot. Without this the slot outlives
        // the reset holding a selection over a grid that was just erased — the
        // lifetime invariant on the field, violated.
        self.parked_text_selection.clear();
        // Direct reset does not pass through `post_process`, so publish its
        // wholesale coordinate change here. Drain the grid's reset sentinels:
        // otherwise the next unrelated `process` batch would report this same
        // reset a second time. The counters themselves deliberately survive.
        let _ = self.grid.take_absolute_row_update();
        let _ = self.grid.take_selection_row_update();
        let _ = self.grid.take_content_scroll_delta();
        // Drain the grid's coordinates-moved signal and selection damage, or the
        // next unrelated batch reports this reset again; the selection was
        // cleared above.
        let _ = self.grid.take_coordinates_invalidated();
        let _ = self.grid.take_selection_damage();
        // The band record travels with the flag and is drained with it. The
        // `invalidate()` below deliberately publishes no band batch: a reset moves
        // everything, so a band-translating host must see an unexplained epoch step
        // and discard — fail-closed by construction, not by a flag it could miss.
        let _ = self.grid.take_row_band_moves();
        self.content_scroll_state.invalidate();
        // RIS moved every input of the encoder fold (kitty stack, xterm keyboard
        // state, DECCKM/DECKPAM/mouse modes) without passing through
        // `process()`, so republish the lock-free mirror here.
        self.refresh_mode_mirror();
        // The alt-screen archive: `reset_common_fields` bumped the reset
        // generation, and this path never reaches `process_at`, so wipe here.
        self.alt_archive_after_host_reset();
    }
}
