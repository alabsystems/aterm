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
use crate::session_picker::{PickerIntent, PickerRow, SessionPickerState};

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
            .filter(|h| {
                matches!(intent, PickerIntent::Connect) || peers.contains(h.sid.as_str())
            })
            .map(|h| {
                let title = h
                    .ctx
                    .meta
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .get("title")
                    .map(str::to_string)
                    .unwrap_or_else(|| h.title.clone());
                PickerRow {
                    connected: peers.contains(h.sid.as_str()),
                    sid: h.sid,
                    local_id: h.local_id,
                    title,
                }
            })
            .collect();
        // Stable listing (the connection_facts BTreeMap discipline): by sid.
        rows.sort_by(|a, b| a.sid.as_str().cmp(b.sid.as_str()));
        rows
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
        let state = SessionPickerState::new(wid, subject, subject_title, intent, rows);
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
                    .map(|row| (p.subject.clone(), p.intent, row.sid.clone()))
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
        subject: SessionId,
        intent: PickerIntent,
        chosen: SessionId,
    ) {
        match intent {
            PickerIntent::Connect | PickerIntent::Configure => {
                let _ = self.open_confirm_card(wid, subject, chosen, None, "menu");
            }
            PickerIntent::Disconnect => {
                self.disconnect_pair(&subject, &chosen, "menu");
            }
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
                    TKey::Named(TNamed::Enter | TNamed::NumpadEnter) => {
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
            InputEvent::Text(t) | InputEvent::Paste(t) => {
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
                    .map(|row| (p.subject.clone(), p.intent, row.sid.clone()))
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
        assert!(!lines.iter().any(|l| l.contains(&format!("sid={}", sids[0].as_str()))
            && l.contains("row")),
            "the subject never lists itself: {lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("sid={}", sids[2].as_str())) && l.contains("connected")),
            "connected peers are annotated: {lines:?}"
        );

        // Walk the cursor to the connected peer's row and choose it.
        let target = picker_row_ordinal(&app, wid, &sids[2]);
        app.session_picker_move(wid, target as isize);
        app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
        let card = app.windows[&wid].conn_card().expect("selection opens the card");
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
            app.windows[&wid]
                .session_picker()
                .unwrap()
                .controls_lines()[0]
                .contains("shown=2")
        );
        app.session_picker_input_event(wid, &key(aterm_types::keyboard::NamedKey::Escape));
        assert!(app.windows[&wid].session_picker().is_none(), "Esc closed it");
        assert!(app.connections.records().is_empty(), "closing minted nothing");
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
        let picker = app.windows[&wid].session_picker().expect("several ⇒ picker");
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
        assert!(lines.iter().skip(1).all(|l| l.contains("connected")), "{lines:?}");
    }
}
