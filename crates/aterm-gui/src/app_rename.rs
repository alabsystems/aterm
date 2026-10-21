// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Inline session rename: the GUI face of the user pin (`meta set title`), the top rung of the
//! tab-label chain ([`crate::app_tabs::resolved_terminal_title_rung`]). Double-clicking a tab
//! (or Window ▸ Rename Session…, the `rename_session` keybinding, the tab context menu) opens
//! an editor over it; Return commits, Escape cancels, and an empty commit clears the pin so
//! the label falls back down the chain.
//!
//! macOS presents a real `NSTextField` over the chip and AppKit owns its characters; elsewhere
//! the in-grid strip paints the field and this state owns them. [`RenameSurface`] records
//! which is on screen, and an edit nothing can present is refused rather than stored (an
//! invisible modal would swallow commands).
//!
//! The pin is session metadata and a tab can hold several split sessions, so every entry point
//! edits the tab's focused pane's session, and the edit is keyed on that session id: tabs close
//! and reorder while events are queued, and the strip re-stamps a chip's tab id per position.
//!
//! `App::commit_session_rename` writes through
//! [`crate::session_timeline::write_session_meta`] (the `meta` verb's validation, atomicity
//! and `changed` gate), then runs the chrome refresh directly (the record wakes the
//! session's watchers): posting `Wake::MetaChanged` would run the refresh twice.
//!
//! A pin is not synchronously durable: the restore manifest is written on graceful,
//! non-headless shutdown, so a crash between a rename and a quit loses it.

use crate::app_search::{SearchEdit, apply_field_edit};
use crate::platform::AppRt;
use crate::session_timeline::{MetaEdit, MetaField, MetaWriteError, write_session_meta};
use crate::tab_model::TabId;
use crate::{App, WindowId};

/// Which surface presents a live edit, and so who owns the text: the macOS `NSTextField`'s
/// field editor, or this state for the in-grid field [`crate::tab_bar`] paints. The state says
/// which rather than letting consumers re-derive it from the platform, so a command never
/// reads the wrong text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenameSurface {
    /// The macOS strip's `NSTextField` overlay. `text`/`cursor` hold only the seed; the live
    /// text is in AppKit, read back with `rename_editor_text`.
    Native,
    /// The cell-drawn field the in-grid strip paints over the tab's title; `text`/`cursor` are
    /// the field.
    InGrid,
}

/// A live inline rename on one window's tab strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabRenameEdit {
    /// The session whose value is edited, resolved once at begin from the tab's focused pane;
    /// the only thing a commit applies to.
    pub session: u64,
    /// The tab the editor is painted over, for positioning only: how the native editor
    /// follows a reorder and learns its tab closed. Never a commit target.
    pub tab: TabId,
    /// Which editor is on screen, set at begin from what presented and never re-derived.
    pub surface: RenameSurface,
    /// The metadata field the commit writes: the title pin (Rename Session…, the double-click)
    /// or the role (Window ▸ Set Role…). Fixed at begin.
    pub field: MetaField,
    /// The in-grid field's text: seeded from the stored value (empty when unset), changed only
    /// through [`crate::app_search::apply_field_edit`], the find bar's reducer.
    pub text: String,
    /// Caret position in [`Self::text`], a byte offset on a char boundary.
    pub cursor: usize,
}

impl App {
    /// The session being renamed in `window`, or `None` when no editor is open: whether keys
    /// and commands belong to a text field rather than the terminal.
    pub(crate) fn rename_edit_session(&self, window: WindowId) -> Option<u64> {
        Some(self.windows.get(&window)?.rename_edit.as_ref()?.session)
    }

    /// The live edit on `window` when the in-grid strip presents it: the gate every
    /// own-rendered path (key routing, strip paint, strip clicks) asks, so none acts on a
    /// macOS field whose text it does not own.
    pub(crate) fn inline_rename_edit(&self, window: WindowId) -> Option<&TabRenameEdit> {
        self.windows
            .get(&window)?
            .rename_edit
            .as_ref()
            .filter(|edit| edit.surface == RenameSurface::InGrid)
    }

    /// The window with a live inline rename, if any: the menu dispatch asks before letting a
    /// key equivalent reach the terminal.
    pub(crate) fn any_rename_edit(&self) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, ws)| ws.rename_edit.is_some())
            .map(|(wid, _)| *wid)
    }

    /// The front window's live edit, if it has one. A menu command belongs to the front
    /// window, and AppKit does not end field editing when a window stops being key, so an
    /// editor left open in a background window must not swallow the front's ⌘C or be
    /// committed by its ⌘W. `focus_order`'s last entry is the front; before any OS focus event
    /// (headless, tests) any live edit counts.
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

    /// Whether `window` can present an inline rename editor: macOS has a native field;
    /// elsewhere the tab strip paints it, so `tab_strip_rows = 0` leaves nowhere to put it.
    /// Surfaces grey the command out on `false`.
    pub(crate) fn can_rename_session(&self, window: WindowId) -> bool {
        if self.tab_strip_enabled() {
            return true;
        }
        self._toolbars
            .get(&window)
            .is_some_and(|handle| self.apprt.can_present_tab_rename(handle))
    }

    /// Open the inline pin editor over `tab` of `window`, editing that tab's focused pane's
    /// session. Returns whether an edit is now live.
    ///
    /// The seed is the pin itself (empty when unpinned), never the chip's presented label, so a
    /// first Return cannot pin an OSC-derived title as if the user typed it; the resolved label
    /// is the placeholder, so an empty field visibly means "fall back to that".
    ///
    /// A second begin is idempotent for the same session, chip and field, and otherwise
    /// settles (commits) the live edit before opening the new one.
    pub(crate) fn begin_session_rename(&mut self, window: WindowId, tab: TabId) -> bool {
        self.begin_session_meta_edit(window, tab, MetaField::Title)
    }

    /// [`Self::begin_session_rename`] for any presentation field: seeded with the field's
    /// stored value, placeheld with the resolved label (title) or the field's name (role),
    /// and writing that field on commit.
    pub(crate) fn begin_session_meta_edit(
        &mut self,
        window: WindowId,
        tab: TabId,
        field: MetaField,
    ) -> bool {
        let Some(index) = self.tab_index_for_id(window, tab) else {
            aterm_log::info!("rename dropped: tab {tab} no longer exists in its window");
            return false;
        };
        let Some(session) = self.tab_terminal_session(window, index) else {
            // A native whole tab owns no session. The menu and palette rows are greyed for
            // it; the double-click path can still land here.
            aterm_log::info!("rename dropped: tab {tab} has no terminal session");
            return false;
        };
        let live = self
            .windows
            .get(&window)
            .and_then(|ws| ws.rename_edit.as_ref())
            .map(|edit| (edit.session, edit.tab, edit.field));
        // Same session, chip and field: nothing to do, so a repeat double-click does not
        // reopen the field under the caret. Compared whole so a session shown on another tab
        // re-presents (the editor moves) and a Set Role… over an open rename settles it first.
        if live == Some((session, tab, field)) {
            return true;
        }
        // Replacing a different live edit settles (commits) it, like every other way of
        // leaving the field. Settling also tears the old editor down, so the platform never
        // holds two fields and the old one cannot post a late commit.
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
            // The role has no label chain to fall back down: an empty field
            // means "no role", and the placeholder says which field this is.
            _ => field.wire_name().to_string(),
        };
        // The native editor first, where it exists; the in-grid strip only when it is on
        // screen (`tab_strip_rows > 0`).
        let native = match self._toolbars.get(&window) {
            Some(handle) => self
                .apprt
                .begin_tab_rename(handle, tab, session, &seed, &placeholder),
            None => false,
        };
        let surface = if native {
            RenameSurface::Native
        } else if self.tab_strip_enabled() {
            RenameSurface::InGrid
        } else {
            // Nothing would paint this edit: keeping the state would be an invisible modal
            // that swallows commands and nothing can end (headless included).
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
        // The in-grid field is strip paint: it needs the repaint AppKit gives the native one.
        self.request_rename_repaint(window);
        true
    }

    /// Open the editor over `window`'s active tab, the subject every entry point outside the
    /// tab context uses (menu bar, palette, `invoke`, keybinding).
    pub(crate) fn begin_active_session_rename(&mut self, window: WindowId) -> bool {
        self.begin_active_session_meta_edit(window, MetaField::Title)
    }

    /// [`Self::begin_active_session_rename`] for any field (Window ▸ Set Role… passes
    /// `MetaField::Role`).
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

    /// Tear down `window`'s editor (platform and state) without writing: the unconditional
    /// half of every ending (Escape, a commit, a vanished tab, a superseding edit).
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
            // The editor was painted, so its removal repaints even when nothing was
            // written (Escape, a no-op commit).
            w.request_redraw();
        }
    }

    /// Ask `window` to repaint: the in-grid field is drawn into the tab strip, so every
    /// keystroke needs a frame. The strip's caches see the edit through
    /// [`crate::App::tab_strip_fingerprint_from_parts`], which hashes it.
    pub(crate) fn request_rename_repaint(&mut self, window: WindowId) {
        if let Some(w) = self
            .windows
            .get(&window)
            .and_then(|ws| ws.os_window.as_ref())
        {
            w.request_redraw();
        }
    }

    /// Settle the live edit on `window`: end it the way leaving a field does, keeping what was
    /// typed (the Finder/Xcode rule the macOS editor follows at `controlTextDidEndEditing:`).
    /// A menu-bar command, a tab context-menu command, a click on another chip and a click
    /// outside the strip all come here; Escape and a vanished tab are the only cancels.
    ///
    /// The text comes from its owner: AppKit's field editor for a native edit, the state for an
    /// in-grid one. A native edit with no readable field ends without writing.
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

    /// Apply one field edit to `window`'s in-grid rename field and repaint. Returns whether an
    /// in-grid edit was live. The reducer is the find bar's ([`apply_field_edit`]), so the two
    /// fields agree on caret motion, word kills and char boundaries.
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
        // A caret move repaints too: the caret is a painted reverse-video cell.
        apply_field_edit(&mut state.text, &mut state.cursor, edit);
        // Canonicalize as the user types, as the macOS field editor does
        // (`controlTextDidChange:`): the field holds only values the store accepts, so a full
        // field simply stops taking more. Compared trimmed, like the native one, so a space
        // typed mid-word does not move the caret.
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

    /// Paste the clipboard into `window`'s in-grid rename field (one line; the reducer strips
    /// control characters). The field owns the keystroke, so nothing reaches the shell.
    ///
    /// On X11 only an own-selection read is instant; a foreign owner needs a
    /// `ConvertSelection` round-trip that can park the loop for ~1 s, so this synchronous path
    /// does not paste from a foreign owner. The `⌘V` is swallowed either way.
    pub(crate) fn rename_paste_in(&mut self, window: WindowId) -> bool {
        #[cfg(target_os = "linux")]
        let text = crate::control::pbpaste_owned();
        #[cfg(not(target_os = "linux"))]
        let text = crate::control::pbpaste();
        let Some(text) = text else {
            return false;
        };
        self.rename_field_edit(window, SearchEdit::Insert(text))
    }

    /// Cancel the live edit iff it is still the one `session` named. A cancel for
    /// a stale session is dropped: the editor it belonged to is already gone.
    pub(crate) fn cancel_session_rename(&mut self, window: WindowId, session: u64) {
        if self.rename_edit_session(window) != Some(session) {
            return;
        }
        self.end_session_rename_editor(window);
    }

    /// Commit the live edit: tear the editor down, then write the pin.
    ///
    /// Empty or whitespace-only `text` clears the pin with [`MetaEdit::Clear`], never
    /// `Set("")`: both store `None`, but only `Clear` records the documented `-` marker.
    ///
    /// A commit that lost a race (the tab closed mid-edit, another edit replaced this one) is
    /// dropped instead of landing on the wrong session.
    pub(crate) fn commit_session_rename(&mut self, window: WindowId, session: u64, text: &str) {
        if self.rename_edit_session(window) != Some(session) {
            aterm_log::info!("rename commit dropped: session {session} is no longer being edited");
            return;
        }
        // Read the field before teardown, so a Set Role… commit never lands on the title.
        let field = self
            .windows
            .get(&window)
            .and_then(|ws| ws.rename_edit.as_ref())
            .map_or(MetaField::Title, |edit| edit.field);
        self.end_session_rename_editor(window);
        match self.write_session_meta_field(session, field, text) {
            Ok(_) => {}
            Err(error) => {
                // Both editors canonicalize as the user types, so only a scripted `invoke`
                // can reach this.
                aterm_log::info!("rename refused for session {session}: {error:?}");
            }
        }
    }

    /// Write one session's title pin (empty clears it) and refresh what it invalidates.
    /// Returns whether the stored value moved.
    ///
    /// The GUI twin of the control socket's `meta` arm over the same typed API:
    /// `write_session_meta` validates, stores and records, and returns with every guard
    /// released, which matters because the refresh re-takes `ctx.meta` per tab on this thread
    /// and `std::sync::Mutex` is not reentrant. The fan-out runs only on an actual change.
    #[cfg(test)]
    pub(crate) fn write_session_title_pin(
        &mut self,
        session: u64,
        text: &str,
    ) -> Result<bool, MetaWriteError> {
        self.write_session_meta_field(session, MetaField::Title, text)
    }

    /// [`Self::write_session_title_pin`] for any presentation field. A write
    /// that moved one renames the session where its label is read: the
    /// workers' Team chips and strip marks of a manager
    /// ([`Self::rebuild_teams`]) and a needs pill naming it
    /// ([`Self::reproject_pill_of`]); a ROLE write additionally refreshes its
    /// presence and the operator status item — the same fan-out the
    /// `Wake::MetaChanged` arm runs for a wire `meta set`, called directly for
    /// the reason the title path gives above.
    pub(crate) fn write_session_meta_field(
        &mut self,
        session: u64,
        field: MetaField,
        text: &str,
    ) -> Result<bool, MetaWriteError> {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            // The session exited while the editor was open: nothing left to name.
            return Ok(false);
        };
        let edit = if text.trim().is_empty() {
            MetaEdit::Clear
        } else {
            MetaEdit::Set(text)
        };
        // The window's own editor: the person's.
        let changed = write_session_meta(&ctx, field, edit, aterm_slog::Actor::Human)?;
        if changed {
            // The record landed under the meta guard, so the chrome cache's `high_id` gate
            // moved and the tooltip and context menu recompose with the label.
            self.refresh_meta_dependent_chrome(session);
            self.rebuild_teams();
            if field != MetaField::Title {
                self.refresh_presence_session(session);
                self.mark_status_dirty();
            }
            self.reproject_pill_of(session);
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_timeline::MetaWriteError;

    use crate::session_pin_for_test as pin;

    /// The recorded `meta-change` payloads, oldest-first.
    fn meta_events(app: &App, session: u64) -> Vec<String> {
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let tl = ctx.timeline.lock().unwrap();
        tl.since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.to_string())
            .collect()
    }

    /// A commit pins the title, the tab label follows, one `meta-change` is recorded, and the
    /// editor closes.
    #[test]
    fn a_commit_pins_the_title_and_closes_the_editor() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        assert!(app.begin_active_session_rename(wid));
        assert_eq!(app.rename_edit_session(wid), Some(0));

        app.commit_session_rename(wid, 0, "build agent");
        assert_eq!(pin(&app, 0).as_deref(), Some("build agent"));
        assert_eq!(app.tab_titles(wid)[0], "build agent");
        assert_eq!(app.rename_edit_session(wid), None, "the editor closed");
        assert_eq!(
            meta_events(&app, 0),
            vec!["field=title value=build%20agent"]
        );
    }

    /// Renaming a different tab while one edit is live commits the first, like every other way
    /// of leaving a field.
    #[test]
    fn opening_another_rename_commits_the_one_it_replaces() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        let first = app.windows[&wid].tab_set.tabs()[0].id;
        assert!(app.begin_session_rename(wid, first));
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("x".to_string()));

        // Re-opening over the same chip is a no-op, so the replace needs a second tab.
        app.push_stub_tab(wid, crate::stub_session(1));
        let second = app.windows[&wid].tab_set.tabs()[1].id;
        assert!(app.begin_session_rename(wid, second));
        assert_eq!(
            pin(&app, 0).as_deref(),
            Some("x"),
            "the replaced edit was committed, not dropped"
        );
        assert_eq!(
            app.rename_edit_session(wid),
            Some(1),
            "the new edit is live on the second tab"
        );
    }

    /// Escape writes and records nothing and closes the editor; an existing pin survives.
    #[test]
    fn a_cancel_writes_nothing_and_leaves_an_existing_pin_alone() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        assert!(app.begin_active_session_rename(wid));
        app.commit_session_rename(wid, 0, "first");
        assert_eq!(pin(&app, 0).as_deref(), Some("first"));

        assert!(app.begin_active_session_rename(wid));
        app.cancel_session_rename(wid, 0);
        assert_eq!(
            pin(&app, 0).as_deref(),
            Some("first"),
            "cancel wrote nothing"
        );
        assert_eq!(app.rename_edit_session(wid), None);
        assert_eq!(meta_events(&app, 0).len(), 1, "cancel recorded nothing");
    }

    /// An empty commit clears the pin: the label falls back to the OSC title and the event
    /// says `value=-`, the cleared marker, not the `value=` a `Set("")` would record.
    #[test]
    fn an_empty_commit_clears_the_pin_and_the_ladder_falls_back() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        // The real OSC 2 path, so the fallback rung is live.
        crate::term_lock(&app.pool.get(0).expect("session 0").term)
            .process(b"\x1b]2;vim src/main.rs\x07");
        assert!(app.begin_active_session_rename(wid));
        app.commit_session_rename(wid, 0, "build agent");
        assert_eq!(app.tab_titles(wid)[0], "build agent");

        assert!(app.begin_active_session_rename(wid));
        app.commit_session_rename(wid, 0, "   ");
        assert_eq!(pin(&app, 0), None, "whitespace-only commits CLEAR the pin");
        assert_eq!(
            app.tab_titles(wid)[0],
            "vim src/main.rs",
            "the ladder falls back to the OSC title"
        );
        assert_eq!(
            meta_events(&app, 0),
            vec!["field=title value=build%20agent", "field=title value=-"],
        );
    }

    /// A commit queued behind its tab's close writes nothing, and a begin against the dead tab
    /// id is refused rather than retargeted at whatever tab holds the slot now.
    #[test]
    fn a_close_during_the_edit_drops_the_commit_instead_of_retargeting() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        let tab = app.windows[&wid].tab_set.active_id().expect("a tab");
        assert!(app.begin_session_rename(wid, tab));

        // The strip sees its edited tab gone and cancels; the queued commit lands after.
        app.cancel_session_rename(wid, 0);
        app.commit_session_rename(wid, 0, "ghost");
        assert_eq!(
            pin(&app, 0),
            None,
            "a commit with no live edit writes nothing"
        );
        assert!(meta_events(&app, 0).is_empty());

        // And the stale tab id never resolves to a different tab.
        app.windows.get_mut(&wid).unwrap().rename_edit = None;
        let gone = crate::tab_model::TabId::from_stored(u64::MAX);
        assert!(!app.begin_session_rename(wid, gone));
        assert_eq!(app.rename_edit_session(wid), None);
    }

    /// No menu command runs through an open editor: macOS resolves a key equivalent before
    /// the first responder sees the key, so a structural command settles the edit first, and
    /// `RenameSession` itself leaves the open field alone.
    #[test]
    fn a_menu_command_ends_the_edit_before_it_runs_and_rename_stays_idempotent() {
        use crate::menu::MenuAction;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        assert!(app.begin_active_session_rename(wid));

        assert!(
            !app.divert_menu_action_around_rename(MenuAction::RenameSession),
            "the rename command is never diverted away from itself"
        );
        assert_eq!(
            app.rename_edit_session(wid),
            Some(0),
            "re-issuing Rename Session leaves the open editor alone"
        );

        assert!(
            !app.divert_menu_action_around_rename(MenuAction::CloseTab),
            "a structural command still runs — after the edit is settled"
        );
        assert_eq!(
            app.rename_edit_session(wid),
            None,
            "the editor is gone before Close Tab acts on the tab it was over"
        );
    }

    /// The pin write refuses rather than truncates (the control socket's ladder), and a
    /// refusal leaves the store untouched.
    #[test]
    fn an_over_cap_pin_is_refused_and_stores_nothing() {
        let mut app = App::headless_for_test();
        let over = "x".repeat(crate::session_timeline::META_TITLE_MAX + 1);
        assert_eq!(
            app.write_session_title_pin(0, &over),
            Err(MetaWriteError::TooLong {
                cap: crate::session_timeline::META_TITLE_MAX
            })
        );
        assert_eq!(pin(&app, 0), None);
        assert!(meta_events(&app, 0).is_empty());
    }

    /// With no native editor and no in-grid strip (`tab_strip_rows == 0`, the macOS default and
    /// every plain `headless_for_test`), a rename is refused outright and stores no edit that
    /// nothing could end.
    #[test]
    fn a_surface_less_window_refuses_the_edit_instead_of_storing_a_phantom() {
        use crate::menu::MenuAction;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert_eq!(app.tab_strip_rows, 0, "negative control: no strip");
        assert!(
            !app.can_rename_session(wid),
            "the menu/palette capability agrees with the write path"
        );

        assert!(
            !app.begin_active_session_rename(wid),
            "nothing can present this edit, so it is refused"
        );
        assert_eq!(app.rename_edit_session(wid), None, "and nothing is stored");

        // The divert path is clear: the next command runs.
        assert!(!app.divert_menu_action_around_rename(MenuAction::CloseTab));

        // With the strip on, the same call succeeds: the refusal is about presentation.
        app.tab_strip_rows = 1;
        assert!(app.can_rename_session(wid), "a visible strip is a surface");
        assert!(app.begin_active_session_rename(wid));
        assert_eq!(app.rename_edit_session(wid), Some(0));
        assert!(
            app.inline_rename_edit(wid).is_some(),
            "the in-grid strip is what presents it"
        );
    }

    /// The in-grid editor is driven and ended through the engine-neutral input seam (the one
    /// controller `key`/`text` verbs arrive on), so a headless instance can use it.
    #[test]
    fn the_input_seam_drives_the_in_grid_field_and_commits_it() {
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        assert!(app.begin_active_session_rename(wid));

        assert!(app.rename_input_event(wid, &InputEvent::Text("build agent".into())));
        assert_eq!(
            app.inline_rename_edit(wid).map(|edit| edit.text.as_str()),
            Some("build agent")
        );
        // A caret motion and a delete, through the shared field reducer.
        let key = |named: NamedKey| InputEvent::Key {
            key: Key::Named(named),
            mods: Modifiers::empty(),
            event_type: KeyEventType::Press,
            base_layout: None,
            layout_text: None,
        };
        assert!(app.rename_input_event(wid, &key(NamedKey::ArrowLeft)));
        assert!(app.rename_input_event(wid, &key(NamedKey::Backspace)));
        assert_eq!(
            app.inline_rename_edit(wid).map(|edit| edit.text.as_str()),
            Some("build aget")
        );

        assert!(app.rename_input_event(wid, &key(NamedKey::Enter)));
        assert_eq!(pin(&app, 0).as_deref(), Some("build aget"));
        assert_eq!(app.rename_edit_session(wid), None, "the editor closed");
    }

    /// ⎋ through the same seam cancels and writes nothing.
    #[test]
    fn the_input_seam_cancels_on_escape_and_writes_nothing() {
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key, KeyEventType, Modifiers, NamedKey};
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        assert!(app.begin_active_session_rename(wid));
        assert!(app.rename_input_event(wid, &InputEvent::Text("discard me".into())));
        assert!(app.rename_input_event(
            wid,
            &InputEvent::Key {
                key: Key::Named(NamedKey::Escape),
                mods: Modifiers::empty(),
                event_type: KeyEventType::Press,
                base_layout: None,
                layout_text: None,
            }
        ));
        assert_eq!(app.rename_edit_session(wid), None);
        assert_eq!(pin(&app, 0), None);
        assert!(meta_events(&app, 0).is_empty());
    }

    /// The field canonicalizes as the user types, like the macOS field editor: a mid-word space
    /// survives and an over-cap paste is clamped, so the field only holds storable values.
    #[test]
    fn the_in_grid_field_holds_only_storable_values() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        assert!(app.begin_active_session_rename(wid));

        app.rename_field_edit(wid, SearchEdit::Insert("build ".into()));
        assert_eq!(
            app.inline_rename_edit(wid).map(|edit| edit.text.as_str()),
            Some("build "),
            "a trailing space mid-typing is kept — it is about to become a word"
        );
        app.rename_field_edit(
            wid,
            SearchEdit::Insert("x".repeat(crate::session_timeline::META_TITLE_MAX + 10)),
        );
        let text = app
            .inline_rename_edit(wid)
            .map(|edit| edit.text.clone())
            .expect("still editing");
        assert_eq!(text.len(), crate::session_timeline::META_TITLE_MAX);

        let session = app.rename_edit_session(wid).expect("editing");
        app.commit_session_rename(wid, session, &text);
        assert_eq!(
            pin(&app, 0).as_deref(),
            Some(text.as_str()),
            "the field never held a value the store would refuse"
        );
    }

    /// The strip's repaint fingerprint carries the live in-grid field, so the RepaintKey
    /// early-out and the painted-row cache both see every keystroke.
    #[test]
    fn a_keystroke_in_the_field_moves_the_strip_fingerprint() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        let fingerprint = |app: &mut App| {
            let titles = app.tab_titles(wid);
            let metadata = app.tab_strip_metadata(wid);
            app.tab_strip_fingerprint_from_parts(wid, &titles, &metadata, 0)
        };
        let idle = fingerprint(&mut app);
        assert!(app.begin_active_session_rename(wid));
        let opened = fingerprint(&mut app);
        assert_ne!(idle, opened, "opening the editor repaints the strip");

        app.rename_field_edit(wid, SearchEdit::Insert("a".into()));
        let typed = fingerprint(&mut app);
        assert_ne!(opened, typed, "a typed character repaints the strip");

        // A caret move is painted state too.
        app.rename_field_edit(wid, SearchEdit::MoveStart);
        assert_ne!(typed, fingerprint(&mut app), "the caret is painted state");
    }
}
