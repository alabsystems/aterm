// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for the connection confirm/configure card ([`crate::conn_card`],
//! design §3.3 + §2.5): open (the tab menu's Configure…, the session picker's
//! choice, and — via [`App::open_confirm_card`] — the drag stage's drop),
//! drive (arrows/Tab move focus, Left/Right cycle, pointer on the chips), and
//! settle: **Enter confirms, Esc cancels, nothing mints before Confirm.**
//!
//! Modelled on `app_tab_menu.rs` throughout: the state lives in the single
//! modal [`crate::overlay::Overlay`] slot, so the key gate structurally sits
//! BEFORE every keybinding/`[key_sequences]`/PTY path (winit side) and the
//! engine-neutral `App::input` gate swallows controller bytes identically —
//! Enter/Esc can never leak into T's PTY (§3.3). Confirm executes the card's
//! pure [`crate::conn_card::ConnCardPlan`] through the ONE declarative
//! [`crate::connections::connect_in`] seam (set semantics, §2.5) and pokes the
//! §2.4 freshness funnel; confirming here is also a §1.4#8 first-use surface
//! (same once-per-config latch as the spawn presets, so never double-noticed).

use winit::window::CursorIcon;

use aterm_session::SessionId;

use crate::App;
use crate::WindowId;
use crate::conn_card::{ConnCardHit, ConnCardState, PairKinds};

impl App {
    /// The session's display title by sid: user meta title ▸ registry title
    /// (the fleet-glance rung), or `None` for an unregistered sid.
    pub(crate) fn session_title_by_sid(&self, sid: &SessionId) -> Option<String> {
        let g = self.store.read().unwrap_or_else(|p| p.into_inner());
        g.by_sid(sid).map(|h| {
            h.ctx
                .meta
                .lock()
                .unwrap_or_else(|p| p.into_inner())
                .get("title")
                .map(str::to_string)
                .unwrap_or_else(|| h.title.clone())
        })
    }

    /// The strip column of the tab chip hosting session `sid` in `wid` — the
    /// card's anchor (T's chip for a drop, §3.3). Band-left when the session
    /// has no chip there (overflow / other-window peer).
    fn conn_anchor_col(&self, wid: WindowId, sid: &SessionId) -> u16 {
        let local = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.by_sid(sid).map(|h| h.local_id)
        };
        let Some(local) = local else {
            return 0;
        };
        let Some(ws) = self.windows.get(&wid) else {
            return 0;
        };
        let index = (0..ws.tab_set.len())
            .find(|&i| self.tab_terminal_session(wid, i) == Some(local));
        let Some(index) = index else {
            return 0;
        };
        ws.tab_segments
            .iter()
            .find(|seg| matches!(seg.kind, crate::tab_bar::TabHit::Select(i) if i == index))
            .map_or(0, |seg| seg.start_col)
    }

    /// Open the shared confirm/configure card for `src` ⇄ `dst` in `window`
    /// (design §3.3 + §2.5 — ONE component; the drag stage calls this with
    /// origin `"drag"`, the menu/picker paths with `"menu"`). `prefill = None`
    /// derives the existing pair state from the LIVE edge tables (the §2.5
    /// configure prefill); `Some` lets the caller carry an already-derived
    /// snapshot. Self-drop refused (§3.3); `false` when either endpoint is
    /// unregistered. Nothing mints here — only Confirm acts.
    pub(crate) fn open_confirm_card(
        &mut self,
        window: WindowId,
        src: SessionId,
        dst: SessionId,
        prefill: Option<PairKinds>,
        origin: &'static str,
    ) -> bool {
        if src == dst {
            // §3.3: self-drop refused before any UI state moves.
            aterm_log::info!("connection card refused: self-connection");
            return false;
        }
        let (Some(src_title), Some(dst_title)) = (
            self.session_title_by_sid(&src),
            self.session_title_by_sid(&dst),
        ) else {
            aterm_log::info!("connection card refused: endpoint not registered");
            return false;
        };
        let prefill = prefill.unwrap_or_else(|| {
            let (s2d, d2s) = crate::connections::pair_kinds(&self.store, &src, &dst);
            PairKinds {
                src_to_dst: s2d,
                dst_to_src: d2s,
            }
        });
        let anchor_col = self.conn_anchor_col(window, &dst);
        let state = ConnCardState::new(
            window,
            src,
            src_title,
            dst,
            dst_title,
            prefill,
            origin,
            anchor_col,
            usize::from(self.tab_strip_rows),
        );
        let Some(ws) = self.windows.get_mut(&window) else {
            return false;
        };
        // §3.3: the popover is hosted on T's window — focus it first for the
        // drag path so Enter lands in the card, not wherever focus was.
        if origin == "drag"
            && let Some(w) = &ws.os_window
        {
            w.focus_window();
        }
        // Structural mutual exclusion: the one overlay slot.
        ws.overlay = Some(crate::overlay::Overlay::ConnCard(state));
        ws.scroll_residual = 0.0;
        if let Some(w) = &ws.os_window {
            w.request_redraw();
        }
        self.settle_pointer_drags(window);
        let _ = self.conn_card_claims_pointer(window);
        if let Some((x, y)) = self.windows.get(&window).map(|ws| ws.last_cursor_px) {
            self.conn_card_pointer_motion(window, x, y);
        }
        self.overlay_a11y_update();
        true
    }

    /// Close the card on `wid` WITHOUT acting — the Esc path. Nothing was
    /// minted before Confirm, so cancel has no state to undo.
    pub(crate) fn conn_card_exit(&mut self, wid: WindowId) {
        let mut closed = false;
        if let Some(ws) = self.windows.get_mut(&wid)
            && ws.conn_card().is_some()
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

    /// Confirm the card on `wid` (Enter / the Confirm button): close, then
    /// execute its pure plan through the declarative seams — `connect_in` per
    /// selected direction (set semantics, idempotent on an unchanged half),
    /// `disconnect_kind_in` per deselected-but-existing direction. Endpoints
    /// are re-resolved by sid NOW; one that died while the card was open makes
    /// its half a logged no-op. Ends with the §2.4 freshness poke and the
    /// §1.4#8 first-use notice (shared latch — never double-noticed).
    pub(crate) fn conn_card_confirm(&mut self, wid: WindowId) {
        let Some((plan, origin, src, dst)) = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.conn_card())
            .map(|card| (card.plan(), card.origin, card.src.clone(), card.dst.clone()))
        else {
            return;
        };
        self.conn_card_exit(wid);
        let dst_ctx = |app: &App, sid: &SessionId| {
            let g = app.store.read().unwrap_or_else(|p| p.into_inner());
            g.by_sid(sid).map(|h| h.ctx.clone())
        };
        let mut minted = false;
        for (from, to, kind) in &plan.connects {
            match dst_ctx(self, to) {
                Some(ctx) => {
                    minted |= crate::connections::connect_in(
                        &self.connections,
                        from,
                        to,
                        &ctx.edges,
                        &ctx.nonce,
                        *kind,
                        origin,
                    );
                }
                None => aterm_log::info!(
                    "connection card: peer {} vanished before confirm; half skipped",
                    to.as_str()
                ),
            }
        }
        for (from, to) in &plan.disconnects {
            match dst_ctx(self, to) {
                Some(ctx) => {
                    let _ = crate::connections::disconnect_kind_in(
                        &self.connections,
                        from,
                        to,
                        &ctx.edges,
                        None,
                        origin,
                    );
                }
                None => aterm_log::info!(
                    "connection card: peer {} vanished before confirm; half skipped",
                    to.as_str()
                ),
            }
        }
        // §1.4#8: confirming from this card is a first-use surface. The latch
        // is the SAME config-lifetime marker the spawn presets use, so exactly
        // one surface ever notices (no double-noticing).
        if minted
            && crate::connections::first_use_notice_should_show(crate::app_config::config_path())
        {
            let src_drives = plan.connects.iter().any(|(f, _, _)| *f == src);
            let dst_drives = plan.connects.iter().any(|(f, _, _)| *f == dst);
            self.notice = Some(crate::notice::TransientNotice::session_connection(
                crate::connections::first_use_connect_notice_text(src_drives, dst_drives),
                std::time::Instant::now(),
            ));
        }
        // The §2.4 freshness poke: marks + menus + tooltips recompose now (the
        // revision was bumped inside the seams above).
        self.refresh_connection_surfaces();
    }

    /// Dissolve BOTH directions of `a` ⇄ `b` (the menu Disconnect row and the
    /// picker's Disconnect intent share this): recorded halves pair-precisely
    /// by held token, unrecorded wire-grant rows via the op-filtered sweep.
    /// Rows TOWARD an unregistered endpoint died with its table, so only the
    /// registered halves exist to dissolve. Ends with the §2.4 poke —
    /// unconditionally, so a nothing-to-revoke race still recomposes to truth.
    pub(crate) fn disconnect_pair(&mut self, a: &SessionId, b: &SessionId, origin: &str) {
        let ctx_of = |app: &App, sid: &SessionId| {
            let g = app.store.read().unwrap_or_else(|p| p.into_inner());
            g.by_sid(sid).map(|h| h.ctx.clone())
        };
        // a → b half: rows live in b's table.
        if let Some(ctx) = ctx_of(self, b) {
            let _ = crate::connections::disconnect_kind_in(
                &self.connections,
                a,
                b,
                &ctx.edges,
                None,
                origin,
            );
        }
        // b → a half: rows live in a's table.
        if let Some(ctx) = ctx_of(self, a) {
            let _ = crate::connections::disconnect_kind_in(
                &self.connections,
                b,
                a,
                &ctx.edges,
                None,
                origin,
            );
        }
        self.refresh_connection_surfaces();
    }

    fn conn_card_repaint(&mut self, wid: WindowId) {
        self.sync_conn_card_pointer_cursor(wid);
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// Move the focused control row (Up/Down/Tab).
    pub(crate) fn conn_card_focus_move(&mut self, wid: WindowId) {
        if let Some(c) = self.windows.get_mut(&wid).and_then(|ws| ws.conn_card_mut()) {
            c.move_focus();
        }
        self.conn_card_repaint(wid);
    }

    /// Cycle the focused control's value (Left/Right).
    pub(crate) fn conn_card_cycle(&mut self, wid: WindowId, delta: isize) {
        if let Some(c) = self.windows.get_mut(&wid).and_then(|ws| ws.conn_card_mut()) {
            c.cycle_value(delta);
        }
        self.conn_card_repaint(wid);
    }

    /// While the card is open on `wid`, drive it from the keyboard and SWALLOW
    /// every key (the modal-overlay gate contract — §3.3: Enter/Esc never
    /// reach the PTY): Enter confirms, Esc cancels, Up/Down/Tab move the
    /// focus, Left/Right cycle the focused control; everything else is eaten.
    pub(crate) fn on_key_conn_card_mode(
        &mut self,
        wid: WindowId,
        ev: &winit::event::KeyEvent,
    ) -> bool {
        use winit::keyboard::{Key, NamedKey};
        if self.windows.get(&wid).and_then(|ws| ws.conn_card()).is_none() {
            return false;
        }
        match &ev.logical_key {
            Key::Named(NamedKey::Escape) => self.conn_card_exit(wid),
            Key::Named(NamedKey::Enter) => self.conn_card_confirm(wid),
            Key::Named(NamedKey::ArrowUp | NamedKey::ArrowDown | NamedKey::Tab) => {
                self.conn_card_focus_move(wid);
            }
            Key::Named(NamedKey::ArrowLeft) => self.conn_card_cycle(wid, -1),
            Key::Named(NamedKey::ArrowRight) => self.conn_card_cycle(wid, 1),
            _ => {}
        }
        true
    }

    /// The ENGINE-NEUTRAL twin of [`Self::on_key_conn_card_mode`] — reached by
    /// controller `key` verbs. The caller still swallows the event from the
    /// PTY (the `App::input` overlay gate).
    pub(crate) fn conn_card_input_event(&mut self, wid: WindowId, ev: &crate::input::InputEvent) {
        use aterm_types::keyboard::{Key as TKey, KeyEventType, NamedKey as TNamed};
        if self.windows.get(&wid).and_then(|ws| ws.conn_card()).is_none() {
            return;
        }
        if let crate::input::InputEvent::Key {
            key, event_type, ..
        } = ev
        {
            if matches!(event_type, KeyEventType::Release) {
                return;
            }
            match key {
                TKey::Named(TNamed::Escape) => self.conn_card_exit(wid),
                TKey::Named(TNamed::Enter | TNamed::NumpadEnter) => self.conn_card_confirm(wid),
                TKey::Named(TNamed::ArrowUp | TNamed::ArrowDown | TNamed::Tab) => {
                    self.conn_card_focus_move(wid);
                }
                TKey::Named(TNamed::ArrowLeft) => self.conn_card_cycle(wid, -1),
                TKey::Named(TNamed::ArrowRight) => self.conn_card_cycle(wid, 1),
                _ => {}
            }
        }
    }

    // ---- Pointer boundary (the palette_claims_pointer discipline) ----------

    /// Modal pointer boundary: whether the open card owns the gesture on
    /// `wid`, clearing native hover/press retained underneath (never a
    /// click-through layer). Mirrors [`Self::tab_menu_claims_pointer`].
    pub(crate) fn conn_card_claims_pointer(&mut self, wid: WindowId) -> bool {
        if self
            .windows
            .get(&wid)
            .is_none_or(|ws| ws.conn_card().is_none())
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
        self.sync_conn_card_pointer_cursor(wid);
        true
    }

    /// The hit target under a window-space point, through the same zoom-aware
    /// transform the card was composited with.
    fn conn_card_hit_at_pointer(&self, wid: WindowId, x: f64, y: f64) -> Option<ConnCardHit> {
        let card = self.windows.get(&wid)?.conn_card()?;
        let transform = self.overlay_coordinate_transform(wid)?;
        let (frame_x, frame_y) = self.window_to_frame(wid, x, y);
        let local_x = (frame_x - transform.origin_x) / f64::from(transform.scale);
        let local_y = (frame_y - transform.origin_y) / f64::from(transform.scale);
        crate::conn_card::conn_card_hit(card, &transform.geom, local_x as f32, local_y as f32)
    }

    /// Keep the OS cursor aligned with the card's chip hover.
    pub(crate) fn sync_conn_card_pointer_cursor(&mut self, wid: WindowId) {
        let pointer = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.conn_card())
            .is_some_and(ConnCardState::pointer_over_target);
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

    fn repaint_conn_card_pointer(&mut self, wid: WindowId, changed: bool) {
        self.sync_conn_card_pointer_cursor(wid);
        if !changed {
            return;
        }
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
        self.overlay_a11y_update();
    }

    /// Hover the chip under a pointer motion; outside points stay swallowed.
    pub(crate) fn conn_card_pointer_motion(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.conn_card_hit_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.conn_card_mut())
            .is_some_and(|c| c.pointer_hover(hit));
        self.repaint_conn_card_pointer(wid, changed);
    }

    pub(crate) fn conn_card_pointer_press(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.conn_card_hit_at_pointer(wid, x, y);
        let changed = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.conn_card_mut())
            .is_some_and(|c| c.pointer_press(hit));
        self.repaint_conn_card_pointer(wid, changed);
    }

    /// Settle a left release: same-target press+release activates — chips move
    /// the selection in place; Confirm/Cancel settle through the SAME seams as
    /// Enter/Esc.
    pub(crate) fn conn_card_pointer_release(&mut self, wid: WindowId, x: f64, y: f64) {
        let hit = self.conn_card_hit_at_pointer(wid, x, y);
        let (mut changed, activated) = self
            .windows
            .get_mut(&wid)
            .and_then(|ws| ws.conn_card_mut())
            .map_or((false, None), |c| c.pointer_release(hit));
        if let Some(target) = activated {
            match target {
                ConnCardHit::Confirm => {
                    self.repaint_conn_card_pointer(wid, changed);
                    self.conn_card_confirm(wid);
                    return;
                }
                ConnCardHit::Cancel => {
                    self.repaint_conn_card_pointer(wid, changed);
                    self.conn_card_exit(wid);
                    return;
                }
                chip => {
                    changed |= self
                        .windows
                        .get_mut(&wid)
                        .and_then(|ws| ws.conn_card_mut())
                        .is_some_and(|c| c.activate_hit(chip));
                }
            }
        }
        self.repaint_conn_card_pointer(wid, changed);
    }
}

#[cfg(test)]
mod tests {
    use aterm_session::{ConnectionKind, decide_edge};

    use crate::App;
    use crate::WindowId;
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

    /// Two registered stub sessions in one window; returns their sids.
    fn app_with_pair() -> (App, WindowId, aterm_session::SessionId, aterm_session::SessionId) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        let (a, b) = {
            let g = app.store.read().unwrap();
            let snap = g.snapshot();
            (snap[0].sid.clone(), snap[1].sid.clone())
        };
        (app, wid, a, b)
    }

    /// Opening seeds the §3.3 DECIDED default (S→T, both) and a self-pair is
    /// refused before any UI state moves.
    #[test]
    fn open_seeds_the_default_and_refuses_self() {
        let (mut app, wid, a, b) = app_with_pair();
        assert!(!app.open_confirm_card(wid, a.clone(), a.clone(), None, "drag"));
        assert!(app.windows[&wid].conn_card().is_none());
        assert!(app.open_confirm_card(wid, a.clone(), b.clone(), None, "drag"));
        let card = app.windows[&wid].conn_card().expect("card open");
        assert_eq!(card.src, a);
        assert_eq!(card.dst, b);
        assert_eq!(
            app.windows[&wid].overlay().map(|o| o.kind()),
            Some(OverlayKind::ConnCard)
        );
    }

    /// THE §3.3 CATCH, end to end on the engine-neutral seam: with the card
    /// open, Enter is swallowed from the PTY (the overlay gate records the
    /// consumed press) and CONFIRMS — the edges exist afterward; Esc on a
    /// fresh card is swallowed too and mints NOTHING.
    #[test]
    fn enter_confirms_and_esc_never_mints_and_keys_never_reach_the_pty() {
        use aterm_types::keyboard::NamedKey;
        let (mut app, wid, a, b) = app_with_pair();

        // Esc first: cancel leaves no residue anywhere.
        assert!(app.open_confirm_card(wid, a.clone(), b.clone(), None, "menu"));
        app.input(wid, key(NamedKey::Escape), crate::Source::Human);
        assert!(app.windows[&wid].conn_card().is_none(), "Esc closed it");
        assert!(app.connections.records().is_empty(), "Esc minted NOTHING");
        {
            let g = app.store.read().unwrap();
            assert!(g.by_sid(&b).unwrap().ctx.edges.lock().unwrap().is_empty());
        }

        // Enter: the default S→T both mints through the connect seam.
        assert!(app.open_confirm_card(wid, a.clone(), b.clone(), None, "menu"));
        app.input(wid, key(NamedKey::Enter), crate::Source::Human);
        assert!(app.windows[&wid].conn_card().is_none(), "Enter closed it");
        // The overlay gate swallowed the press from the PTY: the consumed-press
        // set holds Enter (its release will be swallowed too).
        assert!(
            app.windows[&wid]
                .overlay_consumed_keys
                .contains(&aterm_types::keyboard::Key::Named(NamedKey::Enter)),
            "the modal gate, not the PTY, consumed Enter"
        );
        let records = app.connections.records();
        let rec = records
            .get(&(a.clone(), b.clone()))
            .expect("confirm recorded the pair");
        assert_eq!(rec.kind(), Some(ConnectionKind::Both));
        let (ops, all_permitted) = {
            let g = app.store.read().unwrap();
            let h = g.by_sid(&b).unwrap();
            let edges = h.ctx.edges.lock().unwrap();
            (
                edges.len(),
                rec.tokens.iter().all(|(op, tok)| {
                    decide_edge(&edges, tok, &b, *op, &h.ctx.nonce).is_permitted()
                }),
            )
        };
        assert_eq!(ops, 3, "both = pull + push rows in T's table");
        assert!(all_permitted);
    }

    /// Confirm applies the EDITED kind (the state-machine contract): cycling
    /// direction/kind then Enter calls the connect seam with exactly that
    /// selection — here T→S push, so rows land in S's (the destination's)
    /// table and none in T's.
    #[test]
    fn confirm_applies_the_edited_direction_and_kind() {
        use aterm_types::keyboard::NamedKey;
        let (mut app, wid, a, b) = app_with_pair();
        assert!(app.open_confirm_card(wid, a.clone(), b.clone(), None, "menu"));
        // direction: SrcToDst -> DstToSrc
        app.input(wid, key(NamedKey::ArrowRight), crate::Source::Human);
        // focus: kind; Both -> wraps to Pull -> Push
        app.input(wid, key(NamedKey::Tab), crate::Source::Human);
        app.input(wid, key(NamedKey::ArrowRight), crate::Source::Human);
        app.input(wid, key(NamedKey::ArrowRight), crate::Source::Human);
        {
            let card = app.windows[&wid].conn_card().unwrap();
            assert_eq!(card.direction, crate::conn_card::CardDirection::DstToSrc);
            assert_eq!(card.kind, ConnectionKind::Push);
        }
        app.input(wid, key(NamedKey::Enter), crate::Source::Human);
        let records = app.connections.records();
        assert!(records.get(&(a.clone(), b.clone())).is_none(), "S→T not minted");
        let rec = records.get(&(b.clone(), a.clone())).expect("T→S minted");
        assert_eq!(rec.kind(), Some(ConnectionKind::Push));
        let g = app.store.read().unwrap();
        assert_eq!(g.by_sid(&a).unwrap().ctx.edges.lock().unwrap().len(), 2);
        assert!(g.by_sid(&b).unwrap().ctx.edges.lock().unwrap().is_empty());
    }

    /// The CONFIGURE path (§2.5): the card prefills from the existing pair,
    /// an edit re-kinds atomically through set semantics, and deselecting the
    /// direction disconnects — the sheet's final state IS the pair's state.
    #[test]
    fn configure_prefills_and_set_semantics_rekinds_and_disconnects() {
        use aterm_types::keyboard::NamedKey;
        let (mut app, wid, a, b) = app_with_pair();
        // Existing connection: a → b, pull.
        {
            let g = app.store.read().unwrap();
            let ctx = g.by_sid(&b).unwrap().ctx.clone();
            drop(g);
            assert!(crate::connections::connect_in(
                &app.connections,
                &a,
                &b,
                &ctx.edges,
                &ctx.nonce,
                ConnectionKind::Pull,
                "test",
            ));
        }
        assert!(app.open_confirm_card(wid, a.clone(), b.clone(), None, "menu"));
        {
            let card = app.windows[&wid].conn_card().unwrap();
            assert_eq!(card.direction, crate::conn_card::CardDirection::SrcToDst);
            assert_eq!(card.kind, ConnectionKind::Pull, "prefilled from the live pair");
        }
        // Re-kind to push and confirm.
        app.input(wid, key(NamedKey::Tab), crate::Source::Human);
        app.input(wid, key(NamedKey::ArrowRight), crate::Source::Human);
        app.input(wid, key(NamedKey::Enter), crate::Source::Human);
        assert_eq!(
            app.connections.records()[&(a.clone(), b.clone())].kind(),
            Some(ConnectionKind::Push)
        );
        {
            let g = app.store.read().unwrap();
            let edges = g.by_sid(&b).unwrap().ctx.edges.lock().unwrap().edges();
            assert!(edges.iter().all(|e| e.op != aterm_session::Op::ReadScreen));
        }

        // Re-open (prefill = push) and deselect the direction: T→S selected
        // instead ⇒ the S→T half is disconnected, T→S minted.
        assert!(app.open_confirm_card(wid, a.clone(), b.clone(), None, "menu"));
        app.input(wid, key(NamedKey::ArrowRight), crate::Source::Human);
        app.input(wid, key(NamedKey::Enter), crate::Source::Human);
        {
            let records = app.connections.records();
            assert!(records.get(&(a.clone(), b.clone())).is_none(), "old half gone");
            assert!(records.get(&(b.clone(), a.clone())).is_some(), "new half live");
        }
        let g = app.store.read().unwrap();
        assert!(
            g.by_sid(&b).unwrap().ctx.edges.lock().unwrap().is_empty(),
            "the deselected S→T half was revoked, not orphaned"
        );
    }

    /// The card is mutually exclusive with the palette/tab-menu (one overlay
    /// slot), and its `controls conn-card` mirror reads the open surface.
    #[test]
    fn card_shares_the_one_overlay_slot_and_mirrors_controls() {
        use crate::app_introspect::AuxTarget;
        let (mut app, wid, a, b) = app_with_pair();
        app.palette_enter();
        assert!(app.windows[&wid].palette().is_some());
        assert!(app.open_confirm_card(wid, a.clone(), b.clone(), None, "menu"));
        assert!(app.windows[&wid].palette().is_none(), "palette closed");
        let lines = app.read_aux_controls(AuxTarget::ConnCard);
        assert!(
            lines[0].contains(&format!("src={}", a.as_str()))
                && lines[0].contains(&format!("dst={}", b.as_str()))
                && lines[0].contains("direction=src-to-dst")
                && lines[0].contains("kind=both"),
            "{lines:?}"
        );
        let front = app.read_aux_controls(AuxTarget::Front);
        assert!(front[0].starts_with("overlay kind=conn-card open=true"), "{front:?}");
        assert_eq!(AuxTarget::parse("conn-card"), Some(AuxTarget::ConnCard));
        app.conn_card_exit(wid);
        assert_eq!(
            app.read_aux_controls(AuxTarget::ConnCard),
            vec!["conn-card open=false".to_string()]
        );
    }
}
