// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inline session rename: the GUI face of the user pin (`meta set title`), the top rung of the tab-label chain ([`crate::app_tabs::resolved_terminal_title_rung`]).

use crate::app_search::{SearchEdit, apply_field_edit};
use crate::platform::AppRt;
use crate::session_timeline::{MetaEdit, MetaField, MetaWriteError, write_session_meta};
use crate::tab_model::TabId;
use crate::{App, WindowId};

/// Which surface presents a live edit, and so who owns the text: the macOS `NSTextField`'s field editor, or this state for the in-grid field [`crate::tab_bar`] paints.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenameSurface {
    /// The macOS strip's `NSTextField` overlay.
    Native,
    /// The cell-drawn field the in-grid strip paints over the tab's title; `text`/`cursor` are the field.
    InGrid,
}

/// A live inline rename on one window's tab strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabRenameEdit {
    /// The session whose value is edited, resolved once at begin from the tab's focused pane; the only thing a commit applies to.
    pub session: u64,
    /// The tab the editor is painted over, for positioning only: how the native editor follows a reorder and learns its tab closed.
    pub tab: TabId,
    /// Which editor is on screen, set at begin from what presented and never re-derived.
    pub surface: RenameSurface,
    /// The metadata field the commit writes: the title pin (Rename Session…, the double-click) or the role (Window ▸ Set Role…).
    pub field: MetaField,
    /// The in-grid field's text: seeded from the stored value (empty when unset), changed only through [`crate::app_search::apply_field_edit`], the find bar's reducer.
    pub text: String,
    /// Caret position in [`Self::text`], a byte offset on a char boundary.
    pub cursor: usize,
}

impl App {
    /// The session being renamed in `window`, or `None` when no editor is open: whether keys and commands belong to a text field rather than the terminal.
    pub(crate) fn rename_edit_session(&self, window: WindowId) -> Option<u64> {
        Some(self.windows.get(&window)?.rename_edit.as_ref()?.session)
    }

    /// The live edit on `window` when the in-grid strip presents it: the gate every own-rendered path (key routing, strip paint, strip clicks) asks, so none acts on a macOS field whose text it does not own.
    pub(crate) fn inline_rename_edit(&self, window: WindowId) -> Option<&TabRenameEdit> {
        self.windows
            .get(&window)?
            .rename_edit
            .as_ref()
            .filter(|edit| edit.surface == RenameSurface::InGrid)
    }

    /// The window with a live inline rename, if any: the menu dispatch asks before letting a key equivalent reach the terminal.
    pub(crate) fn any_rename_edit(&self) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, ws)| ws.rename_edit.is_some())
            .map(|(wid, _)| *wid)
    }

    /// The front window's live edit, if it has one.
    pub(crate) fn front_rename_edit(&self) -> Option<WindowId> {
        match self.focus_order.last().copied() {
            Some(front) => self
                .windows
                .get(&front)
                .is_some_and(|ws| ws.rename_edit.is_some())
                .then_some(front),
            None => self.any_rename_edit(),
        }
    }

    /// Whether `window` can present an inline rename editor: macOS has a native field; elsewhere the tab strip paints it, so `tab_strip_rows = 0` leaves nowhere to put it.
    pub(crate) fn can_rename_session(&self, window: WindowId) -> bool {
        if self.tab_strip_enabled() {
            return true;
        }
        self._toolbars
            .get(&window)
            .is_some_and(|handle| self.apprt.can_present_tab_rename(handle))
    }

    /// Open the inline pin editor over `tab` of `window`, editing that tab's focused pane's session.
    pub(crate) fn begin_session_rename(&mut self, window: WindowId, tab: TabId) -> bool {
        self.begin_session_meta_edit(window, tab, MetaField::Title)
    }

    /// [`Self::begin_session_rename`] for any presentation field: seeded with the field's stored value, placeheld with the resolved label (title) or the field's name (role), and writing that field on commit.
    pub(crate) fn begin_session_meta_edit(
        &mut self,
        window: WindowId,
        tab: TabId,
        field: MetaField,
    ) -> bool {
        // A press on the Workers chip means the worker it shows, whose name it reads.
        let tab = self
            .windows
            .get(&window)
            .and_then(|ws| {
                let shown = ws.tab_set.active().filter(|active| active.parked)?;
                (ws.tab_set.home() == Some(tab)).then_some(shown.id)
            })
            .unwrap_or(tab);
        let Some(index) = self.tab_index_for_id(window, tab) else {
            aterm_log::info!("rename dropped: tab {tab} no longer exists in its window");
            return false;
        };
        let Some(session) = self.tab_terminal_session(window, index) else {
            aterm_log::info!("rename dropped: tab {tab} has no terminal session");
            return false;
        };
        let live = self
            .windows
            .get(&window)
            .and_then(|ws| ws.rename_edit.as_ref())
            .map(|edit| (edit.session, edit.tab, edit.field));
        if live == Some((session, tab, field)) {
            return true;
        }
        if live.is_some() {
            self.settle_rename_edit(window);
        }
        let seed = self
            .pool
            .get(session)
            .map(|s| s.ctx.clone())
            .and_then(|ctx| {
                ctx.meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .presentation_value(field.wire_name())
            })
            .unwrap_or_default();
        let placeholder = match field {
            MetaField::Title => self
                .tab_titles(window)
                .get(index)
                .cloned()
                .unwrap_or_default(),
            _ => field.wire_name().to_string(),
        };
        // A worker's editor stands over the chip that stands for it.
        let chip = self
            .windows
            .get(&window)
            .and_then(|ws| ws.tab_set.chip_of(tab))
            .unwrap_or(tab);
        let native = match self._toolbars.get(&window) {
            Some(handle) => self
                .apprt
                .begin_tab_rename(handle, chip, session, &seed, &placeholder),
            None => false,
        };
        let surface = if native {
            RenameSurface::Native
        } else if self.tab_strip_enabled() {
            RenameSurface::InGrid
        } else {
            aterm_log::info!(
                "rename dropped: no editor surface on this window (session {session}); \
                 the in-grid strip is off (tab_strip_rows = 0) and no native editor presented"
            );
            return false;
        };
        let cursor = seed.len();
        if let Some(ws) = self.windows.get_mut(&window) {
            ws.rename_edit = Some(TabRenameEdit {
                session,
                tab,
                surface,
                field,
                text: seed,
                cursor,
            });
        } else {
            return false;
        }
        self.request_rename_repaint(window);
        true
    }

    /// Open the editor over `window`'s active tab, the subject every entry point outside the tab context uses (menu bar, palette, `invoke`, keybinding).
    pub(crate) fn begin_active_session_rename(&mut self, window: WindowId) -> bool {
        self.begin_active_session_meta_edit(window, MetaField::Title)
    }

    /// [`Self::begin_active_session_rename`] for any field (Window ▸ Set Role… passes `MetaField::Role`).
    pub(crate) fn begin_active_session_meta_edit(
        &mut self,
        window: WindowId,
        field: MetaField,
    ) -> bool {
        let Some(tab) = self
            .windows
            .get(&window)
            .and_then(|ws| ws.tab_set.active_id())
        else {
            return false;
        };
        self.begin_session_meta_edit(window, tab, field)
    }

    /// Tear down `window`'s editor (platform and state) without writing: the unconditional half of every ending (Escape, a commit, a vanished tab, a superseding edit).
    pub(crate) fn end_session_rename_editor(&mut self, window: WindowId) {
        let had_edit = self
            .windows
            .get_mut(&window)
            .and_then(|ws| ws.rename_edit.take())
            .is_some();
        if let Some(handle) = self._toolbars.get(&window) {
            self.apprt.end_tab_rename(handle);
        }
        if had_edit
            && let Some(w) = self
                .windows
                .get(&window)
                .and_then(|ws| ws.os_window.as_ref())
        {
            w.request_redraw();
        }
    }

    /// Ask `window` to repaint: the in-grid field is drawn into the tab strip, so every keystroke needs a frame.
    pub(crate) fn request_rename_repaint(&mut self, window: WindowId) {
        if let Some(w) = self
            .windows
            .get(&window)
            .and_then(|ws| ws.os_window.as_ref())
        {
            w.request_redraw();
        }
    }

    /// Settle the live edit on `window`: end it the way leaving a field does, keeping what was typed (the Finder/Xcode rule the macOS editor follows at `controlTextDidEndEditing:`).
    pub(crate) fn settle_rename_edit(&mut self, window: WindowId) {
        let Some((session, surface, text)) = self
            .windows
            .get(&window)
            .and_then(|ws| ws.rename_edit.as_ref())
            .map(|edit| (edit.session, edit.surface, edit.text.clone()))
        else {
            return;
        };
        let text = match surface {
            RenameSurface::InGrid => Some(text),
            RenameSurface::Native => self
                ._toolbars
                .get(&window)
                .and_then(|handle| self.apprt.rename_editor_text(handle)),
        };
        match text {
            Some(text) => self.commit_session_rename(window, session, &text),
            None => self.end_session_rename_editor(window),
        }
    }

    /// Apply one field edit to `window`'s in-grid rename field and repaint.
    pub(crate) fn rename_field_edit(&mut self, window: WindowId, edit: SearchEdit) -> bool {
        let Some(ws) = self.windows.get_mut(&window) else {
            return false;
        };
        let Some(state) = ws
            .rename_edit
            .as_mut()
            .filter(|state| state.surface == RenameSurface::InGrid)
        else {
            return false;
        };
        apply_field_edit(&mut state.text, &mut state.cursor, edit);
        let canonical =
            crate::session_timeline::sanitize_presentation_line(&state.text, state.field.cap());
        if state.text.trim() != canonical {
            state.text = canonical;
            state.cursor = state.cursor.min(state.text.len());
            while state.cursor > 0 && !state.text.is_char_boundary(state.cursor) {
                state.cursor -= 1;
            }
        }
        self.request_rename_repaint(window);
        true
    }

    /// Paste the clipboard into `window`'s in-grid rename field (one line; the reducer strips control characters). A control client's gesture reads no clipboard ([`Self::refuse_clipboard_gesture`]).
    pub(crate) fn rename_paste_in(&mut self, window: WindowId) -> bool {
        if self.refuse_clipboard_gesture("rename-field paste") {
            return false;
        }
        #[cfg(target_os = "linux")]
        let text = crate::control::pbpaste_owned();
        #[cfg(not(target_os = "linux"))]
        let text = crate::control::pbpaste();
        let Some(text) = text else {
            return false;
        };
        self.rename_field_edit(window, SearchEdit::Insert(text))
    }

    /// Cancel the live edit iff it is still the one `session` named.
    pub(crate) fn cancel_session_rename(&mut self, window: WindowId, session: u64) {
        if self.rename_edit_session(window) != Some(session) {
            return;
        }
        self.end_session_rename_editor(window);
    }

    /// Commit the live edit: tear the editor down, then write the pin.
    pub(crate) fn commit_session_rename(&mut self, window: WindowId, session: u64, text: &str) {
        if self.rename_edit_session(window) != Some(session) {
            aterm_log::info!("rename commit dropped: session {session} is no longer being edited");
            return;
        }
        let field = self
            .windows
            .get(&window)
            .and_then(|ws| ws.rename_edit.as_ref())
            .map_or(MetaField::Title, |edit| edit.field);
        self.end_session_rename_editor(window);
        match self.write_session_meta_field(session, field, text) {
            Ok(_) => {}
            Err(error) => {
                aterm_log::info!("rename refused for session {session}: {error:?}");
            }
        }
    }

    /// Write one session's title pin (empty clears it) and refresh what it invalidates.
    #[cfg(test)]
    pub(crate) fn write_session_title_pin(
        &mut self,
        session: u64,
        text: &str,
    ) -> Result<bool, MetaWriteError> {
        self.write_session_meta_field(session, MetaField::Title, text)
    }

    /// `Self::write_session_title_pin` for any presentation field.
    pub(crate) fn write_session_meta_field(
        &mut self,
        session: u64,
        field: MetaField,
        text: &str,
    ) -> Result<bool, MetaWriteError> {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            return Ok(false);
        };
        let edit = if text.trim().is_empty() {
            MetaEdit::Clear
        } else {
            MetaEdit::Set(text)
        };
        let changed = write_session_meta(&ctx, field, edit, aterm_slog::Actor::Human)?;
        if changed {
            self.refresh_meta_dependent_chrome(session);
            self.rebuild_teams();
            if field != MetaField::Title {
                self.refresh_presence_session(session);
                self.mark_status_dirty();
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
#[path = "app_rename_tests.rs"]
mod tests;
