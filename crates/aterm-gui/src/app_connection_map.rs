// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the connection map ([`crate::connection_map`], design §5):
//! snapshot the fabric (chips grouped by window + one arrow per flow
//! direction) from the ONE edge fold, open the overlay on the frontmost
//! window WITH raise/focus (§5.1 host+raise — the `OperatorAction::Show`
//! shape), keep the OPEN map fresh through the §2.4 `ConnectionsChanged`
//! funnel, stamp the live lease/watcher annotations from the `who` seam at
//! paint time, and settle the selection acts — Enter raises through the SAME
//! body as the `raise` wire verb, Delete disconnects behind the inline
//! confirm through the same [`crate::connections`] seams as every other
//! surface.
//!
//! Modelled on `app_session_picker.rs`: the state lives in the one modal
//! [`crate::overlay::Overlay`] slot (keys structurally gated before the
//! terminal), every mutator repaints + refreshes the a11y tree, and the
//! pointer follows the palette's claim/hover/arm discipline.

use winit::window::CursorIcon;

use aterm_session::SessionId;

use crate::App;
use crate::WindowId;
use crate::connection_map::{ConnectionMapState, MapActivation, MapAnnotation, MapChip, MapFlow, MapGroup};

impl App {
    /// Snapshot the map's GRAPH: chips grouped by window (tab order, pane
    /// leaves included), a trailing `elsewhere` group for registered sessions
    /// no window shows plus FOREIGN sids appearing in edges (the §4.1 honesty
    /// rule — the tables are the record, so a wire-granted stranger lists),
    /// and one [`MapFlow`] per directed pair from the ONE aggregation
    /// ([`crate::connections::all_edges`] — the `flows` verb's collector,
    /// never a second fold). Lock discipline: registry snapshot first, each
    /// edge table locked briefly inside `all_edges`, no store lock held
    /// across a table lock.
    fn connection_map_parts(&self) -> (Vec<MapGroup>, Vec<MapFlow>) {
        let handles = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot()
        };
        let title_of = |h: &crate::session_store::SessionHandle| {
            h.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get("title")
                .map(str::to_string)
                .unwrap_or_else(|| h.title.clone())
        };
        let by_local: std::collections::HashMap<u64, &crate::session_store::SessionHandle> =
            handles.iter().map(|h| (h.local_id, h)).collect();

        // Flows: fold the edge rows per DIRECTED pair; write-class ops spell
        // the push half (the `pair_kinds` rule), self-loops spell nothing.
        let mut folded: std::collections::BTreeMap<(String, String), (SessionId, SessionId, bool)> =
            std::collections::BTreeMap::new();
        for edge in crate::connections::all_edges(&self.store) {
            if edge.src == edge.dst {
                continue; // §1.5: a self-loop is never a connection
            }
            let write = matches!(
                edge.op,
                aterm_session::Op::WriteInput | aterm_session::Op::Signal
            );
            let entry = folded
                .entry((edge.src.as_str().to_string(), edge.dst.as_str().to_string()))
                .or_insert((edge.src, edge.dst, false));
            entry.2 |= write;
        }
        let flows: Vec<MapFlow> = folded
            .into_values()
            .map(|(src, dst, push)| MapFlow { src, dst, push })
            .collect();

        // Groups: walk each window's tabs (and every pane LEAF — a split's
        // sibling sessions belong to the same window group, not "elsewhere").
        let mut wids: Vec<WindowId> = self.windows.keys().copied().collect();
        wids.sort();
        let mut placed: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let mut groups: Vec<MapGroup> = Vec::new();
        for (ordinal, wid) in wids.iter().enumerate() {
            let Some(ws) = self.windows.get(wid) else {
                continue;
            };
            let mut chips: Vec<MapChip> = Vec::new();
            for tab in ws.tab_set.tabs() {
                for view in tab.root.leaves() {
                    let Some(local) = self
                        .view_store
                        .get(view)
                        .copied()
                        .and_then(crate::tab_model::View::terminal_session)
                    else {
                        continue;
                    };
                    let Some(h) = by_local.get(&local) else {
                        continue; // an unregistered stub labels no chip
                    };
                    if placed.insert(local) {
                        chips.push(MapChip {
                            sid: h.sid.clone(),
                            local_id: Some(local),
                            title: title_of(h),
                        });
                    }
                }
            }
            if chips.is_empty() {
                continue; // an all-native window has no session chips to head
            }
            // Header: the window ordinal + its active tab's presentation title
            // (the strip's own words — no extra term locks on this path).
            let active_title = ws
                .tab_set
                .active_index()
                .and_then(|i| ws.tab_set.tabs().get(i))
                .map(|t| {
                    crate::session_timeline::sanitize_presentation_line(&t.presentation.title, 40)
                })
                .filter(|t| !t.is_empty());
            let label = match active_title {
                Some(t) => format!("Window {} \u{2014} {t}", ordinal + 1),
                None => format!("Window {}", ordinal + 1),
            };
            groups.push(MapGroup { label, chips });
        }

        // The trailing group: registered-but-unplaced sessions, then foreign
        // sids named only by edge rows (not raisable — `local_id: None`).
        let mut elsewhere: Vec<MapChip> = handles
            .iter()
            .filter(|h| !placed.contains(&h.local_id))
            .map(|h| MapChip {
                sid: h.sid.clone(),
                local_id: Some(h.local_id),
                title: title_of(h),
            })
            .collect();
        let known: std::collections::HashSet<&str> = handles
            .iter()
            .map(|h| h.sid.as_str())
            .collect();
        let mut foreign: Vec<&SessionId> = Vec::new();
        for flow in &flows {
            for sid in [&flow.src, &flow.dst] {
                if !known.contains(sid.as_str()) && !foreign.contains(&sid) {
                    foreign.push(sid);
                }
            }
        }
        elsewhere.extend(foreign.into_iter().map(|sid| MapChip {
            sid: sid.clone(),
            local_id: None,
            title: "unknown".to_string(),
        }));
        if !elsewhere.is_empty() {
            groups.push(MapGroup {
                label: "elsewhere".to_string(),
                chips: elsewhere,
            });
        }
        (groups, flows)
    }

    /// The live per-session annotations from the SAME seam the `who` verb
    /// reads — the turn lease on each session's ctx + the subscriber registry
    /// (both main-thread-reachable: the App owns `store` and `subscribers`).
    fn connection_map_annotations(&self) -> std::collections::BTreeMap<String, MapAnnotation> {
        let handles = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.snapshot()
        };
        let subs = self.subscribers.lock().unwrap_or_else(|p| p.into_inner());
        let now_us = crate::metrics::now_us();
        handles
            .iter()
            .map(|h| {
                let driving = h
                    .ctx
                    .turn_lease
                    .lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .as_ref()
                    .and_then(|l| l.driving_token(now_us));
                (
                    h.sid.as_str().to_string(),
                    MapAnnotation {
                        driving,
                        watchers: subs.watchers(h.local_id),
                    },
                )
            })
            .collect()
    }

    /// Open the connection map (§5.1 host+raise): on the FRONTMOST window,
    /// which is raised/focused with it — the `OperatorAction::Show` shape.
    /// `Err` (nothing opens) with no window resolvable, so the wire caller
    /// reports honestly; the menu path logs and moves on (the palette-enter
    /// precedent).
    pub(crate) fn open_connection_map(&mut self) -> Result<(), String> {
        let Some(wid) = self.frontmost_window else {
            return Err("no front window to open the connection map on".to_string());
        };
        let (groups, flows) = self.connection_map_parts();
        let mut state = ConnectionMapState::new(wid, groups, flows);
        let _ = state.set_annotations(self.connection_map_annotations());
        let Some(ws) = self.windows.get_mut(&wid) else {
            return Err("the front window is gone".to_string());
        };
        // Structural mutual exclusion: the one overlay slot.
        ws.overlay = Some(crate::overlay::Overlay::ConnectionMap(state));
        ws.scroll_residual = 0.0;
        if let Some(w) = &ws.os_window {
            // §5.1 host+raise: the map is not just placed on the front window
            // — that window comes forward with it.
            w.focus_window();
            w.request_redraw();
        }
        self.settle_pointer_drags(wid);
        let _ = self.connection_map_claims_pointer(wid);
        if let Some((x, y)) = self.windows.get(&wid).map(|ws| ws.last_cursor_px) {
            self.connection_map_pointer_motion(wid, x, y);
        }
        self.overlay_a11y_update();
        Ok(())
    }

    /// Close the map on `wid` (no-op unless it is the open variant).
    pub(crate) fn connection_map_exit(&mut self, wid: WindowId) {
        let mut closed = false;
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.connection_map().is_some()
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

    /// Re-snapshot the graph into every OPEN map — the §2.4 freshness leg of
    /// [`App::refresh_connection_surfaces`]: an authority act recomposes the
    /// map like the marks and menus, selection surviving by identity.
    pub(crate) fn connection_map_refresh_all(&mut self) {
        let open: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.connection_map().is_some())
            .map(|(wid, _)| *wid)
            .collect();
        if open.is_empty() {
            return;
        }
        let (groups, flows) = self.connection_map_parts();
        let annotations = self.connection_map_annotations();
        for wid in open {
            if let Some(m) = self.windows.get_mut(&wid).and_then(|ws| ws.connection_map_mut()) {
                m.retarget(groups.clone(), flows.clone());
                let _ = m.set_annotations(annotations.clone());
            }
        }
        self.overlay_a11y_update();
    }

    /// Stamp the live annotations into an open map on `wid` — called from the
    /// redraw path, so the §5.2 lease/watcher terms are read AT PAINT TIME
    /// (liveness has no wake funnel, §5.1: the map is exactly as fresh as its
    /// most recent paint; the annotation term in the fingerprint makes a
    /// changed value re-present through the ordinary repaint key).
    pub(crate) fn connection_map_prepaint(&mut self, wid: WindowId) {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.connection_map().is_none())
        {
            return;
        }
        let annotations = self.connection_map_annotations();
        if let Some(m) = self.windows.get_mut(&wid).and_then(|ws| ws.connection_map_mut()) {
            let _ = m.set_annotations(annotations);
        }
    }

    fn connection_map_repaint(&mut self, wid: WindowId) {
        self.sync_connection_map_pointer_cursor(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// Move the map cursor by `delta` over the chip/arrow items.
    pub(crate) fn connection_map_move(&mut self, wid: WindowId, delta: isize) {
        if let Some(m) = self.windows.get_mut(&wid).and_then(|ws| ws.connection_map_mut()) {
            m.move_selection(delta);
        }
        self.connection_map_repaint(wid);
    }

    /// The Esc press: cancel an armed confirm first, close second.
    pub(crate) fn connection_map_escape(&mut self, wid: WindowId) {
        let close = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.connection_map_mut())
            .is_some_and(ConnectionMapState::escape);
        if close {
            self.connection_map_exit(wid);
        } else {
            self.connection_map_repaint(wid);
        }
    }

    /// The Delete/Backspace press: the inline-confirm two-step ending in a
    /// single-direction disconnect (§5.2 — Delete acts on ONE arrow, unlike
    /// the menu's whole-pair Disconnect).
    pub(crate) fn connection_map_delete(&mut self, wid: WindowId) {
        let resolved = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.connection_map_mut())
            .and_then(ConnectionMapState::delete_pressed);
        if let Some((src, dst)) = resolved {
            self.connection_map_disconnect(&src, &dst);
        }
        self.connection_map_repaint(wid);
    }

    /// The Enter/Click activation: chips raise (the `raise` verb's body —
    /// the map closes first so focus lands where the raise put it), an armed
    /// flow disconnects, an unarmed flow arms its confirm.
    pub(crate) fn connection_map_activate(&mut self, wid: WindowId) {
        let act = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.connection_map_mut())
            .map(ConnectionMapState::activate);
        match act {
            Some(MapActivation::Raise(local, sid)) => {
                let Some(local) = local else {
                    aterm_log::info!(
                        "connection map: {} is not hosted here (nothing to raise)",
                        sid.as_str()
                    );
                    self.connection_map_repaint(wid);
                    return;
                };
                self.connection_map_exit(wid);
                if let Err(e) = self.raise_session_by_id(local) {
                    aterm_log::info!("connection map raise {}: {e}", sid.as_str());
                }
            }
            Some(MapActivation::Disconnect(src, dst)) => {
                self.connection_map_disconnect(&src, &dst);
                self.connection_map_repaint(wid);
            }
            Some(MapActivation::Armed) => self.connection_map_repaint(wid),
            Some(MapActivation::None) | None => {}
        }
    }

    /// Dissolve exactly the `src → dst` direction (origin `map`): recorded
    /// halves pair-precisely by held token, unrecorded wire-grant rows via
    /// the sweep ([`crate::connections::disconnect_kind_in`]). Rows live in
    /// the DESTINATION's table; a dst gone from the registry has no table
    /// left to touch, so only the refresh runs (the menu's honesty rule: the
    /// map said "connected", so recompose to the truth regardless).
    fn connection_map_disconnect(&mut self, src: &SessionId, dst: &SessionId) {
        let ctx = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.by_sid(dst).map(|h| h.ctx.clone())
        };
        if let Some(ctx) = ctx {
            let _ = crate::connections::disconnect_kind_in(
                &self.connections,
                src,
                dst,
                &ctx.edges,
                None,
                "map",
            );
        }
        // The §2.4 poke: marks + menus + the open map itself recompose now.
        self.refresh_connection_surfaces();
    }

    /// While the map is open on `wid`, drive it from the keyboard and SWALLOW
    /// every key (the modal-overlay gate contract): Up/Down move, Enter
    /// activates, Delete/Backspace runs the disconnect confirm, Esc cancels
    /// then closes. Mirrors `on_key_session_picker_mode`.
    pub(crate) fn on_key_connection_map_mode(
        &mut self,
        wid: WindowId,
        ev: &winit::event::KeyEvent,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.connection_map())
            .is_none()
        {
            return false;
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.connection_map_escape(wid),
            Key::Named(NamedKey::ArrowUp) => self.connection_map_move(wid, -1),
            Key::Named(NamedKey::ArrowDown) => self.connection_map_move(wid, 1),
            Key::Named(NamedKey::Enter) => self.connection_map_activate(wid),
            Key::Named(NamedKey::Delete | NamedKey::Backspace) => {
                self.connection_map_delete(wid);
            }
            _ => {}
        }
        true
    }

    /// The ENGINE-NEUTRAL twin of [`Self::on_key_connection_map_mode`] —
    /// reached by controller `key`/`text` verbs. The caller still swallows
    /// the event from the PTY.
    pub(crate) fn connection_map_input_event(
        &mut self,
        wid: WindowId,
        ev: &crate::input::InputEvent,
    ) {
        use crate::input::InputEvent;
        use aterm_types::keyboard::{Key as TKey, KeyEventType, NamedKey as TNamed};
        if self
            .windows
            .get(&wid)
            .and_then(|ws| ws.connection_map())
            .is_none()
        {
            return;
        }
        if let InputEvent::Key {
            key, event_type, ..
        } = ev
        {
            if matches!(event_type, KeyEventType::Release) {
                return;
            }
            match key {
                TKey::Named(TNamed::Escape) => self.connection_map_escape(wid),
                TKey::Named(TNamed::ArrowUp) => self.connection_map_move(wid, -1),
                TKey::Named(TNamed::ArrowDown) => self.connection_map_move(wid, 1),
                TKey::Named(TNamed::Enter | TNamed::NumpadEnter) => {
                    self.connection_map_activate(wid);
                }
                TKey::Named(TNamed::Delete | TNamed::Backspace) => {
                    self.connection_map_delete(wid);
                }
                _ => {}
            }
        }
    }

    // ---- Pointer boundary (the palette_claims_pointer discipline) ----------

    /// Modal pointer boundary: whether the open map owns the gesture on
    /// `wid`. Mirrors [`Self::session_picker_claims_pointer`].
    pub(crate) fn connection_map_claims_pointer(&mut self, wid: WindowId) -> bool {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.connection_map().is_none())
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
        self.sync_connection_map_pointer_cursor(wid);
        true
    }

    /// The item index under a window-space point, through the same zoom-aware
    /// transform the card was composited with.
    fn connection_map_item_at_pointer(&self, wid: WindowId, x: f64, y: f64) -> Option<usize> {
        let map = self.windows.get(&wid)?.connection_map()?;
        let transform = self.overlay_coordinate_transform(wid)?;
        let (frame_x, frame_y) = self.window_to_frame(wid, x, y);
        let local_x = (frame_x - transform.origin_x) / f64::from(transform.scale);
        let local_y = (frame_y - transform.origin_y) / f64::from(transform.scale);
        crate::connection_map::map_item_hit(map, &transform.geom, local_x as f32, local_y as f32)
    }

    /// Keep the OS cursor aligned with the map's item hover.
    pub(crate) fn sync_connection_map_pointer_cursor(&mut self, wid: WindowId) {
        let pointer = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.connection_map())
            .is_some_and(ConnectionMapState::pointer_over_item);
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

    fn repaint_connection_map_pointer(&mut self, wid: WindowId, changed: bool) {
        self.sync_connection_map_pointer_cursor(wid);
        if !changed {
            return;
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// Hover-select the item under a pointer motion.
    pub(crate) fn connection_map_pointer_motion(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.connection_map_item_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.connection_map_mut())
            .is_some_and(|m| m.pointer_hover(hit));
        self.repaint_connection_map_pointer(wid, changed);
    }

    pub(crate) fn connection_map_pointer_press(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.connection_map_item_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.connection_map_mut())
            .is_some_and(|m| m.pointer_press(hit));
        self.repaint_connection_map_pointer(wid, changed);
    }

    /// Settle a left release: same-item press+release activates through the
    /// SAME seam as Enter (chip raises; a flow arms, then confirms).
    pub(crate) fn connection_map_pointer_release(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.connection_map_item_at_pointer(wid, x, y);
        let (changed, activate) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.connection_map_mut())
            .map_or((false, false), |m| m.pointer_release(hit));
        self.repaint_connection_map_pointer(wid, changed);
        if activate {
            self.connection_map_activate(wid);
        }
    }

    /// Scroll the band (wheel) — the cursor stays put; the palette's residual
    /// banking already normalized `delta` to whole lines.
    pub(crate) fn connection_map_pointer_wheel(&mut self, wid: WindowId, delta: isize) {
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.connection_map_mut())
            .is_some_and(|m| m.scroll_by(delta));
        self.repaint_connection_map_pointer(wid, changed);
    }
}

#[cfg(test)]
mod tests {
    use aterm_session::ConnectionKind;

    use crate::App;
    use crate::WindowId;
    use crate::app_introspect::AuxTarget;
    use crate::input::InputEvent;
    use crate::overlay::OverlayKind;

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

    /// The open map's ITEM ordinal of the chip for `sid` — walk target for
    /// cursor navigation, immune to listing-order assumptions.
    fn chip_ordinal(app: &App, wid: WindowId, sid: &aterm_session::SessionId) -> usize {
        app.windows[&wid]
            .connection_map()
            .expect("map open")
            .controls_lines()
            .iter()
            .filter(|l| l.contains(" chip ") || l.contains(" flow "))
            .position(|l| l.contains(&format!("sid={}", sid.as_str())))
            .expect("the session is listed")
    }

    /// `open connections` (the §5.1 host path): the map opens on the front
    /// window, groups the sessions under a window header, lists one labeled
    /// arrow per direction, and `controls connections` mirrors it; closed it
    /// reports honestly. `view.connections` stays the palette id and the
    /// `connections` keyword round-trips through `AuxTarget`.
    #[test]
    fn open_lists_groups_and_flows_and_controls_mirror() {
        let (mut app, wid, sids) = app_with_three();
        connect(&app, &sids[0], &sids[2]);
        assert_eq!(
            app.read_aux_controls(AuxTarget::Connections),
            vec!["connections open=false".to_string()]
        );
        assert!(app.open_connection_map().is_ok());
        assert_eq!(
            app.windows[&wid].overlay().map(|o| o.kind()),
            Some(OverlayKind::ConnectionMap)
        );
        // The overlay keyword pipes back through `controls <kind>` (§5.3).
        assert_eq!(AuxTarget::parse("connections"), Some(AuxTarget::Connections));
        assert_eq!(
            app.windows[&wid].overlay().unwrap().kind().keyword(),
            "connections"
        );
        let lines = app.read_aux_controls(AuxTarget::Connections);
        assert!(lines[0].contains("sessions=3") && lines[0].contains("flows=1"), "{lines:?}");
        assert!(
            lines.iter().any(|l| l.starts_with("connections group label=\"Window 1")),
            "window header present: {lines:?}"
        );
        assert!(
            lines.iter().any(|l| l.contains(&format!(
                "flow src={} dst={} kind=push",
                sids[0].as_str(),
                sids[2].as_str()
            ))),
            "the Both pair folds to one push arrow: {lines:?}"
        );
        // The wire/menu close leg.
        assert!(app.close_aux_overlay(AuxTarget::Connections).is_ok());
        assert!(app.windows[&wid].connection_map().is_none());
    }

    /// Enter on a chip RAISES through the `raise` body: the hosting window's
    /// tab switches to that session and the map closes.
    #[test]
    fn enter_on_a_chip_raises_and_closes() {
        let (mut app, wid, sids) = app_with_three();
        app.switch_tab_in(wid, 0);
        assert!(app.open_connection_map().is_ok());
        let target = chip_ordinal(&app, wid, &sids[2]);
        app.connection_map_move(wid, target as isize);
        app.connection_map_input_event(wid, &key(aterm_types::keyboard::NamedKey::Enter));
        assert!(app.windows[&wid].connection_map().is_none(), "map closed");
        assert_eq!(
            app.windows[&wid].tab_set.active_index(),
            Some(2),
            "the raise selected the session's tab"
        );
    }

    /// Delete runs the inline confirm two-step on ONE arrow direction, the
    /// §2.4 refresh recomposes the open map, and Esc cancels an armed confirm
    /// before it ever closes the map.
    #[test]
    fn delete_confirms_inline_and_esc_cancels_first() {
        let (mut app, wid, sids) = app_with_three();
        connect(&app, &sids[0], &sids[1]);
        connect(&app, &sids[1], &sids[0]);
        assert!(app.open_connection_map().is_ok());
        // Walk to the a→b flow (3 chips, then flows in sid order).
        let flow_item = {
            let lines = app.windows[&wid].connection_map().unwrap().controls_lines();
            lines
                .iter()
                .filter(|l| l.contains(" chip ") || l.contains(" flow "))
                .position(|l| {
                    l.contains(&format!(
                        "flow src={} dst={}",
                        sids[0].as_str(),
                        sids[1].as_str()
                    ))
                })
                .expect("the flow is listed")
        };
        app.connection_map_move(wid, flow_item as isize);
        // First Delete only ARMS; nothing dissolves.
        app.connection_map_input_event(wid, &key(aterm_types::keyboard::NamedKey::Delete));
        assert!(
            app.connections
                .records()
                .contains_key(&(sids[0].clone(), sids[1].clone())),
            "armed, not dissolved"
        );
        // Esc cancels the confirm — the map STAYS open.
        app.connection_map_input_event(wid, &key(aterm_types::keyboard::NamedKey::Escape));
        assert!(app.windows[&wid].connection_map().is_some());
        assert!(
            app.windows[&wid].connection_map().unwrap().controls_lines()[0].contains("confirm=-")
        );
        // Delete twice dissolves EXACTLY that direction; the reverse survives
        // and the open map recomposed through the refresh funnel.
        app.connection_map_input_event(wid, &key(aterm_types::keyboard::NamedKey::Delete));
        app.connection_map_input_event(wid, &key(aterm_types::keyboard::NamedKey::Delete));
        {
            let records = app.connections.records();
            assert!(records.get(&(sids[0].clone(), sids[1].clone())).is_none());
            assert!(records.get(&(sids[1].clone(), sids[0].clone())).is_some());
        }
        let lines = app.windows[&wid].connection_map().unwrap().controls_lines();
        assert!(lines[0].contains("flows=1"), "map recomposed live: {lines:?}");
        // A bare Esc now closes.
        app.connection_map_input_event(wid, &key(aterm_types::keyboard::NamedKey::Escape));
        assert!(app.windows[&wid].connection_map().is_none());
    }

    /// A session CLOSE while the map is open: the close-time sweep dissolves
    /// its edges with no wake funnel, so the retire path itself must retarget
    /// the open map — the dead session's chip and arrows leave NOW, not on
    /// the next unrelated authority act.
    #[test]
    fn session_close_retargets_the_open_map() {
        let (mut app, _wid, sids) = app_with_three();
        connect(&app, &sids[0], &sids[2]);
        assert!(app.open_connection_map().is_ok());
        let lines = app.read_aux_controls(AuxTarget::Connections);
        assert!(lines[0].contains("sessions=3") && lines[0].contains("flows=1"), "{lines:?}");

        let local = {
            let g = app.store.read().unwrap();
            g.by_sid(&sids[2]).unwrap().local_id
        };
        app.retire_session_registration(local);

        let lines = app.read_aux_controls(AuxTarget::Connections);
        assert!(
            lines[0].contains("sessions=2") && lines[0].contains("flows=0"),
            "the open map re-snapshotted on close: {lines:?}"
        );
        assert!(
            !lines.iter().any(|l| l.contains(sids[2].as_str())),
            "no chip or arrow survives the closed session: {lines:?}"
        );
    }

    /// The paint-time annotation stamp reads the `who` seam: a live turn
    /// lease shows on the chip after the prepaint hook, and clears with it.
    #[test]
    fn prepaint_stamps_live_lease_annotations() {
        let (mut app, wid, sids) = app_with_three();
        assert!(app.open_connection_map().is_ok());
        let ctx = {
            let g = app.store.read().unwrap();
            g.by_sid(&sids[1]).unwrap().ctx.clone()
        };
        *ctx.turn_lease.lock().unwrap() = Some(crate::Lease::Turn(42));
        app.connection_map_prepaint(wid);
        let lines = app.windows[&wid].connection_map().unwrap().controls_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("sid={}", sids[1].as_str())) && l.contains("driving=42")),
            "{lines:?}"
        );
        *ctx.turn_lease.lock().unwrap() = None;
        app.connection_map_prepaint(wid);
        let lines = app.windows[&wid].connection_map().unwrap().controls_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains(&format!("sid={}", sids[1].as_str())) && l.contains("driving=-")),
            "{lines:?}"
        );
    }
}
