// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-tab SPLIT-PANE tree (iTerm2-style panes within one tab).
//!
//! A window has TABS (the existing [`crate::TabIndex`] + `Vec<Session>` model);
//! each TAB now owns a binary [`PaneTree`] of live sessions. A fresh tab is a
//! single [`PaneNode::Leaf`] — the exact one-session-per-tab behavior, so with no
//! splits the geometry is byte-identical to before. Splitting the FOCUSED leaf
//! turns it into a [`PaneNode::Split`] of two leaves (the original session and a
//! freshly-spawned sibling), and closing the focused leaf collapses its parent.
//!
//! This module is PURE GEOMETRY + tree bookkeeping over session ids (`u64`, the
//! same stable ids [`crate::Session::id`] routes `Wake`s with). It owns no
//! `Terminal`, no PTY, and no rendering — the GUI ([`crate::App`]) maps the
//! [`PaneRect`]s this produces back onto the live `Vec<Session>` and composes
//! their per-pane snapshots into the one window frame. Keeping it headless makes
//! the layout math (and the split/close/focus state machine) unit-testable with
//! no window, PTY, or event loop, mirroring [`crate::TabIndex`].
//!
//! DIVIDERS: a split reserves ONE cell line between its children (drawn by the
//! GUI). The `ratio` is the FIRST child's fraction of the splittable extent
//! (everything but the 1-cell divider); MVP always splits 50/50, but the ratio is
//! stored per-split so a later divider drag is a pure data edit (no structural
//! change). Each child is clamped to at least 1 cell so a tiny window never yields
//! a 0-extent pane.
//!
//! # The MINIMUM PANE (2026-08-25 splits audit)
//!
//! A split that cannot fit is REFUSED, not minted. Before this rule, splitting a
//! 24x80 window thirteen times produced fourteen panes — five of them sharing the
//! IDENTICAL off-grid rect `24,79,1x1` (row 24 of a 0..23-row grid), focus landed
//! on a pane nobody could see, and each invisible pane had spawned a REAL SHELL
//! (14 `pwsh.exe` + 14 `conhost.exe` alive). Two things went wrong and both are
//! fixed here:
//!
//! 1. **Creation.** [`split_fits_in`] is the law: a split is allowed only when
//!    BOTH resulting panes are at least [`MIN_PANE_ROWS`] x [`MIN_PANE_COLS`]
//!    cells. The caller ([`crate::App::split_focused_pane_in`]) asks BEFORE it
//!    spawns anything, so a refused split costs exactly zero processes and
//!    answers the person the way every other impossible gesture in this app does
//!    (a stderr line + the transient failure card). It measures the UNZOOMED rect,
//!    because a split un-zooms: a zoomed pane fills the window and is about to
//!    hand that room straight back, so trusting it would wave through the very
//!    split this rule exists to refuse ([`PaneTree::focused_rect`]).
//! 2. **Resize.** A window can still be dragged smaller than its open layout
//!    needs, and shrinking must never restructure the tree (your panes are not
//!    the window manager's to close). THE RULE: **shrinking CLAMPS, it never
//!    collapses.** [`PaneTree::compute_layout`] pins every rect INSIDE the window
//!    grid — an origin can never exceed the last row/column and an extent can
//!    never run past the edge — while keeping at least one cell per pane. Below
//!    the minimum, panes may therefore COINCIDE on the last row/column (unavoidable:
//!    N panes cannot be disjoint in fewer than N cells), but nothing ever lands
//!    outside the grid and the TREE IS UNTOUCHED, so growing the window back
//!    restores the exact geometry it had.
//!
//! The other engine that places panes — [`crate::tab_model::Tab::visible_plan`],
//! which serves heterogeneous native/terminal tabs — obeys the same rule by a
//! different route: its children plus the divider gap TILE the parent exactly, so
//! no rect can escape the bounds by construction. It differs from this module in
//! only one respect, and deliberately: it keeps panes DISJOINT and lets an extent
//! reach zero, where this module keeps a one-cell floor and lets panes coincide.
//! Both keep every leaf, neither restructures, and both round-trip on regrowth
//! (`tab_model::tests::shrinking_a_canonical_plan_stays_inside_the_bounds_and_drops_no_leaf`).

use crate::tab_model::{
    FocusDirection, LogicalDividerHit, LogicalPoint, LogicalRect, RemoveLeaf, SplitAxis, SplitTree,
};

/// Which way a [`PaneNode::Split`] divides its rectangle.  This remains the
/// terminal command spelling; the generic tree stores the corresponding
/// content axis explicitly.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SplitDir {
    /// Children sit SIDE BY SIDE, split by a vertical divider (Cmd-D). The first
    /// child is the LEFT pane, the second the RIGHT; the columns are divided.
    Vertical,
    /// Children are STACKED, split by a horizontal divider (Cmd-Shift-D). The
    /// first child is the TOP pane, the second the BOTTOM; the rows are divided.
    Horizontal,
}

/// Terminal compatibility name for the generic content split tree.  Its leaf
/// payload is still a session id during Phase A; window integration resolves
/// stable `ViewId`s through the process `ViewStore` before reaching this adapter.
pub type PaneNode = SplitTree<u64>;

/// One visible pane's placement in the window grid, in CELL coordinates. The GUI
/// locks that session's `Terminal`, snapshots it at `(rows, cols)`, and blits the
/// cells into the composite window frame at `(row_off, col_off)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PaneRect {
    /// The session occupying this rect (`Leaf::session`).
    pub session: u64,
    /// Top-left cell row offset of this pane within the window grid.
    pub row_off: u16,
    /// Top-left cell column offset of this pane within the window grid.
    pub col_off: u16,
    /// This pane's height in cells (`>= 1`).
    pub rows: u16,
    /// This pane's width in cells (`>= 1`).
    pub cols: u16,
}

/// A tab's pane layout: the binary tree plus the id of the FOCUSED leaf (the pane
/// that keyboard input + the control socket target, and whose cursor draws solid).
/// Every tab owns one; a fresh tab is a single leaf focused on its own session.
#[derive(Clone, Debug, PartialEq)]
pub struct PaneTree {
    root: PaneNode,
    /// The session id of the focused leaf. Always references a leaf that exists in
    /// `root` (maintained by `split`/`close`); used to route input + draw the solid
    /// cursor in exactly one pane.
    focus: u64,
    /// When `true`, the focused pane is temporarily ZOOMED to fill the whole
    /// window ([`compute_layout`](Self::compute_layout) returns just it). A purely
    /// presentational toggle over the unchanged tree — unzoom restores the layout
    /// exactly. Ignored for a single-pane tab. iTerm2-style pane zoom.
    zoomed: bool,
}

/// The smallest fraction a divider drag may leave the FIRST child (and, by
/// symmetry via `1 - MIN_RATIO` = [`MAX_RATIO`], the second). Keeps both panes
/// visibly non-trivial so a drag to the very edge never collapses a pane to a
/// sliver. The logical split engine also enforces a nonzero leaf floor.
const MIN_RATIO: f32 = 0.05;
/// The largest fraction a divider drag may give the first child — the mirror of
/// [`MIN_RATIO`], so the SECOND child also keeps at least `MIN_RATIO`.
const MAX_RATIO: f32 = 0.95;

/// The fewest ROWS a pane the app CREATES may have: a prompt line, the command
/// line it wraps onto, and one line of output. Two rows is a prompt with nowhere
/// to answer; one row is the 1x1 ghost the splits audit found.
pub(crate) const MIN_PANE_ROWS: u16 = 3;
/// The fewest COLUMNS a pane the app CREATES may have. Sixteen is the width at
/// which a short prompt and a short command still share one line; narrower than
/// that, every single line a shell prints wraps, and the pane stops being a place
/// you can work.
pub(crate) const MIN_PANE_COLS: u16 = 16;

/// Can a pane of `rows` x `cols` cells be split in `dir` and leave BOTH halves at
/// least [`MIN_PANE_ROWS`] x [`MIN_PANE_COLS`]?
///
/// THE EXACT GEOMETRY, not an estimate. A fresh split is always 50/50 over the
/// splittable extent — everything but the 1-cell divider — so the divided axis
/// yields `ceil((extent - 1) / 2)` and `floor((extent - 1) / 2)`; the SMALLER half
/// is the floor, and it clears the minimum exactly when `extent - 1 >= 2 * min`.
/// The perpendicular axis is untouched by the split, so it must ALREADY clear its
/// own minimum — a 2-row pane split left/right is two 2-row panes, and neither is
/// a pane anyone can use.
///
/// `pane_tree_min_fit_matches_the_layout_engine` re-derives this against the real
/// [`PaneTree::compute_layout`] over every window size in a wide sweep, so this
/// closed form can never drift away from the layout it predicts.
#[must_use]
pub(crate) fn split_fits_in(dir: SplitDir, rows: u16, cols: u16) -> bool {
    match dir {
        SplitDir::Vertical => rows >= MIN_PANE_ROWS && cols.saturating_sub(1) >= 2 * MIN_PANE_COLS,
        SplitDir::Horizontal => {
            cols >= MIN_PANE_COLS && rows.saturating_sub(1) >= 2 * MIN_PANE_ROWS
        }
    }
}

/// The smallest pane, in cells, that `dir` can be split out of — the geometry
/// [`split_fits_in`] demands, spelled for the refusal message so the person is
/// told the number they are short of rather than just "no".
#[must_use]
pub(crate) fn split_needs(dir: SplitDir) -> (u16, u16) {
    match dir {
        SplitDir::Vertical => (MIN_PANE_ROWS, 2 * MIN_PANE_COLS + 1),
        SplitDir::Horizontal => (2 * MIN_PANE_ROWS + 1, MIN_PANE_COLS),
    }
}

impl SplitDir {
    /// How this split reads in a sentence the user is shown.
    pub(crate) fn human(self) -> &'static str {
        match self {
            SplitDir::Vertical => "left/right",
            SplitDir::Horizontal => "top/bottom",
        }
    }
}

/// One pane divider's identity + geometry, produced by [`PaneTree::divider_at`] and
/// consumed by [`PaneTree::ratio_for_pointer`] / [`PaneTree::set_divider_ratio`] to
/// drive a drag-to-resize. The embedded logical hit names the exact split with a
/// root-to-node path and carries the divided-axis span used for pointer ratios.
#[derive(Clone, PartialEq, Debug)]
pub struct DividerHit {
    /// Canonical logical-pixel divider identity.
    logical: LogicalDividerHit,
    /// Which way the hit split divides (vertical divider = columns, horizontal =
    /// rows). Lets the GUI pick the resize cursor (E-W vs N-S).
    pub dir: SplitDir,
}

impl PaneTree {
    /// A new single-pane tab holding `session` (the day-one one-session-per-tab
    /// layout). Focus is that one session.
    #[must_use]
    pub fn new(session: u64) -> Self {
        PaneTree {
            root: PaneNode::Leaf(session),
            focus: session,
            zoomed: false,
        }
    }

    /// Toggle pane ZOOM: when on, [`compute_layout`](Self::compute_layout) returns
    /// only the focused pane filling the window. A no-op (stays off) for a
    /// single-pane tab. Returns the new zoom state.
    pub fn toggle_zoom(&mut self) -> bool {
        self.zoomed = !self.zoomed && self.len() > 1;
        self.zoomed
    }

    /// The currently FOCUSED session id (the pane keyboard input + the control
    /// socket target). Always a live leaf.
    #[must_use]
    pub fn focus(&self) -> u64 {
        self.focus
    }

    /// Move focus to `session` if it is a leaf in this tab. No-op (returns `false`)
    /// for an unknown id, so a stale focus request can never desync `focus`.
    pub fn set_focus(&mut self, session: u64) -> bool {
        if self.contains(session) {
            self.focus = session;
            true
        } else {
            false
        }
    }

    /// Whether `session` is a leaf anywhere in this tab.
    #[must_use]
    pub fn contains(&self, session: u64) -> bool {
        self.root.contains(session)
    }

    /// Every leaf session id in this tab, in left-to-right / top-to-bottom tree
    /// order. Used to resize/tear-down a whole tab's panes and to test round-trips.
    #[must_use]
    pub fn sessions(&self) -> Vec<u64> {
        self.root.leaves()
    }

    /// Visit every leaf session id WITHOUT allocating — [`Self::sessions`] for hot
    /// paths (the per-present output→present attribution walks every tab's leaves
    /// each frame; a `Vec` per tab per present would be steady-state churn).
    #[allow(
        dead_code,
        reason = "allocation-free compatibility traversal retained for downstream terminal adapters"
    )]
    pub fn for_each_session(&self, f: &mut impl FnMut(u64)) {
        fn walk(node: &PaneNode, f: &mut impl FnMut(u64)) {
            match node {
                PaneNode::Leaf(session) => f(*session),
                PaneNode::Split { first, second, .. } => {
                    walk(first, f);
                    walk(second, f);
                }
            }
        }
        walk(&self.root, f);
    }

    /// Project the terminal compatibility tree into another leaf identity while
    /// preserving its exact split directions and ratios.
    #[must_use]
    pub(crate) fn map_sessions<T>(&self, mut map: impl FnMut(u64) -> T) -> SplitTree<T> {
        self.root.map(&mut |session| map(*session))
    }

    #[must_use]
    pub(crate) fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Convert this tab's live pane tree into a serializable
    /// [`crate::restore::PaneLayout`] for session restore (RESTORE-1). `session_meta(id)`
    /// supplies each leaf's `(cwd, title)` — the GUI reads them off the live
    /// [`crate::Session`] — and the FOCUSED leaf is tagged so restore can re-focus it.
    /// Each leaf ALSO records its process-local `u64` session id (`local_id`). A durable
    /// cold-quit restore ignores it and respawns fresh sessions, re-assigning ids in tree
    /// order via [`rebuild`](Self::rebuild); a SEAMLESS-update boot uses it as the
    /// layout↔live-fd bridge, re-adopting the running shell into its exact original pane.
    #[must_use]
    pub fn to_layout(
        &self,
        session_meta: &impl Fn(u64) -> (Option<String>, String),
    ) -> crate::restore::PaneLayout {
        Self::node_to_layout(&self.root, self.focus, session_meta)
    }

    fn node_to_layout(
        node: &PaneNode,
        focus: u64,
        meta: &impl Fn(u64) -> (Option<String>, String),
    ) -> crate::restore::PaneLayout {
        use crate::restore::{PaneLayout, SplitKind};
        match node {
            PaneNode::Leaf(session) => {
                let (cwd, title) = meta(*session);
                PaneLayout::Leaf {
                    cwd,
                    title,
                    focused: *session == focus,
                    local_id: Some(*session),
                }
            }
            PaneNode::Split {
                axis,
                ratio,
                first,
                second,
            } => PaneLayout::Split {
                dir: match axis {
                    SplitAxis::Horizontal => SplitKind::Vertical,
                    SplitAxis::Vertical => SplitKind::Horizontal,
                },
                ratio: *ratio,
                first: Box::new(Self::node_to_layout(first, focus, meta)),
                second: Box::new(Self::node_to_layout(second, focus, meta)),
            },
        }
    }

    /// Rebuild a tab's pane tree from a persisted [`crate::restore::PaneLayout`],
    /// assigning the `fresh` session ids to leaves in tree order (the same
    /// left-to-right / top-to-bottom order [`sessions`](Self::sessions) yields).
    /// `fresh` MUST hold at least one id per leaf (`PaneLayout::leaf_count`); focus goes
    /// to the leaf the layout tagged `focused`, else the first leaf. Returns `None` when
    /// `fresh` is too short (the caller then falls back to a plain single-pane tab), so a
    /// short spawn list can never panic-index.
    #[must_use]
    pub fn rebuild(layout: &crate::restore::PaneLayout, fresh: &[u64]) -> Option<Self> {
        if fresh.len() < layout.leaf_count() {
            return None;
        }
        let mut next = 0usize;
        let mut focus = None;
        let root = Self::node_from_layout(layout, fresh, &mut next, &mut focus);
        // The layout tagged no focused leaf → default to the first spawned session.
        let focus = focus.unwrap_or(fresh[0]);
        Some(PaneTree {
            root,
            focus,
            zoomed: false,
        })
    }

    fn node_from_layout(
        layout: &crate::restore::PaneLayout,
        fresh: &[u64],
        next: &mut usize,
        focus: &mut Option<u64>,
    ) -> PaneNode {
        use crate::restore::{PaneLayout, SplitKind};
        match layout {
            PaneLayout::Leaf { focused, .. } => {
                let session = fresh[*next];
                *next += 1;
                if *focused && focus.is_none() {
                    *focus = Some(session);
                }
                PaneNode::Leaf(session)
            }
            PaneLayout::Split {
                dir,
                ratio,
                first,
                second,
            } => {
                // Recurse in first-then-second order so leaf ids match tree order.
                let first = Box::new(Self::node_from_layout(first, fresh, next, focus));
                let second = Box::new(Self::node_from_layout(second, fresh, next, focus));
                PaneNode::Split {
                    axis: match dir {
                        SplitKind::Vertical => SplitAxis::Horizontal,
                        SplitKind::Horizontal => SplitAxis::Vertical,
                    },
                    // A persisted ratio is clamped to the same ergonomic bounds a live
                    // divider drag is held to, so a hand-edited manifest can't collapse a
                    // pane to a sliver.
                    ratio: (*ratio).clamp(MIN_RATIO, MAX_RATIO),
                    first,
                    second,
                }
            }
        }
    }

    /// The number of live panes (leaves) in this tab. `1` for a fresh tab.
    #[must_use]
    pub fn len(&self) -> usize {
        self.root.len()
    }

    /// Split the FOCUSED leaf in `dir`, inserting `new_session` as the SECOND child
    /// (right/bottom) and keeping the original session as the first. Focus moves to
    /// the new pane (the standard "split and type in the new one" behavior). The
    /// split is 50/50. No-op (returns `false`) if the focused
    /// leaf somehow isn't found (it always is), leaving the tree untouched so the
    /// caller can drop the just-spawned session.
    ///
    /// TRUST anchor: this is the `Split` action of the ty-proven `pane_tree` machine
    /// (`pane_tree_model()`); the Tier-1 binding is `pane_tree_conformance`.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "pane_tree",
            action = "Split",
            project = "aterm_gui::pane_tree_conformance::project"
        )
    )]
    pub fn split_focused(&mut self, dir: SplitDir, new_session: u64) -> bool {
        let focus = self.focus;
        let axis = match dir {
            SplitDir::Vertical => SplitAxis::Horizontal,
            SplitDir::Horizontal => SplitAxis::Vertical,
        };
        if self.root.split_leaf(focus, axis, new_session) {
            self.focus = new_session;
            // A structural change exits zoom (the layout the user zoomed is gone).
            self.zoomed = false;
            true
        } else {
            false
        }
    }

    /// Close the FOCUSED pane (Cmd-W). See [`Self::close_pane`].
    pub fn close_focused(&mut self) -> CloseOutcome {
        self.close_pane(self.focus)
    }

    /// Close the pane holding `session` (the FOCUSED pane via [`Self::close_focused`],
    /// or any pane whose reader hit EOF). Returns the outcome:
    /// * [`CloseOutcome::Collapsed`] — the leaf was removed and its parent replaced
    ///   by the SIBLING sub-tree; focus re-seats on the nearest surviving leaf. The
    ///   tab (and window) keeps living.
    /// * [`CloseOutcome::LastPane`] — that leaf was the tab's ONLY pane, so the whole
    ///   tab should close (the caller removes the tab; the engine's last tab closing
    ///   exits the app, unchanged).
    ///
    /// Either way the returned `closed` is the session id that was removed, so the
    /// caller tears down exactly that session (closes its PTY master → its reader
    /// thread ends) and deregisters it. Every OTHER pane's session — and its reader
    /// thread — is untouched. An unknown id is treated as the focused pane (the
    /// caller only calls this for live panes; `close_session` filters unknown ids).
    ///
    /// TRUST anchor: the `CloseOutcome::Collapsed` arm is the `Close` action of the
    /// ty-proven `pane_tree` machine (`pane_tree_model()`) — the tree shrinks by one
    /// leaf and focus re-seats on a survivor IN RANGE. (`LastPane` is a tab-machine
    /// transition, out of this model's scope.) Tier-1 binding: `pane_tree_conformance`.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "pane_tree",
            action = "Close",
            project = "aterm_gui::pane_tree_conformance::project"
        )
    )]
    pub fn close_pane(&mut self, session: u64) -> CloseOutcome {
        let closed = if self.contains(session) {
            session
        } else {
            self.focus
        };
        // The only pane in the tab: nothing to collapse into; the tab closes.
        if matches!(self.root, PaneNode::Leaf(_)) {
            return CloseOutcome::LastPane { closed };
        }
        debug_assert_eq!(self.root.remove_leaf(closed), RemoveLeaf::Removed);
        // Re-seat focus on the nearest surviving leaf if the focused pane was the
        // one removed; otherwise focus stays where it was (a background pane's EOF
        // must not steal focus from the pane the user is typing in).
        if self.focus == closed || !self.contains(self.focus) {
            self.focus = self.root.first_leaf();
        }
        // A structural change exits zoom (the zoomed layout no longer applies).
        self.zoomed = false;
        CloseOutcome::Collapsed { closed }
    }

    /// Compute every visible pane's placement for a window of `rows`×`cols` cells.
    /// Returns one [`PaneRect`] per leaf, with 1-cell dividers reserved between
    /// split children (the gaps are NOT covered by any rect; the GUI paints them).
    /// A single-leaf tab yields exactly one rect covering the whole window — the
    /// non-split geometry, byte-identical to today.
    ///
    /// EVERY RETURNED RECT LIES INSIDE THE WINDOW GRID. That is the shrink rule of
    /// this module (see the module docs): a window dragged below what its open
    /// layout needs CLAMPS — origins pin to the last row/column, extents stop at
    /// the edge, each pane keeps at least one cell — and the TREE is never
    /// restructured, so growing back restores the exact geometry. Without the
    /// clamp a 1-row pane split in two put its second child at `row_off = 24` of a
    /// 24-row grid and the `.max(1)` below inflated it into a 1x1 phantom: a pane
    /// the compositor drew off the end of the world.
    #[must_use]
    pub fn compute_layout(&self, rows: u16, cols: u16) -> Vec<PaneRect> {
        self.layout_cells(rows, cols, self.zoomed)
    }

    /// [`Self::compute_layout`] with zoom named EXPLICITLY instead of read off the
    /// tree. The one caller that must override it is [`Self::focused_rect`]: a
    /// split un-zooms (see [`Self::split_focused`]), so the rectangle a split is
    /// about to divide is the UNZOOMED one, never the full window a zoomed pane
    /// currently occupies. Measuring the zoomed rect would let a split through on
    /// the strength of room the pane is about to give back — and that is exactly
    /// the shell-leaking split this module exists to refuse.
    #[must_use]
    fn layout_cells(&self, rows: u16, cols: u16, zoomed: bool) -> Vec<PaneRect> {
        let win_rows = rows.max(1);
        let win_cols = cols.max(1);
        // Zoomed: the focused pane alone fills the window (other panes are hidden
        // until unzoom). Single-pane tabs ignore the flag and take the normal path.
        if zoomed && self.len() > 1 {
            return vec![PaneRect {
                session: self.focus,
                row_off: 0,
                col_off: 0,
                rows: win_rows,
                cols: win_cols,
            }];
        }
        self.root
            .layout(
                LogicalRect::new(0.0, 0.0, f32::from(win_cols), f32::from(win_rows)),
                1.0,
                1.0,
            )
            .into_iter()
            .map(|leaf| {
                // The old terminal adapter promises nonzero grids even for a
                // pathologically tiny host. Logical app layout itself remains
                // bounded and may report zero extent in that impossible fit — so
                // the floor is applied here, and then pinned back INSIDE the grid.
                let row_off = (leaf.rect.origin.y.round() as u16).min(win_rows - 1);
                let col_off = (leaf.rect.origin.x.round() as u16).min(win_cols - 1);
                PaneRect {
                    session: leaf.value,
                    row_off,
                    col_off,
                    rows: (leaf.rect.size.height.round() as u16)
                        .max(1)
                        .min(win_rows - row_off),
                    cols: (leaf.rect.size.width.round() as u16)
                        .max(1)
                        .min(win_cols - col_off),
                }
            })
            .collect()
    }

    /// The FOCUSED pane's rect in a `rows`×`cols` window, measured on the UNZOOMED
    /// layout — the geometry a split would actually divide, since splitting exits
    /// zoom. `None` only if the focused leaf somehow isn't in the tree, and the
    /// caller must treat that as "unmeasurable", never as "fits".
    #[must_use]
    pub fn focused_rect(&self, rows: u16, cols: u16) -> Option<PaneRect> {
        self.layout_cells(rows, cols, false)
            .into_iter()
            .find(|r| r.session == self.focus)
    }

    /// Hit-test: the session id of the pane whose rect contains cell `(row, col)`,
    /// or `None` when the point lands on a divider / outside the grid. Used by
    /// click-to-focus.
    #[must_use]
    #[allow(
        dead_code,
        reason = "terminal compatibility hit-test retained while canonical ViewId routing is authoritative"
    )]
    pub fn pane_at(&self, row: u16, col: u16, rows: u16, cols: u16) -> Option<u64> {
        if self.zoomed && self.len() > 1 {
            return Some(self.focus);
        }
        self.root.leaf_at(
            LogicalPoint {
                x: f32::from(col),
                y: f32::from(row),
            },
            LogicalRect::new(0.0, 0.0, f32::from(cols.max(1)), f32::from(rows.max(1))),
            1.0,
            1.0,
        )
    }

    /// The session of the pane directly adjacent to the focused one in `dir`, or
    /// `None` when there is no pane on that side. Used by keyboard pane navigation
    /// (directional focus). A candidate must lie on the `dir` side of the focused
    /// rect AND share some perpendicular extent with it (so "left" of a tall pane
    /// only considers panes that overlap its rows); ties break toward the larger
    /// overlap, then the smaller offset (top-/left-most), for a stable choice.
    #[must_use]
    pub fn focus_neighbor(&self, dir: FocusDir, rows: u16, cols: u16) -> Option<u64> {
        if self.zoomed {
            return None;
        }
        let direction = match dir {
            FocusDir::Left => FocusDirection::Left,
            FocusDir::Right => FocusDirection::Right,
            FocusDir::Up => FocusDirection::Up,
            FocusDir::Down => FocusDirection::Down,
        };
        self.root.neighbor(
            self.focus,
            direction,
            LogicalRect::new(0.0, 0.0, f32::from(cols.max(1)), f32::from(rows.max(1))),
            1.0,
            1.0,
        )
    }

    /// Hit-test a DIVIDER: if cell `(row, col)` lands on a split's 1-cell divider
    /// line (the gap [`compute_layout`] reserves between a split's children, owned
    /// by no pane), return that divider's [`DividerHit`]; otherwise `None` (the cell
    /// is inside a pane or outside the grid). Used by drag-to-resize to start a
    /// divider drag. Zoomed or single-pane tabs have no draggable divider (the
    /// focused pane fills the window), so this is always `None` for them.
    #[must_use]
    pub fn divider_at(&self, row: u16, col: u16, rows: u16, cols: u16) -> Option<DividerHit> {
        if self.len() == 1 || (self.zoomed && self.len() > 1) {
            return None;
        }
        let logical = self.root.divider_at(
            LogicalPoint {
                x: f32::from(col),
                y: f32::from(row),
            },
            LogicalRect::new(0.0, 0.0, f32::from(cols.max(1)), f32::from(rows.max(1))),
            1.0,
            1.0,
        )?;
        let dir = match logical.axis {
            SplitAxis::Horizontal => SplitDir::Vertical,
            SplitAxis::Vertical => SplitDir::Horizontal,
        };
        Some(DividerHit { logical, dir })
    }

    /// Map a pointer at cell `(row, col)` to the new FIRST-child fraction for the
    /// split named by `hit`, BEFORE clamping (the raw geometric ratio). The pointer
    /// is projected onto the hit split's divided axis and divided by the split's
    /// splittable extent, so dropping the
    /// divider where the pointer is yields that ratio. Returns `None` only for a
    /// degenerate split too small to hold a divider. The caller passes the result to
    /// [`Self::set_divider_ratio`], which applies the `[MIN_RATIO, MAX_RATIO]` clamp.
    #[must_use]
    pub fn ratio_for_pointer(&self, hit: &DividerHit, row: u16, col: u16) -> Option<f32> {
        SplitTree::<u64>::ratio_for_pointer(
            &hit.logical,
            LogicalPoint {
                x: f32::from(col),
                y: f32::from(row),
            },
            1.0,
        )
    }

    /// Set the FIRST-child fraction of the split named by `hit` to `ratio`, clamped
    /// to `[MIN_RATIO, MAX_RATIO]` so neither pane collapses to a sliver. Returns
    /// `true` once the targeted split's `ratio` was written (it always is for a
    /// `hit` produced by [`Self::divider_at`] on the same tree); `false` if the path
    /// no longer names a split (e.g. the tree changed under a stale hit), leaving the
    /// tree untouched. A pure DATA edit — no structural change, so focus/zoom are
    /// preserved; the caller relays out + repaints.
    pub fn set_divider_ratio(&mut self, hit: &DividerHit, ratio: f32) -> bool {
        self.root.set_divider_ratio(&hit.logical, ratio)
    }
}

/// A direction for keyboard pane-focus navigation ([`PaneTree::focus_neighbor`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusDir {
    /// Move focus to the pane on the left.
    Left,
    /// Move focus to the pane on the right.
    Right,
    /// Move focus to the pane above.
    Up,
    /// Move focus to the pane below.
    Down,
}

/// The result of [`PaneTree::close_focused`]: which session was removed and
/// whether the tab survives (a sibling remained) or must close (it was the last
/// pane).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CloseOutcome {
    /// A sibling remained: the parent split collapsed into it. `closed` is the
    /// removed session.
    Collapsed { closed: u64 },
    /// The focused pane was the tab's only one; the whole tab should close.
    /// `closed` is that session.
    LastPane { closed: u64 },
}

impl CloseOutcome {
    /// The session id that was removed (to tear down + deregister), in both cases.
    #[must_use]
    pub fn closed(self) -> u64 {
        match self {
            CloseOutcome::Collapsed { closed } | CloseOutcome::LastPane { closed } => closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh single-pane tab lays out as ONE rect covering the whole window —
    /// the non-split geometry must be byte-identical to "no panes" (the whole
    /// no-regression contract rests on this).
    #[test]
    fn single_pane_fills_window() {
        let t = PaneTree::new(7);
        assert_eq!(t.len(), 1);
        assert_eq!(t.focus(), 7);
        let rects = t.compute_layout(24, 80);
        assert_eq!(rects.len(), 1);
        assert_eq!(
            rects[0],
            PaneRect {
                session: 7,
                row_off: 0,
                col_off: 0,
                rows: 24,
                cols: 80
            }
        );
    }

    /// RESTORE-1 round-trip: a multi-pane tab → `to_layout` → `rebuild` with fresh ids
    /// reconstructs the SAME tree shape (split dirs + ratios + tree order), re-focuses
    /// the tagged leaf, and carries each leaf's cwd/title through the layout. Also proves
    /// a short spawn list fails safe (`None`) instead of panic-indexing.
    #[test]
    fn layout_round_trips_with_fresh_ids() {
        // Tab shape: 1 | (2 stacked over 3), focus on session 3.
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Vertical, 2)); // focus → 2
        assert!(t.split_focused(SplitDir::Horizontal, 3)); // focus → 3
        assert_eq!(t.focus(), 3);
        let order = t.sessions(); // [1, 2, 3] in tree order

        // Serialize: each leaf's cwd = "/s{id}", title = "t{id}".
        let meta = |id: u64| (Some(format!("/s{id}")), format!("t{id}"));
        let layout = t.to_layout(&meta);
        assert_eq!(layout.leaf_count(), 3);

        // Rebuild with brand-new ids assigned in tree order.
        let fresh = [10u64, 11, 12];
        let rebuilt = PaneTree::rebuild(&layout, &fresh).expect("rebuild");
        assert_eq!(
            rebuilt.sessions(),
            vec![10, 11, 12],
            "same tree order, new ids"
        );
        // Focus followed the tagged leaf: old focus 3 was the 3rd leaf → fresh[2] = 12.
        assert_eq!(rebuilt.focus(), 12);

        // Re-serializing the rebuilt tree (mapping fresh ids back to the original meta)
        // yields a STRUCTURALLY identical layout — dirs, ratios, focus flag, per-leaf
        // cwd/title all preserved. The per-leaf `local_id` deliberately DIFFERS: it
        // tracks the live session id (now 10/11/12, not 1/2/3), the seamless-handoff
        // bridge — so it is normalized away for this structural comparison.
        let meta2 = |id: u64| {
            let orig = order[fresh.iter().position(|&f| f == id).unwrap()];
            (Some(format!("/s{orig}")), format!("t{orig}"))
        };
        fn structural(l: &crate::restore::PaneLayout) -> crate::restore::PaneLayout {
            use crate::restore::PaneLayout::{Leaf, Split};
            match l {
                Leaf {
                    cwd,
                    title,
                    focused,
                    ..
                } => Leaf {
                    cwd: cwd.clone(),
                    title: title.clone(),
                    focused: *focused,
                    local_id: None,
                },
                Split {
                    dir,
                    ratio,
                    first,
                    second,
                } => Split {
                    dir: *dir,
                    ratio: *ratio,
                    first: Box::new(structural(first)),
                    second: Box::new(structural(second)),
                },
            }
        }
        assert_eq!(structural(&rebuilt.to_layout(&meta2)), structural(&layout));
        // The rebuilt leaves DO carry the fresh live ids (the handoff bridge).
        assert_eq!(
            rebuilt.to_layout(&meta2).leaves()[0].local_id(),
            Some(10),
            "leaf local_id tracks the live session id"
        );

        // Too few fresh ids → None (caller falls back to a plain tab), never a panic.
        assert!(PaneTree::rebuild(&layout, &[10, 11]).is_none());
    }

    /// Cmd-D vertical split: two panes side by side, a 1-cell divider column
    /// between them, both full height. 80 cols → 79 splittable → 40 | divider | 39.
    #[test]
    fn vertical_split_geometry() {
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Vertical, 2));
        assert_eq!(t.focus(), 2, "focus follows the new pane");
        let mut rects = t.compute_layout(24, 80);
        rects.sort_by_key(|r| r.col_off);
        assert_eq!(rects.len(), 2);
        // Left pane: session 1, cols 0..40.
        assert_eq!(
            rects[0],
            PaneRect {
                session: 1,
                row_off: 0,
                col_off: 0,
                rows: 24,
                cols: 40
            }
        );
        // Right pane: session 2, starts after 40 + 1-cell divider = col 41, 39 wide.
        assert_eq!(
            rects[1],
            PaneRect {
                session: 2,
                row_off: 0,
                col_off: 41,
                rows: 24,
                cols: 39
            }
        );
        // The divider column (40) is covered by NO rect.
        assert!(
            rects
                .iter()
                .all(|r| !(r.col_off..r.col_off + r.cols).contains(&40))
        );
    }

    /// Cmd-Shift-D horizontal split: two panes stacked, a 1-cell divider row
    /// between them, both full width. 24 rows → 23 splittable → 12 | divider | 11.
    #[test]
    fn horizontal_split_geometry() {
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Horizontal, 2));
        let mut rects = t.compute_layout(24, 80);
        rects.sort_by_key(|r| r.row_off);
        assert_eq!(rects.len(), 2);
        assert_eq!(
            rects[0],
            PaneRect {
                session: 1,
                row_off: 0,
                col_off: 0,
                rows: 12,
                cols: 80
            }
        );
        assert_eq!(
            rects[1],
            PaneRect {
                session: 2,
                row_off: 13,
                col_off: 0,
                rows: 11,
                cols: 80
            }
        );
        assert!(
            rects
                .iter()
                .all(|r| !(r.row_off..r.row_off + r.rows).contains(&12))
        );
    }

    /// A 2x2 golden: vertical split, then horizontally split the (focused) right
    /// pane. Three leaves (1 | (2 / 3)), every rect disjoint, no divider overlap.
    #[test]
    fn nested_2x2_layout() {
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Vertical, 2)); // focus → 2 (right)
        assert!(t.split_focused(SplitDir::Horizontal, 3)); // split right → top 2 / bottom 3
        assert_eq!(t.len(), 3, "1 | (2 / 3) — three panes");
        assert_eq!(t.sessions(), vec![1, 2, 3]);
        let rects = t.compute_layout(24, 80);
        assert_eq!(rects.len(), 3);
        // Left pane spans full height.
        let left = rects.iter().find(|r| r.session == 1).unwrap();
        assert_eq!(left.rows, 24);
        // The two right panes share the right column band and stack.
        let top = rects.iter().find(|r| r.session == 2).unwrap();
        let bot = rects.iter().find(|r| r.session == 3).unwrap();
        assert_eq!(top.col_off, bot.col_off);
        assert_eq!(top.cols, bot.cols);
        assert!(top.row_off < bot.row_off);
        // No two rects overlap (cell-by-cell disjointness over the window).
        assert!(rects_disjoint(&rects));
    }

    /// Directional focus over the 2x2 golden `1 | (2 / 3)`: from each pane, the
    /// neighbor in each direction is the adjacent pane (or None at an edge).
    #[test]
    fn focus_neighbor_directions() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2); // 1 | 2, focus 2
        t.split_focused(SplitDir::Horizontal, 3); // 1 | (2 / 3), focus 3 (bottom-right)
        assert_eq!(t.focus(), 3);
        // From bottom-right (3): up→2, left→1, right/down→edge.
        assert_eq!(t.focus_neighbor(FocusDir::Up, 24, 80), Some(2));
        assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), Some(1));
        assert_eq!(t.focus_neighbor(FocusDir::Right, 24, 80), None);
        assert_eq!(t.focus_neighbor(FocusDir::Down, 24, 80), None);
        // From top-right (2): left→1, down→3, up/right→edge.
        assert!(t.set_focus(2));
        assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), Some(1));
        assert_eq!(t.focus_neighbor(FocusDir::Down, 24, 80), Some(3));
        assert_eq!(t.focus_neighbor(FocusDir::Up, 24, 80), None);
        assert_eq!(t.focus_neighbor(FocusDir::Right, 24, 80), None);
        // From the full-height left pane (1): right→a right-band pane; left→edge.
        assert!(t.set_focus(1));
        assert!(matches!(
            t.focus_neighbor(FocusDir::Right, 24, 80),
            Some(2 | 3)
        ));
        assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), None);
    }

    /// A single-pane tab has no neighbor in any direction.
    #[test]
    fn focus_neighbor_single_pane_none() {
        let t = PaneTree::new(1);
        for dir in [
            FocusDir::Left,
            FocusDir::Right,
            FocusDir::Up,
            FocusDir::Down,
        ] {
            assert_eq!(t.focus_neighbor(dir, 24, 80), None);
        }
    }

    /// Zoom shows only the focused pane full-window; unzoom restores; a single-pane
    /// tab can't zoom; and a structural change (split) exits zoom.
    #[test]
    fn zoom_focused_pane_fills_and_restores() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2); // 1 | 2, focus 2
        assert_eq!(t.compute_layout(24, 80).len(), 2);

        assert!(t.toggle_zoom(), "zoom on (multi-pane)");
        let z = t.compute_layout(24, 80);
        assert_eq!(z.len(), 1, "zoom shows only the focused pane");
        assert_eq!(z[0].session, 2);
        assert_eq!(
            (z[0].row_off, z[0].col_off, z[0].rows, z[0].cols),
            (0, 0, 24, 80)
        );

        assert!(!t.toggle_zoom(), "toggle off");
        assert_eq!(t.compute_layout(24, 80).len(), 2, "unzoom restores layout");

        // Single-pane tabs ignore zoom (stays off).
        let mut s = PaneTree::new(9);
        assert!(!s.toggle_zoom());
        assert_eq!(s.compute_layout(24, 80).len(), 1);

        // A split exits zoom: all panes show again.
        let mut x = PaneTree::new(1);
        x.split_focused(SplitDir::Vertical, 2);
        assert!(x.toggle_zoom());
        x.split_focused(SplitDir::Horizontal, 3);
        assert_eq!(
            x.compute_layout(24, 80).len(),
            3,
            "split exits zoom -> all panes shown"
        );
    }

    /// Split → close round-trip: closing the focused (new) pane collapses back to
    /// the original single pane, with focus re-seated on the survivor. The
    /// surviving session is untouched (its reader thread stays alive — the caller
    /// only tears down `closed`).
    #[test]
    fn split_then_close_round_trips() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2);
        assert_eq!(t.focus(), 2);
        let outcome = t.close_focused();
        assert_eq!(outcome, CloseOutcome::Collapsed { closed: 2 });
        assert_eq!(t.len(), 1, "collapsed back to one pane");
        assert_eq!(t.sessions(), vec![1], "the sibling survives");
        assert_eq!(t.focus(), 1, "focus re-seats on the survivor");
        // And the survivor lays out full-window again — byte-identical to fresh.
        assert_eq!(
            t.compute_layout(24, 80),
            PaneTree::new(1).compute_layout(24, 80)
        );
    }

    /// Closing the focused pane in a deeper tree collapses only its parent; the
    /// other branch is structurally untouched.
    #[test]
    fn close_collapses_only_parent() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2); // 1 | 2, focus 2
        t.split_focused(SplitDir::Horizontal, 3); // 1 | (2 / 3), focus 3
        // Close 3: its parent (the 2/3 split) collapses into 2; left branch (1) stays.
        let outcome = t.close_focused();
        assert_eq!(outcome, CloseOutcome::Collapsed { closed: 3 });
        assert_eq!(t.sessions(), vec![1, 2]);
        assert_eq!(t.focus(), 1, "focus re-seats on the left/top-most survivor");
        // The geometry is now exactly a 2-pane vertical split of 1 | 2.
        let mut expected = PaneTree::new(1);
        expected.split_focused(SplitDir::Vertical, 2);
        assert_eq!(t.compute_layout(24, 80), expected.compute_layout(24, 80));
    }

    /// Closing a BACKGROUND pane (reader EOF on a non-focused pane) collapses it
    /// but does NOT steal focus from the pane the user is typing in.
    #[test]
    fn close_background_pane_keeps_focus() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2); // 1 | 2, focus 2
        // Re-focus the left pane (session 1), then session 2's reader hits EOF.
        assert!(t.set_focus(1));
        let outcome = t.close_pane(2);
        assert_eq!(outcome, CloseOutcome::Collapsed { closed: 2 });
        assert_eq!(t.sessions(), vec![1]);
        assert_eq!(t.focus(), 1, "focus stays on the pane the user is using");
    }

    /// Closing the LAST pane signals the tab should close (LastPane), not a
    /// collapse — the caller removes the tab (and the last tab closing exits).
    #[test]
    fn close_last_pane_signals_tab_close() {
        let mut t = PaneTree::new(9);
        let outcome = t.close_focused();
        assert_eq!(outcome, CloseOutcome::LastPane { closed: 9 });
        assert_eq!(outcome.closed(), 9);
    }

    /// Focus → session mapping: click-to-focus picks the pane under the cell, and
    /// a divider cell maps to no pane (focus unchanged).
    #[test]
    fn pane_at_hit_test() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2); // 40 | divider(40) | 39
        // A cell in the left band → session 1.
        assert_eq!(t.pane_at(5, 10, 24, 80), Some(1));
        // A cell in the right band → session 2.
        assert_eq!(t.pane_at(5, 60, 24, 80), Some(2));
        // The divider column → no pane.
        assert_eq!(t.pane_at(5, 40, 24, 80), None);
        // Out of grid → no pane.
        assert_eq!(t.pane_at(99, 99, 24, 80), None);
        // set_focus follows the hit-test result.
        assert!(t.set_focus(1));
        assert_eq!(t.focus(), 1);
        assert!(!t.set_focus(999), "unknown id is rejected, focus unchanged");
        assert_eq!(t.focus(), 1);
    }

    /// `set_focus`/`contains` reject ids that aren't leaves in this tab.
    #[test]
    fn focus_only_live_leaves() {
        let t = PaneTree::new(3);
        assert!(t.contains(3));
        assert!(!t.contains(4));
    }

    /// A 2x1 golden across an odd width: 81 cols → 80 splittable → 40 | 40, divider
    /// at col 40. (Round-trips the even/odd split math.)
    #[test]
    fn vertical_split_odd_width() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2);
        let mut rects = t.compute_layout(24, 81);
        rects.sort_by_key(|r| r.col_off);
        assert_eq!(rects[0].cols, 40);
        assert_eq!(rects[1].col_off, 41);
        assert_eq!(rects[1].cols, 40);
    }

    /// A degenerate tiny window still yields one non-zero rect per pane (never a
    /// 0-extent pane the renderer would choke on).
    #[test]
    fn tiny_window_no_zero_panes() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2);
        for rect in t.compute_layout(1, 2) {
            assert!(
                rect.rows >= 1 && rect.cols >= 1,
                "no 0-extent pane: {rect:?}"
            );
        }
    }

    /// Divider drag on a vertical split: hitting the divider column yields a
    /// `DividerHit`, and `set_divider_ratio` moves the boundary — the left pane
    /// grows/shrinks while the geometry stays a valid 2-pane split.
    #[test]
    fn vertical_divider_drag_moves_boundary() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2); // 80 cols -> 40 | divider(40) | 39
        // The divider sits at column 40, any row.
        let hit = t.divider_at(5, 40, 24, 80).expect("divider hit at col 40");
        assert_eq!(hit.dir, SplitDir::Vertical);
        // A cell inside a pane is NOT on the divider.
        assert!(t.divider_at(5, 10, 24, 80).is_none());
        assert!(t.divider_at(5, 60, 24, 80).is_none());
        // Drag the divider left to column 20: ratio ~ 20/79.
        let ratio = t.ratio_for_pointer(&hit, 5, 20).expect("ratio for pointer");
        assert!((ratio - 20.0 / 79.0).abs() < 1e-3, "ratio {ratio}");
        assert!(t.set_divider_ratio(&hit, ratio));
        let mut rects = t.compute_layout(24, 80);
        rects.sort_by_key(|r| r.col_off);
        // Left pane shrank to ~20 cols; divider is now just past it.
        assert_eq!(rects[0].session, 1);
        assert_eq!(rects[0].cols, 20);
        assert_eq!(rects[1].col_off, 21);
        assert_eq!(rects[1].cols, 59); // 79 splittable - 20 first
    }

    /// `set_divider_ratio` CLAMPS to `[MIN_RATIO, MAX_RATIO]`: dragging the divider
    /// to (or past) an edge never collapses a pane to zero.
    #[test]
    fn divider_ratio_clamps() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2);
        let hit = t.divider_at(5, 40, 24, 80).expect("divider hit");
        // Drag hard left (ratio 0.0) clamps to MIN_RATIO; both panes survive.
        assert!(t.set_divider_ratio(&hit, 0.0));
        for r in t.compute_layout(24, 80) {
            assert!(r.cols >= 1, "no zero-width pane after min clamp: {r:?}");
        }
        let left = t
            .compute_layout(24, 80)
            .into_iter()
            .min_by_key(|r| r.col_off)
            .unwrap();
        // MIN_RATIO of 79 splittable ≈ 4 cells (round(0.05*79)=4), well above 0.
        assert!(left.cols >= (MIN_RATIO * 79.0).floor() as u16);
        // Drag hard right (ratio 1.0) clamps to MAX_RATIO; right pane survives.
        assert!(t.set_divider_ratio(&hit, 1.0));
        for r in t.compute_layout(24, 80) {
            assert!(r.cols >= 1, "no zero-width pane after max clamp: {r:?}");
        }
    }

    /// Headless hit-test → ratio mapping over a horizontal split: the divider row is
    /// found and a pointer maps to the proportional ratio along the rows.
    #[test]
    fn horizontal_divider_hit_and_ratio() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Horizontal, 2); // 24 rows -> 12 | divider(12) | 11
        let hit = t.divider_at(12, 30, 24, 80).expect("divider hit at row 12");
        assert_eq!(hit.dir, SplitDir::Horizontal);
        // Off the divider row → None.
        assert!(t.divider_at(3, 30, 24, 80).is_none());
        // Drag to row 6: ratio ~ 6/23.
        let ratio = t.ratio_for_pointer(&hit, 6, 30).expect("ratio");
        assert!((ratio - 6.0 / 23.0).abs() < 1e-3, "ratio {ratio}");
        assert!(t.set_divider_ratio(&hit, ratio));
        let mut rects = t.compute_layout(24, 80);
        rects.sort_by_key(|r| r.row_off);
        assert_eq!(rects[0].rows, 6);
        assert_eq!(rects[1].row_off, 7);
    }

    /// In a NESTED tree the hit-test targets the correct (inner) split: dragging the
    /// inner divider edits only that split, leaving the outer one untouched.
    #[test]
    fn nested_divider_targets_inner_split() {
        // 1 | (2 / 3): vertical outer, horizontal inner on the right band.
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2);
        t.split_focused(SplitDir::Horizontal, 3);
        let rects = t.compute_layout(24, 80);
        let top = rects.iter().find(|r| r.session == 2).unwrap();
        let bot = rects.iter().find(|r| r.session == 3).unwrap();
        // The inner (horizontal) divider row sits between panes 2 and 3, in the
        // right column band. It is the row just past pane 2's bottom.
        let div_row = top.row_off + top.rows;
        let probe_col = top.col_off + 1;
        let hit = t
            .divider_at(div_row, probe_col, 24, 80)
            .expect("inner divider hit");
        assert_eq!(hit.dir, SplitDir::Horizontal, "inner split is horizontal");
        // Record the OUTER (left) pane width; editing the inner split must not move it.
        let left_before = rects.iter().find(|r| r.session == 1).unwrap().cols;
        // Drag the inner divider up.
        let ratio = t
            .ratio_for_pointer(&hit, top.row_off + 2, probe_col)
            .unwrap();
        assert!(t.set_divider_ratio(&hit, ratio));
        let after = t.compute_layout(24, 80);
        let left_after = after.iter().find(|r| r.session == 1).unwrap().cols;
        assert_eq!(left_before, left_after, "outer split untouched");
        // Pane 2 (the inner first child) actually moved.
        let top_after = after.iter().find(|r| r.session == 2).unwrap();
        assert_ne!(top_after.rows, top.rows, "inner first child resized");
        // Sanity: still three disjoint panes.
        assert_eq!(after.len(), 3);
        assert!(rects_disjoint(&after));
        let _ = bot;
    }

    /// A single-pane (and a zoomed) tab has NO draggable divider.
    #[test]
    fn no_divider_when_single_or_zoomed() {
        let t = PaneTree::new(1);
        assert!(
            t.divider_at(5, 5, 24, 80).is_none(),
            "single pane: no divider"
        );
        let mut z = PaneTree::new(1);
        z.split_focused(SplitDir::Vertical, 2);
        assert!(
            z.divider_at(5, 40, 24, 80).is_some(),
            "split: has a divider"
        );
        z.toggle_zoom();
        assert!(
            z.divider_at(5, 40, 24, 80).is_none(),
            "zoomed: focused pane fills window, no divider"
        );
    }

    /// A stale `DividerHit` whose path no longer names a split is a safe no-op
    /// (`set_divider_ratio` returns false, the tree is untouched).
    #[test]
    fn stale_divider_hit_is_noop() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2);
        let hit = t.divider_at(5, 40, 24, 80).unwrap();
        // Collapse back to one pane — the split the hit named is gone.
        t.close_pane(2);
        let before = t.compute_layout(24, 80);
        assert!(!t.set_divider_ratio(&hit, 0.3), "stale path → no write");
        assert_eq!(before, t.compute_layout(24, 80), "tree untouched");
    }

    /// THE MINIMUM-PANE LAW, cross-checked against the layout engine it predicts.
    ///
    /// [`split_fits_in`] is a closed form (`extent - 1 >= 2 * min`, plus the
    /// perpendicular minimum); [`PaneTree::compute_layout`] is the real geometry.
    /// A closed form that drifts from its engine is worse than none — it would
    /// either refuse splits that fit or, far worse, admit the 1x1 ghosts this
    /// whole rule exists to stop. So: over EVERY window size in a wide sweep and
    /// both directions, actually split a fresh tree and demand the two answers
    /// agree exactly — `true` iff both resulting panes clear the minimum.
    #[test]
    fn pane_tree_min_fit_matches_the_layout_engine() {
        let mut checked_true = 0u32;
        let mut checked_false = 0u32;
        for rows in 0u16..=48 {
            for cols in 0u16..=120 {
                for dir in [SplitDir::Vertical, SplitDir::Horizontal] {
                    let mut t = PaneTree::new(1);
                    assert!(t.split_focused(dir, 2));
                    let rects = t.compute_layout(rows, cols);
                    // A window of `rows`x`cols` IS the focused pane's rect for a
                    // fresh single-pane tab, so this is the same question the
                    // caller asks before it spawns anything.
                    let engine_ok = rects
                        .iter()
                        .all(|r| r.rows >= MIN_PANE_ROWS && r.cols >= MIN_PANE_COLS);
                    let predicted = split_fits_in(dir, rows.max(1), cols.max(1));
                    assert_eq!(
                        predicted, engine_ok,
                        "{dir:?} split of {cols}x{rows}: predicate said {predicted}, \
                         layout produced {rects:?}"
                    );
                    if predicted {
                        checked_true += 1;
                    } else {
                        checked_false += 1;
                    }
                }
            }
        }
        // Both verdicts are actually exercised (a sweep that only ever says "no"
        // would pass a predicate hard-coded to false).
        assert!(
            checked_true > 0 && checked_false > 0,
            "sweep covers both verdicts"
        );
    }

    /// The exact boundary, spelled out: a left/right split needs
    /// `2 * MIN_PANE_COLS + 1` columns (two panes plus the divider) and
    /// `MIN_PANE_ROWS` rows — the perpendicular axis a split does NOT divide must
    /// ALREADY be usable, because splitting a 2-row pane sideways yields two
    /// 2-row panes and neither is a pane you can work in.
    #[test]
    fn split_fit_boundary_is_two_panes_plus_the_divider() {
        let need = 2 * MIN_PANE_COLS + 1;
        assert!(split_fits_in(SplitDir::Vertical, MIN_PANE_ROWS, need));
        assert!(!split_fits_in(SplitDir::Vertical, MIN_PANE_ROWS, need - 1));
        assert!(
            !split_fits_in(SplitDir::Vertical, MIN_PANE_ROWS - 1, need),
            "the undivided axis must already clear the minimum"
        );
        let need_rows = 2 * MIN_PANE_ROWS + 1;
        assert!(split_fits_in(
            SplitDir::Horizontal,
            need_rows,
            MIN_PANE_COLS
        ));
        assert!(!split_fits_in(
            SplitDir::Horizontal,
            need_rows - 1,
            MIN_PANE_COLS
        ));
        assert!(!split_fits_in(
            SplitDir::Horizontal,
            need_rows,
            MIN_PANE_COLS - 1
        ));
        // A 0x0 window can't underflow the `extent - 1` arithmetic into a "yes".
        assert!(!split_fits_in(SplitDir::Vertical, 0, 0));
        assert!(!split_fits_in(SplitDir::Horizontal, 0, 0));
        // And the refusal message's numbers ARE the boundary.
        assert_eq!(split_needs(SplitDir::Vertical), (MIN_PANE_ROWS, need));
        assert_eq!(
            split_needs(SplitDir::Horizontal),
            (need_rows, MIN_PANE_COLS)
        );
    }

    /// ZOOM CANNOT SMUGGLE A SPLIT THROUGH THE MINIMUM.
    ///
    /// A zoomed pane occupies the WHOLE window — but [`PaneTree::split_focused`]
    /// clears `zoomed`, so the rectangle a split actually divides is the pane's
    /// UNZOOMED one. If the gate measured the zoomed rect it would approve a split
    /// on the strength of room the pane is about to hand straight back, and mint
    /// exactly the unusable pane (with a live shell behind it) that this whole
    /// rule exists to refuse.
    ///
    /// So: [`PaneTree::focused_rect`] must report the unzoomed rect even while
    /// zoomed — and the proof that it is the RIGHT rect is that performing the
    /// split reproduces it as the union of the two children.
    #[test]
    fn focused_rect_ignores_zoom_because_splitting_unzooms() {
        // 40 columns is the size that makes the trap concrete: the WINDOW clears a
        // left/right split's 33-column bar, each HALF of it does not.
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Vertical, 2)); // 40 -> 20 | div | 19
        let unzoomed = t.focused_rect(24, 40).expect("focused leaf is in the tree");
        assert_eq!(
            (unzoomed.col_off, unzoomed.cols, unzoomed.rows),
            (21, 19, 24),
            "{unzoomed:?}"
        );

        assert!(t.toggle_zoom(), "the split tab zooms");
        // Zoom IS a presentation transform: the visible layout is the full window…
        assert_eq!(
            t.compute_layout(24, 40),
            vec![PaneRect {
                session: 2,
                row_off: 0,
                col_off: 0,
                rows: 24,
                cols: 40,
            }],
            "a zoomed pane fills the window on screen"
        );
        // …and THAT rect would clear the bar. This is the temptation.
        assert!(split_fits_in(SplitDir::Vertical, 24, 40));

        // But the rect a SPLIT would divide is unchanged by zoom, and it does not.
        let measured = t.focused_rect(24, 40).expect("focused leaf is in the tree");
        assert_eq!(
            measured, unzoomed,
            "zoom must not inflate the pane a split measures"
        );
        assert!(
            !split_fits_in(SplitDir::Vertical, measured.rows, measured.cols),
            "19 columns cannot hold two 16-column panes plus a divider"
        );

        // GROUND TRUTH: split anyway and the children tile exactly the rect
        // `focused_rect` reported — not the 40-column window zoom was showing.
        assert!(t.split_focused(SplitDir::Vertical, 3));
        assert!(!t.zoomed, "a split exits zoom");
        let children: Vec<_> = t
            .compute_layout(24, 40)
            .into_iter()
            .filter(|r| r.session == 2 || r.session == 3)
            .collect();
        let left = children.iter().map(|r| r.col_off).min().expect("two kids");
        let right = children
            .iter()
            .map(|r| r.col_off + r.cols)
            .max()
            .expect("two kids");
        assert_eq!(
            (left, right - left),
            (unzoomed.col_off, unzoomed.cols),
            "the split divided the UNZOOMED rect: {children:?}"
        );
    }

    /// THE SHRINK RULE: a window dragged below what its open layout needs CLAMPS.
    /// Every rect stays INSIDE the grid — this is the exact defect the splits
    /// audit found, where a 14-pane tab in a 24x80 window reported five panes at
    /// the identical OFF-GRID rect `24,79,1x1` (row 24 of rows 0..23). The tree is
    /// never restructured, so growing the window back restores the geometry byte
    /// for byte.
    #[test]
    fn shrinking_below_the_minimum_clamps_inside_the_grid() {
        // Build a deep tree the way the audit did: alternate the two directions.
        let mut t = PaneTree::new(1);
        for (i, dir) in [
            SplitDir::Vertical,
            SplitDir::Horizontal,
            SplitDir::Vertical,
            SplitDir::Horizontal,
            SplitDir::Vertical,
            SplitDir::Horizontal,
            SplitDir::Vertical,
        ]
        .into_iter()
        .enumerate()
        {
            assert!(t.split_focused(dir, i as u64 + 2));
        }
        assert_eq!(t.len(), 8);
        let roomy = t.compute_layout(48, 200);

        // Shrink far past anything this tree can hold, in a wide sweep of tiny
        // windows — including the degenerate 0 and 1 extents.
        for rows in 0u16..=8 {
            for cols in 0u16..=8 {
                let win_rows = rows.max(1);
                let win_cols = cols.max(1);
                let rects = t.compute_layout(rows, cols);
                assert_eq!(rects.len(), 8, "shrinking never drops a pane");
                for r in &rects {
                    assert!(r.rows >= 1 && r.cols >= 1, "no 0-extent pane: {r:?}");
                    assert!(
                        r.row_off < win_rows && r.col_off < win_cols,
                        "origin inside the {win_cols}x{win_rows} grid: {r:?}"
                    );
                    assert!(
                        r.row_off + r.rows <= win_rows && r.col_off + r.cols <= win_cols,
                        "extent inside the {win_cols}x{win_rows} grid: {r:?}"
                    );
                }
            }
        }

        // Reversible: the tree was only ever READ, so growing back is exact.
        assert_eq!(
            t.compute_layout(48, 200),
            roomy,
            "shrink → grow round-trips"
        );
        assert_eq!(t.len(), 8, "and no pane was collapsed on the way");
    }

    /// Helper: are all rects pairwise cell-disjoint? (No two panes claim the same
    /// cell — dividers are gaps owned by neither.)
    fn rects_disjoint(rects: &[PaneRect]) -> bool {
        for (i, a) in rects.iter().enumerate() {
            for b in &rects[i + 1..] {
                let rows_overlap = a.row_off < b.row_off + b.rows && b.row_off < a.row_off + a.rows;
                let cols_overlap = a.col_off < b.col_off + b.cols && b.col_off < a.col_off + a.cols;
                if rows_overlap && cols_overlap {
                    return false;
                }
            }
        }
        true
    }
}
