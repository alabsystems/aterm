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
pub(crate) const fn split_fits_in(dir: SplitDir, rows: u16, cols: u16) -> bool {
    let min_rows = MIN_PANE_ROWS + 1; // each child also owns a subtab header
    match dir {
        SplitDir::Vertical => rows >= min_rows && cols.saturating_sub(1) >= 2 * MIN_PANE_COLS,
        SplitDir::Horizontal => cols >= MIN_PANE_COLS && rows.saturating_sub(1) >= 2 * min_rows,
    }
}

/// Minimum `(rows, cols)` required to split a pane in `dir`.
#[must_use]
pub(crate) const fn split_needs(dir: SplitDir) -> (u16, u16) {
    match dir {
        SplitDir::Vertical => (MIN_PANE_ROWS + 1, 2 * MIN_PANE_COLS + 1),
        SplitDir::Horizontal => (2 * (MIN_PANE_ROWS + 1) + 1, MIN_PANE_COLS),
    }
}

impl SplitDir {
    /// User-facing name for this split direction.
    pub(crate) const fn human(self) -> &'static str {
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
    pub(crate) const fn new(session: u64) -> Self {
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
    pub(crate) const fn focus(&self) -> u64 {
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
    #[cfg(test)]
    #[must_use]
    pub(crate) fn sessions(&self) -> Vec<u64> {
        self.root.leaves()
    }

    /// Visit leaf session IDs in tree order without allocating a `Vec`.
    pub(crate) fn visit_sessions(&self, f: &mut impl FnMut(u64)) {
        self.root.visit(&mut |&session| f(session));
    }

    /// Map leaf session IDs through `map` while preserving split structure and ratios.
    #[must_use]
    pub(crate) fn map_sessions<T>(&self, mut map: impl FnMut(u64) -> T) -> SplitTree<T> {
        self.root.map(&mut |session| map(*session))
    }

    /// Whether `root` has the exact same split topology, ratios, and terminal sessions as `self`.
    #[must_use]
    pub(crate) fn matches_projected<T: Copy>(
        &self,
        root: &SplitTree<T>,
        session_of: &impl Fn(T) -> Option<u64>,
    ) -> bool {
        fn eq_tree<T: Copy>(
            a: &PaneNode,
            b: &SplitTree<T>,
            session_of: &impl Fn(T) -> Option<u64>,
        ) -> bool {
            match (a, b) {
                (SplitTree::Leaf(session), SplitTree::Leaf(view)) => {
                    session_of(*view) == Some(*session)
                }
                (
                    SplitTree::Split {
                        axis: axis_a,
                        ratio: ratio_a,
                        first: first_a,
                        second: second_a,
                    },
                    SplitTree::Split {
                        axis: axis_b,
                        ratio: ratio_b,
                        first: first_b,
                        second: second_b,
                    },
                ) => {
                    axis_a == axis_b
                        && ratio_a == ratio_b
                        && eq_tree(first_a, first_b, session_of)
                        && eq_tree(second_a, second_b, session_of)
                }
                _ => false,
            }
        }
        eq_tree(&self.root, root, session_of)
    }

    #[must_use]
    pub(crate) const fn is_zoomed(&self) -> bool {
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

    /// Outer pane allocations before subtab header rows are reserved. Zoom is an
    /// argument because [`Self::focused_rect`] measures unzoomed: a split un-zooms,
    /// so a zoomed pane's full-window rect would admit a split that does not fit.
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
    pub(crate) const fn closed(self) -> u64 {
        match self {
            CloseOutcome::Collapsed { closed } | CloseOutcome::LastPane { closed } => closed,
        }
    }
}

#[cfg(test)]
#[path = "pane_tests.rs"]
mod tests;
