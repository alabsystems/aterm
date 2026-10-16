// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-tab split panes: a binary [`PaneTree`] of session ids (the ids
//! [`crate::Session::id`] routes `Wake`s with) over the generic [`SplitTree`], the
//! compatibility projection a terminal-only tab keeps of its canonical tree
//! ([`crate::tab_model::Tab`]). Pure geometry and bookkeeping, with no `Terminal`,
//! PTY or rendering, so the layout math and the split/close/focus state machine
//! test headless.
//!
//! A split reserves a one-cell divider between its children; `ratio` is the first
//! child's fraction of the extent minus the divider. A fresh split is 50/50.
//!
//! # The minimum pane
//!
//! 1. **Creation.** A split is refused unless both resulting panes are at least
//!    [`MIN_PANE_ROWS`] x [`MIN_PANE_COLS`] ([`split_fits_in`]). The caller
//!    ([`crate::App::split_focused_pane_in`]) asks before it spawns anything, and
//!    measures the unzoomed rect ([`PaneTree::focused_rect`]) since a split
//!    un-zooms.
//! 2. **Resize.** Shrinking clamps; it never restructures the tree.
//!    [`PaneTree::compute_layout`] keeps every rect inside the window grid with at
//!    least one cell per pane, so below the minimum panes may coincide on the last
//!    row or column, and growing back restores the exact geometry.
//!
//! [`crate::tab_model::Tab::visible_plan`] meets the same rule differently: its
//! children and dividers tile the parent exactly, keeping panes disjoint and
//! letting an extent reach zero
//! (`tab_model::tests::shrinking_a_canonical_plan_stays_inside_the_bounds_and_drops_no_leaf`).

use crate::tab_model::{
    FocusDirection, LogicalDividerHit, LogicalPoint, LogicalRect, RemoveLeaf, SplitAxis, SplitTree,
};

/// Which way a [`PaneNode::Split`] divides its rectangle, in the terminal
/// commands' spelling (named for the divider); the generic tree stores the
/// content axis.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SplitDir {
    /// Side by side across a vertical divider (Cmd-D): first child left, second
    /// right.
    Vertical,
    /// Stacked across a horizontal divider (Cmd-Shift-D): first child top, second
    /// bottom.
    Horizontal,
}

/// The generic split tree with session ids as leaves. Window code resolves stable
/// `ViewId`s through the `ViewStore` before reaching this adapter.
pub(crate) type PaneNode = SplitTree<u64>;

/// One visible pane's content placement in the window grid, in cell coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct PaneRect {
    /// The session occupying this rect.
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

/// A tab's pane layout: the binary tree and its focused leaf, the pane keyboard
/// input and the control socket target.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaneTree {
    root: PaneNode,
    /// The focused leaf's session id, always a leaf in `root`: it routes input and
    /// draws the solid cursor in exactly one pane.
    focus: u64,
    /// The focused pane fills the window ([`compute_layout`](Self::compute_layout)
    /// returns just it): presentation only over the unchanged tree. A single-pane
    /// tab ignores it.
    zoomed: bool,
}

/// The smallest first-child fraction: the bound a divider drag is clamped to in
/// the generic tree, applied here to a restored ratio so neither pane is a sliver.
const MIN_RATIO: f32 = 0.05;
/// The largest first-child fraction, mirroring [`MIN_RATIO`].
const MAX_RATIO: f32 = 0.95;

/// The fewest rows a pane the app creates may have: a prompt line, the line it
/// wraps onto, and one line of output.
pub(crate) const MIN_PANE_ROWS: u16 = 3;
/// The fewest columns a pane the app creates may have: the width at which a short
/// prompt and a short command still share one line.
pub(crate) const MIN_PANE_COLS: u16 = 16;

/// Can a pane allocation of `rows` x `cols` cells be split in `dir` and leave
/// both halves at least [`MIN_PANE_ROWS`] x [`MIN_PANE_COLS`] content cells,
/// with one subtab header row each?
///
/// Exact, not an estimate: a fresh split is 50/50 over the extent minus the
/// 1-cell divider, so the smaller half is `floor((extent - 1) / 2)` and clears
/// the minimum iff `extent - 1 >= 2 * min`. `rows` includes any existing header,
/// since the two new headers replace it. `pane_tree_min_fit_matches_the_layout_engine`
/// checks this against [`PaneTree::compute_layout`].
#[must_use]
pub(crate) fn split_fits_in(dir: SplitDir, rows: u16, cols: u16) -> bool {
    let min_rows = MIN_PANE_ROWS + 1; // each child also owns a subtab header
    match dir {
        SplitDir::Vertical => rows >= min_rows && cols.saturating_sub(1) >= 2 * MIN_PANE_COLS,
        SplitDir::Horizontal => cols >= MIN_PANE_COLS && rows.saturating_sub(1) >= 2 * min_rows,
    }
}

/// The smallest pane `(rows, cols)` that `dir` can be split out of, as
/// [`split_fits_in`] demands, for the refusal message.
#[must_use]
pub(crate) fn split_needs(dir: SplitDir) -> (u16, u16) {
    match dir {
        SplitDir::Vertical => (MIN_PANE_ROWS + 1, 2 * MIN_PANE_COLS + 1),
        SplitDir::Horizontal => (2 * (MIN_PANE_ROWS + 1) + 1, MIN_PANE_COLS),
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

/// A divider hit from [`PaneTree::divider_at`], for [`PaneTree::ratio_for_pointer`]
/// and [`PaneTree::set_divider_ratio`] in a drag-to-resize. The logical hit names
/// the split by its root-to-node path and carries the divided-axis span.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct DividerHit {
    /// Canonical logical-pixel divider identity.
    logical: LogicalDividerHit,
    /// Which way the hit split divides; picks the resize cursor (E-W vs N-S).
    pub dir: SplitDir,
}

impl PaneTree {
    /// A single-pane tab holding and focused on `session`.
    #[must_use]
    pub(crate) fn new(session: u64) -> Self {
        PaneTree {
            root: PaneNode::Leaf(session),
            focus: session,
            zoomed: false,
        }
    }

    /// Toggle zoom (see `zoomed`) and return the new state; a single-pane tab
    /// stays unzoomed.
    pub(crate) fn toggle_zoom(&mut self) -> bool {
        self.zoomed = !self.zoomed && self.len() > 1;
        self.zoomed
    }

    /// The focused session id; always a live leaf.
    #[must_use]
    pub(crate) fn focus(&self) -> u64 {
        self.focus
    }

    /// Move focus to `session` if it is a leaf in this tab; `false` (focus kept)
    /// for an unknown id.
    pub(crate) fn set_focus(&mut self, session: u64) -> bool {
        if self.contains(session) {
            self.focus = session;
            true
        } else {
            false
        }
    }

    /// Whether `session` is a leaf anywhere in this tab.
    #[must_use]
    pub(crate) fn contains(&self, session: u64) -> bool {
        self.root.contains(session)
    }

    /// Every leaf session id in this tab, in left-to-right / top-to-bottom tree
    /// order.
    #[must_use]
    pub(crate) fn sessions(&self) -> Vec<u64> {
        self.root.leaves()
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

    /// This tree as a [`crate::restore::PaneLayout`] for session restore, with each
    /// leaf's `(cwd, title)` from `session_meta` and the focused leaf tagged. Each
    /// leaf also records its session id (`local_id`): a cold-quit restore ignores it
    /// and respawns via [`rebuild`](Self::rebuild); a seamless-update boot uses it to
    /// re-adopt the running shell into its original pane.
    #[must_use]
    pub(crate) fn to_layout(
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
    /// assigning the `fresh` ids to leaves in tree order (as [`sessions`](Self::sessions)
    /// yields them). Focus goes to the leaf tagged `focused`, else the first. `None`
    /// when `fresh` holds fewer ids than the layout has leaves.
    #[must_use]
    pub(crate) fn rebuild(layout: &crate::restore::PaneLayout, fresh: &[u64]) -> Option<Self> {
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
                    // A hand-edited manifest cannot leave a sliver either.
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

    /// Split the focused leaf 50/50 in `dir`: the original session stays first,
    /// `new_session` becomes the second child (right/bottom) and takes focus.
    /// `false` (tree untouched, so the caller can drop the new session) if the
    /// focused leaf is not found.
    ///
    /// The `pane_tree` model's `Split` action, bound by `pane_tree_conformance`.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "pane_tree",
            action = "Split",
            project = "aterm_gui::pane_tree_conformance::project"
        )
    )]
    pub(crate) fn split_focused(&mut self, dir: SplitDir, new_session: u64) -> bool {
        let focus = self.focus;
        let axis = match dir {
            SplitDir::Vertical => SplitAxis::Horizontal,
            SplitDir::Horizontal => SplitAxis::Vertical,
        };
        if self.root.split_leaf(focus, axis, new_session) {
            self.focus = new_session;
            // A structural change exits zoom.
            self.zoomed = false;
            true
        } else {
            false
        }
    }

    /// Close the focused pane (Cmd-W). See [`Self::close_pane`].
    pub(crate) fn close_focused(&mut self) -> CloseOutcome {
        self.close_pane(self.focus)
    }

    /// Close the pane holding `session` (the focused one, or one whose reader hit
    /// EOF); an unknown id closes the focused pane. Returns:
    /// * [`CloseOutcome::Collapsed`]: the parent split is replaced by the sibling
    ///   sub-tree and focus re-seats on a survivor.
    /// * [`CloseOutcome::LastPane`]: it was the tab's only pane, so the tab closes.
    ///
    /// Either way `closed` is the session to tear down; every other pane's session
    /// is untouched.
    ///
    /// `Collapsed` is the `pane_tree` model's `Close` action (bound by `pane_tree_conformance`);
    /// `LastPane` is a tab-level transition outside the model.
    #[cfg_attr(
        test,
        aterm_spec::refines(
            machine = "pane_tree",
            action = "Close",
            project = "aterm_gui::pane_tree_conformance::project"
        )
    )]
    pub(crate) fn close_pane(&mut self, session: u64) -> CloseOutcome {
        let closed = if self.contains(session) {
            session
        } else {
            self.focus
        };
        // The only pane in the tab: nothing to collapse into; the tab closes.
        if matches!(self.root, PaneNode::Leaf(_)) {
            return CloseOutcome::LastPane { closed };
        }
        // Bound, never inside a `debug_assert*!` (a release build drops the argument). The
        // heir is read before the collapse erases the parent naming it.
        let heir = self.root.absorbing_neighbor(closed);
        let removed = self.root.remove_leaf(closed);
        debug_assert_eq!(removed, RemoveLeaf::Removed);
        // Focus moves only off the removed pane (a background pane's EOF never
        // steals it), and to the pane that absorbs the freed space, not the
        // tree's top-left `first_leaf()`: the pane that grew gets the keyboard.
        if self.focus == closed || !self.contains(self.focus) {
            self.focus = heir.unwrap_or_else(|| self.root.first_leaf());
        }
        // A structural change exits zoom.
        self.zoomed = false;
        CloseOutcome::Collapsed { closed }
    }

    /// Every visible pane's content [`PaneRect`] for a `rows`×`cols` window: one per
    /// leaf, a subtab header row above each split pane that keeps a content row, and
    /// 1-cell divider gaps between split children that no rect covers. A
    /// single-leaf tab gets one rect covering the whole window.
    ///
    /// Every rect lies inside the grid: below what the layout needs, origins pin to
    /// the last row/column and extents stop at the edge, each pane keeping at least
    /// one cell (see the module docs).
    #[must_use]
    pub(crate) fn compute_layout(&self, rows: u16, cols: u16) -> Vec<PaneRect> {
        let mut rects = self.layout_cells(rows, cols, self.zoomed);
        if rects.len() > 1 {
            for rect in &mut rects {
                if crate::tab_model::pane_header_rect(LogicalRect::new(
                    f32::from(rect.col_off),
                    f32::from(rect.row_off),
                    f32::from(rect.cols),
                    f32::from(rect.rows),
                ))
                .is_some()
                {
                    rect.row_off += 1;
                    rect.rows -= 1;
                }
            }
        }
        rects
    }

    /// Outer pane allocations before subtab header rows are reserved. Zoom is an
    /// argument because [`Self::focused_rect`] measures unzoomed: a split un-zooms,
    /// so a zoomed pane's full-window rect would admit a split that does not fit.
    #[must_use]
    fn layout_cells(&self, rows: u16, cols: u16, zoomed: bool) -> Vec<PaneRect> {
        let win_rows = rows.max(1);
        let win_cols = cols.max(1);
        // Zoomed: the focused pane alone fills the window. A single-pane tab
        // ignores the flag.
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
                // This adapter promises nonzero grids, but the logical layout may
                // report a zero extent in an impossible fit: apply the floor, then
                // pin the rect back inside the grid.
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

    /// The focused pane's outer allocation, header included, in a `rows`×`cols`
    /// window, measured unzoomed: a split divides this allocation. `None` (focus not
    /// in the tree) means unmeasurable, never "fits".
    #[must_use]
    pub(crate) fn focused_rect(&self, rows: u16, cols: u16) -> Option<PaneRect> {
        self.layout_cells(rows, cols, false)
            .into_iter()
            .find(|r| r.session == self.focus)
    }

    /// The pane adjacent to the focused one in `dir`, for keyboard pane navigation;
    /// `None` at an edge or while zoomed. A candidate lies on the `dir` side and
    /// shares some perpendicular extent with the focused rect; the nearest wins,
    /// ties going to the larger overlap, then the smaller (top-/left-most) offset.
    #[must_use]
    pub(crate) fn focus_neighbor(&self, dir: FocusDir, rows: u16, cols: u16) -> Option<u64> {
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

    /// The divider whose 1-cell gap (reserved by [`compute_layout`], owned by no
    /// pane) holds cell `(row, col)`, for drag-to-resize. Always `None` for a
    /// single-pane or zoomed tab.
    #[must_use]
    pub(crate) fn divider_at(
        &self,
        row: u16,
        col: u16,
        rows: u16,
        cols: u16,
    ) -> Option<DividerHit> {
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

    /// The unclamped first-child fraction that puts `hit`'s divider under a pointer
    /// at cell `(row, col)`; [`Self::set_divider_ratio`] clamps it. `None` for a
    /// split too small to hold a divider.
    #[must_use]
    pub(crate) fn ratio_for_pointer(&self, hit: &DividerHit, row: u16, col: u16) -> Option<f32> {
        SplitTree::<u64>::ratio_for_pointer(
            &hit.logical,
            LogicalPoint {
                x: f32::from(col),
                y: f32::from(row),
            },
            1.0,
        )
    }

    /// Set the first-child fraction of the split named by `hit` to `ratio`, clamped
    /// to `[MIN_RATIO, MAX_RATIO]`. `false` (tree untouched) when the path no
    /// longer names a split, as for a stale hit. A data edit: focus and zoom keep.
    pub(crate) fn set_divider_ratio(&mut self, hit: &DividerHit, ratio: f32) -> bool {
        self.root.set_divider_ratio(&hit.logical, ratio)
    }
}

/// A direction for keyboard pane-focus navigation ([`PaneTree::focus_neighbor`]).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FocusDir {
    /// Move focus to the pane on the left.
    Left,
    /// Move focus to the pane on the right.
    Right,
    /// Move focus to the pane above.
    Up,
    /// Move focus to the pane below.
    Down,
}

/// The result of [`PaneTree::close_pane`]: the removed session, and whether the
/// tab survives (a sibling remained) or must close (it was the last pane).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CloseOutcome {
    /// A sibling remained: the parent split collapsed into it. `closed` is the
    /// removed session.
    Collapsed { closed: u64 },
    /// The pane was the tab's only one; the whole tab should close. `closed` is
    /// that session.
    LastPane { closed: u64 },
}

impl CloseOutcome {
    /// The session id that was removed (to tear down + deregister), in both cases.
    #[must_use]
    pub(crate) fn closed(self) -> u64 {
        match self {
            CloseOutcome::Collapsed { closed } | CloseOutcome::LastPane { closed } => closed,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A fresh single-pane tab lays out as one rect covering the whole window.
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

    /// A multi-pane tab → `to_layout` → `rebuild` with fresh ids keeps the tree
    /// shape (dirs, ratios, order), the focused leaf and each leaf's cwd/title; a
    /// short spawn list fails safe (`None`).
    #[test]
    fn layout_round_trips_with_fresh_ids() {
        // Tab shape: 1 | (2 stacked over 3), focus on session 3.
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Vertical, 2)); // focus → 2
        assert!(t.split_focused(SplitDir::Horizontal, 3)); // focus → 3
        assert_eq!(t.focus(), 3);
        let order = t.sessions(); // [1, 2, 3] in tree order

        let meta = |id: u64| (Some(format!("/s{id}")), format!("t{id}"));
        let layout = t.to_layout(&meta);
        assert_eq!(layout.leaf_count(), 3);

        let fresh = [10u64, 11, 12];
        let rebuilt = PaneTree::rebuild(&layout, &fresh).expect("rebuild");
        assert_eq!(
            rebuilt.sessions(),
            vec![10, 11, 12],
            "same tree order, new ids"
        );
        // Focus followed the tagged leaf: old focus 3 was the 3rd leaf → fresh[2] = 12.
        assert_eq!(rebuilt.focus(), 12);

        // Re-serializing the rebuilt tree (fresh ids mapped back to the original
        // meta) yields a structurally identical layout. `local_id` differs (it
        // tracks the live id, now 10/11/12), so it is normalized away here.
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
        // The rebuilt leaves carry the fresh live ids.
        assert_eq!(
            rebuilt.to_layout(&meta2).leaves()[0].local_id(),
            Some(10),
            "leaf local_id tracks the live session id"
        );

        // Too few fresh ids → None (caller falls back to a plain tab), never a panic.
        assert!(PaneTree::rebuild(&layout, &[10, 11]).is_none());
    }

    /// Cmd-D vertical split: two panes side by side across a 1-cell divider column, each
    /// under a header row. 80 cols → 79 splittable → 40 | divider | 39.
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
                row_off: 1,
                col_off: 0,
                rows: 23,
                cols: 40
            }
        );
        // Right pane: session 2, starts after 40 + 1-cell divider = col 41, 39 wide.
        assert_eq!(
            rects[1],
            PaneRect {
                session: 2,
                row_off: 1,
                col_off: 41,
                rows: 23,
                cols: 39
            }
        );
        // No rect covers the divider column (40).
        assert!(
            rects
                .iter()
                .all(|r| !(r.col_off..r.col_off + r.cols).contains(&40))
        );
    }

    /// Cmd-Shift-D horizontal split: two full-width panes stacked over a 1-cell divider row,
    /// each under a header row. 24 rows → 23 splittable → 12 | divider | 11.
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
                row_off: 1,
                col_off: 0,
                rows: 11,
                cols: 80
            }
        );
        assert_eq!(
            rects[1],
            PaneRect {
                session: 2,
                row_off: 14,
                col_off: 0,
                rows: 10,
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
        let left = rects.iter().find(|r| r.session == 1).unwrap();
        assert_eq!(left.rows, 23);
        let top = rects.iter().find(|r| r.session == 2).unwrap();
        let bot = rects.iter().find(|r| r.session == 3).unwrap();
        assert_eq!(top.col_off, bot.col_off);
        assert_eq!(top.cols, bot.cols);
        assert!(top.row_off < bot.row_off);
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

    /// Split, then close the new pane: back to the original single pane, focused.
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
        // The survivor lays out as a fresh tab does.
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
        assert_eq!(
            t.focus(),
            2,
            "focus re-seats on the survivor that ABSORBS the freed space (2, the \
             pane above the closed one), not on the tree's top-left leaf"
        );
        let mut expected = PaneTree::new(1);
        expected.split_focused(SplitDir::Vertical, 2);
        assert_eq!(t.compute_layout(24, 80), expected.compute_layout(24, 80));
    }

    /// Closing the focused pane focuses the survivor that absorbs its cells, not
    /// the tree's `first_leaf()`: the pane that grows into the hole takes the next
    /// keystroke. Each shape names that survivor.
    #[test]
    fn close_focuses_the_pane_that_absorbs_the_freed_space() {
        // Build a tree with `build`, focus the pane about to be closed, and hand
        // back the tree plus the layout it had while that pane still existed.
        fn tree(build: &dyn Fn(&mut PaneTree), focus_on: u64) -> (PaneTree, Vec<PaneRect>) {
            let mut t = PaneTree::new(1);
            build(&mut t);
            assert!(t.set_focus(focus_on), "the pane to close must exist");
            let before = t.compute_layout(40, 120);
            (t, before)
        }
        let rect = |layout: &[PaneRect], session: u64| -> PaneRect {
            *layout
                .iter()
                .find(|r| r.session == session)
                .expect("laid-out pane")
        };

        // 1 | (2 / 3), close the bottom-right pane: `first_leaf()` would answer 1,
        // the far column, which does not grow.
        let (mut t, before) = tree(
            &|t| {
                t.split_focused(SplitDir::Vertical, 2); // 1 | 2
                t.split_focused(SplitDir::Horizontal, 3); // 1 | (2 / 3)
            },
            3,
        );
        let closed_rect = rect(&before, 3);
        let survivor_before = rect(&before, 2);
        assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 3 });
        assert_eq!(t.focus(), 2, "1 | (2 / 3), close 3 -> focus 2");
        let after = t.compute_layout(40, 120);
        let survivor_after = rect(&after, 2);
        assert!(
            survivor_after.rows > survivor_before.rows,
            "the pane that gets focus is the one that GREW: {survivor_before:?} -> \
             {survivor_after:?}"
        );
        assert!(
            survivor_after.row_off <= closed_rect.row_off
                && survivor_after.row_off + survivor_after.rows
                    >= closed_rect.row_off + closed_rect.rows
                && survivor_after.col_off <= closed_rect.col_off,
            "and it now covers the closed pane's cells: {closed_rect:?} inside \
             {survivor_after:?}"
        );

        // Nested sibling: 1 | ((2 | 4) / 3), close 3. The sub-tree `(2 | 4)` takes
        // the space; the leaf against the seam is its last one, 4.
        let (mut t, _) = tree(
            &|t| {
                t.split_focused(SplitDir::Vertical, 2); // 1 | 2
                t.split_focused(SplitDir::Horizontal, 3); // 1 | (2 / 3)
                assert!(t.set_focus(2));
                t.split_focused(SplitDir::Vertical, 4); // 1 | ((2 | 4) / 3)
            },
            3,
        );
        assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 3 });
        assert_eq!(
            t.focus(),
            4,
            "1 | ((2 | 4) / 3), close 3 -> focus 4, the leaf against the seam"
        );

        // First-child close, mirrored: 1 / (2 | 3), close 2. The sibling lies to
        // the right, so the near leaf is its first one, 3, not 1 two rows up.
        let (mut t, _) = tree(
            &|t| {
                t.split_focused(SplitDir::Horizontal, 2); // 1 / 2
                t.split_focused(SplitDir::Vertical, 3); // 1 / (2 | 3)
            },
            2,
        );
        assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 2 });
        assert_eq!(t.focus(), 3, "1 / (2 | 3), close 2 -> focus 3");

        // Where the absorbing neighbour is also the tree's first leaf: (1 / 3) | 2,
        // close 3 -> 1.
        let (mut t, _) = tree(
            &|t| {
                t.split_focused(SplitDir::Vertical, 2); // 1 | 2
                assert!(t.set_focus(1));
                t.split_focused(SplitDir::Horizontal, 3); // (1 / 3) | 2
            },
            3,
        );
        assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 3 });
        assert_eq!(t.focus(), 1, "(1 / 3) | 2, close 3 -> focus 1");
    }

    /// Closing a background pane (reader EOF) collapses it without taking focus.
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

    /// Closing the last pane reports `LastPane` (the tab closes), not a collapse.
    #[test]
    fn close_last_pane_signals_tab_close() {
        let mut t = PaneTree::new(9);
        let outcome = t.close_focused();
        assert_eq!(outcome, CloseOutcome::LastPane { closed: 9 });
        assert_eq!(outcome.closed(), 9);
    }

    /// `set_focus` moves focus to a live leaf and rejects an unknown id.
    #[test]
    fn set_focus_follows_live_leaves() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2);
        assert!(t.set_focus(1));
        assert_eq!(t.focus(), 1);
        assert!(!t.set_focus(999), "unknown id is rejected, focus unchanged");
        assert_eq!(t.focus(), 1);
    }

    /// `contains` names only this tab's leaves.
    #[test]
    fn focus_only_live_leaves() {
        let t = PaneTree::new(3);
        assert!(t.contains(3));
        assert!(!t.contains(4));
    }

    /// An odd width: 81 cols → 80 splittable → 40 | 40, divider at col 40.
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

    /// A degenerate tiny window still yields a non-zero rect per pane.
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

    #[test]
    fn subtab_content_geometry_matches_the_canonical_plan() {
        use crate::tab_model::{LeafSizing, LogicalSize, Tab, TabId, TabPresentation, ViewId};

        let mut tree = PaneTree::new(1);
        tree.split_focused(SplitDir::Vertical, 2);
        tree.split_focused(SplitDir::Horizontal, 3);
        let tab = Tab::from_root(
            TabId::from_stored(1),
            tree.map_sessions(ViewId::from_stored),
            ViewId::from_stored(tree.focus()),
            false,
            TabPresentation::terminal("shell"),
        );
        for rows in 4..=48 {
            for cols in [8, 33, 80, 121] {
                let mut plan = tab.visible_plan(
                    LogicalRect::new(0.0, 0.0, f32::from(cols), f32::from(rows)),
                    1.0,
                    |_| LeafSizing::new(LogicalSize::new(2.0, 1.0), LogicalSize::new(80.0, 24.0)),
                );
                plan.reserve_pane_headers();
                for (body, leaf) in tree.compute_layout(rows, cols).iter().zip(&plan.leaves) {
                    assert_eq!(
                        LogicalRect::new(
                            f32::from(body.col_off),
                            f32::from(body.row_off),
                            f32::from(body.cols),
                            f32::from(body.rows),
                        ),
                        leaf.rect,
                        "canonical sizing/input and compatibility blits must agree at {cols}x{rows}"
                    );
                }
            }
        }
    }

    /// A hit on a vertical split's divider column yields a `DividerHit`, and
    /// `set_divider_ratio` moves the boundary.
    #[test]
    fn vertical_divider_drag_moves_boundary() {
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2); // 80 cols -> 40 | divider(40) | 39
        // The divider sits at column 40, any row.
        let hit = t.divider_at(5, 40, 24, 80).expect("divider hit at col 40");
        assert_eq!(hit.dir, SplitDir::Vertical);
        // A cell inside a pane is not on the divider.
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

    /// `set_divider_ratio` clamps to `[MIN_RATIO, MAX_RATIO]`: dragging the divider
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
        assert!(t.divider_at(3, 30, 24, 80).is_none());
        // Drag to row 6: ratio ~ 6/23.
        let ratio = t.ratio_for_pointer(&hit, 6, 30).expect("ratio");
        assert!((ratio - 6.0 / 23.0).abs() < 1e-3, "ratio {ratio}");
        assert!(t.set_divider_ratio(&hit, ratio));
        let mut rects = t.compute_layout(24, 80);
        rects.sort_by_key(|r| r.row_off);
        assert_eq!(rects[0].rows, 5);
        assert_eq!(rects[1].row_off, 8);
    }

    /// In a nested tree the hit-test targets the inner split: dragging its divider
    /// leaves the outer split untouched.
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
        // Editing the inner split must not move the outer (left) pane.
        let left_before = rects.iter().find(|r| r.session == 1).unwrap().cols;
        let ratio = t
            .ratio_for_pointer(&hit, top.row_off + 2, probe_col)
            .unwrap();
        assert!(t.set_divider_ratio(&hit, ratio));
        let after = t.compute_layout(24, 80);
        let left_after = after.iter().find(|r| r.session == 1).unwrap().cols;
        assert_eq!(left_before, left_after, "outer split untouched");
        let top_after = after.iter().find(|r| r.session == 2).unwrap();
        assert_ne!(top_after.rows, top.rows, "inner first child resized");
        assert_eq!(after.len(), 3);
        assert!(rects_disjoint(&after));
        let _ = bot;
    }

    /// A single-pane (and a zoomed) tab has no draggable divider.
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

    /// [`split_fits_in`]'s closed form agrees with [`PaneTree::compute_layout`]
    /// over every window size in a sweep, both directions: `true` iff both panes
    /// of a real split clear the minimum.
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
                    // For a fresh single-pane tab the window is the focused pane's rect:
                    // the question the caller asks before it spawns anything.
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
        // Both verdicts occur, so a predicate hard-coded to `false` would fail.
        assert!(
            checked_true > 0 && checked_false > 0,
            "sweep covers both verdicts"
        );
    }

    /// The exact boundary: a left/right split needs `2 * MIN_PANE_COLS + 1` columns (two panes
    /// plus the divider) and `MIN_PANE_ROWS + 1` rows (content plus each child's header row).
    #[test]
    fn split_fit_boundary_is_two_panes_plus_the_divider() {
        let pane_rows = MIN_PANE_ROWS + 1;
        let need = 2 * MIN_PANE_COLS + 1;
        assert!(split_fits_in(SplitDir::Vertical, pane_rows, need));
        assert!(!split_fits_in(SplitDir::Vertical, pane_rows, need - 1));
        assert!(
            !split_fits_in(SplitDir::Vertical, pane_rows - 1, need),
            "the undivided axis must already clear the minimum"
        );
        let need_rows = 2 * pane_rows + 1;
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
        // The refusal message's numbers are the boundary.
        assert_eq!(split_needs(SplitDir::Vertical), (pane_rows, need));
        assert_eq!(
            split_needs(SplitDir::Horizontal),
            (need_rows, MIN_PANE_COLS)
        );
    }

    /// [`PaneTree::focused_rect`] reports the unzoomed rect while zoomed: a split
    /// clears zoom and divides that rect, as the split's two children show.
    #[test]
    fn focused_rect_ignores_zoom_because_splitting_unzooms() {
        // At 40 columns the window clears a left/right split's 33-column bar; each half does not.
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Vertical, 2)); // 40 -> 20 | div | 19
        let unzoomed = t.focused_rect(24, 40).expect("focused leaf is in the tree");
        assert_eq!(
            (unzoomed.col_off, unzoomed.cols, unzoomed.rows),
            (21, 19, 24),
            "{unzoomed:?}"
        );

        assert!(t.toggle_zoom(), "the split tab zooms");
        // Zoom is a presentation transform: the visible layout is the full window…
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
        // …and that rect would clear the bar.
        assert!(split_fits_in(SplitDir::Vertical, 24, 40));

        // But the rect a split would divide is unchanged by zoom, and it does not.
        let measured = t.focused_rect(24, 40).expect("focused leaf is in the tree");
        assert_eq!(
            measured, unzoomed,
            "zoom must not inflate the pane a split measures"
        );
        assert!(
            !split_fits_in(SplitDir::Vertical, measured.rows, measured.cols),
            "19 columns cannot hold two 16-column panes plus a divider"
        );

        // Split anyway: the children tile exactly the rect `focused_rect`
        // reported, not the 40-column window zoom was showing.
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

    /// A window shrunk below what its layout needs clamps: every rect stays inside
    /// the grid, and the untouched tree restores the exact geometry on regrowth.
    #[test]
    fn shrinking_below_the_minimum_clamps_inside_the_grid() {
        // A deep tree: alternate the two directions.
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

        // The tree was only read, so growing back is exact.
        assert_eq!(
            t.compute_layout(48, 200),
            roomy,
            "shrink → grow round-trips"
        );
        assert_eq!(t.len(), 8, "and no pane was collapsed on the way");
    }

    /// Whether no two rects claim the same cell (dividers belong to neither).
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
