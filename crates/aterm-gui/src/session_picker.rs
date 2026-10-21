// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The PICKER overlay (`controls session-picker`): a palette-style
//! type-to-filter card for File ▸ New Window / New Tab With Identity… (round
//! 18's identity rows): every agent identity by name, plus a last row that
//! creates the identity the FILTER names; choosing opens the window or tab
//! under it, introspectable (`controls`) on every platform.
//!
//! It also picked a session for the connection ids (Connect to Session…,
//! Configure Connection…, Disconnect Session…) until those were retired
//! (`docs/DESIGN-owner-surfaces-2026-10-16.md` §7): every in-session client
//! already holds Owner, and workers start with `spawn agent=claude`.
//!
//! It reuses the palette's fuzzy filter, cursor/scroll/pointer arming and card shape, and lives
//! in the modal [`crate::overlay::Overlay`] slot, so its keys are consumed before the terminal.

#[cfg(test)]
use aterm_render::Theme;

#[cfg(a11y_tree)]
use crate::palette::{a11y_epoch_from, a11y_slot_for, list_overlay_a11y};
use crate::palette::{finish_overlay_card, fuzzy_subsequence, overlay_card_frame, row_text_writer};
use crate::settings::{Roles, SettingsGeom, text_w};
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TrayInput, rgba};

/// The most rows shown at once; beyond this the filtered list scrolls.
const MAX_ROWS: usize = 12;
/// Chrome rows framing the scrolling band: title + query on top, hint footer.
const CHROME_ROWS: usize = 3;
/// Maximum logical width of the floating card (the palette's rule).
const MAX_CARD_WIDTH: f32 = 720.0;

/// What choosing a row DOES — carried from open to activation so the
/// selection dispatches without re-deriving intent from ambient state.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum PickerIntent {
    /// Open a new WINDOW whose first shell runs under the chosen identity.
    NewWindowWithIdentity,
    /// Open a new tab in the front window under the chosen identity.
    NewTabWithIdentity,
}

impl PickerIntent {
    /// The card's title line.
    pub(crate) fn title(self) -> &'static str {
        match self {
            PickerIntent::NewWindowWithIdentity => "New Window With Identity",
            PickerIntent::NewTabWithIdentity => "New Tab With Identity",
        }
    }

    /// The `controls` spelling.
    fn wire(self) -> &'static str {
        match self {
            PickerIntent::NewWindowWithIdentity => "new-window-identity",
            PickerIntent::NewTabWithIdentity => "new-tab-identity",
        }
    }
}

/// One choosable row: an existing agent identity, or the row that creates the
/// identity the filter names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerRow {
    /// An identity on disk, by (folded) name, with how many live sessions wear it.
    Identity { name: String, sessions: usize },
    /// The last row of an identity picker: create the identity the QUERY names.
    /// It never filters out, and activating it with a query that is not a valid,
    /// new name is a no-op.
    NewIdentity,
}

/// What activating a row chooses; the new-identity row yields the name typed into the filter.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PickerChoice {
    /// An identity to spawn under; `create` only for the new-identity row.
    Identity { name: String, create: bool },
}

impl PickerRow {
    /// The stable key `controls` and the a11y epoch identify a row by.
    fn key(&self) -> String {
        match self {
            PickerRow::Identity { name, .. } => format!("identity={name}"),
            PickerRow::NewIdentity => "identity=+new".to_string(),
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct PickerLayout {
    card: (f32, f32, f32, f32),
    card_rows: usize,
    body_rows: usize,
}

/// Transient state of the open picker — the
/// [`crate::overlay::Overlay::SessionPicker`] payload.
pub(crate) struct SessionPickerState {
    /// The hosting window.
    pub(crate) window: crate::WindowId,
    pub(crate) intent: PickerIntent,
    rows: Vec<PickerRow>,
    query: String,
    /// Cursor within the filtered set.
    selected: usize,
    scroll: usize,
    /// Filtered index under the pointer, distinct from `selected`.
    pointer_over: Option<usize>,
    pointer_armed: Option<usize>,
}

impl SessionPickerState {
    pub(crate) fn new(window: crate::WindowId, intent: PickerIntent, rows: Vec<PickerRow>) -> Self {
        Self {
            window,
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
    /// The new-identity row reads the live query, so it is a method of the state.
    fn row_line(&self, row: &PickerRow) -> String {
        match row {
            PickerRow::Identity { name, sessions } => match sessions {
                0 => name.clone(),
                1 => format!("{name}  \u{00b7} 1 session"),
                n => format!("{name}  \u{00b7} {n} sessions"),
            },
            PickerRow::NewIdentity => {
                let query = self.query.trim();
                match crate::agent_identity::parse_name(query) {
                    _ if query.is_empty() => "+ New identity\u{2026} (type its name)".to_string(),
                    Err(why) => format!(
                        "+ New identity: {}",
                        why.strip_prefix("identity: ").unwrap_or(&why)
                    ),
                    Ok(name) if self.new_identity_name().is_none() => {
                        format!("+ New identity: \u{201c}{name}\u{201d} exists, choose it above")
                    }
                    Ok(name) => format!("+ New identity \u{201c}{name}\u{201d}"),
                }
            }
        }
    }

    /// The identity the new-identity row would create: the query, parsed and folded by
    /// [`crate::agent_identity::parse_name`], when not already listed.
    pub(crate) fn new_identity_name(&self) -> Option<String> {
        let name = crate::agent_identity::parse_name(self.query.trim()).ok()?;
        let listed = self
            .rows
            .iter()
            .any(|row| matches!(row, PickerRow::Identity { name: n, .. } if *n == name));
        (!listed).then_some(name)
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

    /// Indices into `rows` passing the palette's fuzzy filter over the row line, in listing order.
    pub(crate) fn filtered(&self) -> Vec<usize> {
        self.filtered_indices().collect()
    }

    fn filtered_indices(&self) -> impl Iterator<Item = usize> + '_ {
        // Trimmed, as the new-identity row reads it, so " work" still finds the `work` row.
        let q = self.query.trim().to_ascii_lowercase();
        (0..self.rows.len()).filter(move |&i| {
            // Opening the picker shows every identity. No display-string formatting
            // is needed to answer an empty filter.
            if q.is_empty() || matches!(self.rows[i], PickerRow::NewIdentity) {
                return true;
            }
            let hay = self.row_line(&self.rows[i]);
            fuzzy_subsequence(&q, hay.chars().map(|c| c.to_ascii_lowercase()))
        })
    }

    fn filtered_count(&self) -> usize {
        if self.query.is_empty() {
            self.rows.len()
        } else {
            self.filtered_indices().count()
        }
    }

    fn body(&self) -> usize {
        self.filtered_count().min(MAX_ROWS)
    }

    /// Move the cursor over the filtered set (wrapping), keeping it on-screen.
    pub(crate) fn move_selection(&mut self, delta: isize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        let n = self.filtered_count();
        if n == 0 {
            return;
        }
        self.selected = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        self.clamp_scroll_for_count(n);
    }

    /// Scroll the band without wrapping (wheel); the cursor keeps its slot.
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let n = self.filtered_count();
        let body = n.min(MAX_ROWS);
        let before = (
            self.selected,
            self.scroll,
            self.pointer_over,
            self.pointer_armed,
        );
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
            self.clamp_scroll_for_count(n);
        }
        before
            != (
                self.selected,
                self.scroll,
                self.pointer_over,
                self.pointer_armed,
            )
    }

    /// Move the cursor to filtered index `idx` (a11y Focus/Click).
    #[cfg(a11y_tree)]
    pub(crate) fn select(&mut self, idx: usize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        let n = self.filtered_count();
        if n == 0 {
            return;
        }
        self.selected = idx.min(n - 1);
        self.clamp_scroll_for_count(n);
    }

    /// Hover a filtered row, dragging the selection with the pointer.
    pub(crate) fn pointer_hover(&mut self, idx: Option<usize>) -> bool {
        let before = (self.selected, self.scroll, self.pointer_over);
        if let Some(idx) = idx {
            let n = self.filtered_count();
            if n > 0 {
                self.selected = idx.min(n - 1);
                self.clamp_scroll_for_count(n);
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

    /// Settle a left release: `(changed, activate)`; activation requires the same filtered
    /// index at press and release.
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

    fn clamp_scroll_for_count(&mut self, n: usize) {
        let body = n.min(MAX_ROWS);
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

    /// What activating `row` chooses, or `None` when it chooses nothing yet
    /// (the new-identity row before a valid, new name is typed).
    pub(crate) fn choice_for(&self, row: &PickerRow) -> Option<PickerChoice> {
        match row {
            PickerRow::Identity { name, .. } => Some(PickerChoice::Identity {
                name: name.clone(),
                create: false,
            }),
            PickerRow::NewIdentity => self
                .new_identity_name()
                .map(|name| PickerChoice::Identity { name, create: true }),
        }
    }

    /// The cursor's row, or `None` when the filter matches nothing.
    pub(crate) fn selected_row(&self) -> Option<&PickerRow> {
        self.row_at_filtered(self.selected)
    }

    /// The row at a filtered index.
    pub(crate) fn row_at_filtered(&self, idx: usize) -> Option<&PickerRow> {
        self.filtered_indices().nth(idx).map(|i| &self.rows[i])
    }

    /// `(scroll, total, visible)` for `controls front`.
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        (self.scroll, self.rows.len(), self.body())
    }

    /// Lines for `controls session-picker`: the rows the card paints.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let vis = self.filtered();
        let mut out = Vec::with_capacity(vis.len() + 1);
        out.push(format!(
            "session-picker window={} intent={} rows={} shown={} selected={} query={:?}",
            self.window.0,
            self.intent.wire(),
            self.rows.len(),
            vis.len(),
            self.selected,
            self.query,
        ));
        for (slot, &i) in vis.iter().enumerate() {
            out.push(format!(
                "session-picker row {} selected={} text={:?}",
                self.rows[i].key(),
                slot == self.selected,
                self.row_line(&self.rows[i]),
            ));
        }
        out
    }

    /// Repaint fingerprint (never `0` while open).
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.window.0.hash(&mut h);
        std::mem::discriminant(&self.intent).hash(&mut h);
        self.query.hash(&mut h);
        self.selected.hash(&mut h);
        self.scroll.hash(&mut h);
        self.rows.len().hash(&mut h);
        for row in &self.rows {
            self.row_line(row).hash(&mut h);
        }
        h.finish() | 1
    }

    /// A11y row-identity epoch over the filtered set (the palette's scheme).
    #[cfg(a11y_tree)]
    fn a11y_epoch(&self) -> u32 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.query.hash(&mut h);
        for i in self.filtered() {
            self.rows[i].key().hash(&mut h);
        }
        a11y_epoch_from(h.finish())
    }

    /// Decode a node minted by the current epoch to its filtered index.
    #[cfg(a11y_tree)]
    pub(crate) fn a11y_filtered_index(&self, node: aterm_a11y::model::NodeId) -> Option<usize> {
        a11y_slot_for(self.a11y_epoch(), node, self.filtered_count())
    }
}

/// The card geometry paint and hit-testing share: centred, width-capped, content height.
fn picker_layout_for_count(count: usize, g: &SettingsGeom) -> PickerLayout {
    let tray_w = (g.cols as f32 * g.cw).max(0.0);
    let tray_h = (g.panel_rows as f32 * g.ch).max(0.0);
    let desired_margin = (g.cw * 2.0).max(16.0);
    let max_margin = (tray_w * 0.5 - g.cw.max(0.0)).max(0.0);
    let margin = desired_margin.min(max_margin);
    let card_w = (tray_w - margin * 2.0).clamp(0.0, MAX_CARD_WIDTH);
    let card_rows = (CHROME_ROWS + count.clamp(1, MAX_ROWS)).min(g.panel_rows);
    let card_h = (card_rows as f32 * g.ch).clamp(0.0, tray_h);
    PickerLayout {
        card: (
            ((tray_w - card_w) * 0.5).max(0.0),
            ((tray_h - card_h) * 0.5).max(0.0),
            card_w,
            card_h,
        ),
        card_rows,
        body_rows: count
            .min(MAX_ROWS)
            .min(card_rows.saturating_sub(CHROME_ROWS)),
    }
}

#[cfg(test)]
fn picker_layout(state: &SessionPickerState, g: &SettingsGeom) -> PickerLayout {
    picker_layout_for_count(state.filtered_count(), g)
}

/// Painted selection/hit rectangle for one visible slot.
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

/// Filtered-list index under a card-local point; only painted rows hit.
pub(crate) fn picker_row_hit(
    state: &SessionPickerState,
    g: &SettingsGeom,
    x: f32,
    y: f32,
) -> Option<usize> {
    let count = state.filtered_count();
    let layout = picker_layout_for_count(count, g);
    for slot in 0..layout.body_rows {
        let (rx, ry, rw, rh) = picker_row_rect_in(&layout, g, slot)?;
        if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
            let filtered = state.scroll + slot;
            return (filtered < count).then_some(filtered);
        }
    }
    None
}

/// Paint the picker: the intent's title, the pinned query row, one row per
/// filtered identity (cursor washed+ringed), a key-hint footer. PURE.
pub(crate) fn picker_tray(
    state: &SessionPickerState,
    g: &SettingsGeom,
    theme: impl Into<crate::chrome_band::ChromePalette>,
) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let vis = state.filtered();
    let layout = picker_layout_for_count(vis.len(), g);
    let (card_x, card_y, card_w, _) = layout.card;
    let mut prims = overlay_card_frame(layout.card, ch, &r, true);
    let text_at = row_text_writer(px, ch);

    // Title row: the intent.
    text_at(
        &mut prims,
        card_x + cw,
        card_y,
        TypeStep::Body,
        state.intent.title().to_string(),
        rgba(r.text_primary, 0xFF),
    );

    // Pinned query row (the palette's framed field).
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

    // The scrolling band.
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
            state.row_line(row),
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
                "no identities yet".to_string()
            } else {
                "no match".to_string()
            },
            rgba(r.text_tertiary, 0xFF),
        );
    }

    // Key-hint footer.
    finish_overlay_card(
        prims,
        "\u{2191}\u{2193} move   \u{23ce} choose   esc close",
        layout.card,
        layout.card_rows,
        g,
        &r,
    )
}

/// The picker's accessibility tree, as the palette's: a list of the filtered rows with
/// epoch-guarded ids, focus following the cursor.
#[cfg(a11y_tree)]
pub(crate) fn picker_a11y(state: &SessionPickerState) -> aterm_a11y::model::TreeUpdate {
    use aterm_a11y::model::{Action, Node, Role};

    let vis = state.filtered();
    let rows = vis.iter().map(|&i| {
        let mut node = Node::new(Role::MenuItem);
        node.set_label(state.row_line(&state.rows[i]));
        node.add_action(Action::Focus);
        node.add_action(Action::Click);
        node
    });
    list_overlay_a11y(
        state.intent.title(),
        state.a11y_epoch(),
        state.selected,
        rows,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rows() -> Vec<PickerRow> {
        vec![
            PickerRow::Identity {
                name: "build".to_string(),
                sessions: 0,
            },
            PickerRow::Identity {
                name: "review".to_string(),
                sessions: 1,
            },
            PickerRow::Identity {
                name: "scratch".to_string(),
                sessions: 2,
            },
        ]
    }

    fn picker() -> SessionPickerState {
        SessionPickerState::new(crate::WindowId(0), PickerIntent::NewTabWithIdentity, rows())
    }

    fn chosen(p: &SessionPickerState, row: &PickerRow) -> String {
        match p.choice_for(row) {
            Some(PickerChoice::Identity { name, .. }) => name,
            None => "-".to_string(),
        }
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

    /// Type-to-filter narrows by name (the palette's fuzzy subsequence over
    /// the painted row line); backspace widens again; the cursor resets to the
    /// top on every edit; nothing matching selects nothing.
    #[test]
    fn filter_narrows_by_name_and_resets_the_cursor() {
        let mut p = picker();
        assert_eq!(p.filtered().len(), 3);
        p.move_selection(2);
        assert_eq!(chosen(&p, p.selected_row().unwrap()), "scratch");
        for c in "revi".chars() {
            p.push_char(c);
        }
        assert_eq!(p.filtered().len(), 1);
        assert_eq!(chosen(&p, p.selected_row().unwrap()), "review");
        for _ in 0..4 {
            p.backspace();
        }
        assert_eq!(p.filtered().len(), 3);
        for c in "zzz".chars() {
            p.push_char(c);
        }
        assert!(p.filtered().is_empty());
        assert!(p.selected_row().is_none());
    }

    /// The cursor wraps over the filtered set (the palette's move/clamp
    /// discipline).
    #[test]
    fn selection_wraps_and_survives_scroll() {
        let mut p = picker();
        p.move_selection(-1);
        assert_eq!(chosen(&p, p.selected_row().unwrap()), "scratch");
        p.move_selection(1);
        assert_eq!(chosen(&p, p.selected_row().unwrap()), "build");
        assert_eq!(p.scroll_extent(), (0, 3, 3));
    }

    /// Activation requires the same filtered row at press and release.
    #[test]
    fn pointer_release_requires_same_armed_row() {
        let mut p = picker();
        assert!(p.pointer_press(Some(0)));
        let (_, activate) = p.pointer_release(Some(1));
        assert!(!activate, "moved off the armed row");
        assert!(p.pointer_press(Some(1)));
        let (_, activate) = p.pointer_release(Some(1));
        assert!(activate);
        assert_eq!(chosen(&p, p.row_at_filtered(1).unwrap()), "review");
    }

    /// Painter and hit-test share one row rectangle.
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
    }

    /// The new-identity row creates only a name `parse_name` takes that is not
    /// already listed (case-folded, a stray leading space ignored).
    #[test]
    fn the_new_identity_row_creates_only_an_unlisted_valid_name() {
        let mut p = SessionPickerState::new(
            crate::WindowId(0),
            PickerIntent::NewTabWithIdentity,
            vec![
                PickerRow::Identity {
                    name: "work".to_string(),
                    sessions: 0,
                },
                PickerRow::NewIdentity,
            ],
        );
        for (query, want) in [
            ("", None),
            ("my work", None),
            ("Work", None),
            (" work", None),
            ("play", Some("play")),
        ] {
            p.query = query.to_string();
            assert_eq!(p.new_identity_name().as_deref(), want, "{query:?}");
        }
        p.query = " work".to_string();
        assert_eq!(p.filtered_indices().collect::<Vec<_>>(), [0, 1]);
    }

    /// `controls session-picker` mirrors the painted rows + live cursor.
    #[test]
    fn controls_lines_mirror_the_surface() {
        let mut p = picker();
        p.move_selection(1);
        let lines = p.controls_lines();
        assert!(lines[0].contains("intent=new-tab-identity"), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("identity=review") && l.contains("selected=true")),
            "{lines:?}"
        );
        // The fingerprint tracks the cursor and is nonzero.
        let before = p.fingerprint();
        assert_ne!(before, 0);
        p.move_selection(1);
        assert_ne!(p.fingerprint(), before);
    }
}
