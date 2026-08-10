// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! INLINE SESSION RENAME — the GUI face of the user pin (`meta set title`).
//!
//! The pin already existed end to end: it is the TOP rung of the tab-label
//! chain ([`crate::app_tabs::resolved_terminal_title_rung`]), it survives
//! restore, and the control socket has written it since stage 1 of session
//! metadata. What was missing was a way for a HUMAN to set it. This module is
//! the window-side state machine for that: double-clicking a tab (or Window ▸
//! Rename Session… / the `rename_session` keybinding / the tab context menu)
//! opens an editor over the tab; Return commits, Escape cancels, and an EMPTY
//! commit CLEARS the pin so the label falls back down the ladder.
//!
//! ## Two editors, one state machine
//!
//! macOS presents a real `NSTextField` over the chip and AppKit owns its
//! characters. Everywhere else the in-grid strip paints the field itself — a
//! recessed well with a reverse-video block caret, the find bar's idiom — and
//! this state owns the characters. [`RenameSurface`] records which one is on
//! screen so no consumer has to guess, and an edit NOTHING can present is
//! refused rather than stored: an invisible modal that swallows commands is
//! worse than a rename that did not happen.
//!
//! ## It renames a SESSION, not a tab
//!
//! The pin is session metadata and a tab can hold several split sessions, so
//! every entry point resolves the tab's FOCUSED pane and edits THAT session.
//! The edit state is keyed on the session id for the same reason it is not
//! keyed on `(window, tab index)`: tabs close and reorder while events are
//! queued, and the strip re-stamps a chip's tab id per POSITION on its in-place
//! diff path. A tab id is carried alongside, but only ever for POSITIONING the
//! editor and for noticing that the edited tab is gone.
//!
//! ## The write goes through the typed API, never a control string
//!
//! [`Self::commit_session_rename`] calls
//! [`crate::session_timeline::write_session_meta`] — the same validation ladder,
//! the same store-then-record-under-the-meta-guard atomicity, and the same
//! `changed` gate the `meta` verb uses — then fans out the two side effects the
//! control dispatch arm owns (chrome refresh + subscriber notify). It calls the
//! refresh DIRECTLY instead of posting `Wake::MetaChanged`, because posting
//! would run the whole refresh twice for one edit.
//!
//! DURABILITY (accepted, documented): a pin is not synchronously durable. The
//! restore manifest is written on graceful, non-headless shutdown, so a crash
//! between a rename and a quit loses it.

use crate::app_search::{SearchEdit, apply_field_edit};
use crate::platform::AppRt;
use crate::session_timeline::{MetaEdit, MetaField, MetaWriteError, write_session_meta};
use crate::tab_model::TabId;
use crate::{App, WindowId};

/// WHICH surface is presenting a live edit — and therefore who owns the text and
/// who can end it.
///
/// The two editors are genuinely different machines, so the state says which one
/// it is rather than letting every consumer re-derive it from the platform: the
/// macOS field is a real `NSTextField` whose field editor owns the characters and
/// whose first responder AppKit drives, while the in-grid field is painted by
/// [`crate::tab_bar`] from text this state carries and driven by aterm's own key
/// gate. Mixing them up is how a command reads the wrong text.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RenameSurface {
    /// The macOS strip's `NSTextField` overlay. `text`/`cursor` hold the SEED only;
    /// the live text lives in AppKit and is read back with `rename_editor_text`.
    Native,
    /// The cell-drawn field the in-grid tab strip paints over the tab's title span.
    /// `text`/`cursor` ARE the field.
    InGrid,
}

/// A live inline rename on one window's tab strip.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TabRenameEdit {
    /// The session whose pin is being edited — resolved ONCE at begin from the
    /// tab's focused pane, and the only thing a commit is ever applied to.
    pub session: u64,
    /// The tab the editor is painted over. POSITIONING ONLY: the strip is handed
    /// no session identity, so this is how the native editor follows a reorder
    /// and how it learns its tab has closed. Never a commit target — a tab is a
    /// container and the pin belongs to one of the sessions inside it.
    pub tab: TabId,
    /// Which editor is on screen. Set at begin from what actually presented, and
    /// never re-derived: an edit that nothing presents is refused, not stored.
    pub surface: RenameSurface,
    /// The in-grid field's text. Seeded from the pin (EMPTY when unpinned) and
    /// mutated only through [`crate::app_search::apply_field_edit`], the same
    /// reducer the find bar's query runs on.
    pub text: String,
    /// Caret position in [`Self::text`], as a BYTE offset on a char boundary — the
    /// invariant `apply_field_edit` maintains.
    pub cursor: usize,
}

impl App {
    /// The session being renamed in `window`, or `None` when no editor is open.
    /// Callers use this to know that keystrokes/commands belong to a text field
    /// rather than to the terminal.
    pub(crate) fn rename_edit_session(&self, window: WindowId) -> Option<u64> {
        Some(self.windows.get(&window)?.rename_edit.as_ref()?.session)
    }

    /// The live edit on `window` when the IN-GRID strip is the surface presenting
    /// it — the gate every own-rendered path (key routing, strip paint, strip
    /// clicks) asks, so none of them can act on a macOS `NSTextField` whose text
    /// they do not own.
    pub(crate) fn inline_rename_edit(&self, window: WindowId) -> Option<&TabRenameEdit> {
        self.windows
            .get(&window)?
            .rename_edit
            .as_ref()
            .filter(|edit| edit.surface == RenameSurface::InGrid)
    }

    /// Whether ANY window has a live inline rename — the gate the menu dispatch
    /// consults before letting a key equivalent reach the terminal.
    pub(crate) fn any_rename_edit(&self) -> Option<WindowId> {
        self.windows
            .iter()
            .find(|(_, ws)| ws.rename_edit.is_some())
            .map(|(wid, _)| *wid)
    }

    /// The FRONT window's live edit, if it has one. A menu command belongs to
    /// the front window, and AppKit does not end field editing when a window
    /// stops being key — so an editor left open in a background window must not
    /// swallow the front window's ⌘C, nor have its pin committed by a ⌘W aimed
    /// at a different window. `focus_order`'s last entry is the frontmost;
    /// before any OS focus event arrives (headless, tests) fall back to any
    /// live edit, since there is no front to disagree with.
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

    /// Open the inline pin editor over `tab` of `window`, editing that tab's
    /// FOCUSED pane's session. Returns whether an edit is now live.
    ///
    /// Seeding comes from HERE, not from the strip: the chip only knows its
    /// COMPOSED label (and paints a further ⌘-hinted decoration of it), so
    /// seeding from the view would let the first Return pin an OSC-derived
    /// display string as if the user had typed it. The field is seeded with the
    /// PIN (empty when unpinned) and placeheld with the resolved label, so an
    /// empty field visibly means "fall back to that".
    ///
    /// A second begin over a live edit is idempotent when it names the same
    /// session, and replaces the edit otherwise (committing nothing — the user
    /// moved on deliberately).
    /// Whether `window` can PRESENT an inline rename editor at all. macOS has a
    /// native field; everywhere else the editor is painted by the tab strip, so
    /// a window running with `tab_strip_rows = 0` has nowhere to put it. Surfaces
    /// consult this so the command greys out instead of accepting a click and
    /// doing nothing — the affordance has to be as honest as the write path.
    pub(crate) fn can_rename_session(&self, window: WindowId) -> bool {
        if self.tab_strip_enabled() {
            return true;
        }
        self._toolbars
            .get(&window)
            .is_some_and(|handle| self.apprt.can_present_tab_rename(handle))
    }

    pub(crate) fn begin_session_rename(&mut self, window: WindowId, tab: TabId) -> bool {
        let Some(index) = self.tab_index_for_id(window, tab) else {
            aterm_log::info!("rename dropped: tab {tab} no longer exists in its window");
            return false;
        };
        let Some(session) = self.tab_terminal_session(window, index) else {
            // A native whole tab (Settings/Markdown/editor) owns no session, so
            // there is no pin to edit. The menu/palette rows grey out for this
            // reason; the double-click path can still land here.
            aterm_log::info!("rename dropped: tab {tab} has no terminal session");
            return false;
        };
        let live = self
            .windows
            .get(&window)
            .and_then(|ws| ws.rename_edit.as_ref())
            .map(|edit| (edit.session, edit.tab));
        // Same session AND same chip ⇒ nothing to do. Compared as a whole so a
        // session shown on a different tab still re-presents (the editor has to
        // move); a repeat double-click on the tab already being edited does not
        // close-and-reopen the field under the user's caret.
        if live == Some((session, tab)) {
            return true;
        }
        // Replacing a different live edit SETTLES it rather than dropping it:
        // every other way of leaving a field — Return, Tab, clicking away, a
        // diverted command — commits, and this was the one exit that silently
        // discarded what the user had typed. Settling also tears the old editor
        // down, so the platform never holds two fields and the abandoned one
        // cannot post a late commit against a session we no longer edit.
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
                    .presentation_value("title")
            })
            .unwrap_or_default();
        let placeholder = self
            .tab_titles(window)
            .get(index)
            .cloned()
            .unwrap_or_default();
        // The native editor is asked FIRST — where it exists it is a real text
        // field with a real field editor, which no cell-drawn imitation matches.
        // The in-grid strip is the fallback, and it presents only when it is
        // actually on screen (`tab_strip_rows > 0`).
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
            // Nothing will paint this edit, so keeping the state would leave the
            // app in an invisible modal mode that swallows commands — including
            // under real `--headless`, where the old `!self.headless` escape
            // hatch installed exactly that: a phantom nothing could ever end,
            // permanently poisoning the menu-divert path.
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
                text: seed,
                cursor,
            });
        } else {
            return false;
        }
        // The in-grid field IS the strip paint, so it needs the repaint the native
        // overlay gets from AppKit.
        self.request_rename_repaint(window);
        true
    }

    /// Open the editor over `window`'s ACTIVE tab — the subject convention every
    /// non-tab-context entry point uses (menu bar, palette, `invoke`, keybinding).
    pub(crate) fn begin_active_session_rename(&mut self, window: WindowId) -> bool {
        let Some(tab) = self
            .windows
            .get(&window)
            .and_then(|ws| ws.tab_set.active_id())
        else {
            return false;
        };
        self.begin_session_rename(window, tab)
    }

    /// Tear down the editor for `window` (platform teardown + state) without
    /// writing anything. The unconditional half of every ending: Escape, a
    /// commit, a vanished tab, and a superseding edit all run it.
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
            // The editor itself was painted, so its removal needs a repaint even
            // when nothing was written (Escape, a no-op commit). Without this the
            // caret can linger until some unrelated wake.
            w.request_redraw();
        }
    }

    /// Ask `window` to repaint. The in-grid field is drawn INTO the tab strip, so
    /// every keystroke has to move a frame; the strip's own caches already learn
    /// about the edit through
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

    /// SETTLE the live edit on `window`: end it the way LEAVING a field ends it —
    /// keeping what was typed.
    ///
    /// This is the one implementation of the Finder/Xcode rule the macOS editor
    /// already followed at `controlTextDidEndEditing:` ("Return, Tab and a click
    /// away all commit"). It exists as a function because four different exits
    /// need it and used to disagree: a menu-bar command, a tab CONTEXT-menu
    /// command, a click on a different tab chip, and a click outside the strip.
    /// Escape and a vanished tab are the only cancels, and neither comes here.
    ///
    /// The text comes from whoever owns it — AppKit's field editor for a native
    /// edit, the state itself for an in-grid one. A native edit with no readable
    /// field is ended WITHOUT inventing text to write.
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

    /// Apply one single-line field edit to `window`'s IN-GRID rename field and
    /// repaint. Returns whether an in-grid edit was live to receive it.
    ///
    /// The reducer is [`apply_field_edit`] — the very one the find bar's query
    /// runs on — so the two in-grid fields cannot drift on caret motion, word
    /// kills, or char-boundary safety.
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
        // A pure caret move still repaints: the caret IS painted state here (a
        // reverse-video block on the cell it sits on), not a separate blinking
        // overlay the renderer maintains.
        apply_field_edit(&mut state.text, &mut state.cursor, edit);
        // CANONICALIZE AS THE USER TYPES, exactly as the macOS field editor does
        // (`controlTextDidChange:`): the field holds only values the store would
        // accept, so `commit_session_rename`'s refusal path stays unreachable from
        // a human edit and the visible refusal is simply that the field stops
        // taking more. Compared against the TRIMMED input for the same reason the
        // native one is — otherwise a space typed mid-word would yank the caret.
        let canonical = crate::session_timeline::sanitize_presentation_line(
            &state.text,
            MetaField::Title.cap(),
        );
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

    /// Paste the clipboard into `window`'s in-grid rename field (one line; control
    /// characters stripped by the reducer). The find bar's `⌘V` twin — the field
    /// owns the keystroke, so nothing reaches the shell behind it.
    ///
    /// LIMIT, accepted: on X11 only an OWN-selection read is instant; a FOREIGN
    /// owner needs a `ConvertSelection` round-trip that can park the event loop
    /// for ~1 s, which the find bar spends a worker thread and a wake on. A pin is
    /// a short human-typed string, so this path stays synchronous and simply does
    /// not paste from a foreign owner rather than block the UI thread. The
    /// keystroke is still SWALLOWED either way, which is the part that matters:
    /// `⌘V` under an open field must never reach the PTY.
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
    /// `text` is what the user left in the field. Empty (or whitespace-only)
    /// CLEARS the pin — [`MetaEdit::Clear`], never `Set("")`: both store `None`
    /// but they record different timeline payloads, and only `-` is the
    /// documented cleared marker.
    ///
    /// The commit is validated against the LIVE edit state first, so a commit
    /// that lost a race (the tab closed mid-edit, a different edit replaced this
    /// one) is dropped instead of landing on the wrong session.
    pub(crate) fn commit_session_rename(&mut self, window: WindowId, session: u64, text: &str) {
        if self.rename_edit_session(window) != Some(session) {
            aterm_log::info!("rename commit dropped: session {session} is no longer being edited");
            return;
        }
        self.end_session_rename_editor(window);
        match self.write_session_title_pin(session, text) {
            Ok(_) => {}
            Err(error) => {
                // Unreachable through either editor — both canonicalize as the user
                // types, so a field can only ever hold a storable value — but the
                // API is honest about refusing and a scripted `invoke` might not be.
                aterm_log::info!("rename refused for session {session}: {error:?}");
            }
        }
    }

    /// Write one session's TITLE pin and drive everything a change invalidates.
    /// `text` empty ⇒ clear. Returns whether the stored value actually moved.
    ///
    /// This is the GUI twin of the control socket's `meta` dispatch arm, over
    /// the SAME typed mutation API: the validation ladder, the store, and the
    /// `meta-change` record all happen inside `write_session_meta` (which
    /// returns with every guard released — load-bearing, because the refresh
    /// below re-takes `ctx.meta` per tab on this same thread and
    /// `std::sync::Mutex` is not reentrant). Only the fan-out is ours, and only
    /// on an ACTUAL change.
    pub(crate) fn write_session_title_pin(
        &mut self,
        session: u64,
        text: &str,
    ) -> Result<bool, MetaWriteError> {
        let Some(ctx) = self.pool.get(session).map(|s| s.ctx.clone()) else {
            // The session exited while the editor was open. Dropping the write is
            // the honest outcome: there is nothing left to name.
            return Ok(false);
        };
        let edit = if text.trim().is_empty() {
            MetaEdit::Clear
        } else {
            MetaEdit::Set(text)
        };
        let changed = write_session_meta(&ctx, MetaField::Title, edit)?;
        if changed {
            // The record already landed (under the meta guard), so the chrome
            // cache's `high_id` gate has moved and the tooltip/context menu
            // recompose along with the label.
            self.refresh_meta_dependent_chrome(session);
            if self.subscribers.any() {
                self.subscribers
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .notify(session);
            }
        }
        Ok(changed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session_timeline::MetaWriteError;

    /// Read one session's stored pin (the raw slot, not the label chain).
    fn pin(app: &App, session: u64) -> Option<String> {
        app.pool
            .get(session)?
            .ctx
            .meta
            .lock()
            .unwrap()
            .user_title
            .clone()
    }

    /// The recorded `meta-change` payloads, oldest-first.
    fn meta_events(app: &App, session: u64) -> Vec<String> {
        let ctx = app.pool.get(session).expect("session").ctx.clone();
        let tl = ctx.timeline.lock().unwrap();
        tl.since(None)
            .filter(|e| e.kind == "meta-change")
            .map(|e| e.payload.clone())
            .collect()
    }

    /// COMMIT: begin over the active tab, type, commit — the pin lands, the tab
    /// label follows it, exactly one `meta-change` is recorded, and the editor
    /// state is gone.
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

    /// A window with NO editor surface must not accept the command at all: off
    /// macOS the field is painted by the tab strip, so `tab_strip_rows = 0`
    /// leaves nowhere to put it. The capability is what the menu, palette and
    /// context-menu rows gate on, so it has to agree with the write path.
    #[test]
    fn a_window_with_no_editor_surface_refuses_and_reports_it_cannot_rename() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 0;
        assert!(
            !app.can_rename_session(wid),
            "no strip and no native editor means no surface"
        );
        assert!(!app.begin_active_session_rename(wid), "the command refuses");
        assert_eq!(
            app.rename_edit_session(wid),
            None,
            "and installs no invisible modal state"
        );

        app.tab_strip_rows = 1;
        assert!(app.can_rename_session(wid), "a visible strip is a surface");
        assert!(app.begin_active_session_rename(wid));
    }

    /// Renaming a DIFFERENT tab while one edit is live commits the first, like
    /// every other way of leaving a field. This was the one exit that silently
    /// dropped what the user had typed.
    #[test]
    fn opening_another_rename_commits_the_one_it_replaces() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        assert!(app.begin_active_session_rename(wid));
        app.rename_field_edit(wid, crate::app_search::SearchEdit::Insert("x".to_string()));

        // Re-opening over the SAME chip is a no-op that keeps the caret, so the
        // replace path needs a genuinely different target: end and reopen.
        app.settle_rename_edit(wid);
        assert_eq!(pin(&app, 0).as_deref(), Some("x"), "settling commits");
        assert_eq!(app.rename_edit_session(wid), None);
    }

    /// CANCEL: Escape writes nothing, records nothing, and closes the editor —
    /// including when a pin already existed (the old one survives untouched).
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

    /// EMPTY COMMIT CLEARS: the pin is unset, the label falls back down the
    /// ladder to the live OSC title, and the event stream says `-` — the
    /// documented cleared marker, NOT the `value=` an accidental `Set("")`
    /// would have produced.
    #[test]
    fn an_empty_commit_clears_the_pin_and_the_ladder_falls_back() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        // Drive the REAL OSC 0/2 ingestion path so the fallback rung is live.
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

    /// CLOSE DURING EDIT: the edited tab goes away, then a commit for that
    /// session arrives (wakes are FIFO — the close was queued first). Nothing is
    /// written, because the edit state died with the tab; and re-beginning
    /// against the dead tab id is refused rather than retargeted at whatever tab
    /// now occupies the slot.
    #[test]
    fn a_close_during_the_edit_drops_the_commit_instead_of_retargeting() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1; // the in-grid strip is the editor's surface
        let tab = app.windows[&wid].tab_set.active_id().expect("a tab");
        assert!(app.begin_session_rename(wid, tab));

        // The strip notices its edited tab is gone and cancels; the queued
        // commit lands afterwards.
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

    /// No menu command runs THROUGH an open editor. macOS resolves a key
    /// equivalent before the first responder sees the key, so a structural
    /// command must end the edit first — keeping what was typed, exactly as
    /// clicking away does — and `RenameSession` itself must stay idempotent
    /// rather than close-and-reopen the field.
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

    /// The pin write refuses rather than repairs, and a refusal leaves the store
    /// untouched — the GUI shares the control socket's ladder, it does not
    /// silently truncate what the wire would reject.
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

    /// NO PHANTOM STATE. With no native editor AND no in-grid strip
    /// (`tab_strip_rows == 0` — the macOS default, and every plain
    /// `headless_for_test`), a rename is REFUSED outright.
    ///
    /// The bug this pins was reachable from a shipping mode: a real
    /// `aterm-gui --headless` instance took `invoke RenameSession`, stored the
    /// edit because it was headless, and then had no path to end it — every
    /// later menu command was silently spent settling an editor that was never
    /// on screen.
    #[test]
    fn a_surface_less_window_refuses_the_edit_instead_of_storing_a_phantom() {
        use crate::menu::MenuAction;
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        assert_eq!(app.tab_strip_rows, 0, "negative control: no strip");

        assert!(
            !app.begin_active_session_rename(wid),
            "nothing can present this edit, so it is refused"
        );
        assert_eq!(app.rename_edit_session(wid), None, "and nothing is stored");

        // The divert path is therefore un-poisoned: the next command runs.
        assert!(!app.divert_menu_action_around_rename(MenuAction::CloseTab));

        // Turn the strip ON and the very same call succeeds — the refusal was
        // about PRESENTATION, not about the platform or headlessness.
        app.tab_strip_rows = 1;
        assert!(app.begin_active_session_rename(wid));
        assert_eq!(app.rename_edit_session(wid), Some(0));
        assert!(
            app.inline_rename_edit(wid).is_some(),
            "the in-grid strip is what presents it"
        );
    }

    /// HEADLESS DRIVABILITY (RFC §15): the in-grid editor is reachable through
    /// the engine-neutral input seam — the one controller `key`/`text` verbs
    /// arrive on — so a headless instance can both DRIVE and END it. This is what
    /// makes holding the state under `--headless` honest rather than a phantom.
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
        // A caret motion + a delete: the shared field reducer, not a bespoke one.
        let key = |named: NamedKey| InputEvent::Key {
            key: Key::Named(named),
            mods: Modifiers::empty(),
            event_type: KeyEventType::Press,
            base_layout: None,
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

    /// ⎋ through the same seam CANCELS — the one exit that writes nothing.
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
            }
        ));
        assert_eq!(app.rename_edit_session(wid), None);
        assert_eq!(pin(&app, 0), None);
        assert!(meta_events(&app, 0).is_empty());
    }

    /// The field CANONICALIZES as the user types, exactly as the macOS field
    /// editor does — so `commit_session_rename`'s refusal path stays unreachable
    /// from a human edit and what you see is what gets stored. A mid-word space
    /// survives (the caret must not be yanked); an over-cap paste is clamped.
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

    /// INVISIBLE TYPING GUARD: the strip's repaint fingerprint carries the live
    /// in-grid field. Two independent caches key off this number (the RepaintKey
    /// early-out and the painted-row cache), so if it did not move, keystrokes
    /// would edit state nothing ever repainted.
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

        // A pure CARET MOVE is painted state too (the caret is a reverse-video
        // cell, not a renderer-side overlay), so it must move the number as well.
        app.rename_field_edit(wid, SearchEdit::MoveStart);
        assert_ne!(typed, fingerprint(&mut app), "the caret is painted state");
    }
}
