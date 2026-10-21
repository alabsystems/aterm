// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the picker ([`crate::session_picker`]): File ▸ New Window /
//! New Tab With Identity… opens it over the identity roster, type-to-filter,
//! and a choice spawns under the identity (or provisions the one the filter
//! names). The connection intents it once carried (Connect / Configure /
//! Disconnect) were retired (`docs/DESIGN-owner-surfaces-2026-10-16.md` §7).
//!
//! Modelled on `app_palette.rs`: the state lives in the one modal
//! [`crate::overlay::Overlay`] slot (keys structurally gated before the
//! terminal), every mutator repaints + refreshes the a11y tree, and the
//! pointer follows the palette's claim/hover/arm discipline.

use crate::App;
use crate::WindowId;
use crate::session_picker::{PickerChoice, PickerIntent, PickerRow, SessionPickerState};

impl App {
    /// The identity picker's rows: every identity on disk by name (the
    /// `identities` roster), each with the number of live sessions wearing it,
    /// then the row that creates the identity the filter names.
    fn identity_picker_rows(&self) -> Vec<PickerRow> {
        let live: Vec<String> = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot()
                .into_iter()
                .filter_map(|h| h.identity.as_deref().map(str::to_string))
                .collect()
        };
        let mut rows: Vec<PickerRow> = crate::agent_identity::roster()
            .into_iter()
            .map(|name| PickerRow::Identity {
                sessions: live.iter().filter(|n| **n == name).count(),
                name,
            })
            .collect();
        rows.push(PickerRow::NewIdentity);
        rows
    }

    /// Open the identity picker on `wid` for File ▸ New Window / New Tab With
    /// Identity… (`intent` must be one of the two identity intents). Always
    /// opens: an empty roster still offers the new-identity row.
    pub(crate) fn open_identity_picker(&mut self, wid: WindowId, intent: PickerIntent) -> bool {
        let rows = self.identity_picker_rows();
        let state = SessionPickerState::new(wid, intent, rows);
        self.install_session_picker(wid, state)
    }

    /// Put `state` in `wid`'s one overlay slot and arm the pointer, as every
    /// picker intent opens.
    fn install_session_picker(&mut self, wid: WindowId, state: SessionPickerState) -> bool {
        self.install_overlay_card(
            wid,
            crate::overlay::Overlay::SessionPicker(state),
            false,
            Self::session_picker_claims_pointer,
            Self::session_picker_pointer_motion,
        )
    }

    /// Close the picker on `wid` (no-op unless it is the open variant).
    pub(crate) fn session_picker_exit(&mut self, wid: WindowId) {
        self.close_overlay_card(wid, |ws| ws.session_picker().is_some());
    }

    /// Move the picker cursor by `delta` over the filtered set.
    pub(crate) fn session_picker_move(&mut self, wid: WindowId, delta: isize) {
        if let Some(p) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.session_picker_mut())
        {
            p.move_selection(delta);
        }
        self.repaint_session_picker_pointer(wid, true);
    }

    /// Append a filter character.
    pub(crate) fn session_picker_filter_push(&mut self, wid: WindowId, c: char) {
        if let Some(p) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.session_picker_mut())
        {
            p.push_char(c);
        }
        self.repaint_session_picker_pointer(wid, true);
    }

    /// Delete the last filter character.
    pub(crate) fn session_picker_backspace(&mut self, wid: WindowId) {
        if let Some(p) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.session_picker_mut())
        {
            p.backspace();
        }
        self.repaint_session_picker_pointer(wid, true);
    }

    /// Settle the chosen row (Enter / a pointer activation): close the picker,
    /// then spawn under the chosen identity, provisioning the one the filter
    /// names. An empty filter (no cursor row), or a new-identity row whose
    /// query is no new valid name, is a no-op and the picker stays open.
    pub(crate) fn session_picker_activate(&mut self, wid: WindowId) {
        let Some((intent, chosen)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.session_picker())
            .and_then(|p| {
                p.selected_row()
                    .and_then(|row| p.choice_for(row))
                    .map(|choice| (p.intent, choice))
            })
        else {
            return;
        };
        self.session_picker_exit(wid);
        self.settle_picker_choice(wid, intent, chosen);
    }

    /// The one dispatch seam keyboard and pointer activation share.
    fn settle_picker_choice(&mut self, wid: WindowId, intent: PickerIntent, chosen: PickerChoice) {
        let PickerChoice::Identity { name, create } = chosen;
        self.spawn_with_identity(
            wid,
            intent == PickerIntent::NewWindowWithIdentity,
            &name,
            create,
        );
    }

    /// Spawn under identity `name` — a new WINDOW or a new TAB in `wid`.
    /// `create` is the picker's new-identity row: the only menu path that may
    /// provision one, exactly as `spawn identity=` is on the wire. A failure is
    /// posted, never silent. The shell spawns off the event loop, a tab's and a
    /// window's alike, and so does a new identity it needs: its primer syncs
    /// every file it writes (`tab_spawn`). Headless opens no window, as it
    /// ignores Cmd-N.
    pub(crate) fn spawn_with_identity(
        &mut self,
        wid: WindowId,
        window: bool,
        name: &str,
        create: bool,
    ) {
        let ask = crate::tab_spawn::SpawnAsk {
            identity_prep: if create {
                crate::tab_spawn::IdentityPrep::Create
            } else {
                crate::tab_spawn::IdentityPrep::Exists
            },
            ..crate::tab_spawn::SpawnAsk::new(None, Some(name))
        };
        let then = crate::tab_spawn::TabSpawnThen::Nothing;
        if !window {
            self.open_off_loop(wid, crate::tab_spawn::SpawnPlace::Tab, ask, then);
        } else if self.headless {
            aterm_log::info!("headless: ignoring a new window with identity {name}");
        } else {
            self.open_window_off_loop(ask, None, then);
        }
    }

    /// While the picker is open on `wid`, drive it from the keyboard and swallow
    /// every key: printable chars filter, Backspace deletes, Up/Down move, Enter
    /// chooses, Esc closes.
    pub(crate) fn on_key_session_picker_mode(
        &mut self,
        wid: WindowId,
        ev: &aterm_window::event::KeyEvent,
    ) -> bool {
        use aterm_window::keyboard::{Key, NamedKey};
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.session_picker())
            .is_none()
        {
            return false;
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.session_picker_exit(wid),
            Key::Named(NamedKey::ArrowUp) => self.session_picker_move(wid, -1),
            Key::Named(NamedKey::ArrowDown) => self.session_picker_move(wid, 1),
            Key::Named(NamedKey::Enter) => self.session_picker_activate(wid),
            Key::Named(NamedKey::Backspace) => self.session_picker_backspace(wid),
            Key::Named(NamedKey::Space) => self.session_picker_filter_push(wid, ' '),
            Key::Character(s) => {
                for c in s.chars().filter(|c| !c.is_control()) {
                    self.session_picker_filter_push(wid, c);
                }
            }
            _ => {
                if let Some(t) = ev.text.as_deref() {
                    for c in t.chars().filter(|c| !c.is_control()) {
                        self.session_picker_filter_push(wid, c);
                    }
                }
            }
        }
        true
    }

    /// The engine-neutral twin of [`Self::on_key_session_picker_mode`], for the
    /// controller's `key`/`text` verbs. The caller still keeps the event from the PTY.
    pub(crate) fn session_picker_input_event(
        &mut self,
        wid: WindowId,
        ev: &crate::input::InputEvent,
    ) {
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key as TKey, KeyEventType, NamedKey as TNamed};
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.session_picker())
            .is_none()
        {
            return;
        }
        // An overlay reads a keypad key as its main-block twin: `build_key_input`
        // keeps the keypad identity (`Numpad5`) for the PTY encoders, which the arms
        // below have no case for. `InputEvent::keypad_folded` is the shared fold.
        let folded = ev.keypad_folded();
        let ev = folded.as_ref().unwrap_or(ev);
        match ev {
            InputEvent::Key {
                key, event_type, ..
            } => {
                if matches!(event_type, KeyEventType::Release) {
                    return;
                }
                match key {
                    TKey::Named(TNamed::Escape) => self.session_picker_exit(wid),
                    TKey::Named(TNamed::ArrowUp) => self.session_picker_move(wid, -1),
                    TKey::Named(TNamed::ArrowDown) => self.session_picker_move(wid, 1),
                    TKey::Named(TNamed::Enter) => {
                        self.session_picker_activate(wid);
                    }
                    TKey::Named(TNamed::Backspace) => self.session_picker_backspace(wid),
                    TKey::Named(TNamed::Space) => self.session_picker_filter_push(wid, ' '),
                    TKey::Character(c) if !c.is_control() => {
                        self.session_picker_filter_push(wid, *c);
                    }
                    _ => {}
                }
            }
            InputEvent::Text(t) | InputEvent::Paste(t, _) => {
                for c in t.chars().filter(|c| !c.is_control()) {
                    self.session_picker_filter_push(wid, c);
                }
            }
            _ => {}
        }
    }

    // ---- Pointer ----------------------------------------------------------------

    /// Modal pointer boundary: whether the open picker owns the gesture on
    /// `wid`. Shares [`Self::overlay_card_claims_pointer`] with the other modal
    /// cards.
    pub(crate) fn session_picker_claims_pointer(&mut self, wid: WindowId) -> bool {
        self.overlay_card_claims_pointer(
            wid,
            |ws| ws.session_picker().is_some(),
            Self::sync_session_picker_pointer_cursor,
        )
    }

    /// The filtered row index under a window-space point, through the zoom-aware
    /// transform the card was composited with.
    fn session_picker_row_at_pointer(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let picker = self.windows.get(&wid)?.session_picker()?;
        let (transform, x, y) = self.overlay_card_point(wid, x, y)?;
        crate::session_picker::picker_row_hit(picker, &transform.geom, x, y)
    }

    /// Keep the OS cursor aligned with the picker's row hover.
    pub(crate) fn sync_session_picker_pointer_cursor(&mut self, wid: WindowId) {
        let pointer = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.session_picker())
            .is_some_and(SessionPickerState::pointer_over_row);
        self.set_overlay_card_cursor(wid, pointer);
    }

    fn repaint_session_picker_pointer(&mut self, wid: WindowId, changed: bool) {
        self.sync_session_picker_pointer_cursor(wid);
        self.redraw_overlay_card(wid, changed);
    }

    /// Hover-select the row under a pointer motion.
    pub(crate) fn session_picker_pointer_motion(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.session_picker_row_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.session_picker_mut())
            .is_some_and(|p| p.pointer_hover(hit));
        self.repaint_session_picker_pointer(wid, changed);
    }

    pub(crate) fn session_picker_pointer_press(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.session_picker_row_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.session_picker_mut())
            .is_some_and(|p| p.pointer_press(hit));
        self.repaint_session_picker_pointer(wid, changed);
    }

    /// Settle a left release: a same-row press and release chooses, as Enter does.
    pub(crate) fn session_picker_pointer_release(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.session_picker_row_at_pointer(wid, x, y);
        let (changed, activate) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.session_picker_mut())
            .map_or((false, false), |p| p.pointer_release(hit));
        self.repaint_session_picker_pointer(wid, changed);
        if !activate {
            return;
        }
        let Some((intent, chosen)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.session_picker())
            .and_then(|p| {
                hit.and_then(|idx| p.row_at_filtered(idx))
                    .and_then(|row| p.choice_for(row))
                    .map(|choice| (p.intent, choice))
            })
        else {
            return;
        };
        self.session_picker_exit(wid);
        self.settle_picker_choice(wid, intent, chosen);
    }

    /// Scroll the band, then re-resolve the stationary pointer (the palette's
    /// wheel rule).
    pub(crate) fn session_picker_pointer_wheel(&mut self, wid: WindowId, delta: isize) {
        let mut changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.session_picker_mut())
            .is_some_and(|p| p.scroll_by(delta));
        let (x, y) = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
        let hit = self.session_picker_row_at_pointer(wid, x, y);
        changed |= self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.session_picker_mut())
            .is_some_and(|p| p.pointer_hover(hit));
        self.repaint_session_picker_pointer(wid, changed);
    }
}

#[cfg(test)]
mod tests {
    use crate::App;
    use crate::WindowId;
    use crate::input::InputEvent;
    use crate::overlay::OverlayKind;
    use crate::session_picker::PickerIntent;

    fn key(named: aterm_types::keyboard::NamedKey) -> InputEvent {
        InputEvent::Key {
            key: aterm_types::keyboard::Key::Named(named),
            mods: aterm_types::keyboard::Modifiers::empty(),
            base_layout: None,
            layout_text: None,
            event_type: aterm_types::keyboard::KeyEventType::Press,
        }
    }

    /// Three registered stub sessions in one window; returns their sids in
    /// registration order (tab 0 is the harness session).
    fn app_with_three() -> (App, WindowId, Vec<aterm_session::SessionId>) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        let sids = {
            let g = app.store.read().unwrap();
            let mut handles = g.snapshot();
            handles.sort_by_key(|h| h.local_id);
            handles.into_iter().map(|h| h.sid).collect()
        };
        (app, wid, sids)
    }

    /// ROUND 18'S IDENTITY ROWS, through the one picker surface: New Tab /
    /// Window With Identity… lists every identity on disk by name (with how
    /// many live sessions wear it) and a last row that creates the identity the
    /// FILTER names. A name that does not parse, or one already listed, chooses
    /// nothing and keeps the picker open; a valid new name is chosen for
    /// creation (the menu's one create path, as `spawn identity=` is the wire's)
    /// and closes the picker. The window it asks for is spawned off the loop,
    /// and the identity is made there, before the shell (`tab_spawn`), never in
    /// the turn: here no event loop hosts the window, so it is said not to open,
    /// and nothing was made.
    /// A scratch state root (`ATERM_STATE_HOME`, under the env module's lock, as
    /// the restore identity test does): no test touches the machine's real one.
    #[test]
    fn the_identity_picker_lists_the_roster_and_creates_the_named_one() {
        let state = aterm_tempfile::tempdir().expect("scratch state root");
        crate::test_env::scoped("ATERM_STATE_HOME", state.path(), || {
            identity_picker_body(&state.path().join("identities"));
        });
    }

    fn identity_picker_body(root: &std::path::Path) {
        for name in ["alpha", "beta"] {
            crate::agent_identity::ensure(name, true).expect("provision a fixture identity");
        }
        let (mut app, wid, _sids) = app_with_three();
        assert!(app.open_identity_picker(wid, PickerIntent::NewWindowWithIdentity));
        let lines = |app: &App| {
            app.windows[&wid]
                .session_picker()
                .expect("the picker is open")
                .controls_lines()
        };
        let listed = lines(&app);
        assert!(
            listed[0].contains("intent=new-window-identity"),
            "{listed:?}"
        );
        assert!(listed[1].contains("identity=alpha") && listed[2].contains("identity=beta"));
        assert!(listed[3].contains("identity=+new"), "{listed:?}");

        // An existing name in the filter chooses its own row; the new-identity row
        // refuses to recreate it.
        for c in "beta".chars() {
            app.session_picker_filter_push(wid, c);
        }
        let picker = app.windows[&wid].session_picker().unwrap();
        assert_eq!(picker.new_identity_name(), None, "beta already exists");
        assert_eq!(
            picker.selected_row().and_then(|row| picker.choice_for(row)),
            Some(crate::session_picker::PickerChoice::Identity {
                name: "beta".to_string(),
                create: false,
            })
        );

        // A name the grammar refuses chooses nothing: Enter keeps the picker.
        for _ in 0..4 {
            app.session_picker_backspace(wid);
        }
        for c in "Bad Name".chars() {
            app.session_picker_filter_push(wid, c);
        }
        app.session_picker_activate(wid);
        assert!(
            app.windows[&wid].session_picker().is_some(),
            "an invalid name chooses nothing and the picker stays open"
        );

        // A valid new name: the new-identity row provisions it and closes.
        for _ in 0.."Bad Name".len() {
            app.session_picker_backspace(wid);
        }
        for c in "Gamma".chars() {
            app.session_picker_filter_push(wid, c);
        }
        let picker = app.windows[&wid].session_picker().unwrap();
        assert_eq!(
            picker.new_identity_name(),
            Some("gamma".to_string()),
            "the typed name is folded, as `parse_name` folds it"
        );
        assert_eq!(
            picker.selected_row().and_then(|row| picker.choice_for(row)),
            Some(crate::session_picker::PickerChoice::Identity {
                name: "gamma".to_string(),
                create: true,
            })
        );
        // A windowed instance, the one that opens windows.
        app.headless = false;
        app.session_picker_activate(wid);
        assert!(
            app.windows[&wid].session_picker().is_none(),
            "choosing closes"
        );
        assert!(
            app.messages
                .live_rows()
                .any(|l| l.msg.title == "Couldn't open a new window"),
            "no event loop hosts the window here, and that is said"
        );
        assert!(
            !root.join("gamma").exists(),
            "the identity is made where the window's shell spawns, not in the turn"
        );
        assert_eq!(crate::agent_identity::roster(), ["alpha", "beta"]);
    }

    /// Filter narrows in the palette style and Esc closes without acting: no
    /// identity is created, no tab opens.
    #[test]
    fn filter_narrows_and_esc_closes_without_acting() {
        let (mut app, wid, _sids) = app_with_three();
        let tabs = app.windows[&wid].tab_set.len();
        assert!(app.open_identity_picker(wid, PickerIntent::NewTabWithIdentity));
        assert_eq!(
            app.windows[&wid].overlay().map(|o| o.kind()),
            Some(OverlayKind::SessionPicker)
        );
        app.session_picker_input_event(wid, &InputEvent::Text("zzzz no".to_string()));
        let lines = app.windows[&wid].session_picker().unwrap().controls_lines();
        // Only the new-identity row never filters out.
        assert!(lines[0].contains("shown=1"), "{lines:?}");
        // Enter on a name the grammar refuses is a no-op — the picker stays.
        app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
        assert!(app.windows[&wid].session_picker().is_some());
        app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Escape));
        assert!(
            app.windows[&wid].session_picker().is_none(),
            "Esc closed it"
        );
        assert_eq!(
            app.windows[&wid].tab_set.len(),
            tabs,
            "closing opened nothing"
        );
    }
}
