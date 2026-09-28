// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the session picker ([`crate::session_picker`], design
//! §2.3/§2.5 — the wave-2 "picker slice"): open with an intent
//! (Connect / Configure / Disconnect), type-to-filter over the live registry,
//! and settle a choice — Connect/Configure open the shared confirm card
//! ([`crate::app_conn_card`]) for subject ⇄ chosen; Disconnect dissolves both
//! directions through the same seam the tab menu's Disconnect row uses.
//!
//! Modelled on `app_palette.rs`: the state lives in the one modal
//! [`crate::overlay::Overlay`] slot (keys structurally gated before the
//! terminal), every mutator repaints + refreshes the a11y tree, and the
//! pointer follows the palette's claim/hover/arm discipline.

use winit::window::CursorIcon;

use aterm_session::SessionId;

use crate::App;
use crate::WindowId;
use crate::session_picker::{
    PickerChoice, PickerIntent, PickerRow, SessionPickerState, SessionRow,
};

impl App {
    /// Gather the picker's choosable rows for `subject` under `intent`:
    /// Connect lists every live registered session except the subject
    /// (connected peers annotated); Configure/Disconnect list ONLY the
    /// subject's connected peers (§2.3 — those ids act on an existing pair).
    /// Registry facts only; titles resolve user meta title ▸ registry title.
    fn picker_rows(&self, subject: &SessionId, intent: PickerIntent) -> Vec<PickerRow> {
        let peers: std::collections::HashSet<String> = self
            .connection_facts(subject)
            .into_iter()
            .map(|f| f.peer_sid.as_str().to_string())
            .collect();
        let g = self.store.read().unwrap_or_else(|p| p.into_inner());
        let mut rows: Vec<PickerRow> = g
            .snapshot()
            .into_iter()
            .filter(|h| h.sid != *subject)
            .filter(|h| !matches!(h.state, crate::session_store::SessionState::Exited))
            .filter(|h| matches!(intent, PickerIntent::Connect) || peers.contains(h.sid.as_str()))
            .map(|h| {
                let title = h
                    .ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get("title")
                    .map(str::to_string)
                    .unwrap_or_else(|| h.title.clone());
                PickerRow::Session(SessionRow {
                    connected: peers.contains(h.sid.as_str()),
                    sid: h.sid,
                    local_id: h.local_id,
                    title,
                })
            })
            .collect();
        // Stable listing (the connection_facts BTreeMap discipline): by sid.
        rows.sort_by(|a, b| {
            a.sid()
                .map(SessionId::as_str)
                .cmp(&b.sid().map(SessionId::as_str))
        });
        rows
    }

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
        debug_assert!(intent.picks_identity());
        let rows = self.identity_picker_rows();
        let state = SessionPickerState::new(wid, None, String::new(), intent, rows);
        self.install_session_picker(wid, state)
    }

    /// Open the session picker on `wid` for `subject` under `intent`. Returns
    /// `false` — nothing opens — when the subject is unregistered, or when a
    /// Configure/Disconnect intent has no connected peer to act on (those ids
    /// NEVER guess, §2.3; an empty Connect picker still opens and states its
    /// empty truth).
    pub(crate) fn open_session_picker(
        &mut self,
        wid: WindowId,
        subject: SessionId,
        intent: PickerIntent,
    ) -> bool {
        let Some(subject_title) = self.session_title_by_sid(&subject) else {
            aterm_log::info!("session picker refused: subject not registered");
            return false;
        };
        let rows = self.picker_rows(&subject, intent);
        if rows.is_empty() && !matches!(intent, PickerIntent::Connect) {
            aterm_log::info!("session picker: no connected peer to act on");
            return false;
        }
        let state = SessionPickerState::new(wid, Some(subject), subject_title, intent, rows);
        self.install_session_picker(wid, state)
    }

    /// Put `state` in `wid`'s one overlay slot and arm the pointer, as every
    /// picker intent opens.
    fn install_session_picker(&mut self, wid: WindowId, state: SessionPickerState) -> bool {
        let Some(ws) = self.windows.get_mut(&wid) else {
            return false;
        };
        // Structural mutual exclusion: the one overlay slot.
        ws.overlay = Some(crate::overlay::Overlay::SessionPicker(state));
        ws.scroll_residual = 0.0;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
        self.settle_pointer_drags(wid);
        let _ = self.session_picker_claims_pointer(wid);
        if let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) {
            self.session_picker_pointer_motion(wid, x, y);
        }
        self.overlay_a11y_update();
        true
    }

    /// Close the picker on `wid` (no-op unless it is the open variant).
    pub(crate) fn session_picker_exit(&mut self, wid: WindowId) {
        let mut closed = false;
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.session_picker().is_some()
        {
            ws.overlay = None;
            ws.scroll_residual = 0.0;
            if let Some(w) = &ws.os_window {
                w.request_redraw();
            }
            closed = true;
        }
        self.overlay_a11y_update();
        if closed && let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) {
            self.on_cursor_moved(wid, x, y);
        }
    }

    fn session_picker_repaint(&mut self, wid: WindowId) {
        self.sync_session_picker_pointer_cursor(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        self.overlay_a11y_update();
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
        self.session_picker_repaint(wid);
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
        self.session_picker_repaint(wid);
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
        self.session_picker_repaint(wid);
    }

    /// Settle the chosen session (Enter / a pointer activation): close the
    /// picker, then dispatch by intent — Connect/Configure open THE shared
    /// confirm card for subject ⇄ chosen (§2.5 prefill from the live tables,
    /// origin `menu`), Disconnect dissolves both directions. An empty filter
    /// (no cursor row) is a no-op and the picker stays open.
    pub(crate) fn session_picker_activate(&mut self, wid: WindowId) {
        let Some((subject, intent, chosen)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.session_picker())
            .and_then(|p| {
                p.selected_row()
                    .and_then(|row| p.choice_for(row))
                    .map(|choice| (p.subject.clone(), p.intent, choice))
            })
        else {
            return;
        };
        self.session_picker_exit(wid);
        self.settle_picker_choice(wid, subject, intent, chosen);
    }

    /// The one intent-dispatch seam keyboard and pointer activation share.
    fn settle_picker_choice(
        &mut self,
        wid: WindowId,
        subject: Option<SessionId>,
        intent: PickerIntent,
        chosen: PickerChoice,
    ) {
        match (intent, chosen, subject) {
            (
                PickerIntent::Connect | PickerIntent::Configure,
                PickerChoice::Session(chosen),
                Some(subject),
            ) => {
                let _ = self.open_confirm_card(wid, subject, chosen, None, "menu");
            }
            (PickerIntent::Disconnect, PickerChoice::Session(chosen), Some(subject)) => {
                self.disconnect_pair(&subject, &chosen, "menu");
            }
            (
                PickerIntent::NewWindowWithIdentity | PickerIntent::NewTabWithIdentity,
                PickerChoice::Identity { name, create },
                _,
            ) => {
                self.spawn_with_identity(
                    wid,
                    intent == PickerIntent::NewWindowWithIdentity,
                    &name,
                    create,
                );
            }
            // Rows are built per intent, so a session row never reaches an
            // identity intent (or the reverse); nothing to do if one did.
            _ => {}
        }
    }

    /// Spawn under identity `name` — a new WINDOW (through the event loop, which
    /// owns window creation) or a new TAB in `wid`. `create` is the picker's
    /// new-identity row: the only menu path that may provision one, exactly as
    /// `spawn identity=` is on the wire. A failure is posted, never silent.
    pub(crate) fn spawn_with_identity(
        &mut self,
        wid: WindowId,
        window: bool,
        name: &str,
        create: bool,
    ) {
        let fail = |app: &mut Self, error: String| {
            let _ = app.post_message(if window {
                crate::message_reporters::new_window_failed(&error)
            } else {
                crate::message_reporters::new_tab_failed(&error)
            });
        };
        if let Err(e) = crate::agent_identity::ensure(name, create) {
            fail(self, format!("identity {name}: {e}"));
            return;
        }
        if window {
            match self.proxy.as_ref() {
                Some(proxy) => {
                    let _ = proxy.send_event(crate::Wake::CreateWindowWithIdentity {
                        identity: name.to_string(),
                    });
                }
                None => fail(self, "no event loop to host the new window".to_string()),
            }
        } else if let Err(e) = self.spawn_tab_session(Some(wid), None, None, Some(name)) {
            fail(self, e);
        }
    }

    /// While the picker is open on `wid`, drive it from the keyboard and
    /// SWALLOW every key (the modal-overlay gate contract): printable chars
    /// filter, Backspace deletes, Up/Down move, Enter chooses, Esc closes.
    /// Mirrors `on_key_palette_mode`.
    pub(crate) fn on_key_session_picker_mode(
        &mut self,
        wid: WindowId,
        ev: &winit::event::KeyEvent,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
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

    /// The ENGINE-NEUTRAL twin of [`Self::on_key_session_picker_mode`] —
    /// reached by controller `key`/`text` verbs, mirroring
    /// `palette_input_event`. The caller still swallows the event from the
    /// PTY.
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
        // THE KEYPAD IS ITS MAIN-BLOCK TWIN ON AN OVERLAY, as it is on a native
        // page and in the seam's press classifier. This card reads the key for
        // what it MEANS, and `keymap::build_key_input` hands it the keypad
        // identity (`Numpad5`, `NumpadEnd`) so the PTY encoders can tell KP_5
        // from 5 — which the arms below have no case for: a keypad digit
        // typed NOTHING into the filter and a keypad arrow moved no row.
        // `InputEvent::keypad_folded` is the one fold, shared with the native
        // pages, and a controller's `key kp5` takes the same road.
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

    // ---- Pointer boundary (the palette_claims_pointer discipline) ----------

    /// Modal pointer boundary: whether the open picker owns the gesture on
    /// `wid`. Mirrors [`Self::tab_menu_claims_pointer`].
    pub(crate) fn session_picker_claims_pointer(&mut self, wid: WindowId) -> bool {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.session_picker().is_none())
        {
            return false;
        }
        let mut changed = None;
        if let Some((_, view)) = self.active_native_view(wid)
            && let Some(state) = self.native_runtime.view_state_mut(view)
        {
            let common = state.common_mut();
            let hovered = common.hovered.take().is_some();
            let pressed = common.pressed.take().is_some();
            if hovered || pressed {
                changed = Some(view);
            }
        }
        if let Some(view) = changed {
            self.invalidate_native_view_cache(wid, view, crate::native_app::DamageRegion::All);
            self.request_redraw_all_windows();
        }
        self.sync_session_picker_pointer_cursor(wid);
        true
    }

    /// The FILTERED row index under a window-space point, through the same
    /// zoom-aware transform the card was composited with.
    fn session_picker_row_at_pointer(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let picker = self.windows.get(&wid)?.session_picker()?;
        let transform = self.overlay_coordinate_transform(wid)?;
        let (frame_x, frame_y) = self.window_to_frame(wid, x, y);
        let local_x = (frame_x - transform.origin_x) / f64::from(transform.scale);
        let local_y = (frame_y - transform.origin_y) / f64::from(transform.scale);
        crate::session_picker::picker_row_hit(
            picker,
            &transform.geom,
            local_x as f32,
            local_y as f32,
        )
    }

    /// Keep the OS cursor aligned with the picker's row hover.
    pub(crate) fn sync_session_picker_pointer_cursor(&mut self, wid: WindowId) {
        let pointer = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.session_picker())
            .is_some_and(SessionPickerState::pointer_over_row);
        if let Some(ws) = self.windows.get_mut(&wid)
            && (ws.hover_pointer != pointer || ws.native_text_cursor)
        {
            ws.hover_pointer = pointer;
            ws.native_text_cursor = false;
            if let Some(w) = &ws.os_window {
                w.set_cursor(if pointer {
                    CursorIcon::Pointer
                } else {
                    CursorIcon::Default
                });
            }
        }
    }

    fn repaint_session_picker_pointer(&mut self, wid: WindowId, changed: bool) {
        self.sync_session_picker_pointer_cursor(wid);
        if !changed {
            return;
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        self.overlay_a11y_update();
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

    /// Settle a left release: same-row press+release chooses through the SAME
    /// intent-dispatch seam as Enter.
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
        let Some((subject, intent, chosen)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.session_picker())
            .and_then(|p| {
                hit.and_then(|idx| p.row_at_filtered(idx))
                    .and_then(|row| p.choice_for(row))
                    .map(|choice| (p.subject.clone(), p.intent, choice))
            })
        else {
            return;
        };
        self.session_picker_exit(wid);
        self.settle_picker_choice(wid, subject, intent, chosen);
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

    /// Route a connection COMMAND id invoked against `subject` (design §2.3):
    /// the shared dispatch for the menu-bar-less ids — `session.connect_to`
    /// opens the Connect picker; `session.configure_connection` opens the
    /// sheet directly with exactly one peer, the picker with several, and
    /// refuses with none; `session.disconnect` disconnects directly with one
    /// peer, picks with several — NEVER guesses.
    pub(crate) fn open_connection_ui(
        &mut self,
        wid: WindowId,
        subject: SessionId,
        action: crate::menu::MenuAction,
    ) {
        use crate::menu::MenuAction;
        match action {
            MenuAction::ConnectToSession => {
                let _ = self.open_session_picker(wid, subject, PickerIntent::Connect);
            }
            MenuAction::ConfigureConnection | MenuAction::DisconnectSession => {
                let peers: Vec<SessionId> = self
                    .connection_facts(&subject)
                    .into_iter()
                    .map(|f| f.peer_sid)
                    .collect();
                let configure = matches!(action, MenuAction::ConfigureConnection);
                match peers.as_slice() {
                    [] => {
                        aterm_log::info!(
                            "{}: session {} has no connection",
                            if configure { "configure" } else { "disconnect" },
                            subject.as_str()
                        );
                    }
                    [peer] => {
                        // Unambiguous: one peer acts directly (§2.3 — the
                        // sheet when one; disconnect needs no picker).
                        let peer = peer.clone();
                        if configure {
                            let _ = self.open_confirm_card(wid, subject, peer, None, "menu");
                        } else {
                            self.disconnect_pair(&subject, &peer, "menu");
                        }
                    }
                    _ => {
                        let intent = if configure {
                            PickerIntent::Configure
                        } else {
                            PickerIntent::Disconnect
                        };
                        let _ = self.open_session_picker(wid, subject, intent);
                    }
                }
            }
            other => {
                aterm_log::info!("open_connection_ui: {other:?} is not a connection id");
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use aterm_session::ConnectionKind;

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

    fn connect(app: &App, src: &aterm_session::SessionId, dst: &aterm_session::SessionId) {
        let ctx = {
            let g = app.store.read().unwrap();
            g.by_sid(dst).unwrap().ctx.clone()
        };
        assert!(crate::connections::connect_in(
            &app.connections,
            src,
            dst,
            &ctx.edges,
            &ctx.nonce,
            ConnectionKind::Both,
            "test",
        ));
    }

    /// ROUND 18'S IDENTITY ROWS, through the one picker surface: New Tab /
    /// Window With Identity… lists every identity on disk by name (with how
    /// many live sessions wear it) and a last row that creates the identity the
    /// FILTER names. A name that does not parse, or one already listed, chooses
    /// nothing and keeps the picker open; a valid new name provisions it (the
    /// menu's one create path, as `spawn identity=` is the wire's) and closes.
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
            listed[0].contains("intent=new-window-identity") && listed[0].contains("subject=-")
        );
        assert!(listed[1].contains("identity=alpha") && listed[2].contains("identity=beta"));
        assert!(listed[3].contains("identity=+new"), "{listed:?}");

        // An EXISTING name typed in the filter: its own row is chosen, and the
        // new-identity row refuses to recreate it.
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

        // A valid NEW name: the new-identity row provisions it and closes.
        for _ in 0.."Bad Name".len() {
            app.session_picker_backspace(wid);
        }
        for c in "Gamma".chars() {
            app.session_picker_filter_push(wid, c);
        }
        assert_eq!(
            app.windows[&wid]
                .session_picker()
                .unwrap()
                .new_identity_name(),
            Some("gamma".to_string()),
            "the typed name is folded, as `parse_name` folds it"
        );
        app.session_picker_activate(wid);
        assert!(
            app.windows[&wid].session_picker().is_none(),
            "choosing closes"
        );
        assert!(
            root.join("gamma").is_dir(),
            "the new identity was provisioned under the (scratch) identities root"
        );
        assert_eq!(
            crate::agent_identity::roster(),
            ["alpha", "beta", "gamma"],
            "and the roster now lists it"
        );
    }

    /// The Connect picker lists every OTHER live session (never the subject),
    /// annotates connected peers, and its selection opens THE shared confirm
    /// card for subject ⇄ chosen.
    #[test]
    fn connect_picker_lists_others_and_selection_opens_the_card() {
        let (mut app, wid, sids) = app_with_three();
        connect(&app, &sids[0], &sids[2]);
        assert!(app.open_session_picker(wid, sids[0].clone(), PickerIntent::Connect));
        assert_eq!(
            app.windows[&wid].overlay().map(|o| o.kind()),
            Some(OverlayKind::SessionPicker)
        );
        let lines = app.windows[&wid].session_picker().unwrap().controls_lines();
        assert!(
            !lines
                .iter()
                .any(|l| l.contains(&format!("sid={}", sids[0].as_str())) && l.contains("row")),
            "the subject never lists itself: {lines:?}"
        );
        assert!(
            lines.iter().any(
                |l| l.contains(&format!("sid={}", sids[2].as_str())) && l.contains("connected")
            ),
            "connected peers are annotated: {lines:?}"
        );

        // Walk the cursor to the connected peer's row and choose it.
        let target = picker_row_ordinal(&app, wid, &sids[2]);
        app.session_picker_move(wid, target as isize);
        app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
        let card = app.windows[&wid]
            .conn_card()
            .expect("selection opens the card");
        assert_eq!(card.src, sids[0]);
        assert_eq!(card.dst, sids[2]);
        // Prefilled from the live pair (Both exists already).
        assert_eq!(card.kind, ConnectionKind::Both);
    }

    /// The open picker's FILTERED ordinal of the row for `sid` — cursor
    /// navigation by exact identity, immune to fuzzy-filter cross-matches.
    fn picker_row_ordinal(app: &App, wid: WindowId, sid: &aterm_session::SessionId) -> usize {
        app.windows[&wid]
            .session_picker()
            .expect("picker open")
            .controls_lines()
            .iter()
            .skip(1)
            .position(|l| l.contains(&format!("sid={}", sid.as_str())))
            .expect("the target session is listed")
    }

    /// Filter narrows in the palette style and Esc closes without acting.
    #[test]
    fn filter_narrows_and_esc_closes_without_acting() {
        let (mut app, wid, sids) = app_with_three();
        assert!(app.open_session_picker(wid, sids[0].clone(), PickerIntent::Connect));
        app.session_picker_input_event(wid, &InputEvent::Text("zzzz-no-match".to_string()));
        let lines = app.windows[&wid].session_picker().unwrap().controls_lines();
        assert!(lines[0].contains("shown=0"), "{lines:?}");
        // Enter with no cursor row is a no-op — the picker stays open.
        app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
        assert!(app.windows[&wid].session_picker().is_some());
        // Backspace widens again.
        for _ in 0.."zzzz-no-match".len() {
            app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Backspace));
        }
        assert!(
            app.windows[&wid].session_picker().unwrap().controls_lines()[0].contains("shown=2")
        );
        app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Escape));
        assert!(
            app.windows[&wid].session_picker().is_none(),
            "Esc closed it"
        );
        assert!(
            app.connections.records().is_empty(),
            "closing minted nothing"
        );
    }

    /// `session.configure_connection` (id-invoked): one peer opens the sheet
    /// DIRECTLY; several route through the picker; none refuses. The
    /// disconnect id acts directly with one peer and never guesses among
    /// several (§2.3).
    #[test]
    fn id_invoked_configure_and_disconnect_route_through_the_picker() {
        use crate::menu::MenuAction;
        let (mut app, wid, sids) = app_with_three();

        // No connection: both ids refuse (no picker, no card, no guess).
        app.open_connection_ui(wid, sids[0].clone(), MenuAction::ConfigureConnection);
        assert!(app.windows[&wid].overlay().is_none());
        app.open_connection_ui(wid, sids[0].clone(), MenuAction::DisconnectSession);
        assert!(app.windows[&wid].overlay().is_none());

        // ONE peer: configure opens the sheet directly, prefilled.
        connect(&app, &sids[0], &sids[1]);
        app.open_connection_ui(wid, sids[0].clone(), MenuAction::ConfigureConnection);
        {
            let card = app.windows[&wid].conn_card().expect("one peer ⇒ the sheet");
            assert_eq!(card.dst, sids[1]);
        }
        app.conn_card_exit(wid);

        // SEVERAL peers: configure routes through the picker (never guesses).
        connect(&app, &sids[0], &sids[2]);
        app.open_connection_ui(wid, sids[0].clone(), MenuAction::ConfigureConnection);
        let picker = app.windows[&wid]
            .session_picker()
            .expect("several ⇒ picker");
        assert_eq!(picker.intent, PickerIntent::Configure);
        let lines = picker.controls_lines();
        assert!(lines[0].contains("rows=2"), "peers only: {lines:?}");
        app.session_picker_exit(wid);

        // Disconnect with several peers: picker; choosing one dissolves BOTH
        // directions of exactly that pair.
        app.open_connection_ui(wid, sids[0].clone(), MenuAction::DisconnectSession);
        assert_eq!(
            app.windows[&wid].session_picker().unwrap().intent,
            PickerIntent::Disconnect
        );
        let target = picker_row_ordinal(&app, wid, &sids[1]);
        app.session_picker_move(wid, target as isize);
        app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
        assert!(app.windows[&wid].session_picker().is_none());
        let records = app.connections.records();
        assert!(records.get(&(sids[0].clone(), sids[1].clone())).is_none());
        assert!(
            records.get(&(sids[0].clone(), sids[2].clone())).is_some(),
            "the other pair is untouched"
        );

        // Down to ONE peer: the disconnect id acts directly, no picker.
        drop(records);
        app.open_connection_ui(wid, sids[0].clone(), MenuAction::DisconnectSession);
        assert!(app.windows[&wid].overlay().is_none(), "direct, no picker");
        assert!(app.connections.records().is_empty());
    }

    /// Configure/Disconnect pickers list ONLY connected peers.
    #[test]
    fn configure_picker_lists_only_connected_peers() {
        let (mut app, wid, sids) = app_with_three();
        connect(&app, &sids[0], &sids[1]);
        connect(&app, &sids[0], &sids[2]);
        assert!(app.open_session_picker(wid, sids[0].clone(), PickerIntent::Configure));
        let lines = app.windows[&wid].session_picker().unwrap().controls_lines();
        assert!(lines[0].contains("rows=2"));
        assert!(
            lines.iter().skip(1).all(|l| l.contains("connected")),
            "{lines:?}"
        );
    }
}
