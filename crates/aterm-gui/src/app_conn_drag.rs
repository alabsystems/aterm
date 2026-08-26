// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! `App` glue for drag-to-connect ([`crate::conn_drag`], design §3.1–§3.3):
//! arm the gesture from the strip funnel's connector press, track the cursor
//! through the §3.1 threshold, resolve the drop target across windows
//! (screen-space frame registry → per-window strip/pane hit-test), push the
//! drop-target highlight through [`crate::tab_bar::TabStripMetadata`] to both
//! strip renderers, draw the wire on the SOURCE window's own paint layer, and
//! settle the release: in place ⇒ the Connections menu, over a session ⇒ the
//! §3.3 confirm card via [`App::open_confirm_card`] (origin `"drag"`, primed
//! S→T both — nothing mints before Confirm), over nothing ⇒ dissolve.
//!
//! The NATIVE macOS strip runs the same machine through the `Wake::ConnDrag*`
//! relay (`toolbar.rs` does its own point-space threshold and posts
//! winit-space screen coordinates); its release-in-place opens the native
//! `NSMenu` in-process and never reaches here.
//!
//! PLATFORM SCOPE (§3.2): cross-window resolution works wherever winit can
//! report window positions (macOS / X11 / Windows). On Wayland
//! `inner_position` errors by protocol, the frame registry stays empty, and
//! the drag is same-window-only — the module-level notes in
//! [`crate::conn_drag`] carry the details. Recorded honest limits: winit has
//! no z-order, so overlapping FOREIGN windows tie-break focused-first then
//! registry order; on mixed-DPI macOS desktops the native strip's screen
//! mapping uses the source window's scale (winit's own convention), so a
//! cross-scale hit can be off by the scale delta.

use std::hash::{Hash, Hasher};

use aterm_session::SessionId;

use crate::App;
use crate::WindowId;
use crate::conn_drag::{
    ConnDragOutcome, ConnDragState, ConnDropTarget, FrameRegistry, WindowFrame,
};

impl App {
    /// The stable [`SessionId`] labeling tab `index` of `window` (its focused
    /// terminal leaf), or `None` for a native tab / an unregistered session.
    pub(crate) fn tab_session_sid(&self, wid: WindowId, index: usize) -> Option<SessionId> {
        let local = self.tab_terminal_session(wid, index)?;
        let g = self.store.read().unwrap_or_else(|p| p.into_inner());
        g.by_local(local).map(|h| h.sid.clone())
    }

    /// ARM the connector gesture on a press in `wid`'s connector column
    /// (§3.1; the strip funnel's `TabHit::Connector` arm calls this). Returns
    /// `false` when the tab has no registered session to drag — the caller
    /// then falls back to the press-committed menu open. Self-drop refusal is
    /// installed here structurally: the armed state's target filter rejects
    /// this session from now on.
    pub(crate) fn conn_drag_arm(&mut self, wid: WindowId, tab: usize) -> bool {
        let Some(session) = self.tab_session_sid(wid, tab) else {
            return false;
        };
        let at = self
            .windows
            .get(&wid)
            .map_or((0.0, 0.0), |ws| ws.last_cursor_px);
        // One mouse, one gesture: a stale armed state means its release was
        // lost — dissolve it before the fresh press arms.
        self.conn_drag_abort();
        self.conn_drag = Some(ConnDragState::arm(wid, tab, session, at));
        true
    }

    /// The §3.2 window-frame registry: every window whose client-area position
    /// the backend can report, in winit screen space (physical px). On Wayland
    /// `inner_position` errors for every window, so the registry stays empty —
    /// which is exactly the same-window-only scope §3.2 assigns that platform.
    pub(crate) fn window_frame_registry(&self) -> FrameRegistry {
        let mut reg = FrameRegistry::default();
        for (wid, ws) in &self.windows {
            let Some(w) = &ws.os_window else { continue };
            let Ok(pos) = w.inner_position() else {
                continue;
            };
            let size = w.inner_size();
            reg.push(WindowFrame {
                window: *wid,
                origin: (f64::from(pos.x), f64::from(pos.y)),
                size: (f64::from(size.width), f64::from(size.height)),
            });
        }
        reg
    }

    /// The overlap tie-break ranking for [`FrameRegistry::locate`]: the
    /// focused window first (during a drag that is normally the source — the
    /// press kept its focus), then the source explicitly. winit exposes no
    /// z-order to do better; recorded in the §3.2 notes.
    fn conn_drag_prefer(&self, src: WindowId) -> Vec<WindowId> {
        let mut prefer: Vec<WindowId> = self
            .windows
            .iter()
            .filter(|(_, ws)| ws.focused)
            .map(|(w, _)| *w)
            .collect();
        if !prefer.contains(&src) {
            prefer.push(src);
        }
        prefer
    }

    /// Track a winit-origin (in-grid) gesture: apply the §3.1 threshold, and
    /// while dragging re-resolve the target under the cursor + repaint the
    /// wire. Swallows the motion stream for the gesture's lifetime (the
    /// `on_cursor_moved` intercept routes here exclusively).
    pub(crate) fn conn_drag_motion(&mut self, wid: WindowId, x: f64, y: f64) {
        let Some(mut state) = self.conn_drag.take() else {
            return;
        };
        if state.native || state.src_window != wid {
            self.conn_drag = Some(state);
            return;
        }
        let began = state.track((x, y));
        if began {
            // Snapshot the frame registry ONCE per gesture (windows do not
            // move mid-drag: the moving hand is on the connector) and switch
            // the source cursor to the grab shape for the drag's duration.
            state.frames = self.window_frame_registry();
            if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
                w.set_cursor(winit::window::CursorIcon::Grabbing);
            }
        }
        if !state.dragging {
            self.conn_drag = Some(state);
            return;
        }
        let target = self.resolve_conn_target(&state);
        self.conn_drag_install_over(state, target);
        // The wire follows the cursor on the source window's own layer.
        self.conn_request_redraw(wid);
    }

    /// Store the resolved target on the state (the machine's `set_over`
    /// refuses self-connection) and, on an actual change, push the highlight
    /// to the windows that gained/lost it — `refresh_window_tabs` re-stamps
    /// the metadata both strip renderers consume.
    fn conn_drag_install_over(&mut self, mut state: ConnDragState, target: Option<ConnDropTarget>) {
        let before = state.over.clone();
        let changed = state.set_over(target);
        let after = state.over.clone();
        self.conn_drag = Some(state);
        if !changed {
            return;
        }
        let mut touched: Vec<WindowId> = Vec::new();
        for t in [before, after].into_iter().flatten() {
            if !touched.contains(&t.window) {
                touched.push(t.window);
            }
        }
        for w in touched {
            let _ = self.refresh_window_tabs(w);
            self.conn_request_redraw(w);
        }
    }

    /// Resolve the session under the drag cursor (§3.3: T is the session of
    /// the pane or tab chip under the point). Same-window resolution is
    /// direct; beyond the source window the §3.2 screen mapping + registry
    /// pick the window first. `None` = over nothing connectable.
    fn resolve_conn_target(&self, state: &ConnDragState) -> Option<ConnDropTarget> {
        let (x, y) = state.cursor;
        let prefer = self.conn_drag_prefer(state.src_window);
        if state.native {
            // Native drags track in screen space from the start.
            let (w, local) = state.frames.locate((x, y), &prefer)?;
            return self.conn_target_in(w, local);
        }
        // Inside the source window (or a headless/test window with no
        // reported extent — such a window has no "outside"): resolve locally.
        let inside = self
            .windows
            .get(&state.src_window)
            .is_some_and(|ws| match ws.win_px {
                Some(s) => {
                    x >= 0.0 && y >= 0.0 && x < f64::from(s.width) && y < f64::from(s.height)
                }
                None => true,
            });
        if inside {
            return self.conn_target_in(state.src_window, (x, y));
        }
        // Cross-window: local → screen → foreign window → foreign-local. On
        // Wayland `to_screen` is `None` (empty registry) and the drag stays
        // same-window-scoped, per §3.2.
        let screen = state.frames.to_screen(state.src_window, (x, y))?;
        let (w, local) = state.frames.locate(screen, &prefer)?;
        self.conn_target_in(w, local)
    }

    /// Hit-test window `wid` at ITS local `(x, y)`: a tab chip in the in-grid
    /// strip band wins (any column of the chip — close/connector cells
    /// included — names that tab's session), else the pane under the point
    /// (each pane is a session). Native-strip chips are NOT drop targets in
    /// v1: the native band sits above the client area, outside every frame in
    /// the registry, so a drop there honestly resolves to nothing (recorded
    /// deviation — panes and in-grid chips carry the macOS path).
    fn conn_target_in(&self, wid: WindowId, (x, y): (f64, f64)) -> Option<ConnDropTarget> {
        use crate::tab_bar::TabHit;
        if let Some(col) = self.strip_col_at(wid, x, y) {
            let ws = self.windows.get(&wid)?;
            return match crate::tab_bar::hit_test(&ws.tab_segments, col) {
                Some(TabHit::Select(i) | TabHit::Close(i) | TabHit::Connector(i)) => {
                    let session = self.tab_session_sid(wid, i)?;
                    Some(ConnDropTarget {
                        window: wid,
                        chip: Some(i),
                        session,
                    })
                }
                // `+` / `↻` / bare band: chrome, not a session.
                _ => None,
            };
        }
        let (row, col) = self.pixel_to_cell(wid, x, y);
        let view = self.visible_view_at_cell(wid, row, col)?;
        let local = self
            .view_store
            .get(view)
            .copied()
            .and_then(crate::tab_model::View::terminal_session)?;
        let session = {
            let g = self.store.read().unwrap_or_else(|p| p.into_inner());
            g.by_local(local).map(|h| h.sid.clone())?
        };
        // The highlight rides the ACTIVE tab's chip (a visible pane belongs
        // to it); `session` stays the authority the drop acts on.
        let chip = self
            .windows
            .get(&wid)
            .and_then(|ws| ws.tab_set.active_index());
        Some(ConnDropTarget {
            window: wid,
            chip,
            session,
        })
    }

    /// Settle a released winit-origin gesture (§3.1/§3.3): in place ⇒ the
    /// Connections menu for the pressed tab; past the threshold ⇒ drop into
    /// the confirm card or dissolve. The `on_mouse_input` intercept calls this
    /// on the left release.
    pub(crate) fn conn_drag_release(&mut self) {
        let Some(state) = self.conn_drag.take() else {
            return;
        };
        self.conn_drag_settle(state);
    }

    fn conn_drag_settle(&mut self, state: ConnDragState) {
        let src_window = state.src_window;
        let former = state.over.clone();
        match state.release() {
            ConnDragOutcome::OpenMenu { window, tab } => {
                let _ = self.open_tab_context_menu_at_chip(window, tab);
            }
            ConnDragOutcome::Drop { src, target } => {
                // §3.3: focus T's window first (open_confirm_card does, for
                // origin "drag"), card primed S→T both; nothing mints before
                // Confirm; self-drop was already structurally refused.
                let _ = self.open_confirm_card(target.window, src, target.session, None, "drag");
            }
            ConnDragOutcome::Cancel => {}
        }
        self.conn_drag_clear_visuals(src_window, former);
    }

    /// Dissolve a gesture WITHOUT settling it (lost release, new press over a
    /// stale armed state, native cancel): highlights and wire cleaned, nothing
    /// opened, nothing minted.
    pub(crate) fn conn_drag_abort(&mut self) {
        let Some(state) = self.conn_drag.take() else {
            return;
        };
        let src_window = state.src_window;
        let former = state.over;
        self.conn_drag_clear_visuals(src_window, former);
    }

    /// Tear down the gesture's transient presentation: the pushed drop-target
    /// highlight, the source window's wire card, and the grab cursor.
    fn conn_drag_clear_visuals(&mut self, src_window: WindowId, former: Option<ConnDropTarget>) {
        if let Some(t) = former {
            let _ = self.refresh_window_tabs(t.window);
        }
        if let Some(ws) = self.windows.get_mut(&src_window) {
            ws.conn_wire_card = None;
            if let Some(w) = &ws.os_window {
                w.set_cursor(winit::window::CursorIcon::Default);
            }
        }
        self.request_redraw_all_windows();
    }

    // ---- The native (macOS strip) relay: same machine, wake-driven. ----

    /// `Wake::ConnDragBegin`: the native connector press crossed AppKit-side
    /// threshold. Resolves the STABLE tab id to today's index + session and
    /// starts the drag in screen space.
    pub(crate) fn conn_drag_native_begin(
        &mut self,
        window: WindowId,
        tab: crate::tab_model::TabId,
    ) {
        self.conn_drag_abort();
        let Some(index) = self.tab_index_for_id(window, tab) else {
            return;
        };
        let Some(session) = self.tab_session_sid(window, index) else {
            return;
        };
        let mut state = ConnDragState::native_drag(window, index, session);
        state.frames = self.window_frame_registry();
        self.conn_drag = Some(state);
    }

    /// `Wake::ConnDragTo`: native tracking at winit-screen `(x, y)`.
    pub(crate) fn conn_drag_native_to(&mut self, window: WindowId, x: f64, y: f64) {
        let Some(mut state) = self.conn_drag.take() else {
            return;
        };
        if !state.native || state.src_window != window {
            self.conn_drag = Some(state);
            return;
        }
        let _ = state.track((x, y));
        let target = self.resolve_conn_target(&state);
        self.conn_drag_install_over(state, target);
    }

    /// `Wake::ConnDragDrop`: native release past the threshold — final
    /// position, then settle (drop into the confirm card / dissolve).
    pub(crate) fn conn_drag_native_drop(&mut self, window: WindowId, x: f64, y: f64) {
        self.conn_drag_native_to(window, x, y);
        let Some(state) = self.conn_drag.take() else {
            return;
        };
        if !state.native || state.src_window != window {
            self.conn_drag = Some(state);
            return;
        }
        self.conn_drag_settle(state);
    }

    /// `Wake::ConnDragCancel`: native abort (screen conversion failed).
    pub(crate) fn conn_drag_native_cancel(&mut self, window: WindowId) {
        if self
            .conn_drag
            .as_ref()
            .is_some_and(|d| d.native && d.src_window == window)
        {
            self.conn_drag_abort();
        }
    }

    // ---- Presentation pushes. ----

    /// Stamp the App-pushed drop-target highlight onto a freshly built
    /// metadata snapshot for `wid` (§3.2) — called by BOTH metadata builders
    /// (`tab_strip_metadata` for the native push, `refill_strip_metadata` for
    /// the in-grid fingerprint), so the one flag reaches both renderers and
    /// the strip repaint epoch through the one metadata type.
    pub(crate) fn stamp_conn_drop_target(
        &self,
        wid: WindowId,
        metadata: &mut [crate::tab_bar::TabStripMetadata],
    ) {
        let Some(d) = &self.conn_drag else { return };
        if !d.dragging {
            return;
        }
        let Some(t) = &d.over else { return };
        if t.window != wid {
            return;
        }
        if let Some(item) = t.chip.and_then(|i| metadata.get_mut(i)) {
            item.drop_target = true;
        }
    }

    fn conn_request_redraw(&self, wid: WindowId) {
        if let Some(w) = self.windows.get(&wid).and_then(|ws| ws.os_window.as_ref()) {
            w.request_redraw();
        }
    }

    /// Repaint fingerprint of the drag wire on window `wid`: `0` unless a
    /// winit-origin drag from THIS window is past the threshold with its
    /// cursor over the window (§3.2: the wire renders only on the source
    /// window's own surface, only while the cursor is there). Nonzero values
    /// track the cursor and target, so every motion step re-presents.
    pub(crate) fn conn_wire_fingerprint(&self, wid: WindowId) -> u64 {
        conn_wire_fp_for(
            self.conn_drag.as_ref(),
            wid,
            self.windows.get(&wid).and_then(|ws| ws.win_px),
        )
    }

    /// Rasterize the drag WIRE (§3.2) into this window's paint-only
    /// `conn_wire_card`: an accent line from the pressed tab's connector cell
    /// to the cursor, with a plug dot at each end. Composited with priority
    /// over the level-up/notice/badge cards, under a modal `settings_card`
    /// (which cannot be open mid-drag). No-op ⇒ `conn_wire_card = None`
    /// whenever [`Self::conn_wire_fingerprint`] is `0` — no drag here, armed
    /// only, native origin, or the cursor has left the source window (beyond
    /// it the pushed highlights + the OS cursor carry the signal). Mirrors
    /// `splice_notice`'s rasterize/composite shape.
    pub(crate) fn splice_conn_wire(&mut self, wid: WindowId) {
        let fp = self.conn_wire_fingerprint(wid);
        if fp == 0 {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.conn_wire_card = None;
            }
            return;
        }
        let Some(d) = &self.conn_drag else { return };
        let (src_tab, (cx_px, cy_px)) = (d.src_tab, d.cursor);
        // The connector anchor: the pressed chip's status cell, mid-chip when
        // the marks vanished under narrowing (the drag stays anchored to its
        // tab either way). No segment ⇒ the strip is not on this frame — no
        // honest anchor, no wire.
        let Some(anchor_col) = self.windows.get(&wid).and_then(|ws| {
            ws.tab_segments.iter().find_map(|seg| {
                matches!(seg.kind, crate::tab_bar::TabHit::Select(i) if i == src_tab).then(|| {
                    seg.connector_col
                        .unwrap_or((seg.start_col + seg.end_col) / 2)
                })
            })
        }) else {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.conn_wire_card = None;
            }
            return;
        };
        let (cw, ch) = self.win_cell_size(wid);
        let pad = self.win_pad(wid);
        let pad_top = self.win_pad_top(wid);
        let head = self.win_head(wid);
        let strip = usize::from(self.tab_strip_rows);
        let (cols, rows) = self
            .windows
            .get(&wid)
            .map_or((0usize, 0usize), |ws| (ws.cols as usize, ws.rows as usize));
        if cols == 0 || strip == 0 {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.conn_wire_card = None;
            }
            return;
        }
        // Tray space: origin at the frame's grid top-left (x at `pad`, y at
        // `pad_top + head`) — the same space the strip splice and the notice
        // card place themselves in.
        let (fx, fy) = self.window_to_frame(wid, cx_px, cy_px);
        let cursor = (
            (fx - pad as f64) as f32,
            (fy - (pad_top + head) as f64) as f32,
        );
        let anchor = (
            (f64::from(anchor_col) + 0.5) as f32 * cw as f32,
            (strip as f32 - 0.4) * ch as f32,
        );
        let accent = crate::settings::u32_rgb(self.theme.cursor);
        let color: crate::widget::Rgba = [accent[0], accent[1], accent[2], 235];
        let mut prims = vec![
            crate::widget::DrawPrim::Line {
                x1: anchor.0,
                y1: anchor.1,
                x2: cursor.0,
                y2: cursor.1,
                width: 2.0,
                color,
            },
            // The socket end (small) and the plug under the cursor (larger).
            crate::widget::DrawPrim::Dot {
                cx: anchor.0,
                cy: anchor.1,
                r: 2.5,
                color,
                breathe: false,
            },
            crate::widget::DrawPrim::Dot {
                cx: cursor.0,
                cy: cursor.1,
                r: 3.5,
                color,
                breathe: false,
            },
        ];
        // Crop the raster to the wire's bounds (plus the dot/AA margin), like
        // the notice card — never a full-frame, mostly-transparent buffer.
        const PAINT_MARGIN: f32 = 5.0;
        let tray_w = (cols * cw) as f32;
        let tray_h = ((rows + strip) * ch) as f32;
        let x0 = (anchor.0.min(cursor.0) - PAINT_MARGIN).max(0.0).floor();
        let y0 = (anchor.1.min(cursor.1) - PAINT_MARGIN).max(0.0).floor();
        let x1 = (anchor.0.max(cursor.0) + PAINT_MARGIN).min(tray_w).ceil();
        let y1 = (anchor.1.max(cursor.1) + PAINT_MARGIN).min(tray_h).ceil();
        if x1 <= x0 || y1 <= y0 {
            if let Some(ws) = self.windows.get_mut(&wid) {
                ws.conn_wire_card = None;
            }
            return;
        }
        crate::widget::translate_prims(&mut prims, -x0, -y0);
        let (rgba, pw, ph) = crate::tray_raster::rasterize_tray(
            &prims,
            (x1 - x0) as u32,
            (y1 - y0) as u32,
            1.0,
            [0, 0, 0, 0],
        );
        let geom = {
            let mut h = std::collections::hash_map::DefaultHasher::new();
            cw.hash(&mut h);
            ch.hash(&mut h);
            pad.hash(&mut h);
            pad_top.hash(&mut h);
            head.hash(&mut h);
            cols.hash(&mut h);
            h.finish()
        };
        if let Some(ws) = self.windows.get_mut(&wid) {
            ws.conn_wire_card = Some(crate::SettingsCard {
                rgba,
                pw,
                ph,
                dx: pad as u32 + x0 as u32,
                dy: (pad_top + head) as u32 + y0 as u32,
                fp,
                geom,
            });
        }
    }
}

/// The field-level body of [`App::conn_wire_fingerprint`], borrow-splittable:
/// the RepaintKey builders hold `&mut` on the window entry while `conn_drag`
/// is a disjoint `App` field, so they feed both in directly.
pub(crate) fn conn_wire_fp_for(
    drag: Option<&ConnDragState>,
    wid: WindowId,
    win_px: Option<winit::dpi::PhysicalSize<u32>>,
) -> u64 {
    let Some(d) = drag else { return 0 };
    if d.native || !d.dragging || d.src_window != wid {
        return 0;
    }
    let (x, y) = d.cursor;
    // A window with no reported extent (headless) has no "outside".
    let inside = match win_px {
        Some(s) => x >= 0.0 && y >= 0.0 && x < f64::from(s.width) && y < f64::from(s.height),
        None => true,
    };
    if !inside {
        return 0;
    }
    let mut h = std::collections::hash_map::DefaultHasher::new();
    d.src_tab.hash(&mut h);
    (x.round() as i64).hash(&mut h);
    (y.round() as i64).hash(&mut h);
    if let Some(t) = &d.over {
        t.window.hash(&mut h);
        t.chip.hash(&mut h);
    }
    h.finish() | 1
}

#[cfg(test)]
mod tests {
    use crate::App;
    use crate::WindowId;
    use crate::conn_drag::CONN_DRAG_THRESHOLD_PX;

    /// Two registered stub sessions as two tabs of one window, strip spliced
    /// so the chip segments (and connector columns) exist.
    fn app_with_pair() -> (App, WindowId) {
        let mut app = App::headless_for_test();
        let wid = WindowId(0);
        app.tab_strip_rows = 1;
        app.push_stub_tab(wid, crate::stub_session(app.next_session_id));
        app.splice_tab_strip(wid);
        (app, wid)
    }

    /// The strip pixel position of tab `index`'s chip centre (row 0 of the
    /// one-row strip), from the SAME laid-out segments the hit-test reads.
    fn chip_px(app: &App, wid: WindowId, index: usize) -> (f64, f64) {
        let seg = app.windows[&wid]
            .tab_segments
            .iter()
            .find(|seg| seg.kind == crate::tab_bar::TabHit::Select(index))
            .expect("tab has a segment");
        let (cw, ch) = app.win_cell_size(wid);
        let pad = app.win_pad(wid) as f64;
        let head = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        let col = f64::from((seg.start_col + seg.end_col) / 2);
        (pad + (col + 0.5) * cw as f64, head + ch as f64 * 0.5)
    }

    /// §3.1 press-release in place: the connector press ARMS (no menu yet),
    /// and the release within the threshold opens the Connections menu for
    /// the pressed tab — the funnel's pre-drag behavior, now on the release.
    #[test]
    fn connector_press_arms_and_release_in_place_opens_the_menu() {
        let (mut app, wid) = app_with_pair();
        assert!(app.conn_drag_arm(wid, 0), "tab 0 has a session to drag");
        assert!(app.conn_drag.is_some(), "armed");
        assert!(
            app.windows[&wid].tab_menu.is_none(),
            "the press alone opens NOTHING"
        );
        // A wobble under the threshold is not a drag...
        let (ox, oy) = app.conn_drag.as_ref().unwrap().origin;
        app.conn_drag_motion(wid, ox + 2.0, oy + 1.0);
        assert!(!app.conn_drag.as_ref().unwrap().dragging);
        // ...so the release opens the menu.
        app.conn_drag_release();
        assert!(app.conn_drag.is_none(), "gesture consumed");
        assert!(
            app.windows[&wid].tab_menu.is_some(),
            "release-in-place opened the menu"
        );
        // …for the PRESSED chip, and the modal overlay slot is untouched (the
        // in-grid tab menu is its own window-state surface, not an overlay).
        assert_eq!(
            app.windows[&wid].tab_menu.as_ref().map(|m| m.index),
            Some(0)
        );
        assert!(app.windows[&wid].overlay().is_none());
        // §3.1: THE CONNECTOR PRESS NEVER SELECTS THE TAB. The right-press
        // opening switches first (a context menu's subject must be the front
        // tab); the connector must not, or the gesture that is about to drag
        // FROM this chip would steal the grid on the way. Tab 1 was active
        // before the press and is still active with tab 0's menu up.
        assert_eq!(
            app.windows[&wid].tab_set.active_index(),
            Some(1),
            "the connector opening left the front tab alone"
        );
    }

    /// The full §3.1→§3.3 happy path, same-window: press tab 0's connector,
    /// drag past the threshold onto tab 1's chip (the target highlight is
    /// pushed through the strip metadata), release — the confirm card opens
    /// primed S→T with origin "drag", and NOTHING has minted.
    #[test]
    fn drag_to_a_chip_highlights_it_and_drops_into_the_confirm_card() {
        let (mut app, wid) = app_with_pair();
        let src = app.tab_session_sid(wid, 0).expect("tab 0 session");
        let dst = app.tab_session_sid(wid, 1).expect("tab 1 session");
        assert!(app.conn_drag_arm(wid, 0));
        let (tx, ty) = chip_px(&app, wid, 1);
        // Cross the threshold toward the target, then land on it.
        let (ox, oy) = app.conn_drag.as_ref().unwrap().origin;
        app.conn_drag_motion(wid, ox + CONN_DRAG_THRESHOLD_PX * 2.0, oy);
        assert!(
            app.conn_drag.as_ref().unwrap().dragging,
            "past the threshold"
        );
        app.conn_drag_motion(wid, tx, ty);
        let over = app
            .conn_drag
            .as_ref()
            .unwrap()
            .over
            .clone()
            .expect("target resolved");
        assert_eq!(over.session, dst);
        assert_eq!(over.chip, Some(1));
        // The highlight rides the metadata snapshot both renderers consume.
        let metadata = app.tab_strip_metadata(wid);
        assert!(metadata[1].drop_target, "target chip flagged");
        assert!(!metadata[0].drop_target, "source chip not flagged");
        // The wire renders on the source window's own layer while dragging.
        assert_ne!(app.conn_wire_fingerprint(wid), 0, "wire fp live");
        app.splice_conn_wire(wid);
        assert!(
            app.windows[&wid].conn_wire_card.is_some(),
            "wire card built"
        );

        app.conn_drag_release();
        assert!(app.conn_drag.is_none());
        let card = app.windows[&wid].conn_card().expect("confirm card open");
        assert_eq!(card.src, src);
        assert_eq!(card.dst, dst);
        assert_eq!(card.origin, "drag");
        assert!(
            app.connections.records().is_empty(),
            "NOTHING mints before Confirm (§3.3)"
        );
        // Presentation cleaned: no lingering highlight or wire.
        assert!(app.tab_strip_metadata(wid).iter().all(|m| !m.drop_target));
        assert!(app.windows[&wid].conn_wire_card.is_none());
        assert_eq!(app.conn_wire_fingerprint(wid), 0);
    }

    /// §3.3 drop-on-PANE: a release in the terminal region (below the strip)
    /// targets the session of the pane under the point — here the active
    /// tab's — and the confirm card opens against IT, chip highlight riding
    /// the active tab.
    #[test]
    fn drop_on_a_pane_targets_the_panes_session() {
        let (mut app, wid) = app_with_pair();
        let src = app.tab_session_sid(wid, 0).expect("tab 0 session");
        let dst = app.tab_session_sid(wid, 1).expect("tab 1 session");
        assert_eq!(
            app.windows[&wid].tab_set.active_index(),
            Some(1),
            "tab 1 (the pushed stub) is active — its pane fills the grid"
        );
        assert!(app.conn_drag_arm(wid, 0));
        // Well below the one-row strip: the middle of the terminal grid.
        let (_, ch) = app.win_cell_size(wid);
        let head = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
        let py = head + ch as f64 * 5.5;
        app.conn_drag_motion(wid, 200.0, py);
        let over = app
            .conn_drag
            .as_ref()
            .unwrap()
            .over
            .clone()
            .expect("the pane resolved");
        assert_eq!(over.session, dst, "T is the pane's session");
        assert_eq!(over.chip, Some(1), "highlight rides the active tab's chip");
        app.conn_drag_release();
        let card = app.windows[&wid].conn_card().expect("confirm card open");
        assert_eq!(card.src, src);
        assert_eq!(card.dst, dst);
        assert!(app.connections.records().is_empty(), "nothing minted");
    }

    /// Self-drop refused (§3.3): a drag released back over its own chip
    /// resolves NO target — the state machine's arm-time filter — so the
    /// release cancels: no card, no menu, no records.
    #[test]
    fn drag_back_onto_the_source_chip_cancels() {
        let (mut app, wid) = app_with_pair();
        assert!(app.conn_drag_arm(wid, 0));
        let (ox, oy) = app.conn_drag.as_ref().unwrap().origin;
        app.conn_drag_motion(wid, ox + CONN_DRAG_THRESHOLD_PX * 2.0, oy + 40.0);
        assert!(app.conn_drag.as_ref().unwrap().dragging);
        let (sx, sy) = chip_px(&app, wid, 0);
        app.conn_drag_motion(wid, sx, sy);
        assert!(
            app.conn_drag.as_ref().unwrap().over.is_none(),
            "self-target structurally refused"
        );
        assert!(app.tab_strip_metadata(wid).iter().all(|m| !m.drop_target));
        app.conn_drag_release();
        assert!(app.windows[&wid].conn_card().is_none(), "no card");
        assert!(app.windows[&wid].tab_menu.is_none(), "no menu either");
        assert!(app.connections.records().is_empty());
    }

    /// A drag released over bare strip chrome (the `+` — a window verb, never
    /// a session, §1.2) dissolves — and an abort (lost release) leaves no
    /// residue.
    #[test]
    fn drag_over_chrome_cancels_and_abort_leaves_no_residue() {
        let (mut app, wid) = app_with_pair();
        assert!(app.conn_drag_arm(wid, 0));
        // Aim at the `+` affordance's centre cell in the strip band.
        let (px, py) = {
            let col = app.windows[&wid]
                .tab_segments
                .iter()
                .find(|seg| seg.kind == crate::tab_bar::TabHit::NewTab)
                .expect("the strip has a +")
                .start_col
                + 1;
            let (cw, ch) = app.win_cell_size(wid);
            let pad = app.win_pad(wid) as f64;
            let head = (app.win_pad_top(wid) + app.win_head(wid)) as f64;
            (
                pad + (f64::from(col) + 0.5) * cw as f64,
                head + ch as f64 * 0.5,
            )
        };
        app.conn_drag_motion(wid, px, py);
        let state = app.conn_drag.as_ref().unwrap();
        assert!(state.dragging, "the reach crossed the threshold");
        assert!(state.over.is_none(), "the + is chrome, not a session");
        app.conn_drag_release();
        assert!(app.windows[&wid].conn_card().is_none());
        assert!(app.windows[&wid].tab_menu.is_none());
        assert!(app.connections.records().is_empty());

        assert!(app.conn_drag_arm(wid, 0));
        app.conn_drag_abort();
        assert!(app.conn_drag.is_none());
        assert!(app.windows[&wid].conn_wire_card.is_none());
        assert!(app.windows[&wid].tab_menu.is_none());
    }

    /// The native relay runs the same machine: begin resolves the stable tab
    /// id, tracking in screen space with an empty frame registry (headless —
    /// the Wayland shape) resolves nothing, and the drop then cancels.
    #[test]
    fn native_relay_begin_track_drop_with_no_frames_cancels() {
        let (mut app, wid) = app_with_pair();
        let tab = app.windows[&wid].tab_set.tabs()[0].id;
        app.conn_drag_native_begin(wid, tab);
        let state = app.conn_drag.as_ref().expect("native drag armed");
        assert!(state.native && state.dragging);
        assert_eq!(state.src_tab, 0);
        // Headless: no OS windows ⇒ empty registry ⇒ nothing resolvable
        // anywhere — the same-window-only scoping shape (§3.2 Wayland).
        app.conn_drag_native_to(wid, 500.0, 500.0);
        assert!(app.conn_drag.as_ref().unwrap().over.is_none());
        assert_eq!(
            app.conn_wire_fingerprint(wid),
            0,
            "native drags draw no in-grid wire"
        );
        app.conn_drag_native_drop(wid, 500.0, 500.0);
        assert!(app.conn_drag.is_none());
        assert!(app.windows[&wid].conn_card().is_none());
        assert!(app.connections.records().is_empty());
    }
}
