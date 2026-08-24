// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The SESSION PICKER overlay (design §2.3/§2.5 — the wave-2 "picker slice"):
//! a palette-style choose-a-session card. Three intents share it:
//!
//! * **Connect** — `Connect to Session…` (`session.connect_to`): every live
//!   session except the subject; choosing one opens the shared confirm card
//!   ([`crate::conn_card`]) for subject ⇄ chosen.
//! * **Configure** — `session.configure_connection` invoked with several
//!   peers: only the subject's CONNECTED peers; choosing opens the same card
//!   pre-filled (§2.5 — the sheet when one peer, this picker when several).
//! * **Disconnect** — `session.disconnect` with several peers: connected
//!   peers; choosing dissolves both directions (never guesses, §2.3).
//!
//! Reuses the palette machinery deliberately: the SAME fuzzy filter
//! ([`crate::palette::fuzzy_subsequence`]), the same cursor/scroll/pointer
//! arming discipline, the same centred opaque card shape — so the two
//! type-to-filter surfaces read and drive identically. Lives in the single
//! modal [`crate::overlay::Overlay`] slot, so its keys are structurally gated
//! BEFORE the terminal.

use aterm_render::Theme;
use aterm_session::SessionId;

use crate::palette::fuzzy_subsequence;
use crate::settings::{Roles, SettingsGeom, fit, text_w};
use crate::tray_raster::row_baseline;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// The most session rows shown at once; beyond this the filtered list scrolls.
const MAX_ROWS: usize = 12;
/// Chrome rows framing the scrolling band: title + query on top, hint footer.
const CHROME_ROWS: usize = 3;
/// Maximum logical width of the floating card (the palette's rule).
const MAX_CARD_WIDTH: f32 = 720.0;

/// What choosing a session DOES — carried from open to activation so the
/// selection dispatches without re-deriving intent from ambient state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PickerIntent {
    /// Open the confirm card for subject ⇄ chosen (fresh or existing pair).
    Connect,
    /// Open the confirm card pre-filled for an EXISTING pair.
    Configure,
    /// Dissolve both directions of subject ⇄ chosen.
    Disconnect,
}

impl PickerIntent {
    /// The card's title line.
    pub(crate) fn title(self) -> &'static str {
        match self {
            PickerIntent::Connect => "Connect to Session",
            PickerIntent::Configure => "Configure Connection",
            PickerIntent::Disconnect => "Disconnect",
        }
    }
}

/// One choosable session, snapshotted at open (registry facts; a session that
/// exits while the picker is open is re-checked at dispatch, never trusted).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct PickerRow {
    pub sid: SessionId,
    /// Registry local id (the tab model's addressing grain) — lets activation
    /// raise/resolve without another sid scan.
    pub local_id: u64,
    /// Display title: user meta title ▸ registry title (the fleet-glance rung).
    pub title: String,
    /// Whether the subject already has any connection with this session —
    /// listed honestly beside the title (`· connected`).
    pub connected: bool,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PickerLayout {
    card: (f32, f32, f32, f32),
    card_rows: usize,
    body_rows: usize,
}

/// Transient state of the open session picker — the
/// [`crate::overlay::Overlay::SessionPicker`] payload.
pub(crate) struct SessionPickerState {
    /// The hosting window.
    pub(crate) window: crate::WindowId,
    /// The SUBJECT session ("S" — `@self` of the §2.3 ids) every intent acts
    /// from.
    pub(crate) subject: SessionId,
    subject_title: String,
    pub(crate) intent: PickerIntent,
    rows: Vec<PickerRow>,
    query: String,
    /// Cursor WITHIN the filtered set.
    selected: usize,
    scroll: usize,
    /// Exact filtered index under the pointer (the palette's distinct-from-
    /// selected rule).
    pointer_over: Option<usize>,
    pointer_armed: Option<usize>,
}

impl SessionPickerState {
    pub(crate) fn new(
        window: crate::WindowId,
        subject: SessionId,
        subject_title: String,
        intent: PickerIntent,
        rows: Vec<PickerRow>,
    ) -> Self {
        Self {
            window,
            subject,
            subject_title,
            intent,
            rows,
            query: String::new(),
            selected: 0,
            scroll: 0,
            pointer_over: None,
            pointer_armed: None,
        }
    }

    /// One row's display line — what the filter matches and the card paints.
    fn row_line(row: &PickerRow) -> String {
        let mut title = crate::session_timeline::sanitize_presentation_line(&row.title, 64);
        if title.is_empty() {
            title = "(untitled)".to_string();
        }
        let connected = if row.connected { " \u{00b7} connected" } else { "" };
        format!("\"{title}\"  @{}{connected}", row.sid.as_str())
    }

    /// Append a filter character, resetting the cursor (the palette's rule).
    pub(crate) fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.selected = 0;
        self.scroll = 0;
        self.pointer_over = None;
        self.pointer_armed = None;
    }

    /// Delete the last filter character.
    pub(crate) fn backspace(&mut self) {
        self.query.pop();
        self.selected = 0;
        self.scroll = 0;
        self.pointer_over = None;
        self.pointer_armed = None;
    }

    /// Indices into `rows` passing the fuzzy filter, in listing order — the
    /// SAME subsequence match the palette uses, over `"title"  @sid`.
    pub(crate) fn filtered(&self) -> Vec<usize> {
        let q = self.query.to_ascii_lowercase();
        (0..self.rows.len())
            .filter(|&i| {
                let hay = Self::row_line(&self.rows[i]);
                fuzzy_subsequence(&q, hay.chars().map(|c| c.to_ascii_lowercase()))
            })
            .collect()
    }

    fn body(&self) -> usize {
        self.filtered().len().min(MAX_ROWS)
    }

    /// Move the cursor over the FILTERED set (wrapping), keeping it on-screen.
    pub(crate) fn move_selection(&mut self, delta: isize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.clamp_scroll();
    }

    /// Scroll the band without wrapping (wheel); the cursor keeps its slot.
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let n = self.filtered().len();
        let body = self.body();
        let before = (self.selected, self.scroll, self.pointer_over, self.pointer_armed);
        self.pointer_over = None;
        self.pointer_armed = None;
        if n == 0 || body == 0 {
            self.selected = 0;
            self.scroll = 0;
        } else {
            let max_scroll = n.saturating_sub(body);
            let relative = self.selected.saturating_sub(self.scroll).min(body - 1);
            self.scroll = self.scroll.saturating_add_signed(delta).min(max_scroll);
            self.selected = (self.scroll + relative).min(n - 1);
            self.clamp_scroll();
        }
        before != (self.selected, self.scroll, self.pointer_over, self.pointer_armed)
    }

    /// Move the cursor to FILTERED index `idx` (a11y Focus/Click land here).
    #[cfg_attr(not(feature = "a11y-accesskit"), allow(dead_code))]
    pub(crate) fn select(&mut self, idx: usize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        let n = self.filtered().len();
        if n == 0 {
            return;
        }
        self.selected = idx.min(n - 1);
        self.clamp_scroll();
    }

    /// Hover a filtered row, dragging the selection with the pointer.
    pub(crate) fn pointer_hover(&mut self, idx: Option<usize>) -> bool {
        let before = (self.selected, self.scroll, self.pointer_over);
        if let Some(idx) = idx {
            let n = self.filtered().len();
            if n > 0 {
                self.selected = idx.min(n - 1);
                self.clamp_scroll();
            }
        }
        self.pointer_over = idx;
        before != (self.selected, self.scroll, self.pointer_over)
    }

    /// Arm the hovered row on left press.
    pub(crate) fn pointer_press(&mut self, idx: Option<usize>) -> bool {
        let mut changed = self.pointer_hover(idx);
        changed |= self.pointer_armed != self.pointer_over;
        self.pointer_armed = self.pointer_over;
        changed
    }

    /// Settle a left release: `(changed, activate)` — activation requires the
    /// SAME filtered index at press and release.
    pub(crate) fn pointer_release(&mut self, idx: Option<usize>) -> (bool, bool) {
        let mut changed = self.pointer_hover(idx);
        let armed = self.pointer_armed.take();
        changed |= armed.is_some();
        let activate = idx.is_some() && armed == idx;
        (changed, activate)
    }

    pub(crate) fn pointer_over_row(&self) -> bool {
        self.pointer_over.is_some()
    }

    fn clamp_scroll(&mut self) {
        let n = self.filtered().len();
        let body = self.body();
        if self.selected >= n {
            self.selected = n.saturating_sub(1);
        }
        if body == 0 {
            self.scroll = 0;
            return;
        }
        if self.selected < self.scroll {
            self.scroll = self.selected;
        } else if self.selected >= self.scroll + body {
            self.scroll = self.selected + 1 - body;
        }
        let max_scroll = n.saturating_sub(body);
        if self.scroll > max_scroll {
            self.scroll = max_scroll;
        }
    }

    /// The cursor's row, or `None` when the filter matches nothing.
    pub(crate) fn selected_row(&self) -> Option<&PickerRow> {
        let vis = self.filtered();
        vis.get(self.selected).map(|&i| &self.rows[i])
    }

    /// The row at an exact FILTERED index (pointer release path).
    pub(crate) fn row_at_filtered(&self, idx: usize) -> Option<&PickerRow> {
        let vis = self.filtered();
        vis.get(idx).map(|&i| &self.rows[i])
    }

    /// The overlay height: chrome + the (capped) filtered band, never `0`.
    pub(crate) fn wanted_rows(&self) -> usize {
        CHROME_ROWS + self.filtered().len().clamp(1, MAX_ROWS)
    }

    /// `(scroll, total, visible)` for `controls front`.
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        (self.scroll, self.rows.len(), self.body())
    }

    /// Machine-readable lines for `controls session-picker` — the SAME rows
    /// the card paints, so screen == introspection.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let vis = self.filtered();
        let intent = match self.intent {
            PickerIntent::Connect => "connect",
            PickerIntent::Configure => "configure",
            PickerIntent::Disconnect => "disconnect",
        };
        let mut out = Vec::with_capacity(vis.len() + 1);
        out.push(format!(
            "session-picker window={} intent={} subject={} rows={} shown={} selected={} query={:?}",
            self.window.0,
            intent,
            self.subject.as_str(),
            self.rows.len(),
            vis.len(),
            self.selected,
            self.query,
        ));
        for (slot, &i) in vis.iter().enumerate() {
            out.push(format!(
                "session-picker row sid={} selected={} text={:?}",
                self.rows[i].sid.as_str(),
                slot == self.selected,
                Self::row_line(&self.rows[i]),
            ));
        }
        out
    }

    /// Repaint fingerprint (never `0` while open).
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.window.0.hash(&mut h);
        self.subject.as_str().hash(&mut h);
        std::mem::discriminant(&self.intent).hash(&mut h);
        self.query.hash(&mut h);
        self.selected.hash(&mut h);
        self.scroll.hash(&mut h);
        self.rows.len().hash(&mut h);
        for row in &self.rows {
            row.sid.as_str().hash(&mut h);
            row.title.hash(&mut h);
            row.connected.hash(&mut h);
        }
        h.finish() | 1
    }

    /// A11y row-identity epoch over the FILTERED set (the palette's scheme).
    #[cfg(feature = "a11y-accesskit")]
    fn a11y_epoch(&self) -> u32 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut h);
        for i in self.filtered() {
            self.rows[i].sid.as_str().hash(&mut h);
        }
        let epoch = h.finish() as u32;
        if epoch == u32::MAX { u32::MAX - 1 } else { epoch }
    }

    /// Decode a node minted by the CURRENT epoch to its filtered index.
    #[cfg(feature = "a11y-accesskit")]
    pub(crate) fn a11y_filtered_index(&self, node: accesskit::NodeId) -> Option<usize> {
        let slot = usize::try_from(node.0 & u64::from(u32::MAX))
            .ok()?
            .checked_sub(1)?;
        (a11y_node_id_for(self.a11y_epoch(), slot) == node && slot < self.filtered().len())
            .then_some(slot)
    }
}

/// One pure geometry projection shared by paint and hit-testing — the
/// palette's centred, width-capped, content-height card.
fn picker_layout(state: &SessionPickerState, g: &SettingsGeom) -> PickerLayout {
    let tray_w = (g.cols as f32 * g.cw).max(0.0);
    let tray_h = (g.panel_rows as f32 * g.ch).max(0.0);
    let desired_margin = (g.cw * 2.0).max(16.0);
    let max_margin = (tray_w * 0.5 - g.cw.max(0.0)).max(0.0);
    let margin = desired_margin.min(max_margin);
    let card_w = (tray_w - margin * 2.0).clamp(0.0, MAX_CARD_WIDTH);
    let card_rows = state.wanted_rows().min(g.panel_rows);
    let card_h = (card_rows as f32 * g.ch).clamp(0.0, tray_h);
    PickerLayout {
        card: (
            ((tray_w - card_w) * 0.5).max(0.0),
            ((tray_h - card_h) * 0.5).max(0.0),
            card_w,
            card_h,
        ),
        card_rows,
        body_rows: state.body().min(card_rows.saturating_sub(CHROME_ROWS)),
    }
}

/// Exact painted selection/hit rectangle for one VISIBLE slot.
fn picker_row_rect_in(
    layout: &PickerLayout,
    g: &SettingsGeom,
    slot: usize,
) -> Option<(f32, f32, f32, f32)> {
    if slot >= layout.body_rows {
        return None;
    }
    let (card_x, card_y, card_w, _) = layout.card;
    Some((
        card_x + g.cw,
        card_y + (2 + slot) as f32 * g.ch + 1.0,
        (card_w - g.cw * 2.0).max(0.0),
        (g.ch - 2.0).max(0.0),
    ))
}

/// Filtered-list index under a card-local point (the palette's rule: only
/// painted rows participate).
pub(crate) fn picker_row_hit(
    state: &SessionPickerState,
    g: &SettingsGeom,
    x: f32,
    y: f32,
) -> Option<usize> {
    let visible = state.filtered();
    let layout = picker_layout(state, g);
    for slot in 0..layout.body_rows {
        let (rx, ry, rw, rh) = picker_row_rect_in(&layout, g, slot)?;
        if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
            let filtered = state.scroll + slot;
            return visible.get(filtered).map(|_| filtered);
        }
    }
    None
}

/// Paint the picker: intent title + subject, the pinned query row, one row per
/// filtered session (cursor washed+ringed), a key-hint footer. PURE.
pub(crate) fn picker_tray(state: &SessionPickerState, g: &SettingsGeom, theme: Theme) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let layout = picker_layout(state, g);
    let (card_x, card_y, card_w, card_h) = layout.card;
    let radius = (ch * 0.6).min(14.0);
    let mut prims: Vec<DrawPrim> = vec![
        DrawPrim::Panel {
            x: card_x - 3.0,
            y: card_y + 2.0,
            w: card_w + 6.0,
            h: card_h + 6.0,
            radius: radius + 3.0,
            fill: rgba([0, 0, 0], 0x2A),
            blur: false,
        },
        DrawPrim::Panel {
            x: card_x - 1.0,
            y: card_y + 2.0,
            w: card_w + 2.0,
            h: card_h + 3.0,
            radius: radius + 1.0,
            fill: rgba([0, 0, 0], 0x30),
            blur: false,
        },
        DrawPrim::Panel {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
            radius,
            fill: rgba(r.surface, 0xFF),
            blur: false,
        },
        DrawPrim::Stroke {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
            radius,
            width: 1.0,
            color: rgba(r.separator, 0xE0),
        },
        DrawPrim::ClipPush {
            x: card_x,
            y: card_y,
            w: card_w,
            h: card_h,
        },
    ];

    let text_at = |prims: &mut Vec<DrawPrim>, x: f32, y0: f32, step: TypeStep, s: String, color| {
        if s.is_empty() {
            return;
        }
        let size = step.px(px);
        prims.push(text_prim(
            x,
            row_baseline(y0, ch, size.get()),
            s,
            size,
            TextWeight::Regular,
            TextFace::Mono,
            color,
        ));
    };

    // Title row: the intent + the subject it acts from.
    {
        let subject_title =
            crate::session_timeline::sanitize_presentation_line(&state.subject_title, 48);
        let title = format!(
            "{} \u{2014} @{} \"{}\"",
            state.intent.title(),
            state.subject.as_str(),
            if subject_title.is_empty() {
                "(untitled)"
            } else {
                subject_title.as_str()
            },
        );
        text_at(
            &mut prims,
            card_x + cw,
            card_y,
            TypeStep::Body,
            title,
            rgba(r.text_primary, 0xFF),
        );
    }

    // Pinned query row (the palette's framed field).
    let vis = state.filtered();
    let qy = card_y + ch;
    prims.push(DrawPrim::Stroke {
        x: card_x + cw * 1.5,
        y: qy + 2.0,
        w: (card_w - cw * 3.0).max(0.0),
        h: ch - 4.0,
        radius: ch * 0.28,
        width: 1.25,
        color: rgba(r.separator, 0xCC),
    });
    let prompt = if state.query.is_empty() {
        "\u{203a} type to filter".to_string()
    } else {
        format!("\u{203a} {}", state.query)
    };
    let q_color = if state.query.is_empty() {
        r.text_tertiary
    } else {
        r.text_primary
    };
    text_at(
        &mut prims,
        card_x + cw * 2.25,
        qy,
        TypeStep::Body,
        prompt,
        rgba(q_color, 0xFF),
    );
    let count = format!("{}/{}", vis.len(), state.rows.len());
    text_at(
        &mut prims,
        card_x + card_w - cw * 2.25 - text_w(&count, TypeStep::Caption.px(px).get()),
        qy,
        TypeStep::Caption,
        count,
        rgba(r.text_tertiary, 0xFF),
    );

    // The scrolling session band.
    for slot in 0..layout.body_rows {
        let Some(&idx) = vis.get(state.scroll + slot) else {
            break;
        };
        let row = &state.rows[idx];
        let y0 = card_y + (2 + slot) as f32 * ch;
        if state.scroll + slot == state.selected {
            let (x, y, w, h) = picker_row_rect_in(&layout, g, slot)
                .expect("painted picker slot has a row rectangle");
            prims.push(DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius: ch * 0.3,
                fill: rgba(r.accent, 0x22),
                blur: false,
            });
            prims.push(DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius: ch * 0.3,
                width: 1.5,
                color: rgba(r.accent, 0xCC),
            });
        }
        text_at(
            &mut prims,
            card_x + cw * 2.0,
            y0,
            TypeStep::Body,
            SessionPickerState::row_line(row),
            rgba(r.text_primary, 0xFF),
        );
    }
    if vis.is_empty() {
        text_at(
            &mut prims,
            card_x + cw * 2.0,
            card_y + 2.0 * ch,
            TypeStep::Body,
            if state.rows.is_empty() {
                "no other sessions".to_string()
            } else {
                "no match".to_string()
            },
            rgba(r.text_tertiary, 0xFF),
        );
    }

    // Key-hint footer.
    let hint = "\u{2191}\u{2193} move   \u{23ce} choose   type to filter   esc close";
    let fsize = TypeStep::Caption.px(px);
    let hint_w = text_w(hint, fsize.get());
    let fx = card_x + fit((card_w - hint_w) * 0.5, cw, card_w - hint_w - cw);
    text_at(
        &mut prims,
        fx,
        card_y + layout.card_rows.saturating_sub(1) as f32 * ch,
        TypeStep::Caption,
        hint.to_string(),
        rgba(r.text_tertiary, 0xFF),
    );
    prims.push(DrawPrim::ClipPop);

    TrayInput {
        prims,
        card: layout.card,
    }
}

/// The picker's accessibility tree — the palette scheme verbatim: a ListBox of
/// the FILTERED rows, epoch-guarded ids, focus follows the cursor.
#[cfg(feature = "a11y-accesskit")]
pub(crate) fn picker_a11y(state: &SessionPickerState) -> accesskit::TreeUpdate {
    use accesskit::{Action, Node, NodeId, Role, Tree, TreeId, TreeUpdate};

    const LIST: NodeId = NodeId(u64::MAX);
    let root_id = NodeId(0);
    let vis = state.filtered();
    let epoch = state.a11y_epoch();

    let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(vis.len() + 2);
    let mut items: Vec<NodeId> = Vec::with_capacity(vis.len());
    for (slot, &i) in vis.iter().enumerate() {
        let id = a11y_node_id_for(epoch, slot);
        let mut node = Node::new(Role::MenuItem);
        node.set_label(SessionPickerState::row_line(&state.rows[i]));
        node.add_action(Action::Focus);
        node.add_action(Action::Click);
        nodes.push((id, node));
        items.push(id);
    }

    let mut list = Node::new(Role::ListBox);
    list.set_children(items);
    nodes.push((LIST, list));

    let mut root = Node::new(Role::Window);
    root.set_label(state.intent.title());
    root.set_children(vec![LIST]);
    nodes.push((root_id, root));

    let focus = if vis.is_empty() {
        root_id
    } else {
        a11y_node_id_for(epoch, state.selected.min(vis.len() - 1))
    };

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(root_id)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// Mint a row node id (epoch high half, `slot + 1` low — the palette scheme).
#[cfg(feature = "a11y-accesskit")]
fn a11y_node_id_for(epoch: u32, slot: usize) -> accesskit::NodeId {
    accesskit::NodeId((u64::from(epoch) << 32) | (slot as u64 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<PickerRow> {
        vec![
            PickerRow {
                sid: SessionId::new("s-alpha"),
                local_id: 1,
                title: "build worker".to_string(),
                connected: false,
            },
            PickerRow {
                sid: SessionId::new("s-beta"),
                local_id: 2,
                title: "operator".to_string(),
                connected: true,
            },
            PickerRow {
                sid: SessionId::new("s-gamma"),
                local_id: 3,
                title: "scratch".to_string(),
                connected: false,
            },
        ]
    }

    fn picker() -> SessionPickerState {
        SessionPickerState::new(
            crate::WindowId(0),
            SessionId::new("s-self"),
            "me".to_string(),
            PickerIntent::Connect,
            rows(),
        )
    }

    fn geom() -> SettingsGeom {
        SettingsGeom {
            cw: 8.0,
            ch: 16.0,
            font_px: 13.0,
            cols: 160,
            panel_rows: 48,
        }
    }

    /// Type-to-filter narrows by title AND by sid (the palette's fuzzy
    /// subsequence over the painted row line); backspace widens again; the
    /// cursor resets to the top on every edit.
    #[test]
    fn filter_narrows_by_title_and_sid_and_resets_the_cursor() {
        let mut p = picker();
        assert_eq!(p.filtered().len(), 3);
        p.move_selection(2);
        assert_eq!(p.selected_row().unwrap().sid.as_str(), "s-gamma");
        for c in "oper".chars() {
            p.push_char(c);
        }
        assert_eq!(p.filtered().len(), 1);
        assert_eq!(p.selected_row().unwrap().sid.as_str(), "s-beta");
        // Sid text matches too (the row line carries `@<sid>`).
        p.backspace();
        p.backspace();
        p.backspace();
        p.backspace();
        for c in "s-alp".chars() {
            p.push_char(c);
        }
        assert_eq!(p.selected_row().unwrap().sid.as_str(), "s-alpha");
        // Nothing matches: selection is honestly None.
        for c in "zzz".chars() {
            p.push_char(c);
        }
        assert!(p.filtered().is_empty());
        assert!(p.selected_row().is_none());
    }

    /// The cursor wraps over the filtered set and selection follows filtering
    /// (the palette's move/clamp discipline).
    #[test]
    fn selection_wraps_and_survives_scroll() {
        let mut p = picker();
        p.move_selection(-1);
        assert_eq!(p.selected_row().unwrap().sid.as_str(), "s-gamma");
        p.move_selection(1);
        assert_eq!(p.selected_row().unwrap().sid.as_str(), "s-alpha");
        assert_eq!(p.scroll_extent(), (0, 3, 3));
    }

    /// Pointer press/release arming: activation requires the SAME filtered row
    /// at press and release (the palette's rule).
    #[test]
    fn pointer_release_requires_same_armed_row() {
        let mut p = picker();
        assert!(p.pointer_press(Some(0)));
        let (_, activate) = p.pointer_release(Some(1));
        assert!(!activate, "moved off the armed row");
        assert!(p.pointer_press(Some(1)));
        let (_, activate) = p.pointer_release(Some(1));
        assert!(activate);
        assert_eq!(p.row_at_filtered(1).unwrap().sid.as_str(), "s-beta");
    }

    /// Painter and hit-test share one row rectangle; the connected annotation
    /// is painted so the choice is honest.
    #[test]
    fn hit_test_and_paint_share_rectangles() {
        let p = picker();
        let g = geom();
        let layout = picker_layout(&p, &g);
        let (x, y, w, h) = picker_row_rect_in(&layout, &g, 1).unwrap();
        assert_eq!(picker_row_hit(&p, &g, x + w * 0.5, y + h * 0.5), Some(1));
        assert_eq!(picker_row_hit(&p, &g, x - 2.0 * g.cw, y), None);
        let tray = picker_tray(&p, &g, Theme::default());
        assert_eq!(tray.card, layout.card);
        assert!(
            tray.prims.iter().any(
                |pr| matches!(pr, DrawPrim::Text { s, .. } if s.contains("connected") && s.contains("s-beta"))
            ),
            "the connected annotation is painted"
        );
        assert!(
            tray.prims
                .iter()
                .any(|pr| matches!(pr, DrawPrim::Text { s, .. } if s.contains("Connect to Session"))),
            "the intent titles the card"
        );
    }

    /// `controls session-picker` mirrors the painted rows + live cursor.
    #[test]
    fn controls_lines_mirror_the_surface() {
        let mut p = picker();
        p.move_selection(1);
        let lines = p.controls_lines();
        assert!(lines[0].contains("intent=connect") && lines[0].contains("subject=s-self"));
        assert!(
            lines
                .iter()
                .any(|l| l.contains("sid=s-beta") && l.contains("selected=true")),
            "{lines:?}"
        );
        // Fingerprint tracks the cursor (repaint-key law), nonzero.
        let before = p.fingerprint();
        assert_ne!(before, 0);
        p.move_selection(1);
        assert_ne!(p.fingerprint(), before);
    }
}
