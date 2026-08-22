// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
// Author: Andrew Yates

//! Terminal reset logic — `ResetGroups`, `reset_common_fields`, `Terminal::reset`.
//!
//! Both [`Terminal::reset()`] and `TerminalHandler::reset_terminal_state()` (ESC c / RIS)
//! share [`reset_common_fields`] as a single source of truth (#4114).
//!
//! Extracted from `mod.rs` to reduce file size (#4553).

use crate::grid::Grid;

use super::TaskbarProgress;
#[cfg(feature = "sixel")]
use super::grouped_state::SixelState;
use super::grouped_state::{
    ClipboardState, CursorSaveState, DcsState, Iterm2State, MarksState, NotificationState,
    SemanticState, ShellIntegrationState, TitleState,
};
use super::transient_state::TransientState;
use super::types::{CurrentStyle, TerminalModes};
use super::{CharacterSetState, KittyKeyboardState, Terminal, XtermKeyboardState};

/// Bundled mutable references to grouped state types for terminal reset.
///
/// Keeps `reset_common_fields` under 10 parameters by collecting the
/// protocol/image/title groups that all need `.reset()` or `.clear()`.
/// Adding a new resettable group: add a field here and a call in
/// `ResetGroups::reset()`.
pub(super) struct ResetGroups<'a> {
    pub(super) kitty_keyboard: &'a mut KittyKeyboardState,
    pub(super) xterm_keyboard: &'a mut XtermKeyboardState,
    pub(super) iterm2: &'a mut Iterm2State,
    pub(super) shell: &'a mut ShellIntegrationState,
    pub(super) semantic: &'a mut SemanticState,
    #[cfg(feature = "sixel")]
    pub(super) sixel: &'a mut SixelState,
    pub(super) title: &'a mut TitleState,
    pub(super) dcs: &'a mut DcsState,
    pub(super) notifications: &'a mut NotificationState,
    pub(super) clipboard: &'a mut ClipboardState,
    pub(super) marks_state: &'a mut MarksState,
    pub(super) taskbar_progress: &'a mut Option<TaskbarProgress>,
}

impl ResetGroups<'_> {
    fn reset(&mut self) {
        self.kitty_keyboard.reset();
        self.xterm_keyboard.reset();
        self.iterm2.reset();
        self.shell.reset();
        self.semantic.reset();
        self.title.reset();
        #[cfg(feature = "sixel")]
        self.sixel.reset();
        self.dcs.reset();
        self.notifications.reset();
        self.clipboard.reset();
        // Clear marks and annotations — they reference positions in the now-erased
        // grid. Preserve next_*_id counters for ID uniqueness.
        self.marks_state.marks.clear();
        self.marks_state.annotations.clear();
        // Clear stale taskbar progress indicator.
        *self.taskbar_progress = None;
    }
}

/// Reset shared terminal state — 9 parameters (#4307, was 27→18→9; +1 for #7336,
/// then -1 when the write-only style interner was deleted and `current_style_id`
/// with it).
///
/// Core state fields are passed individually for split-borrow compatibility.
/// Grouped protocol/image/title state is bundled in [`ResetGroups`].
/// Adding a new resettable field: add to the relevant group's `reset()`,
/// or to `TransientState` for ungrouped scalars.
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
    // Modes and style
    *modes = TerminalModes::new();
    style.reset();
    charset.reset();
    // If on the alternate screen, `grid` currently points at the alt grid and
    // `alt_grid` holds the main grid (with its scrollback).  Swap the main
    // grid back before resetting so we erase/reset the *main* grid rather than
    // dropping it (#7402).  On the main screen the slot instead holds the
    // persistent (inactive) alt buffer — leave the main grid in place and let
    // the `*alt_grid = None` below drop the alt buffer.
    if was_alternate_screen {
        if let Some(main_grid) = alt_grid.take() {
            *grid = main_grid;
        }
    }
    // Grid: reset scroll region and horizontal margins before erasing (#3925 Bug 3)
    grid.reset_scroll_region();
    grid.reset_horizontal_margins();
    grid.erase_scrollback();
    // RIS resets SGR first, so cursor template is default (no BCE bg).
    grid.set_cursor_template(crate::grid::Cell::EMPTY, None);
    grid.erase_screen();
    // RIS must clear DECDWL/DECDHL line attributes (erase_screen preserves them
    // per VT spec, but a full reset must clear everything) (#7497).
    grid.clear_line_attributes();
    grid.set_cursor(0, 0);
    grid.reset_tab_stops();
    // Alternate screen — already consumed by the swap above; ensure cleared.
    *alt_grid = None;
    cursor_save.reset();
    // Transient state (response buffer, hyperlinks, underline, SGR stack, etc.)
    transient.reset();
    // Color state: palette, dynamic colors, cursor color, color stack (#7281)
    color.reset();
    // Secure keyboard entry: clear so platform exits secure input mode (#7336)
    *secure_keyboard_entry = false;
    // Grouped protocol, image, and title state
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
        // Preserve host-configured policy flags across reset — these are
        // set by the embedding application, not by escape sequences, and
        // a rogue program sending RIS should not escalate its privileges
        // by clearing them (#7774, #7898).
        let allow_osc52_query = self.modes.allow_osc52_query;
        let allow_osc52_set = self.modes.allow_osc52_set;
        let allow_window_ops = self.modes.allow_window_ops;
        let allow_notifications = self.modes.allow_notifications;
        let allow_session_memory = self.modes.allow_session_memory;
        let allow_palette_reconfigure = self.modes.allow_palette_reconfigure;
        let require_shell_integration_nonce = self.modes.require_shell_integration_nonce;
        // Capability toggle, not runtime negotiation state: RIS must not let a
        // program re-enable a kitty protocol the host declared unsupported.
        let kitty_keyboard_enabled = self.modes.kitty_keyboard_enabled;
        // HOST/CONFIG-derived DISPLAY state with no escape sequence to restore it:
        // ambiguous_width_double (CJK ambiguous-width policy, from config) and
        // color_scheme (the OS light/dark appearance the host pushes in). A rogue
        // program's RIS must not silently flip CJK width or the reported appearance
        // — the app cannot itself put these back (no wire sequence exists).
        let ambiguous_width_double = self.modes.ambiguous_width_double;
        let color_scheme = self.modes.color_scheme;
        let mut groups = ResetGroups {
            kitty_keyboard: &mut self.kitty_keyboard,
            xterm_keyboard: &mut self.xterm_keyboard,
            iterm2: &mut self.iterm2,
            shell: &mut self.shell,
            semantic: &mut self.semantic,
            #[cfg(feature = "sixel")]
            sixel: &mut self.sixel,
            title: &mut self.title,
            dcs: &mut self.dcs,
            notifications: &mut self.notifications,
            clipboard: &mut self.clipboard,
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
        // Restore host policy flags that must survive RIS (#7774, #7878,
        // #7898, #7937). Rationale: these bits are host-embedded policy,
        // not runtime state — a rogue program sending RIS should not be
        // able to escalate by clearing them.
        self.modes.allow_osc52_query = allow_osc52_query;
        self.modes.allow_osc52_set = allow_osc52_set;
        self.modes.allow_window_ops = allow_window_ops;
        self.modes.allow_notifications = allow_notifications;
        self.modes.allow_session_memory = allow_session_memory;
        self.modes.allow_palette_reconfigure = allow_palette_reconfigure;
        self.modes.require_shell_integration_nonce = require_shell_integration_nonce;
        self.modes.kitty_keyboard_enabled = kitty_keyboard_enabled;
        self.modes.ambiguous_width_double = ambiguous_width_double;
        self.modes.color_scheme = color_scheme;
        // Restore the host-preferred default cursor style: RIS wiped modes back to
        // CursorStyle::default(), but the host's configured default must survive a
        // reset (it's host preference, not app state, like the policy flags above).
        self.modes.cursor_style = self.default_cursor_style;
        // Invalidate BiDi render cache — mode flags are reset but the cache
        // may hold stale resolutions from pre-reset content (#7488).
        self.invalidate_bidi_all();
        // Clear selection — anchors point at content that no longer exists.
        self.text_selection.clear();
        // Clear secure keyboard entry flag — if a program enabled it and then
        // crashed, RIS should restore clean state (#7336).
        self.secure_keyboard_entry = false;
        // Direct reset does not pass through `post_process`, so publish its
        // wholesale coordinate change here. Drain the grid's reset sentinels:
        // otherwise the next unrelated `process` batch would report this same
        // reset a second time. The counters themselves deliberately survive.
        let _ = self.grid.take_absolute_row_update();
        let _ = self.grid.take_selection_row_update();
        let _ = self.grid.take_content_scroll_delta();
        // SELECTION CUSTODY Phase 4 added a SECOND grid-side signal for this same
        // "coordinates moved" fact (the sentinel's split — see `post_process`). It
        // must be drained here for exactly the reason the others are: otherwise the
        // reset's own clear leaves it set and the next unrelated batch reports this
        // reset a second time. The selection damage is drained with it — the
        // selection is cleared outright a few lines above, so there is nothing left
        // for a band to say.
        let _ = self.grid.take_coordinates_invalidated();
        let _ = self.grid.take_selection_damage();
        self.content_scroll_state.invalidate();
    }
}
