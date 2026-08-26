// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The anchored CONNECTION CONFIRM/CONFIGURE card (design §3.3 + §2.5): ONE
//! shared component serving both the drop popover (the §3.3 confirm, origin
//! `drag`) and the Configure… sheet (the §2.5 editor, origin `menu`) — the two
//! surfaces are the same card with a different prefill, so wire callers, the
//! sheet, and the drop produce identical transitions through the one
//! declarative [`crate::connections::connect_in`] seam.
//!
//! A sibling of [`crate::tab_menu`] on the [`crate::palette`] pattern: the
//! state lives in the single modal [`crate::overlay::Overlay`] slot, so its
//! key handling structurally sits BEFORE the terminal (`on_key_overlay_mode`
//! runs ahead of every keybinding/`[key_sequences]`/PTY path, and the
//! engine-neutral `App::input` gate swallows controller bytes the same way) —
//! Enter/Esc can NEVER leak into T's PTY, the §3.3 panel's sharpest catch.
//!
//! The card names S and T by sid+title, offers a DIRECTION control
//! (S→T / T→S / both) and a KIND control (pull / push / both), and mints
//! NOTHING before Confirm (Enter); Esc cancels with no residue. Confirming is
//! SET semantics (§2.5): the pair's connections become exactly what the card
//! shows — a direction deselected that existed at open time is disconnected,
//! a selected one is `connect`ed to the edited kind (idempotent when
//! unchanged). Native macOS hosts this same in-grid card in the content view;
//! there is deliberately no separate AppKit sheet.

use aterm_render::Theme;
use aterm_session::{ConnectionKind, SessionId};

use crate::settings::{Roles, SettingsGeom, text_w};
use crate::tray_raster::row_baseline;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// Which flows the card asserts, seen from the pair it was opened over
/// (S = `src`, T = `dst`). The §3.3 DECIDED default is `SrcToDst` ("drag takes
/// control") — but only when nothing exists yet: an existing reverse half
/// promotes the initial selection so confirming the default never silently
/// revokes it (deselection is always an explicit edit).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CardDirection {
    /// S drives T (rows minted into T's table).
    SrcToDst,
    /// T drives S.
    DstToSrc,
    /// Both directions (the `⇆` peer pair — two records).
    Both,
}

/// The direction control's cycle order (Left/Right walk this).
pub(crate) const DIRECTIONS: [CardDirection; 3] = [
    CardDirection::SrcToDst,
    CardDirection::DstToSrc,
    CardDirection::Both,
];

/// The kind control's cycle order.
pub(crate) const KINDS: [ConnectionKind; 3] = [
    ConnectionKind::Pull,
    ConnectionKind::Push,
    ConnectionKind::Both,
];

/// Which control row holds the keyboard focus (Up/Down/Tab toggle; Left/Right
/// cycle the focused control's value).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CardRow {
    Direction,
    Kind,
}

/// A pointer/a11y target on the card — chip values plus the two buttons.
/// Painter and hit-test share one rect table ([`conn_card_hit_rects`]), so the
/// interactive cell can never drift from its visible chip.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum ConnCardHit {
    Direction(CardDirection),
    Kind(ConnectionKind),
    Confirm,
    Cancel,
}

/// What each direction of the pair held AT OPEN time — the configure prefill
/// (derived from the live edge tables, so a wire `grant` with no
/// [`crate::connections::ConnectionRecord`] still prefills honestly) and the
/// baseline [`ConnCardState::plan`] diffs against on Confirm.
#[derive(Clone, Copy, Default, PartialEq, Eq, Debug)]
pub(crate) struct PairKinds {
    /// The S→T half's kind, `None` when no such rows exist.
    pub src_to_dst: Option<ConnectionKind>,
    /// The T→S half's kind.
    pub dst_to_src: Option<ConnectionKind>,
}

/// The exact authority acts one Confirm performs — computed PURE from the card
/// state so the state machine is unit-testable without an `App`; the glue
/// executes it through [`crate::connections::connect_in`] /
/// [`crate::connections::disconnect_kind_in`] and nothing else. Esc never
/// produces a plan, so Esc can never mint.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct ConnCardPlan {
    /// `(src, dst, kind)` declarative connects (set semantics — idempotent on
    /// an unchanged half).
    pub connects: Vec<(SessionId, SessionId, ConnectionKind)>,
    /// `(src, dst)` whole-connection disconnects: directions deselected that
    /// existed at open time.
    pub disconnects: Vec<(SessionId, SessionId)>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ConnCardLayout {
    card: (f32, f32, f32, f32),
}

/// Total visual rows of the card: title, gap, direction row, kind row, gap,
/// buttons.
const CARD_ROWS: usize = 6;
/// Leading/trailing text inset, in cells.
const INSET_CELLS: f32 = 1.5;
/// The label column ("direction" / "kind") width, in cells, before the chips.
const LABEL_CELLS: f32 = 11.0;
/// Horizontal padding inside a chip, in cells (each side).
const CHIP_PAD_CELLS: f32 = 1.0;
/// Gap between chips, in cells.
const CHIP_GAP_CELLS: f32 = 1.0;
/// Minimum card width in cells.
const MIN_CARD_CELLS: f32 = 36.0;

/// Transient state of the open confirm/configure card — the
/// [`crate::overlay::Overlay::ConnCard`] payload. Snapshot semantics: the pair
/// identity and prefill are captured at open; Confirm re-resolves both
/// sessions by sid (a peer that died meanwhile makes its half a logged no-op,
/// never a wrong-target mint).
pub(crate) struct ConnCardState {
    /// The hosting window (T's window for a drop — §3.3 focuses it first).
    pub(crate) window: crate::WindowId,
    /// S — the connection's proposing side (the dragged-from / subject session).
    pub(crate) src: SessionId,
    /// T — the proposed peer (the drop target / picked session).
    pub(crate) dst: SessionId,
    src_title: String,
    dst_title: String,
    /// The audit origin this card confirms with (`menu` | `drag`, §7).
    pub(crate) origin: &'static str,
    /// Strip column the card anchors under (T's chip when resolvable).
    anchor_col: u16,
    /// Tray row of the card's top (the strip band height).
    anchor_row: usize,
    /// What each direction held at open — the plan's diff baseline.
    prefill: PairKinds,
    /// The direction control's current value.
    pub(crate) direction: CardDirection,
    /// The kind control's current value (applies to every selected direction).
    pub(crate) kind: ConnectionKind,
    focus: CardRow,
    pointer_over: Option<ConnCardHit>,
    pointer_armed: Option<ConnCardHit>,
}

impl ConnCardState {
    /// Build the card over pair `src → dst`, prefilled from `prefill`: an
    /// existing half selects itself (so confirming the initial state never
    /// revokes silently); with nothing existing the §3.3 DECIDED default is
    /// S→T `both` ("drag takes control"). When the two halves hold DIFFERENT
    /// kinds the single kind control shows the S→T half's; confirming rewrites
    /// both to what the card shows (visible before Confirm — set semantics).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        window: crate::WindowId,
        src: SessionId,
        src_title: String,
        dst: SessionId,
        dst_title: String,
        prefill: PairKinds,
        origin: &'static str,
        anchor_col: u16,
        anchor_row: usize,
    ) -> Self {
        let (direction, kind) = match (prefill.src_to_dst, prefill.dst_to_src) {
            (None, None) => (CardDirection::SrcToDst, ConnectionKind::Both),
            (Some(k), None) => (CardDirection::SrcToDst, k),
            (None, Some(k)) => (CardDirection::DstToSrc, k),
            (Some(k), Some(_)) => (CardDirection::Both, k),
        };
        Self {
            window,
            src,
            dst,
            src_title,
            dst_title,
            origin,
            anchor_col,
            anchor_row,
            prefill,
            direction,
            kind,
            focus: CardRow::Direction,
            pointer_over: None,
            pointer_armed: None,
        }
    }

    /// Whether the S→T half is selected under the current direction.
    fn src_to_dst_selected(&self) -> bool {
        matches!(self.direction, CardDirection::SrcToDst | CardDirection::Both)
    }

    /// Whether the T→S half is selected.
    fn dst_to_src_selected(&self) -> bool {
        matches!(self.direction, CardDirection::DstToSrc | CardDirection::Both)
    }

    /// The Confirm plan — pure §2.5 SET semantics against the open-time
    /// baseline: selected halves become `kind` (idempotent when unchanged),
    /// deselected halves that existed are disconnected, deselected halves that
    /// never existed produce nothing.
    pub(crate) fn plan(&self) -> ConnCardPlan {
        let mut plan = ConnCardPlan::default();
        let halves = [
            (
                self.src_to_dst_selected(),
                self.prefill.src_to_dst,
                &self.src,
                &self.dst,
            ),
            (
                self.dst_to_src_selected(),
                self.prefill.dst_to_src,
                &self.dst,
                &self.src,
            ),
        ];
        for (selected, had, src, dst) in halves {
            if selected {
                plan.connects.push((src.clone(), dst.clone(), self.kind));
            } else if had.is_some() {
                plan.disconnects.push((src.clone(), dst.clone()));
            }
        }
        plan
    }

    /// Toggle the keyboard focus between the two control rows (Up/Down/Tab).
    pub(crate) fn move_focus(&mut self) {
        self.pointer_over = None;
        self.pointer_armed = None;
        self.focus = match self.focus {
            CardRow::Direction => CardRow::Kind,
            CardRow::Kind => CardRow::Direction,
        };
    }

    /// Focus `row` and cycle its value by `delta` — the a11y Click's cycle
    /// (a screen reader clicks the chooser row it hears; the row advances).
    #[cfg_attr(not(a11y_tree), allow(dead_code))]
    pub(crate) fn cycle_row(&mut self, row: CardRow, delta: isize) {
        self.focus = row;
        self.cycle_value(delta);
    }

    /// Cycle the FOCUSED control's value by `delta` (Left/Right), wrapping.
    pub(crate) fn cycle_value(&mut self, delta: isize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        match self.focus {
            CardRow::Direction => {
                let i = DIRECTIONS
                    .iter()
                    .position(|d| *d == self.direction)
                    .unwrap_or(0);
                let n = DIRECTIONS.len() as isize;
                self.direction =
                    DIRECTIONS[usize::try_from((i as isize + delta).rem_euclid(n)).unwrap_or(0)];
            }
            CardRow::Kind => {
                let i = KINDS.iter().position(|k| *k == self.kind).unwrap_or(0);
                let n = KINDS.len() as isize;
                self.kind =
                    KINDS[usize::try_from((i as isize + delta).rem_euclid(n)).unwrap_or(0)];
            }
        }
    }

    /// Apply one pointer/a11y CHIP activation (`Confirm`/`Cancel` are the
    /// glue's to act on and return `false` here). Returns whether state moved.
    pub(crate) fn activate_hit(&mut self, hit: ConnCardHit) -> bool {
        match hit {
            ConnCardHit::Direction(d) => {
                self.focus = CardRow::Direction;
                let changed = self.direction != d;
                self.direction = d;
                changed
            }
            ConnCardHit::Kind(k) => {
                self.focus = CardRow::Kind;
                let changed = self.kind != k;
                self.kind = k;
                changed
            }
            ConnCardHit::Confirm | ConnCardHit::Cancel => false,
        }
    }

    /// Hover a hit target, moving the row focus with the pointer over chips.
    /// Returns whether anything visible changed.
    pub(crate) fn pointer_hover(&mut self, hit: Option<ConnCardHit>) -> bool {
        let before = (self.pointer_over, self.focus);
        match hit {
            Some(ConnCardHit::Direction(_)) => self.focus = CardRow::Direction,
            Some(ConnCardHit::Kind(_)) => self.focus = CardRow::Kind,
            _ => {}
        }
        self.pointer_over = hit;
        before != (self.pointer_over, self.focus)
    }

    /// Arm the hovered target on left press (the palette's press/release
    /// arming discipline).
    pub(crate) fn pointer_press(&mut self, hit: Option<ConnCardHit>) -> bool {
        let mut changed = self.pointer_hover(hit);
        changed |= self.pointer_armed != self.pointer_over;
        self.pointer_armed = self.pointer_over;
        changed
    }

    /// Settle a left release: activation requires the SAME target at press and
    /// release. Returns `(changed, activated_target)`.
    pub(crate) fn pointer_release(
        &mut self,
        hit: Option<ConnCardHit>,
    ) -> (bool, Option<ConnCardHit>) {
        let mut changed = self.pointer_hover(hit);
        let armed = self.pointer_armed.take();
        changed |= armed.is_some();
        let activated = (armed.is_some() && armed == hit).then_some(hit).flatten();
        (changed, activated)
    }

    pub(crate) fn pointer_over_target(&self) -> bool {
        self.pointer_over.is_some()
    }

    /// The direction control's live arrow, reused by the title line so the
    /// named relationship always mirrors the selection.
    fn arrow(&self) -> &'static str {
        match self.direction {
            CardDirection::SrcToDst => "\u{21e5}",
            CardDirection::DstToSrc => "\u{21e4}",
            CardDirection::Both => "\u{21c6}",
        }
    }

    /// The title line naming S and T by sid+title (§3.3), arrow live with the
    /// direction control. Titles are program-influenced text — sanitized+capped
    /// like every chrome prose line.
    pub(crate) fn title_line(&self) -> String {
        let cap = |t: &str| {
            let s = crate::session_timeline::sanitize_presentation_line(t, 64);
            if s.is_empty() { "(untitled)".to_string() } else { s }
        };
        format!(
            "@{} \"{}\" {} @{} \"{}\"",
            self.src.as_str(),
            cap(&self.src_title),
            self.arrow(),
            self.dst.as_str(),
            cap(&self.dst_title),
        )
    }

    /// The overlay wants the whole viewport; the layout anchors within (the
    /// tab-menu convention).
    pub(crate) fn wanted_rows(&self, avail: usize) -> usize {
        avail
    }

    /// `(scroll, total, visible)` for `controls front` — the card never
    /// scrolls.
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        (0, CARD_ROWS, CARD_ROWS)
    }

    /// Machine-readable lines for `controls conn-card` — the SAME facts the
    /// pixels paint, so screen == introspection.
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let direction = match self.direction {
            CardDirection::SrcToDst => "src-to-dst",
            CardDirection::DstToSrc => "dst-to-src",
            CardDirection::Both => "both",
        };
        let focus = match self.focus {
            CardRow::Direction => "direction",
            CardRow::Kind => "kind",
        };
        vec![
            format!(
                "conn-card window={} src={} dst={} direction={} kind={} origin={} focus={}",
                self.window.0,
                self.src.as_str(),
                self.dst.as_str(),
                direction,
                kind_str(self.kind),
                self.origin,
                focus,
            ),
            format!("conn-card title {:?}", self.title_line()),
        ]
    }

    /// Repaint fingerprint of everything the card paints. Never `0` while open
    /// (`| 1`).
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.window.0.hash(&mut h);
        self.src.as_str().hash(&mut h);
        self.dst.as_str().hash(&mut h);
        self.src_title.hash(&mut h);
        self.dst_title.hash(&mut h);
        self.anchor_col.hash(&mut h);
        self.anchor_row.hash(&mut h);
        std::mem::discriminant(&self.direction).hash(&mut h);
        std::mem::discriminant(&self.kind).hash(&mut h);
        std::mem::discriminant(&self.focus).hash(&mut h);
        format!("{:?}", self.pointer_over).hash(&mut h);
        h.finish() | 1
    }

    /// Decode an a11y node id minted by [`conn_card_a11y`] to its target.
    #[cfg(a11y_tree)]
    pub(crate) fn a11y_hit(node: accesskit::NodeId) -> Option<ConnCardHit> {
        match node.0 {
            1 => Some(ConnCardHit::Direction(CardDirection::SrcToDst)),
            2 => Some(ConnCardHit::Kind(ConnectionKind::Pull)),
            3 => Some(ConnCardHit::Confirm),
            4 => Some(ConnCardHit::Cancel),
            _ => None,
        }
    }
}

/// The card's kind spelling — the wire `kind=` vocabulary (§6). A free
/// function because [`ConnectionKind`] is aterm-session's type (no foreign
/// inherent impl).
pub(crate) fn kind_str(kind: ConnectionKind) -> &'static str {
    match kind {
        ConnectionKind::Pull => "pull",
        ConnectionKind::Push => "push",
        ConnectionKind::Both => "both",
    }
}

/// One pure geometry projection shared by paint and hit-testing (the
/// `palette_layout` discipline): content-sized, anchored under `anchor_col`
/// just below the strip band, clamped fully inside the tray.
fn conn_card_layout(state: &ConnCardState, g: &SettingsGeom) -> ConnCardLayout {
    let tray_w = (g.cols as f32 * g.cw).max(0.0);
    let body_px = TypeStep::Body.px(g.font_px).get();
    let chips_w = |labels: &[&str]| -> f32 {
        LABEL_CELLS * g.cw
            + labels
                .iter()
                .map(|l| text_w(l, body_px) + 2.0 * CHIP_PAD_CELLS * g.cw + CHIP_GAP_CELLS * g.cw)
                .sum::<f32>()
    };
    let content_w = (MIN_CARD_CELLS * g.cw)
        .max(text_w(&state.title_line(), body_px))
        .max(chips_w(&direction_labels()))
        .max(chips_w(&kind_labels()))
        .max(chips_w(&["Confirm \u{23ce}", "Cancel esc"]));
    let card_w = (content_w + 2.0 * INSET_CELLS * g.cw).min((tray_w - 2.0 * g.cw).max(2.0 * g.cw));
    let card_x = (state.anchor_col as f32 * g.cw)
        .min(tray_w - card_w - g.cw)
        .max(g.cw.min(tray_w * 0.5));
    let rows = CARD_ROWS.min(g.panel_rows.max(1));
    let anchor_row = state.anchor_row.min(g.panel_rows.saturating_sub(rows));
    ConnCardLayout {
        card: (
            card_x,
            anchor_row as f32 * g.ch,
            card_w,
            rows as f32 * g.ch,
        ),
    }
}

fn direction_labels() -> [&'static str; 3] {
    ["S \u{2192} T", "T \u{2192} S", "S \u{21c6} T"]
}

fn kind_labels() -> [&'static str; 3] {
    ["pull", "push", "both"]
}

/// The interactive rect table — every chip and button with its exact painted
/// rectangle, derived from the SAME layout the painter uses.
fn conn_card_hit_rects(
    state: &ConnCardState,
    g: &SettingsGeom,
) -> Vec<(ConnCardHit, (f32, f32, f32, f32))> {
    let layout = conn_card_layout(state, g);
    let (card_x, card_y, card_w, _) = layout.card;
    let body_px = TypeStep::Body.px(g.font_px).get();
    let mut out = Vec::with_capacity(8);
    let mut push_row = |row: usize, hits: &[(ConnCardHit, &str)], from_label_col: bool| {
        let y = card_y + row as f32 * g.ch + 1.0;
        let h = (g.ch - 2.0).max(0.0);
        let mut x = card_x
            + INSET_CELLS * g.cw
            + if from_label_col { LABEL_CELLS * g.cw } else { 0.0 };
        for (hit, label) in hits {
            let w = text_w(label, body_px) + 2.0 * CHIP_PAD_CELLS * g.cw;
            let w = w.min((card_x + card_w - x - g.cw).max(0.0));
            out.push((*hit, (x, y, w, h)));
            x += w + CHIP_GAP_CELLS * g.cw;
        }
    };
    let d = direction_labels();
    push_row(
        2,
        &[
            (ConnCardHit::Direction(CardDirection::SrcToDst), d[0]),
            (ConnCardHit::Direction(CardDirection::DstToSrc), d[1]),
            (ConnCardHit::Direction(CardDirection::Both), d[2]),
        ],
        true,
    );
    let k = kind_labels();
    push_row(
        3,
        &[
            (ConnCardHit::Kind(ConnectionKind::Pull), k[0]),
            (ConnCardHit::Kind(ConnectionKind::Push), k[1]),
            (ConnCardHit::Kind(ConnectionKind::Both), k[2]),
        ],
        true,
    );
    push_row(
        5,
        &[
            (ConnCardHit::Confirm, "Confirm \u{23ce}"),
            (ConnCardHit::Cancel, "Cancel esc"),
        ],
        false,
    );
    out
}

/// The hit target under a card-local point, or `None` (outside points are
/// still swallowed by the modal boundary — they just activate nothing).
pub(crate) fn conn_card_hit(
    state: &ConnCardState,
    g: &SettingsGeom,
    x: f32,
    y: f32,
) -> Option<ConnCardHit> {
    conn_card_hit_rects(state, g)
        .into_iter()
        .find(|(_, (rx, ry, rw, rh))| x >= *rx && x < rx + rw && y >= *ry && y < ry + rh)
        .map(|(hit, _)| hit)
}

/// Paint the card: the pair title (arrow live with the direction), the two
/// chip rows (selected chip washed+ringed, focused row's chips full-strength),
/// and the Confirm/Cancel buttons. PURE — captured WYSIWYG through the shared
/// tray path; the palette's shadow/panel/stroke card shape.
pub(crate) fn conn_card_tray(state: &ConnCardState, g: &SettingsGeom, theme: Theme) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let layout = conn_card_layout(state, g);
    let (card_x, card_y, card_w, card_h) = layout.card;
    let radius = (ch * 0.5).min(12.0);
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
        // Opaque like the palette: the tray rasterizer has no backdrop blur.
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

    let text_at = |prims: &mut Vec<DrawPrim>, x: f32, y0: f32, step: TypeStep, s: &str, color| {
        if s.is_empty() {
            return;
        }
        let size = step.px(px);
        prims.push(text_prim(
            x,
            row_baseline(y0, ch, size.get()),
            s.to_string(),
            size,
            TextWeight::Regular,
            TextFace::Mono,
            color,
        ));
    };

    // Row 0: the pair title.
    text_at(
        &mut prims,
        card_x + INSET_CELLS * cw,
        card_y,
        TypeStep::Body,
        &state.title_line(),
        rgba(r.text_primary, 0xFF),
    );

    // Row labels (dim captions).
    text_at(
        &mut prims,
        card_x + INSET_CELLS * cw,
        card_y + 2.0 * ch,
        TypeStep::Caption,
        "direction",
        rgba(r.text_tertiary, 0xFF),
    );
    text_at(
        &mut prims,
        card_x + INSET_CELLS * cw,
        card_y + 3.0 * ch,
        TypeStep::Caption,
        "kind",
        rgba(r.text_tertiary, 0xFF),
    );

    // Chips + buttons off the SHARED rect table.
    for (hit, (x, y, w, h)) in conn_card_hit_rects(state, g) {
        let (label, selected, is_button) = match hit {
            ConnCardHit::Direction(d) => (
                direction_labels()[DIRECTIONS.iter().position(|v| *v == d).unwrap_or(0)],
                state.direction == d,
                false,
            ),
            ConnCardHit::Kind(k) => (
                kind_labels()[KINDS.iter().position(|v| *v == k).unwrap_or(0)],
                state.kind == k,
                false,
            ),
            ConnCardHit::Confirm => ("Confirm \u{23ce}", false, true),
            ConnCardHit::Cancel => ("Cancel esc", false, true),
        };
        let focused_row = matches!(
            (hit, state.focus),
            (ConnCardHit::Direction(_), CardRow::Direction) | (ConnCardHit::Kind(_), CardRow::Kind)
        );
        if selected {
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
                width: if focused_row { 1.5 } else { 1.0 },
                color: rgba(r.accent, if focused_row { 0xCC } else { 0x88 }),
            });
        } else if is_button || state.pointer_over == Some(hit) {
            prims.push(DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius: ch * 0.3,
                width: 1.0,
                color: rgba(r.separator, 0xCC),
            });
        }
        let color = if selected || is_button {
            r.text_primary
        } else if focused_row {
            r.text_secondary
        } else {
            r.text_tertiary
        };
        text_at(
            &mut prims,
            x + CHIP_PAD_CELLS * cw,
            y - 1.0,
            TypeStep::Body,
            label,
            rgba(color, 0xFF),
        );
    }
    prims.push(DrawPrim::ClipPop);

    TrayInput {
        prims,
        card: layout.card,
    }
}

/// The card's accessibility tree — four actionable nodes at STATIC ids (the
/// card has no dynamic row set, so no epoch is needed): the two cycler rows
/// (Click cycles their value) and the two buttons. Focus follows the focused
/// control row.
#[cfg(a11y_tree)]
pub(crate) fn conn_card_a11y(state: &ConnCardState) -> accesskit::TreeUpdate {
    use accesskit::{Action, Node, NodeId, Role, Tree, TreeId, TreeUpdate};

    let root_id = NodeId(0);
    let mk = |role, label: String| {
        let mut node = Node::new(role);
        node.set_label(label);
        node.add_action(Action::Focus);
        node.add_action(Action::Click);
        node
    };
    let direction = match state.direction {
        CardDirection::SrcToDst => "S to T",
        CardDirection::DstToSrc => "T to S",
        CardDirection::Both => "both directions",
    };
    let nodes: Vec<(NodeId, Node)> = vec![
        (
            NodeId(1),
            mk(Role::MenuItem, format!("Direction: {direction}")),
        ),
        (
            NodeId(2),
            mk(Role::MenuItem, format!("Kind: {}", kind_str(state.kind))),
        ),
        (NodeId(3), mk(Role::Button, "Confirm".to_string())),
        (NodeId(4), mk(Role::Button, "Cancel".to_string())),
        (root_id, {
            let mut root = Node::new(Role::Window);
            root.set_label(state.title_line());
            root.set_children(vec![NodeId(1), NodeId(2), NodeId(3), NodeId(4)]);
            root
        }),
    ];
    let focus = match state.focus {
        CardRow::Direction => NodeId(1),
        CardRow::Kind => NodeId(2),
    };
    TreeUpdate {
        nodes,
        tree: Some(Tree::new(root_id)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(s: &str) -> SessionId {
        SessionId::new(s)
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

    fn card(prefill: PairKinds) -> ConnCardState {
        ConnCardState::new(
            crate::WindowId(0),
            sid("s-src"),
            "worker".to_string(),
            sid("s-dst"),
            "operator".to_string(),
            prefill,
            "menu",
            4,
            1,
        )
    }

    /// An empty prefill opens at the §3.3 DECIDED default (S→T, both); an
    /// existing half selects itself so confirming the initial state never
    /// silently revokes it.
    #[test]
    fn prefill_derives_the_initial_selection() {
        let c = card(PairKinds::default());
        assert_eq!(c.direction, CardDirection::SrcToDst);
        assert_eq!(c.kind, ConnectionKind::Both);

        let c = card(PairKinds {
            src_to_dst: None,
            dst_to_src: Some(ConnectionKind::Pull),
        });
        assert_eq!(c.direction, CardDirection::DstToSrc);
        assert_eq!(c.kind, ConnectionKind::Pull);

        let c = card(PairKinds {
            src_to_dst: Some(ConnectionKind::Push),
            dst_to_src: Some(ConnectionKind::Pull),
        });
        assert_eq!(c.direction, CardDirection::Both);
        // The single control shows the S→T half's kind when they differ.
        assert_eq!(c.kind, ConnectionKind::Push);
        // Confirming the initial BOTH state rewrites both halves to it —
        // visible in the card before Confirm, no silent revocation.
        let plan = c.plan();
        assert_eq!(plan.disconnects, Vec::new());
        assert_eq!(plan.connects.len(), 2);
    }

    /// The plan is pure §2.5 set semantics: selected halves connect at the
    /// EDITED kind, a deselected half that existed disconnects, one that never
    /// existed produces nothing.
    #[test]
    fn plan_diffs_the_edit_against_the_open_baseline() {
        // Configure an existing S→T pull down to push.
        let mut c = card(PairKinds {
            src_to_dst: Some(ConnectionKind::Pull),
            dst_to_src: None,
        });
        assert_eq!(c.direction, CardDirection::SrcToDst);
        c.move_focus(); // -> Kind
        c.cycle_value(1); // pull -> push
        assert_eq!(c.kind, ConnectionKind::Push);
        let plan = c.plan();
        assert_eq!(
            plan.connects,
            vec![(sid("s-src"), sid("s-dst"), ConnectionKind::Push)]
        );
        assert!(plan.disconnects.is_empty(), "nothing deselected: {plan:?}");

        // Flip the direction to T→S: the S→T half existed and is now
        // deselected — the plan disconnects it and connects the reverse.
        c.move_focus(); // -> Direction
        c.cycle_value(1); // SrcToDst -> DstToSrc
        let plan = c.plan();
        assert_eq!(
            plan.connects,
            vec![(sid("s-dst"), sid("s-src"), ConnectionKind::Push)]
        );
        assert_eq!(plan.disconnects, vec![(sid("s-src"), sid("s-dst"))]);

        // A fresh pair deselected in one direction disconnects NOTHING (it
        // never existed) — no phantom revokes.
        let c2 = card(PairKinds::default());
        let plan = c2.plan();
        assert_eq!(plan.disconnects, Vec::new());
        assert_eq!(
            plan.connects,
            vec![(sid("s-src"), sid("s-dst"), ConnectionKind::Both)]
        );
    }

    /// Left/Right wrap each control independently; Up/Down/Tab toggle focus.
    #[test]
    fn cycling_wraps_and_focus_toggles() {
        let mut c = card(PairKinds::default());
        c.cycle_value(-1); // SrcToDst backwards wraps to Both
        assert_eq!(c.direction, CardDirection::Both);
        c.cycle_value(1);
        assert_eq!(c.direction, CardDirection::SrcToDst);
        c.move_focus();
        c.cycle_value(1); // Both -> wraps to Pull
        assert_eq!(c.kind, ConnectionKind::Pull);
        assert_eq!(c.direction, CardDirection::SrcToDst, "kind cycling never moves direction");
        c.move_focus();
        c.cycle_value(1);
        assert_eq!(c.direction, CardDirection::DstToSrc);
        assert_eq!(c.kind, ConnectionKind::Pull, "direction cycling never moves kind");
    }

    /// The title names S and T by sid+title and its arrow tracks the live
    /// direction selection.
    #[test]
    fn title_names_the_pair_and_tracks_direction() {
        let mut c = card(PairKinds::default());
        let t = c.title_line();
        assert!(t.contains("@s-src") && t.contains("\"worker\""), "{t}");
        assert!(t.contains("@s-dst") && t.contains("\"operator\""), "{t}");
        assert!(t.contains('\u{21e5}'), "S→T arrow: {t}");
        c.cycle_value(2); // -> Both
        assert!(c.title_line().contains('\u{21c6}'), "peer arrow follows");
    }

    /// Painter and hit-test share one rect table: the point at a painted
    /// chip's centre hits exactly that target, and activating a chip moves the
    /// selection; press/release requires the SAME target.
    #[test]
    fn hit_test_shares_the_painted_chip_rects() {
        let mut c = card(PairKinds::default());
        let g = geom();
        let rects = conn_card_hit_rects(&c, &g);
        for (hit, (x, y, w, h)) in &rects {
            assert_eq!(
                conn_card_hit(&c, &g, x + w * 0.5, y + h * 0.5),
                Some(*hit),
                "chip centre resolves to its own target"
            );
        }
        // Card-local but off every chip: no target (still modal-swallowed).
        let (cx, cy, ..) = conn_card_layout(&c, &g).card;
        assert_eq!(conn_card_hit(&c, &g, cx + 1.0, cy + 1.0), None);

        // Click the T→S chip: press+release on it re-aims the direction.
        let t2s = ConnCardHit::Direction(CardDirection::DstToSrc);
        assert!(c.pointer_press(Some(t2s)));
        let (_, activated) = c.pointer_release(Some(t2s));
        assert_eq!(activated, Some(t2s));
        assert!(c.activate_hit(t2s), "the chip re-aims the direction");
        assert_eq!(c.direction, CardDirection::DstToSrc);

        // Press one chip, release on another: nothing activates.
        assert!(c.pointer_press(Some(ConnCardHit::Confirm)));
        let (_, activated) = c.pointer_release(Some(ConnCardHit::Cancel));
        assert_eq!(activated, None, "moved off the armed target");

        // The tray paints the title and every chip label.
        let tray = conn_card_tray(&c, &g, Theme::default());
        for label in ["pull", "push", "Confirm \u{23ce}", "Cancel esc"] {
            assert!(
                tray.prims
                    .iter()
                    .any(|p| matches!(p, DrawPrim::Text { s, .. } if s == label)),
                "chip {label:?} painted"
            );
        }
    }

    /// Fingerprint moves with every visible edit (repaint-key law), nonzero.
    #[test]
    fn fingerprint_tracks_edits() {
        let mut c = card(PairKinds::default());
        let a = c.fingerprint();
        assert_ne!(a, 0);
        c.cycle_value(1);
        let b = c.fingerprint();
        assert_ne!(a, b);
        c.move_focus();
        assert_ne!(b, c.fingerprint());
    }

    /// The card clamps inside the tray at hostile anchors.
    #[test]
    fn anchored_card_clamps_into_the_tray() {
        let g = geom();
        let far = ConnCardState::new(
            crate::WindowId(0),
            sid("s-a"),
            "a".to_string(),
            sid("s-b"),
            "b".to_string(),
            PairKinds::default(),
            "drag",
            159,
            47,
        );
        let layout = conn_card_layout(&far, &g);
        let (x, y, w, h) = layout.card;
        assert!(x + w <= g.cols as f32 * g.cw);
        assert!(y + h <= g.panel_rows as f32 * g.ch + 0.01);
        assert!(x >= 0.0 && y >= 0.0);
    }
}
