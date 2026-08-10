// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! INLINE SESSION RENAME — the GUI face of the user pin (`meta set title`).
//!
//! The pin already existed end to end: it is the TOP rung of the tab-label
//! chain ([`crate::app_tabs::resolved_terminal_title_rung`]), it survives
//! restore, and the control socket has written it since session-metadata stage
//! 1. What was missing was a way for a HUMAN to set it. This module is the
//! window-side state machine for that: double-clicking a tab (or Window ▸
//! Rename Session… / the `rename_session` keybinding / the tab context menu)
//! opens an editor over the tab; Return commits, Escape cancels, and an EMPTY
//! commit CLEARS the pin so the label falls back down the ladder.
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

use crate::platform::AppRt;
use crate::session_timeline::{MetaEdit, MetaField, MetaWriteError, write_session_meta};
use crate::tab_model::TabId;
use crate::{App, WindowId};

/// A live inline rename on one window's tab strip.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabRenameEdit {
    /// The session whose pin is being edited — resolved ONCE at begin from the
    /// tab's focused pane, and the only thing a commit is ever applied to.
    pub session: u64,
    /// The tab the editor is painted over. POSITIONING ONLY: the strip is handed
    /// no session identity, so this is how the native editor follows a reorder
    /// and how it learns its tab has closed. Never a commit target — a tab is a
    /// container and the pin belongs to one of the sessions inside it.
    pub tab: TabId,
}

impl App {
    /// The session being renamed in `window`, or `None` when no editor is open.
    /// Callers use this to know that keystrokes/commands belong to a text field
    /// rather than to the terminal.
    pub(crate) fn rename_edit_session(&self, window: WindowId) -> Option<u64> {
        self.windows.get(&window)?.rename_edit.map(|e| e.session)
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
        let live = self.windows.get(&window).and_then(|ws| ws.rename_edit);
        // Same session AND same chip ⇒ nothing to do. Compared as a whole so a
        // session shown on a different tab still re-presents (the editor has to
        // move); a repeat double-click on the tab already being edited does not
        // close-and-reopen the field under the user's caret.
        if live == Some(TabRenameEdit { session, tab }) {
            return true;
        }
        // Replacing a different live edit: tear the old editor down first so the
        // platform never holds two fields, and so the abandoned one cannot post
        // a late commit against a session we are no longer editing.
        if live.is_some() {
            self.end_session_rename_editor(window);
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
        if let Some(ws) = self.windows.get_mut(&window) {
            ws.rename_edit = Some(TabRenameEdit { session, tab });
        } else {
            return false;
        }
        let presented = match self._toolbars.get(&window) {
            Some(handle) => self
                .apprt
                .begin_tab_rename(handle, tab, session, &seed, &placeholder),
            None => false,
        };
        if !presented && !self.headless {
            // No editor is on screen, so keeping the state would leave the app in
            // an invisible modal mode that swallows commands. Off macOS there is
            // no strip editor yet; say so once rather than silently doing nothing.
            aterm_log::info!(
                "rename dropped: this platform has no inline tab-strip editor yet (session {session})"
            );
            if let Some(ws) = self.windows.get_mut(&window) {
                ws.rename_edit = None;
            }
            return false;
        }
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
                // Unreachable through the macOS editor, which canonicalizes as the
                // user types (so the field can only hold a storable value), but the
                // API is honest about refusing and a future editor might not be.
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

    /// CANCEL: Escape writes nothing, records nothing, and closes the editor —
    /// including when a pin already existed (the old one survives untouched).
    #[test]
    fn a_cancel_writes_nothing_and_leaves_an_existing_pin_alone() {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
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
}
