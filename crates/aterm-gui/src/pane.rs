// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! In-tab binary split-pane tree, cell-grid layout, focus navigation, and divider resizing.

use crate::tab_model::{
    FocusDirection, LogicalDividerHit, LogicalPoint, LogicalRect, RemoveLeaf, SplitAxis, SplitTree,
};

/// Axis along which a [`PaneNode::Split`] divides its rectangle.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum SplitDir {
    /// Side-by-side split across a vertical divider (`Cmd-D`).
    Vertical,
    /// Top-and-bottom split across a horizontal divider (`Cmd-Shift-D`).
    Horizontal,
}

/// Split tree with session IDs at the leaves.
pub(crate) type PaneNode = SplitTree<u64>;

/// Cell-grid bounding rectangle for one visible pane.
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

/// Binary split tree of session IDs with focus and zoom state for a tab.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct PaneTree {
    root: PaneNode,
    /// The focused leaf's session id, always a leaf in `root`: it routes input and
    /// draws the solid cursor in exactly one pane.
    focus: u64,
    /// The focused pane fills the window ([`compute_layout`](Self::compute_layout)
    /// returns just it): presentation only over the unchanged tree.
    zoomed: bool,
}

/// The smallest first-child fraction: the bound a divider drag is clamped to in
/// the generic tree, applied here to a restored ratio so neither pane is a sliver.
const MIN_RATIO: f32 = 0.05;
/// The largest first-child fraction, mirroring [`MIN_RATIO`].
const MAX_RATIO: f32 = 0.95;

/// Minimum height in rows for a newly split pane.
pub(crate) const MIN_PANE_ROWS: u16 = 3;
/// Minimum width in columns for a newly split pane.
pub(crate) const MIN_PANE_COLS: u16 = 16;

/// Whether a `rows` × `cols` allocation can be split in `dir` while keeping both children at least `MIN_PANE_ROWS` × `MIN_PANE_COLS`.
#[must_use]
pub(crate) fn split_fits_in(dir: SplitDir, rows: u16, cols: u16) -> bool {
    let min_rows = MIN_PANE_ROWS + 1; // each child also owns a subtab header
    match dir {
        SplitDir::Vertical => rows >= min_rows && cols.saturating_sub(1) >= 2 * MIN_PANE_COLS,
        SplitDir::Horizontal => cols >= MIN_PANE_COLS && rows.saturating_sub(1) >= 2 * min_rows,
    }
}

/// Minimum `(rows, cols)` required to split a pane in `dir`.
#[must_use]
pub(crate) fn split_needs(dir: SplitDir) -> (u16, u16) {
    match dir {
        SplitDir::Vertical => (MIN_PANE_ROWS + 1, 2 * MIN_PANE_COLS + 1),
        SplitDir::Horizontal => (2 * (MIN_PANE_ROWS + 1) + 1, MIN_PANE_COLS),
    }
}

impl SplitDir {
    /// User-facing name for this split direction.
    pub(crate) fn human(self) -> &'static str {
        match self {
            SplitDir::Vertical => "left/right",
            SplitDir::Horizontal => "top/bottom",
        }
    }
}

/// Hit-test result identifying a draggable split divider.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct DividerHit {
    /// Canonical logical-pixel divider identity.
    logical: LogicalDividerHit,
    /// Which way the hit split divides; picks the resize cursor (E-W vs N-S).
    pub dir: SplitDir,
}

impl PaneTree {
    /// Create a single-pane tree focused on `session`.
    #[must_use]
    pub(crate) fn new(session: u64) -> Self {
        PaneTree {
            root: PaneNode::Leaf(session),
            focus: session,
            zoomed: false,
        }
    }

    /// Toggle single-pane zoom on multi-pane tabs and return the new zoom state.
    pub(crate) fn toggle_zoom(&mut self) -> bool {
        self.zoomed = !self.zoomed && self.len() > 1;
        self.zoomed
    }

    /// Focused leaf session ID.
    #[must_use]
    pub(crate) fn focus(&self) -> u64 {
        self.focus
    }

    /// Focus `session` if it is a leaf in this tree.
    pub(crate) fn set_focus(&mut self, session: u64) -> bool {
        if self.contains(session) {
            self.focus = session;
            true
        } else {
            false
        }
    }

    /// Whether `session` is a leaf in this tree.
    #[must_use]
    pub(crate) fn contains(&self, session: u64) -> bool {
        self.root.contains(session)
    }

    /// All leaf session IDs in tree order.
    #[must_use]
    pub(crate) fn sessions(&self) -> Vec<u64> {
        self.root.leaves()
    }

    /// Map leaf session IDs through `map` while preserving split structure and ratios.
    #[must_use]
    pub(crate) fn map_sessions<T>(&self, mut map: impl FnMut(u64) -> T) -> SplitTree<T> {
        self.root.map(&mut |session| map(*session))
    }

    #[must_use]
    pub(crate) fn is_zoomed(&self) -> bool {
        self.zoomed
    }

    /// Export this tree as a persisted [`crate::restore::PaneLayout`].
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

    /// Reconstruct a [`PaneTree`] from a persisted layout and fresh session IDs.
    #[must_use]
    pub(crate) fn rebuild(layout: &crate::restore::PaneLayout, fresh: &[u64]) -> Option<Self> {
        if fresh.len() < layout.leaf_count() {
            return None;
        }
        let mut next = 0usize;
        let mut focus = None;
        let root = Self::node_from_layout(layout, fresh, &mut next, &mut focus);
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
                let first = Box::new(Self::node_from_layout(first, fresh, next, focus));
                let second = Box::new(Self::node_from_layout(second, fresh, next, focus));
                PaneNode::Split {
                    axis: match dir {
                        SplitKind::Vertical => SplitAxis::Horizontal,
                        SplitKind::Horizontal => SplitAxis::Vertical,
                    },
                    ratio: (*ratio).clamp(MIN_RATIO, MAX_RATIO),
                    first,
                    second,
                }
            }
        }
    }

    /// Number of live panes (leaves) in this tab.
    #[must_use]
    pub fn len(&self) -> usize {
        self.root.len()
    }

    /// Split the focused leaf 50/50 in `dir`, placing `new_session` second and focusing it.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
            self.zoomed = false;
            true
        } else {
            false
        }
    }

    /// Close the focused pane.
    pub(crate) fn close_focused(&mut self) -> CloseOutcome {
        self.close_pane(self.focus)
    }

    /// Close the pane holding `session` (or the focused pane if unknown) and update focus.
    #[cfg_attr(
        any(test, debug_assertions),
        aterm_spec_anchor::refines(
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
        if matches!(self.root, PaneNode::Leaf(_)) {
            return CloseOutcome::LastPane { closed };
        }
        let heir = self.root.absorbing_neighbor(closed);
        let removed = self.root.remove_leaf(closed);
        debug_assert_eq!(removed, RemoveLeaf::Removed);
        if self.focus == closed || !self.contains(self.focus) {
            self.focus = heir.unwrap_or_else(|| self.root.first_leaf());
        }
        self.zoomed = false;
        CloseOutcome::Collapsed { closed }
    }

    /// Compute visible content [`PaneRect`]s for a `rows` × `cols` window grid.
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

    /// Outer pane allocations before subtab header rows are reserved.
    #[must_use]
    fn layout_cells(&self, rows: u16, cols: u16, zoomed: bool) -> Vec<PaneRect> {
        let win_rows = rows.max(1);
        let win_cols = cols.max(1);
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

    /// Unzoomed outer allocation (including subtab header) of the focused pane.
    #[must_use]
    pub(crate) fn focused_rect(&self, rows: u16, cols: u16) -> Option<PaneRect> {
        self.layout_cells(rows, cols, false)
            .into_iter()
            .find(|r| r.session == self.focus)
    }

    /// Session ID of the pane adjacent to the focused pane in `dir`, if any.
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

    /// Hit-test cell `(row, col)` against active 1-cell split dividers.
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

    /// Compute the unclamped first-child ratio placing `hit`'s divider at `(row, col)`.
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

    /// Set `hit`'s split ratio, clamped to `[MIN_RATIO, MAX_RATIO]`.
    pub(crate) fn set_divider_ratio(&mut self, hit: &DividerHit, ratio: f32) -> bool {
        self.root.set_divider_ratio(&hit.logical, ratio)
    }
}

/// Direction for keyboard pane-focus navigation.
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

/// Outcome of closing a pane in a [`PaneTree`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum CloseOutcome {
    /// Parent split collapsed into the surviving sibling.
    Collapsed { closed: u64 },
    /// Closed pane was the last leaf in the tab.
    LastPane { closed: u64 },
}

impl CloseOutcome {
    /// Session ID of the removed pane.
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
        assert_eq!(rebuilt.focus(), 12);

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
        assert_eq!(
            rebuilt.to_layout(&meta2).leaves()[0].local_id(),
            Some(10),
            "leaf local_id tracks the live session id"
        );

        assert!(PaneTree::rebuild(&layout, &[10, 11]).is_none());
    }

    /// Cmd-D vertical split: two panes side by side across a 1-cell divider column, each
    /// under a header row.
    #[test]
    fn vertical_split_geometry() {
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Vertical, 2));
        assert_eq!(t.focus(), 2, "focus follows the new pane");
        let mut rects = t.compute_layout(24, 80);
        rects.sort_by_key(|r| r.col_off);
        assert_eq!(rects.len(), 2);
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
        assert!(
            rects
                .iter()
                .all(|r| !(r.col_off..r.col_off + r.cols).contains(&40))
        );
    }

    /// Cmd-Shift-D horizontal split: two full-width panes stacked over a 1-cell divider row,
    /// each under a header row.
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
    /// pane.
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
        assert_eq!(t.focus_neighbor(FocusDir::Up, 24, 80), Some(2));
        assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), Some(1));
        assert_eq!(t.focus_neighbor(FocusDir::Right, 24, 80), None);
        assert_eq!(t.focus_neighbor(FocusDir::Down, 24, 80), None);
        assert!(t.set_focus(2));
        assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), Some(1));
        assert_eq!(t.focus_neighbor(FocusDir::Down, 24, 80), Some(3));
        assert_eq!(t.focus_neighbor(FocusDir::Up, 24, 80), None);
        assert_eq!(t.focus_neighbor(FocusDir::Right, 24, 80), None);
        assert!(t.set_focus(1));
        assert!(matches!(
            t.focus_neighbor(FocusDir::Right, 24, 80),
            Some(2 | 3)
        ));
        assert_eq!(t.focus_neighbor(FocusDir::Left, 24, 80), None);
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

        let mut s = PaneTree::new(9);
        assert!(!s.toggle_zoom());
        assert_eq!(s.compute_layout(24, 80).len(), 1);

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
        assert_eq!(
            t.compute_layout(24, 80),
            PaneTree::new(1).compute_layout(24, 80)
        );
    }

    /// Closing the focused pane focuses the survivor that absorbs its cells, not
    /// the tree's `first_leaf()`: the pane that grows into the hole takes the next
    /// keystroke.
    #[test]
    fn close_focuses_the_pane_that_absorbs_the_freed_space() {
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

        let (mut t, _) = tree(
            &|t| {
                t.split_focused(SplitDir::Horizontal, 2); // 1 / 2
                t.split_focused(SplitDir::Vertical, 3); // 1 / (2 | 3)
            },
            2,
        );
        assert_eq!(t.close_focused(), CloseOutcome::Collapsed { closed: 2 });
        assert_eq!(t.focus(), 3, "1 / (2 | 3), close 2 -> focus 3");

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
        let hit = t.divider_at(5, 40, 24, 80).expect("divider hit at col 40");
        assert_eq!(hit.dir, SplitDir::Vertical);
        assert!(t.divider_at(5, 10, 24, 80).is_none());
        assert!(t.divider_at(5, 60, 24, 80).is_none());
        let ratio = t.ratio_for_pointer(&hit, 5, 20).expect("ratio for pointer");
        assert!((ratio - 20.0 / 79.0).abs() < 1e-3, "ratio {ratio}");
        assert!(t.set_divider_ratio(&hit, ratio));
        let mut rects = t.compute_layout(24, 80);
        rects.sort_by_key(|r| r.col_off);
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
        let mut t = PaneTree::new(1);
        t.split_focused(SplitDir::Vertical, 2);
        t.split_focused(SplitDir::Horizontal, 3);
        let rects = t.compute_layout(24, 80);
        let top = rects.iter().find(|r| r.session == 2).unwrap();
        let bot = rects.iter().find(|r| r.session == 3).unwrap();
        let div_row = top.row_off + top.rows;
        let probe_col = top.col_off + 1;
        let hit = t
            .divider_at(div_row, probe_col, 24, 80)
            .expect("inner divider hit");
        assert_eq!(hit.dir, SplitDir::Horizontal, "inner split is horizontal");
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
        assert!(!split_fits_in(SplitDir::Vertical, 0, 0));
        assert!(!split_fits_in(SplitDir::Horizontal, 0, 0));
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
        let mut t = PaneTree::new(1);
        assert!(t.split_focused(SplitDir::Vertical, 2)); // 40 -> 20 | div | 19
        let unzoomed = t.focused_rect(24, 40).expect("focused leaf is in the tree");
        assert_eq!(
            (unzoomed.col_off, unzoomed.cols, unzoomed.rows),
            (21, 19, 24),
            "{unzoomed:?}"
        );

        assert!(t.toggle_zoom(), "the split tab zooms");
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
        assert!(split_fits_in(SplitDir::Vertical, 24, 40));

        let measured = t.focused_rect(24, 40).expect("focused leaf is in the tree");
        assert_eq!(
            measured, unzoomed,
            "zoom must not inflate the pane a split measures"
        );
        assert!(
            !split_fits_in(SplitDir::Vertical, measured.rows, measured.cols),
            "19 columns cannot hold two 16-column panes plus a divider"
        );

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
