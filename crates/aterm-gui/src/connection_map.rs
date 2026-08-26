// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The CONNECTION MAP overlay (design §5.2): the whole instance's session-
//! connection fabric on one card — sessions as chips grouped by window, one
//! labeled arrow per flow direction (`pushes` when a write-class op is present,
//! `pulls` when read-only), live lease/watcher annotations, and a keyboard
//! selection model (chips raise, connections disconnect behind an inline
//! confirm, Esc closes).
//!
//! PURE like the palette/picker: the App glue ([`crate::app_connection_map`])
//! snapshots groups + flows from the ONE edge fold ([`crate::connections::
//! all_edges`] — never a second aggregation) and stamps the live annotations
//! from the `who` verb's seam at paint time; this module owns only layout,
//! selection, and paint. Lives in the single modal [`crate::overlay::Overlay`]
//! slot, so its keys are structurally gated BEFORE the terminal.
//!
//! ## The layered projection (§5.2, REJECTED-11 stands)
//!
//! When the PUSH subgraph is acyclic, window groups are ordered so pushers sit
//! above pushed — each session's level is its LONGEST push-path depth (a
//! general DAG walk: a session pushed from two chains sits below both, never
//! assume a tree), and a group sinks to its deepest member. A push cycle keeps
//! the plain grouped layout — drawn honestly, never forced into layers. Pull
//! arrows never constrain the layering (observation has no "above").

use std::collections::BTreeMap;
use std::collections::HashMap;

use aterm_render::Theme;
use aterm_session::SessionId;

use crate::settings::{Roles, SettingsGeom, fit, text_w};
use crate::tray_raster::row_baseline;
use crate::type_scale::TypeStep;
use crate::widget::{DrawPrim, TextFace, TextWeight, TrayInput, rgba, text_prim};

/// The most body lines shown at once; beyond this the card scrolls.
const MAX_BODY_LINES: usize = 16;
/// Chrome rows framing the scrolling band: title on top, hint footer below.
const CHROME_ROWS: usize = 2;
/// Maximum logical width of the floating card (the palette's rule).
const MAX_CARD_WIDTH: f32 = 760.0;

/// One session chip on the map.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MapChip {
    pub sid: SessionId,
    /// Registry local id — `None` for a FOREIGN sid (a wire-granted src no
    /// local session owns, the §4.1 honesty rule): listed, but not raisable.
    pub local_id: Option<u64>,
    /// Display title: user meta title ▸ registry title (the fleet-glance rung).
    pub title: String,
}

/// One window group of chips (or the trailing group for unhosted sids).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MapGroup {
    /// The window header the group renders under.
    pub label: String,
    pub chips: Vec<MapChip>,
}

/// One directed flow arrow `src → dst` — one arrow PER DIRECTION, so a peer
/// pair `A ⇆ B` is two arrows (drawn honestly, §5.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct MapFlow {
    pub src: SessionId,
    pub dst: SessionId,
    /// Any write-class op (`write-input`/`signal`) in the direction's fold ⇒
    /// `pushes`; read-only ⇒ `pulls` (the [`crate::connections::pair_kinds`]
    /// push-half rule, so the arrow label and the configure sheet agree).
    pub push: bool,
}

impl MapFlow {
    /// The arrow's verb label.
    pub(crate) fn label(&self) -> &'static str {
        if self.push { "pushes" } else { "pulls" }
    }
}

/// Live per-session annotation, re-read from the `who` seam (turn lease +
/// subscriber registry) at PAINT TIME — liveness has no wake funnel (§5.1),
/// so it is exactly as fresh as the most recent paint, never fresher.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub(crate) struct MapAnnotation {
    /// The live turn-lease holder (`who`'s `driving=` token), `None` when idle.
    pub driving: Option<String>,
    /// Live `subscribe` watcher count on the session.
    pub watchers: usize,
}

/// One selectable element — a chip or a flow arrow — in navigation order
/// (every group's chips first, then the arrows).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MapItem {
    Chip { group: usize, chip: usize },
    Flow { flow: usize },
}

/// What activating the selection (Enter / pointer release / a11y Click) means
/// — decided here so keyboard, pointer, and a11y can never diverge.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) enum MapActivation {
    /// Nothing to act on (empty map).
    None,
    /// A chip: raise the session (its registry local id when hosted here).
    Raise(Option<u64>, SessionId),
    /// A flow with NO armed confirm: arm the inline confirm (the same
    /// two-step Delete takes — activation never disconnects un-confirmed).
    Armed,
    /// A flow whose inline confirm was armed: dissolve `src → dst`.
    Disconnect(SessionId, SessionId),
}

/// One painted body line. Headers/placeholder carry no selection; chip/flow
/// lines carry their [`MapItem`] ordinal.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MapLine {
    Group(usize),
    Chip { item: usize, group: usize, chip: usize },
    FlowsHeader,
    Flow { item: usize, flow: usize },
    Empty,
}

/// Longest-path level per push-graph node — `None` when the push subgraph is
/// CYCLIC (the §5.2 fallback signal). Kahn's topological order with a
/// max-of-parents DP: a node reachable along two chains takes the LONGER one
/// (multi-parent DAGs are first-class; never assume a tree). Self-loops never
/// reach here (§1.5 filters them at the fold).
pub(crate) fn push_levels(flows: &[MapFlow]) -> Option<HashMap<String, usize>> {
    let mut nodes: Vec<&str> = Vec::new();
    let mut index: HashMap<&str, usize> = HashMap::new();
    for f in flows.iter().filter(|f| f.push) {
        for sid in [f.src.as_str(), f.dst.as_str()] {
            if let std::collections::hash_map::Entry::Vacant(e) = index.entry(sid) {
                e.insert(nodes.len());
                nodes.push(sid);
            }
        }
    }
    let n = nodes.len();
    let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    let mut indegree: Vec<usize> = vec![0; n];
    for f in flows.iter().filter(|f| f.push) {
        let (s, d) = (index[f.src.as_str()], index[f.dst.as_str()]);
        out_edges[s].push(d);
        indegree[d] += 1;
    }
    let mut level: Vec<usize> = vec![0; n];
    let mut queue: Vec<usize> = (0..n).filter(|&i| indegree[i] == 0).collect();
    let mut seen = 0;
    while let Some(u) = queue.pop() {
        seen += 1;
        for &v in &out_edges[u] {
            level[v] = level[v].max(level[u] + 1);
            indegree[v] -= 1;
            if indegree[v] == 0 {
                queue.push(v);
            }
        }
    }
    if seen < n {
        return None; // a push cycle: some node never reached indegree 0
    }
    Some(
        nodes
            .iter()
            .enumerate()
            .map(|(i, sid)| ((*sid).to_string(), level[i]))
            .collect(),
    )
}

/// Order `groups` for the §5.2 layered projection: pushers above pushed by
/// longest-path level, a group keyed by its DEEPEST member (the deepest pushed
/// session drags its window below every pusher chain feeding it), stable
/// within a level. Returns whether the layered order applied — `false` (order
/// untouched) when the push subgraph is cyclic.
pub(crate) fn order_groups(groups: &mut [MapGroup], flows: &[MapFlow]) -> bool {
    let Some(levels) = push_levels(flows) else {
        return false;
    };
    groups.sort_by_key(|g| {
        g.chips
            .iter()
            .map(|c| levels.get(c.sid.as_str()).copied().unwrap_or(0))
            .max()
            .unwrap_or(0)
    });
    true
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct MapLayout {
    card: (f32, f32, f32, f32),
    card_rows: usize,
    body_rows: usize,
}

/// Transient state of the open connection map — the
/// [`crate::overlay::Overlay::ConnectionMap`] payload.
pub(crate) struct ConnectionMapState {
    /// The hosting (frontmost-at-open) window.
    pub(crate) window: crate::WindowId,
    groups: Vec<MapGroup>,
    flows: Vec<MapFlow>,
    /// Whether the layered projection applied (push subgraph acyclic).
    layered: bool,
    /// Paint-time liveness, keyed by sid string (BTreeMap: deterministic
    /// iteration for the fingerprint).
    annotations: BTreeMap<String, MapAnnotation>,
    /// Cursor over [`Self::items`].
    selected: usize,
    /// Scroll over BODY LINES (headers included), clamped by moves.
    scroll: usize,
    /// The flow ITEM index whose inline disconnect confirm is armed.
    confirm: Option<usize>,
    /// Exact item index under the pointer (the palette's distinct rule).
    pointer_over: Option<usize>,
    pointer_armed: Option<usize>,
}

/// Selection identity that survives a graph refresh (retarget re-finds it).
#[derive(Clone, PartialEq, Eq, Debug)]
enum MapIdentity {
    Chip(SessionId),
    Flow(SessionId, SessionId),
}

impl ConnectionMapState {
    pub(crate) fn new(
        window: crate::WindowId,
        mut groups: Vec<MapGroup>,
        flows: Vec<MapFlow>,
    ) -> Self {
        let layered = order_groups(&mut groups, &flows);
        Self {
            window,
            groups,
            flows,
            layered,
            annotations: BTreeMap::new(),
            selected: 0,
            scroll: 0,
            confirm: None,
            pointer_over: None,
            pointer_armed: None,
        }
    }

    /// Every selectable element in navigation order: chips (group order),
    /// then flow arrows (listing order).
    fn items(&self) -> Vec<MapItem> {
        let mut out = Vec::new();
        for (g, group) in self.groups.iter().enumerate() {
            for c in 0..group.chips.len() {
                out.push(MapItem::Chip { group: g, chip: c });
            }
        }
        for f in 0..self.flows.len() {
            out.push(MapItem::Flow { flow: f });
        }
        out
    }

    /// The painted body lines: group headers + chips, then the arrows under
    /// their own header; an honest placeholder when the instance is empty.
    fn lines(&self) -> Vec<MapLine> {
        let mut out = Vec::new();
        let mut item = 0;
        for (g, group) in self.groups.iter().enumerate() {
            out.push(MapLine::Group(g));
            for c in 0..group.chips.len() {
                out.push(MapLine::Chip {
                    item,
                    group: g,
                    chip: c,
                });
                item += 1;
            }
        }
        if !self.flows.is_empty() {
            out.push(MapLine::FlowsHeader);
            for f in 0..self.flows.len() {
                out.push(MapLine::Flow { item, flow: f });
                item += 1;
            }
        }
        if out.is_empty() {
            out.push(MapLine::Empty);
        }
        out
    }

    /// The body-line ordinal carrying item `idx` (for scroll clamping).
    fn line_of_item(&self, idx: usize) -> Option<usize> {
        self.lines().iter().position(|l| match l {
            MapLine::Chip { item, .. } | MapLine::Flow { item, .. } => *item == idx,
            _ => false,
        })
    }

    fn chip(&self, item: MapItem) -> Option<&MapChip> {
        match item {
            MapItem::Chip { group, chip } => self.groups.get(group)?.chips.get(chip),
            MapItem::Flow { .. } => None,
        }
    }

    /// The selected element, or `None` on an empty map.
    pub(crate) fn selected_item(&self) -> Option<MapItem> {
        self.items().get(self.selected).copied()
    }

    /// A chip's directional roles from THIS snapshot's arrows — the same
    /// §4.1 vocabulary the tab mark renders (`▲` out, `▽` in, `⧗` both).
    fn roles_glyph(&self, sid: &SessionId) -> &'static str {
        let outbound = self.flows.iter().any(|f| f.src == *sid);
        let inbound = self.flows.iter().any(|f| f.dst == *sid);
        match (outbound, inbound) {
            (true, true) => "\u{29d7}",
            (true, false) => "\u{25b2}",
            (false, true) => "\u{25bd}",
            (false, false) => "\u{00b7}",
        }
    }

    /// A chip's display line (paint == a11y == `controls`).
    fn chip_text(&self, chip: &MapChip) -> String {
        let mut title = crate::session_timeline::sanitize_presentation_line(&chip.title, 40);
        if title.is_empty() {
            title = "(untitled)".to_string();
        }
        let mut s = format!(
            "{} \"{title}\"  @{}",
            self.roles_glyph(&chip.sid),
            chip.sid.as_str()
        );
        if let Some(ann) = self.annotations.get(chip.sid.as_str())
            && (ann.driving.is_some() || ann.watchers > 0)
        {
            s.push_str(&format!(
                "  \u{2014} driving={} watchers={}",
                ann.driving.as_deref().unwrap_or("-"),
                ann.watchers
            ));
        }
        s
    }

    /// A flow's display line; the armed inline confirm renders IN the row.
    fn flow_text(&self, item: usize, flow: &MapFlow) -> String {
        let mut s = format!(
            "@{} \u{2500}{}\u{2192} @{}",
            flow.src.as_str(),
            flow.label(),
            flow.dst.as_str()
        );
        if self.confirm == Some(item) {
            s.push_str("  disconnect? \u{23ce}/\u{232b} confirm \u{00b7} esc keeps");
        }
        s
    }

    fn line_text(&self, line: MapLine) -> String {
        match line {
            MapLine::Group(g) => format!("\u{25b8} {}", self.groups[g].label),
            MapLine::Chip { group, chip, .. } => self.chip_text(&self.groups[group].chips[chip]),
            MapLine::FlowsHeader => format!("connections ({})", self.flows.len()),
            MapLine::Flow { item, flow } => self.flow_text(item, &self.flows[flow]),
            MapLine::Empty => "no sessions".to_string(),
        }
    }

    /// Re-stamp the paint-time annotations; reports whether anything changed
    /// (so a caller can skip the repaint poke on a steady state).
    pub(crate) fn set_annotations(&mut self, annotations: BTreeMap<String, MapAnnotation>) -> bool {
        if self.annotations == annotations {
            return false;
        }
        self.annotations = annotations;
        true
    }

    fn identity(&self, idx: usize) -> Option<MapIdentity> {
        match self.items().get(idx)? {
            MapItem::Chip { .. } => self
                .chip(self.items()[idx])
                .map(|c| MapIdentity::Chip(c.sid.clone())),
            MapItem::Flow { flow } => self
                .flows
                .get(*flow)
                .map(|f| MapIdentity::Flow(f.src.clone(), f.dst.clone())),
        }
    }

    /// Replace the graph snapshot (the §2.4 freshness funnel: an authority
    /// act recomposes the OPEN map immediately). Selection and the armed
    /// confirm survive BY IDENTITY — a disconnect elsewhere must not silently
    /// re-aim either at whatever inherited the ordinal.
    pub(crate) fn retarget(&mut self, mut groups: Vec<MapGroup>, flows: Vec<MapFlow>) {
        let keep_selected = self.identity(self.selected);
        let keep_confirm = self.confirm.and_then(|idx| self.identity(idx));
        self.layered = order_groups(&mut groups, &flows);
        self.groups = groups;
        self.flows = flows;
        let find = |state: &Self, id: &MapIdentity| -> Option<usize> {
            state.items().iter().position(|it| match (it, id) {
                (MapItem::Chip { .. }, MapIdentity::Chip(sid)) => {
                    state.chip(*it).is_some_and(|c| c.sid == *sid)
                }
                (MapItem::Flow { flow }, MapIdentity::Flow(src, dst)) => state
                    .flows
                    .get(*flow)
                    .is_some_and(|f| f.src == *src && f.dst == *dst),
                _ => false,
            })
        };
        self.selected = keep_selected
            .as_ref()
            .and_then(|id| find(self, id))
            .unwrap_or(0)
            .min(self.items().len().saturating_sub(1));
        self.confirm = keep_confirm.as_ref().and_then(|id| find(self, id));
        self.pointer_over = None;
        self.pointer_armed = None;
        self.clamp_scroll();
    }

    /// Move the cursor over the items (wrapping); movement CANCELS an armed
    /// confirm — walking away from the row is the keyboard's "keep".
    pub(crate) fn move_selection(&mut self, delta: isize) {
        self.pointer_over = None;
        self.pointer_armed = None;
        let n = self.items().len();
        if n == 0 {
            return;
        }
        let next = (self.selected as isize + delta).rem_euclid(n as isize) as usize;
        if next != self.selected {
            self.confirm = None;
        }
        self.selected = next;
        self.clamp_scroll();
    }

    /// Move the cursor to item `idx` (a11y Focus / pointer land here). A move
    /// to a DIFFERENT item cancels the armed confirm, same as the arrows.
    pub(crate) fn select(&mut self, idx: usize) {
        let n = self.items().len();
        if n == 0 {
            return;
        }
        let next = idx.min(n - 1);
        if next != self.selected {
            self.confirm = None;
        }
        self.selected = next;
        self.clamp_scroll();
    }

    /// Scroll the band without moving the cursor (wheel); the ring may leave
    /// the viewport — honest, the cursor did not move.
    pub(crate) fn scroll_by(&mut self, delta: isize) -> bool {
        let total = self.lines().len();
        let body = total.min(MAX_BODY_LINES);
        let max_scroll = total.saturating_sub(body);
        let before = self.scroll;
        self.scroll = self.scroll.saturating_add_signed(delta).min(max_scroll);
        self.pointer_over = None;
        self.pointer_armed = None;
        before != self.scroll
    }

    fn clamp_scroll(&mut self) {
        let total = self.lines().len();
        let body = total.min(MAX_BODY_LINES);
        if body == 0 {
            self.scroll = 0;
            return;
        }
        if let Some(line) = self.line_of_item(self.selected) {
            if line < self.scroll {
                self.scroll = line;
            } else if line >= self.scroll + body {
                self.scroll = line + 1 - body;
            }
        }
        self.scroll = self.scroll.min(total.saturating_sub(body));
    }

    /// The Delete/Backspace press: on a flow row, arm the inline confirm, or —
    /// already armed on THIS row — resolve to the disconnect pair. Chips (and
    /// an empty map) do nothing: Delete only ever dissolves a connection.
    pub(crate) fn delete_pressed(&mut self) -> Option<(SessionId, SessionId)> {
        let MapItem::Flow { flow } = self.selected_item()? else {
            return None;
        };
        if self.confirm == Some(self.selected) {
            self.confirm = None;
            let f = &self.flows[flow];
            return Some((f.src.clone(), f.dst.clone()));
        }
        self.confirm = Some(self.selected);
        None
    }

    /// The Enter/Click activation — see [`MapActivation`].
    pub(crate) fn activate(&mut self) -> MapActivation {
        match self.selected_item() {
            None => MapActivation::None,
            Some(item @ MapItem::Chip { .. }) => {
                let chip = self.chip(item).expect("selected chip exists");
                MapActivation::Raise(chip.local_id, chip.sid.clone())
            }
            Some(MapItem::Flow { flow }) => {
                if self.confirm == Some(self.selected) {
                    self.confirm = None;
                    let f = &self.flows[flow];
                    MapActivation::Disconnect(f.src.clone(), f.dst.clone())
                } else {
                    self.confirm = Some(self.selected);
                    MapActivation::Armed
                }
            }
        }
    }

    /// The Esc press: an armed confirm is cancelled (the map STAYS open);
    /// otherwise the map closes. Returns `true` to close.
    pub(crate) fn escape(&mut self) -> bool {
        if self.confirm.is_some() {
            self.confirm = None;
            return false;
        }
        true
    }

    // ---- Pointer arming (the palette press/release discipline) -------------

    /// Hover an item, dragging the selection with the pointer.
    pub(crate) fn pointer_hover(&mut self, idx: Option<usize>) -> bool {
        let before = (self.selected, self.confirm, self.pointer_over);
        if let Some(idx) = idx {
            self.select(idx);
        }
        self.pointer_over = idx;
        before != (self.selected, self.confirm, self.pointer_over)
    }

    /// Arm the hovered item on left press.
    pub(crate) fn pointer_press(&mut self, idx: Option<usize>) -> bool {
        let mut changed = self.pointer_hover(idx);
        changed |= self.pointer_armed != self.pointer_over;
        self.pointer_armed = self.pointer_over;
        changed
    }

    /// Settle a left release: `(changed, activate)` — activation requires the
    /// SAME item at press and release.
    pub(crate) fn pointer_release(&mut self, idx: Option<usize>) -> (bool, bool) {
        let mut changed = self.pointer_hover(idx);
        let armed = self.pointer_armed.take();
        changed |= armed.is_some();
        (changed, idx.is_some() && armed == idx)
    }

    pub(crate) fn pointer_over_item(&self) -> bool {
        self.pointer_over.is_some()
    }

    // ---- Overlay-model surface --------------------------------------------

    /// The overlay height: chrome + the (capped) body band, never `0`.
    pub(crate) fn wanted_rows(&self) -> usize {
        CHROME_ROWS + self.lines().len().clamp(1, MAX_BODY_LINES)
    }

    /// `(scroll, total, visible)` for `controls front`.
    pub(crate) fn scroll_extent(&self) -> (usize, usize, usize) {
        let total = self.lines().len();
        (self.scroll, total, total.min(MAX_BODY_LINES))
    }

    /// Machine-readable lines for `controls connections` — the SAME rows the
    /// card paints, so screen == introspection (§5.3 gates the read).
    pub(crate) fn controls_lines(&self) -> Vec<String> {
        let sessions: usize = self.groups.iter().map(|g| g.chips.len()).sum();
        let mut out = vec![format!(
            "connections window={} sessions={} flows={} layered={} selected={} confirm={}",
            self.window.0,
            sessions,
            self.flows.len(),
            self.layered,
            self.selected,
            self.confirm
                .map_or_else(|| "-".to_string(), |i| i.to_string()),
        )];
        let mut item = 0usize;
        for group in &self.groups {
            out.push(format!("connections group label={:?}", group.label));
            for chip in &group.chips {
                let ann = self.annotations.get(chip.sid.as_str());
                out.push(format!(
                    "connections chip sid={} local={} title={:?} selected={} driving={} watchers={}",
                    chip.sid.as_str(),
                    chip.local_id
                        .map_or_else(|| "-".to_string(), |l| l.to_string()),
                    chip.title,
                    item == self.selected,
                    ann.and_then(|a| a.driving.as_deref()).unwrap_or("-"),
                    ann.map_or(0, |a| a.watchers),
                ));
                item += 1;
            }
        }
        for flow in &self.flows {
            out.push(format!(
                "connections flow src={} dst={} kind={} selected={} confirm={}",
                flow.src.as_str(),
                flow.dst.as_str(),
                if flow.push { "push" } else { "pull" },
                item == self.selected,
                self.confirm == Some(item),
            ));
            item += 1;
        }
        out
    }

    /// Repaint fingerprint (never `0` while open). Annotations fold in, so a
    /// paint-time liveness change re-presents through the ordinary repaint key.
    pub(crate) fn fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        self.window.0.hash(&mut h);
        self.layered.hash(&mut h);
        self.selected.hash(&mut h);
        self.scroll.hash(&mut h);
        self.confirm.hash(&mut h);
        for group in &self.groups {
            group.label.hash(&mut h);
            for chip in &group.chips {
                chip.sid.as_str().hash(&mut h);
                chip.local_id.hash(&mut h);
                chip.title.hash(&mut h);
            }
        }
        for flow in &self.flows {
            flow.src.as_str().hash(&mut h);
            flow.dst.as_str().hash(&mut h);
            flow.push.hash(&mut h);
        }
        for (sid, ann) in &self.annotations {
            sid.hash(&mut h);
            ann.driving.hash(&mut h);
            ann.watchers.hash(&mut h);
        }
        h.finish() | 1
    }

    /// A11y row-identity epoch over the item list (the palette's scheme).
    #[cfg(a11y_tree)]
    fn a11y_epoch(&self) -> u32 {
        use std::hash::{Hash, Hasher};
        let mut h = std::collections::hash_map::DefaultHasher::new();
        for item in self.items() {
            match item {
                MapItem::Chip { .. } => {
                    0u8.hash(&mut h);
                    if let Some(c) = self.chip(item) {
                        c.sid.as_str().hash(&mut h);
                    }
                }
                MapItem::Flow { flow } => {
                    1u8.hash(&mut h);
                    self.flows[flow].src.as_str().hash(&mut h);
                    self.flows[flow].dst.as_str().hash(&mut h);
                }
            }
        }
        let epoch = h.finish() as u32;
        if epoch == u32::MAX { u32::MAX - 1 } else { epoch }
    }

    /// Decode a node minted by the CURRENT epoch to its item index.
    #[cfg(a11y_tree)]
    pub(crate) fn a11y_item_index(&self, node: accesskit::NodeId) -> Option<usize> {
        let slot = usize::try_from(node.0 & u64::from(u32::MAX))
            .ok()?
            .checked_sub(1)?;
        (a11y_node_id_for(self.a11y_epoch(), slot) == node && slot < self.items().len())
            .then_some(slot)
    }
}

/// One pure geometry projection shared by paint and hit-testing — the
/// palette's centred, width-capped, content-height card.
fn map_layout(state: &ConnectionMapState, g: &SettingsGeom) -> MapLayout {
    let tray_w = (g.cols as f32 * g.cw).max(0.0);
    let tray_h = (g.panel_rows as f32 * g.ch).max(0.0);
    let desired_margin = (g.cw * 2.0).max(16.0);
    let max_margin = (tray_w * 0.5 - g.cw.max(0.0)).max(0.0);
    let margin = desired_margin.min(max_margin);
    let card_w = (tray_w - margin * 2.0).clamp(0.0, MAX_CARD_WIDTH);
    let card_rows = state.wanted_rows().min(g.panel_rows);
    let card_h = (card_rows as f32 * g.ch).clamp(0.0, tray_h);
    MapLayout {
        card: (
            ((tray_w - card_w) * 0.5).max(0.0),
            ((tray_h - card_h) * 0.5).max(0.0),
            card_w,
            card_h,
        ),
        card_rows,
        body_rows: state
            .lines()
            .len()
            .min(MAX_BODY_LINES)
            .min(card_rows.saturating_sub(CHROME_ROWS)),
    }
}

/// Exact painted selection/hit rectangle for one VISIBLE body slot.
fn map_row_rect_in(layout: &MapLayout, g: &SettingsGeom, slot: usize) -> Option<(f32, f32, f32, f32)> {
    if slot >= layout.body_rows {
        return None;
    }
    let (card_x, card_y, card_w, _) = layout.card;
    Some((
        card_x + g.cw,
        card_y + (1 + slot) as f32 * g.ch + 1.0,
        (card_w - g.cw * 2.0).max(0.0),
        (g.ch - 2.0).max(0.0),
    ))
}

/// Item index under a card-local point — only painted chip/flow lines
/// participate (headers and the placeholder carry no selection).
pub(crate) fn map_item_hit(
    state: &ConnectionMapState,
    g: &SettingsGeom,
    x: f32,
    y: f32,
) -> Option<usize> {
    let lines = state.lines();
    let layout = map_layout(state, g);
    for slot in 0..layout.body_rows {
        let (rx, ry, rw, rh) = map_row_rect_in(&layout, g, slot)?;
        if x >= rx && x < rx + rw && y >= ry && y < ry + rh {
            return match lines.get(state.scroll + slot) {
                Some(MapLine::Chip { item, .. } | MapLine::Flow { item, .. }) => Some(*item),
                _ => None,
            };
        }
    }
    None
}

/// Paint the map: title + counts, the grouped chip/arrow band (cursor
/// washed+ringed; the armed confirm ringed in the danger role), a key-hint
/// footer. PURE.
pub(crate) fn map_tray(state: &ConnectionMapState, g: &SettingsGeom, theme: Theme) -> TrayInput {
    let r = Roles::from_theme(theme);
    let (cw, ch, px) = (g.cw, g.ch, g.font_px);
    let layout = map_layout(state, g);
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

    // Title row: the surface + its honest totals (and the projection in use).
    {
        let sessions: usize = state.groups.iter().map(|grp| grp.chips.len()).sum();
        let title = format!(
            "Connection Map \u{2014} {sessions} session{} \u{00b7} {} connection{}{}",
            if sessions == 1 { "" } else { "s" },
            state.flows.len(),
            if state.flows.len() == 1 { "" } else { "s" },
            if state.layered { "" } else { " \u{00b7} cyclic" },
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

    // The scrolling body band.
    let lines = state.lines();
    for slot in 0..layout.body_rows {
        let Some(line) = lines.get(state.scroll + slot).copied() else {
            break;
        };
        let y0 = card_y + (1 + slot) as f32 * ch;
        let (indent, step, color) = match line {
            MapLine::Group(_) => (cw * 1.5, TypeStep::Body, r.text_secondary),
            MapLine::FlowsHeader => (cw * 1.5, TypeStep::Body, r.text_secondary),
            MapLine::Chip { .. } => (cw * 2.5, TypeStep::Body, r.text_primary),
            MapLine::Flow { .. } => (cw * 2.5, TypeStep::Body, r.text_primary),
            MapLine::Empty => (cw * 2.0, TypeStep::Body, r.text_tertiary),
        };
        if let MapLine::Chip { item, .. } | MapLine::Flow { item, .. } = line
            && item == state.selected
        {
            let (x, y, w, h) = map_row_rect_in(&layout, g, slot)
                .expect("painted map slot has a row rectangle");
            let armed = state.confirm == Some(item);
            let ring = if armed { r.danger } else { r.accent };
            prims.push(DrawPrim::Panel {
                x,
                y,
                w,
                h,
                radius: ch * 0.3,
                fill: rgba(ring, 0x22),
                blur: false,
            });
            prims.push(DrawPrim::Stroke {
                x,
                y,
                w,
                h,
                radius: ch * 0.3,
                width: 1.5,
                color: rgba(ring, 0xCC),
            });
        }
        text_at(
            &mut prims,
            card_x + indent,
            y0,
            step,
            state.line_text(line),
            rgba(color, 0xFF),
        );
    }

    // Key-hint footer (the armed confirm rewrites it to the two-key truth).
    let hint = if state.confirm.is_some() {
        "\u{23ce}/\u{232b} disconnect   esc keep"
    } else {
        "\u{2191}\u{2193} move   \u{23ce} raise   \u{232b} disconnect   esc close"
    };
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

/// The map's accessibility tree — the palette scheme verbatim: a ListBox of
/// the chip/arrow items, epoch-guarded ids, focus follows the cursor.
#[cfg(a11y_tree)]
pub(crate) fn map_a11y(state: &ConnectionMapState) -> accesskit::TreeUpdate {
    use accesskit::{Action, Node, NodeId, Role, Tree, TreeId, TreeUpdate};

    const LIST: NodeId = NodeId(u64::MAX);
    let root_id = NodeId(0);
    let items = state.items();
    let epoch = state.a11y_epoch();

    let mut nodes: Vec<(NodeId, Node)> = Vec::with_capacity(items.len() + 2);
    let mut children: Vec<NodeId> = Vec::with_capacity(items.len());
    for (slot, item) in items.iter().enumerate() {
        let id = a11y_node_id_for(epoch, slot);
        let mut node = Node::new(Role::MenuItem);
        let label = match *item {
            MapItem::Chip { group, chip } => state.chip_text(&state.groups[group].chips[chip]),
            MapItem::Flow { flow } => state.flow_text(slot, &state.flows[flow]),
        };
        node.set_label(label);
        node.add_action(Action::Focus);
        node.add_action(Action::Click);
        nodes.push((id, node));
        children.push(id);
    }

    let mut list = Node::new(Role::ListBox);
    list.set_children(children);
    nodes.push((LIST, list));

    let mut root = Node::new(Role::Window);
    root.set_label("Connection Map");
    root.set_children(vec![LIST]);
    nodes.push((root_id, root));

    let focus = if items.is_empty() {
        root_id
    } else {
        a11y_node_id_for(epoch, state.selected.min(items.len() - 1))
    };

    TreeUpdate {
        nodes,
        tree: Some(Tree::new(root_id)),
        tree_id: TreeId::ROOT,
        focus,
    }
}

/// Mint a row node id (epoch high half, `slot + 1` low — the palette scheme).
#[cfg(a11y_tree)]
fn a11y_node_id_for(epoch: u32, slot: usize) -> accesskit::NodeId {
    accesskit::NodeId((u64::from(epoch) << 32) | (slot as u64 + 1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sid(s: &str) -> SessionId {
        SessionId::new(s)
    }

    fn chip(s: &str, local: u64) -> MapChip {
        MapChip {
            sid: sid(s),
            local_id: Some(local),
            title: format!("t-{s}"),
        }
    }

    fn push(src: &str, dst: &str) -> MapFlow {
        MapFlow {
            src: sid(src),
            dst: sid(dst),
            push: true,
        }
    }

    fn pull(src: &str, dst: &str) -> MapFlow {
        MapFlow {
            src: sid(src),
            dst: sid(dst),
            push: false,
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

    /// Longest-path leveling handles a DIAMOND (multi-parent DAG — never
    /// assume a tree): the join node takes the LONGER chain's depth.
    #[test]
    fn push_levels_layers_an_acyclic_graph_by_longest_path() {
        // a → b → d and a → c → d, plus a → d directly: d's level is 2, not 1.
        let flows = vec![
            push("a", "b"),
            push("a", "c"),
            push("b", "d"),
            push("c", "d"),
            push("a", "d"),
        ];
        let levels = push_levels(&flows).expect("acyclic");
        assert_eq!(levels["a"], 0);
        assert_eq!(levels["b"], 1);
        assert_eq!(levels["c"], 1);
        assert_eq!(levels["d"], 2);
        // Pull arrows never constrain the layering — a pull CYCLE still levels.
        let with_pulls = vec![push("a", "b"), pull("b", "a"), pull("a", "b")];
        assert!(push_levels(&with_pulls).is_some());
    }

    /// A push cycle yields `None` — the §5.2 fallback to the plain layout.
    #[test]
    fn push_levels_refuses_a_cycle() {
        assert!(push_levels(&[push("a", "b"), push("b", "a")]).is_none());
        assert!(push_levels(&[push("a", "b"), push("b", "c"), push("c", "a")]).is_none());
    }

    /// The layered projection orders window groups pushers-above-pushed (a
    /// group keyed by its DEEPEST member) and keeps the given order — flagged
    /// honestly — on a push cycle.
    #[test]
    fn order_groups_layers_pushers_above_pushed_and_falls_back_on_cycle() {
        // Given in "wrong" order: the pushed window listed first.
        let mut groups = vec![
            MapGroup {
                label: "pushed".to_string(),
                chips: vec![chip("b", 2)],
            },
            MapGroup {
                label: "pusher".to_string(),
                chips: vec![chip("a", 1)],
            },
        ];
        assert!(order_groups(&mut groups, &[push("a", "b")]));
        assert_eq!(groups[0].label, "pusher");
        assert_eq!(groups[1].label, "pushed");

        // A push cycle: order untouched, layered=false.
        let mut cyclic = vec![
            MapGroup {
                label: "one".to_string(),
                chips: vec![chip("a", 1)],
            },
            MapGroup {
                label: "two".to_string(),
                chips: vec![chip("b", 2)],
            },
        ];
        assert!(!order_groups(
            &mut cyclic,
            &[push("a", "b"), push("b", "a")]
        ));
        assert_eq!(cyclic[0].label, "one");
        assert_eq!(cyclic[1].label, "two");
    }

    /// A peer pair `A ⇆ B` is TWO labeled arrows — one per direction, each
    /// labeled from ITS OWN op fold (§5.2 draws peers honestly).
    #[test]
    fn peer_pair_lists_one_labeled_arrow_per_direction() {
        let state = ConnectionMapState::new(
            crate::WindowId(0),
            vec![MapGroup {
                label: "Window 1".to_string(),
                chips: vec![chip("a", 1), chip("b", 2)],
            }],
            vec![push("a", "b"), pull("b", "a")],
        );
        let lines = state.controls_lines();
        assert!(lines[0].contains("sessions=2") && lines[0].contains("flows=2"), "{lines:?}");
        assert!(
            lines
                .iter()
                .any(|l| l.contains("flow src=a dst=b kind=push")),
            "{lines:?}"
        );
        assert!(
            lines
                .iter()
                .any(|l| l.contains("flow src=b dst=a kind=pull")),
            "{lines:?}"
        );
        // Both chips carry the ⧗ both-roles glyph in the painted band.
        let tray = map_tray(&state, &geom(), Theme::default());
        let both = tray
            .prims
            .iter()
            .filter(|p| matches!(p, DrawPrim::Text { s, .. } if s.starts_with('\u{29d7}')))
            .count();
        assert_eq!(both, 2, "both endpoints render the peer glyph");
    }

    /// Navigation wraps over chips THEN flows; Delete on a flow arms the
    /// inline confirm, a second Delete resolves it, and moving away cancels.
    #[test]
    fn selection_walks_chips_then_flows_and_delete_two_steps() {
        let mut state = ConnectionMapState::new(
            crate::WindowId(0),
            vec![MapGroup {
                label: "w".to_string(),
                chips: vec![chip("a", 1), chip("b", 2)],
            }],
            vec![push("a", "b")],
        );
        // Chip first: Delete is a no-op there (nothing armed, nothing returned).
        assert_eq!(state.delete_pressed(), None);
        assert!(state.controls_lines()[0].contains("confirm=-"));
        // Enter on a chip resolves to Raise with its local id.
        assert_eq!(
            state.activate(),
            MapActivation::Raise(Some(1), sid("a"))
        );
        // Walk to the flow (2 chips → item 2) and run the two-step Delete.
        state.move_selection(2);
        assert_eq!(state.delete_pressed(), None, "first press only arms");
        assert!(state.controls_lines()[0].contains("confirm=2"));
        assert_eq!(
            state.delete_pressed(),
            Some((sid("a"), sid("b"))),
            "second press resolves"
        );
        // Arm again, then move away: the confirm cancels (keyboard "keep").
        state.delete_pressed();
        state.move_selection(1);
        assert!(state.controls_lines()[0].contains("confirm=-"));
        // Wrap: item 0 again after 3 more moves over 3 items.
        state.move_selection(3);
        assert_eq!(state.activate(), MapActivation::Raise(Some(1), sid("a")));
        // Enter on an unarmed flow ARMS (never disconnects unconfirmed); Esc
        // cancels the confirm and only a second Esc closes.
        state.move_selection(2);
        assert_eq!(state.activate(), MapActivation::Armed);
        assert!(!state.escape(), "first esc keeps the map open");
        assert!(state.escape(), "second esc closes");
        // Enter with the confirm armed resolves to the disconnect.
        assert_eq!(state.activate(), MapActivation::Armed);
        assert_eq!(
            state.activate(),
            MapActivation::Disconnect(sid("a"), sid("b"))
        );
    }

    /// A graph refresh keeps selection and the armed confirm BY IDENTITY, and
    /// drops the confirm with its flow.
    #[test]
    fn retarget_keeps_selection_identity_and_drops_dead_confirms() {
        let groups = || {
            vec![MapGroup {
                label: "w".to_string(),
                chips: vec![chip("a", 1), chip("b", 2)],
            }]
        };
        let mut state = ConnectionMapState::new(
            crate::WindowId(0),
            groups(),
            vec![push("a", "b"), push("b", "a")],
        );
        // Select the b→a flow (items: 2 chips + 2 flows ⇒ index 3) and arm it.
        state.move_selection(3);
        state.delete_pressed();
        // Refresh with a→b gone: the selected/armed b→a flow re-finds itself.
        state.retarget(groups(), vec![push("b", "a")]);
        let lines = state.controls_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("flow src=b dst=a") && l.contains("selected=true") && l.contains("confirm=true")),
            "{lines:?}"
        );
        // Refresh with the armed flow itself gone: selection resets, confirm dies.
        state.retarget(groups(), Vec::new());
        assert!(state.controls_lines()[0].contains("confirm=-"));
        assert_eq!(state.activate(), MapActivation::Raise(Some(1), sid("a")));
    }

    /// Painter and hit-test share one row rectangle; annotations fold into
    /// the fingerprint (a paint-time liveness change re-presents).
    #[test]
    fn hit_test_paint_and_fingerprint_share_one_state() {
        let mut state = ConnectionMapState::new(
            crate::WindowId(0),
            vec![MapGroup {
                label: "Window 1".to_string(),
                chips: vec![chip("a", 1), chip("b", 2)],
            }],
            vec![push("a", "b")],
        );
        let g = geom();
        let layout = map_layout(&state, &g);
        // Slot 0 is the group header (no item); slot 1 is chip a (item 0).
        let (x, y, w, h) = map_row_rect_in(&layout, &g, 1).unwrap();
        assert_eq!(map_item_hit(&state, &g, x + w * 0.5, y + h * 0.5), Some(0));
        let (hx, hy, hw, hh) = map_row_rect_in(&layout, &g, 0).unwrap();
        assert_eq!(
            map_item_hit(&state, &g, hx + hw * 0.5, hy + hh * 0.5),
            None,
            "headers carry no selection"
        );
        let tray = map_tray(&state, &g, Theme::default());
        assert_eq!(tray.card, layout.card);
        assert!(
            tray.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s.contains("Connection Map"))),
            "the title names the surface"
        );
        assert!(
            tray.prims
                .iter()
                .any(|p| matches!(p, DrawPrim::Text { s, .. } if s.contains("pushes"))),
            "the arrow is labeled"
        );

        let before = state.fingerprint();
        assert_ne!(before, 0);
        let mut ann = BTreeMap::new();
        ann.insert(
            "a".to_string(),
            MapAnnotation {
                driving: Some("turn-7".to_string()),
                watchers: 2,
            },
        );
        assert!(state.set_annotations(ann.clone()));
        assert!(!state.set_annotations(ann), "same annotations: no change");
        assert_ne!(state.fingerprint(), before, "annotations fold into the fp");
        let lines = state.controls_lines();
        assert!(
            lines
                .iter()
                .any(|l| l.contains("sid=a") && l.contains("driving=turn-7") && l.contains("watchers=2")),
            "{lines:?}"
        );
    }
}
