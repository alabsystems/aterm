// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content-agnostic tab, view, and split-tree foundations.
//!
//! The terminal UI historically identified both a tab and its focused pane by a
//! session id and laid splits out in terminal cells.  Native tab apps need three
//! identities instead: a stable tab, a stable view, and (for native content) a
//! stable app instance.  This module owns those identities and the generic
//! logical-pixel split geometry.  PTY ownership deliberately remains in
//! [`crate::SessionPool`]; [`View::Terminal`] is only the typed link to it.

use std::collections::HashMap;
use std::fmt;
use std::hash::Hash;
use std::marker::PhantomData;

/// Define a process-stable, non-index identity.
macro_rules! stable_id {
    ($name:ident) => {
        #[derive(
            Clone,
            Copy,
            PartialEq,
            Eq,
            PartialOrd,
            Ord,
            Hash,
            Debug,
            serde::Serialize,
            serde::Deserialize,
        )]
        #[serde(transparent)]
        pub(crate) struct $name(u64);

        #[allow(
            dead_code,
            reason = "stable wire/restore accessors are consumed incrementally by native host surfaces"
        )]
        impl $name {
            /// The stable integer representation used by restore and control
            /// serialization.  It is never a vector index.
            #[must_use]
            pub(crate) const fn get(self) -> u64 {
                self.0
            }

            /// Reconstitute an identity at a validated restore boundary.  Live
            /// allocation goes through [`IdAllocator`] or [`ViewStore`].
            #[must_use]
            pub(crate) const fn from_stored(raw: u64) -> Self {
                Self(raw)
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fmt(f)
            }
        }
    };
}

stable_id!(TabId);
stable_id!(ViewId);
stable_id!(AppInstanceId);

/// The stable-id space has been exhausted.  This is explicit even though a
/// process cannot realistically create `u64::MAX` tabs or views.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct IdExhausted;

/// Monotonic, never-reusing identity source.  Removing an object never rewinds
/// this counter, so a delayed event cannot address a later object by accident.
#[derive(Clone, Debug)]
pub(crate) struct IdAllocator<I> {
    next: u64,
    _id: PhantomData<fn() -> I>,
}

impl<I> Default for IdAllocator<I> {
    fn default() -> Self {
        Self {
            // Zero remains available to decode old manifests while all newly
            // allocated identities are visibly distinct from legacy indices.
            next: 1,
            _id: PhantomData,
        }
    }
}

pub(crate) trait StableId: Copy {
    fn from_raw(raw: u64) -> Self;
    #[allow(dead_code, reason = "used by persisted-id reservation")]
    fn raw(self) -> u64;
}

macro_rules! stable_id_impl {
    ($name:ident) => {
        impl StableId for $name {
            fn from_raw(raw: u64) -> Self {
                Self(raw)
            }

            fn raw(self) -> u64 {
                self.0
            }
        }
    };
}

stable_id_impl!(TabId);
stable_id_impl!(ViewId);
stable_id_impl!(AppInstanceId);

impl<I: StableId> IdAllocator<I> {
    /// Mint the next identity.  An exhausted allocator stays exhausted.
    pub(crate) fn allocate(&mut self) -> Result<I, IdExhausted> {
        let raw = self.next;
        self.next = raw.checked_add(1).ok_or(IdExhausted)?;
        Ok(I::from_raw(raw))
    }

    /// Advance past a restored identity so future allocation cannot collide
    /// with it.  Multiple calls and out-of-order restore are harmless.
    #[allow(dead_code, reason = "used by persisted-id restoration")]
    fn reserve(&mut self, id: I) -> Result<(), IdExhausted> {
        let Some(after) = id.raw().checked_add(1) else {
            self.next = u64::MAX;
            return Err(IdExhausted);
        };
        self.next = self.next.max(after);
        Ok(())
    }
}

/// A terminal view's typed link into the process-wide session pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct TerminalView {
    pub(crate) session: u64,
}

/// The core-owned link for a native view.  Its reducer/view-local state lives in
/// the native app runtime and is keyed by the enclosing [`ViewId`].
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct NativeViewRef {
    pub(crate) instance: AppInstanceId,
}

/// Content of one split leaf.  The closed enum makes PTY-only operations
/// explicit and gives later native apps a first-class, non-PTY representation.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum View {
    Terminal(TerminalView),
    Native(NativeViewRef),
}

impl View {
    /// Resolve the PTY session only for a terminal view.
    #[must_use]
    pub(crate) const fn terminal_session(self) -> Option<u64> {
        match self {
            Self::Terminal(view) => Some(view.session),
            Self::Native(_) => None,
        }
    }
}

/// Process-wide owner of view identities and their typed content links.
#[derive(Debug, Default)]
pub(crate) struct ViewStore {
    ids: IdAllocator<ViewId>,
    views: HashMap<ViewId, View>,
}

impl ViewStore {
    /// Insert a new terminal view over `session`.
    pub(crate) fn insert_terminal(&mut self, session: u64) -> Result<ViewId, IdExhausted> {
        self.insert(View::Terminal(TerminalView { session }))
    }

    /// Insert a new native view linked to `instance`.
    pub(crate) fn insert_native(&mut self, instance: AppInstanceId) -> Result<ViewId, IdExhausted> {
        self.insert(View::Native(NativeViewRef { instance }))
    }

    fn insert(&mut self, view: View) -> Result<ViewId, IdExhausted> {
        let id = self.ids.allocate()?;
        let previous = self.views.insert(id, view);
        debug_assert!(previous.is_none(), "monotonic ViewId collided");
        Ok(id)
    }

    /// Restore a previously persisted identity.  Duplicate ids fail closed and
    /// do not replace the live view.  Future ids advance past the restored one.
    #[allow(
        dead_code,
        reason = "stable-id restore schema lands after the live host"
    )]
    pub(crate) fn restore(&mut self, id: ViewId, view: View) -> Result<(), RestoreIdError> {
        if self.views.contains_key(&id) {
            return Err(RestoreIdError::Duplicate);
        }
        self.ids
            .reserve(id)
            .map_err(|_| RestoreIdError::Exhausted)?;
        self.views.insert(id, view);
        Ok(())
    }

    #[must_use]
    pub(crate) fn get(&self, id: ViewId) -> Option<&View> {
        self.views.get(&id)
    }

    #[must_use]
    pub(crate) fn contains(&self, id: ViewId) -> bool {
        self.views.contains_key(&id)
    }

    /// Remove a view.  Its identity remains burned and is never allocated again.
    pub(crate) fn remove(&mut self, id: ViewId) -> Option<View> {
        self.views.remove(&id)
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (ViewId, View)> + '_ {
        self.views.iter().map(|(id, view)| (*id, *view))
    }

    #[must_use]
    #[allow(dead_code, reason = "inspection and restore capacity query")]
    pub(crate) fn len(&self) -> usize {
        self.views.len()
    }

    #[must_use]
    #[allow(dead_code, reason = "inspection and restore capacity query")]
    pub(crate) fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

/// Restore-time stable-id rejection.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(
    dead_code,
    reason = "public error surface for persisted ViewId restore"
)]
pub(crate) enum RestoreIdError {
    Duplicate,
    Exhausted,
}

/// Split orientation in content coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub(crate) enum SplitAxis {
    /// First child left, second child right.
    Horizontal,
    /// First child above, second child below.
    Vertical,
}

/// One node in a content-agnostic split tree.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum SplitTree<T> {
    Leaf(T),
    Split {
        axis: SplitAxis,
        ratio: f32,
        first: Box<Self>,
        second: Box<Self>,
    },
}

impl<T> SplitTree<T> {
    #[must_use]
    pub(crate) fn leaf(value: T) -> Self {
        Self::Leaf(value)
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { first, second, .. } => first.len() + second.len(),
        }
    }

    pub(crate) fn visit(&self, out: &mut impl FnMut(&T)) {
        match self {
            Self::Leaf(value) => out(value),
            Self::Split { first, second, .. } => {
                first.visit(out);
                second.visit(out);
            }
        }
    }

    /// Allocation-free, short-circuiting twin of `leaves().into_iter().any(..)`: same
    /// left-to-right leaf order as [`Self::visit`], stops at the first match.
    ///
    /// PERF: `leaves()` costs a `len()` recursion plus a heap `Vec` — even for a lone leaf —
    /// which is pure waste when the caller only wants a bool. The predicates on the wake and
    /// present paths (`active_tab_displays_session`, `active_tab_contains_native`) run at the PTY
    /// reader's batch rate, i.e. thousands of times a second under a flood, so they use this.
    pub(crate) fn any_leaf(&self, pred: &mut impl FnMut(&T) -> bool) -> bool {
        match self {
            Self::Leaf(value) => pred(value),
            Self::Split { first, second, .. } => first.any_leaf(pred) || second.any_leaf(pred),
        }
    }

    /// Preserve split structure/ratios while projecting leaf payloads into a
    /// different identity domain (terminal compatibility mirror ↔ stable view).
    #[must_use]
    pub(crate) fn map<U>(&self, map: &mut impl FnMut(&T) -> U) -> SplitTree<U> {
        match self {
            Self::Leaf(value) => SplitTree::Leaf(map(value)),
            Self::Split {
                axis,
                ratio,
                first,
                second,
            } => SplitTree::Split {
                axis: *axis,
                ratio: *ratio,
                first: Box::new(first.map(map)),
                second: Box::new(second.map(map)),
            },
        }
    }
}

impl<T: Copy + Eq> SplitTree<T> {
    #[must_use]
    pub(crate) fn contains(&self, target: T) -> bool {
        match self {
            Self::Leaf(value) => *value == target,
            Self::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }

    #[must_use]
    pub(crate) fn leaves(&self) -> Vec<T> {
        let mut leaves = Vec::with_capacity(self.len());
        self.visit(&mut |value| leaves.push(*value));
        leaves
    }

    /// Replace `target` with a split whose first leaf is the original and whose
    /// second leaf is `new`.  Returns false without mutation if `target` is stale.
    pub(crate) fn split_leaf(&mut self, target: T, axis: SplitAxis, new: T) -> bool {
        match self {
            Self::Leaf(value) if *value == target => {
                let original = *value;
                *self = Self::Split {
                    axis,
                    ratio: 0.5,
                    first: Box::new(Self::Leaf(original)),
                    second: Box::new(Self::Leaf(new)),
                };
                true
            }
            Self::Leaf(_) => false,
            Self::Split { first, second, .. } => {
                first.split_leaf(target, axis, new) || second.split_leaf(target, axis, new)
            }
        }
    }

    /// Describe the parent split that collapses when `target` is removed. The
    /// returned path names that parent; after collapse the same path names the
    /// promoted sibling, which is the stable insertion point for reopen.
    #[must_use]
    pub(crate) fn leaf_placement(
        &self,
        target: T,
    ) -> Option<(SplitPath, SplitBranch, SplitAxis, f32)> {
        fn find<T: Copy + Eq>(
            node: &SplitTree<T>,
            target: T,
            path: &SplitPath,
        ) -> Option<(SplitPath, SplitBranch, SplitAxis, f32)> {
            let SplitTree::Split {
                axis,
                ratio,
                first,
                second,
            } = node
            else {
                return None;
            };
            if matches!(&**first, SplitTree::Leaf(value) if *value == target) {
                return Some((path.clone(), SplitBranch::First, *axis, *ratio));
            }
            if matches!(&**second, SplitTree::Leaf(value) if *value == target) {
                return Some((path.clone(), SplitBranch::Second, *axis, *ratio));
            }
            find(first, target, &path.pushed(SplitBranch::First))
                .or_else(|| find(second, target, &path.pushed(SplitBranch::Second)))
        }
        find(self, target, &SplitPath::root())
    }

    /// Recreate a split whose removed leaf was collapsed onto its sibling. A stale
    /// path or already-present identity fails without mutation.
    pub(crate) fn restore_collapsed_leaf(
        &mut self,
        parent: &SplitPath,
        removed_branch: SplitBranch,
        axis: SplitAxis,
        ratio: f32,
        restored: T,
    ) -> bool {
        if self.contains(restored) {
            return false;
        }
        let Some(node) = node_at_path_mut(self, parent) else {
            return false;
        };
        let survivor = std::mem::replace(node, Self::Leaf(restored));
        let restored = Box::new(Self::Leaf(restored));
        let survivor = Box::new(survivor);
        let (first, second) = match removed_branch {
            SplitBranch::First => (restored, survivor),
            SplitBranch::Second => (survivor, restored),
        };
        *node = Self::Split {
            axis,
            ratio: ratio.clamp(0.05, 0.95),
            first,
            second,
        };
        true
    }

    /// Remove `target` and collapse its parent.  The only leaf is reported but
    /// left in place so the tab owner can atomically remove the whole tab.
    pub(crate) fn remove_leaf(&mut self, target: T) -> RemoveLeaf {
        if !self.contains(target) {
            return RemoveLeaf::NotFound;
        }
        if matches!(self, Self::Leaf(_)) {
            return RemoveLeaf::OnlyLeaf;
        }
        let removed = Self::remove_below_split(self, target);
        debug_assert!(removed);
        RemoveLeaf::Removed
    }

    fn remove_below_split(node: &mut Self, target: T) -> bool {
        let Self::Split { first, second, .. } = node else {
            return false;
        };
        let first_target = matches!(&**first, Self::Leaf(value) if *value == target);
        let second_target = matches!(&**second, Self::Leaf(value) if *value == target);
        if first_target {
            let survivor = std::mem::replace(second.as_mut(), Self::Leaf(target));
            *node = survivor;
            return true;
        }
        if second_target {
            let survivor = std::mem::replace(first.as_mut(), Self::Leaf(target));
            *node = survivor;
            return true;
        }
        if let Self::Split { first, second, .. } = node {
            Self::remove_below_split(first, target) || Self::remove_below_split(second, target)
        } else {
            false
        }
    }

    #[must_use]
    pub(crate) fn first_leaf(&self) -> T {
        match self {
            Self::Leaf(value) => *value,
            Self::Split { first, .. } => first.first_leaf(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum RemoveLeaf {
    Removed,
    OnlyLeaf,
    NotFound,
}

/// Logical point in a tab's content coordinate space.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct LogicalPoint {
    pub(crate) x: f32,
    pub(crate) y: f32,
}

/// Logical extent in a tab's content coordinate space.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct LogicalSize {
    pub(crate) width: f32,
    pub(crate) height: f32,
}

impl LogicalSize {
    #[must_use]
    pub(crate) const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// Half-open logical rectangle.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub(crate) struct LogicalRect {
    pub(crate) origin: LogicalPoint,
    pub(crate) size: LogicalSize,
}

impl LogicalRect {
    #[must_use]
    pub(crate) const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            origin: LogicalPoint { x, y },
            size: LogicalSize { width, height },
        }
    }

    #[must_use]
    pub(crate) fn contains(self, point: LogicalPoint) -> bool {
        point.x >= self.origin.x
            && point.y >= self.origin.y
            && point.x < self.origin.x + self.size.width
            && point.y < self.origin.y + self.size.height
    }
}

/// One laid-out split leaf.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LayoutLeaf<T> {
    pub(crate) value: T,
    pub(crate) rect: LogicalRect,
}

/// One leaf's sizing contract in the tab's logical coordinate space.
///
/// `minimum` is a hard ergonomic request when the host has enough room.  If the
/// whole tree cannot fit, every leaf remains present and the shortage is shared
/// deterministically instead of dropping a sibling.  `preferred` is used to
/// distribute that shortage and is otherwise advisory; the user's divider ratio
/// remains the primary placement choice.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) struct LeafSizing {
    pub(crate) minimum: LogicalSize,
    pub(crate) preferred: LogicalSize,
}

impl LeafSizing {
    #[must_use]
    pub(crate) const fn new(minimum: LogicalSize, preferred: LogicalSize) -> Self {
        Self { minimum, preferred }
    }

    #[must_use]
    fn sanitized(self) -> Self {
        let component = |value: f32| {
            if value.is_finite() {
                value.max(0.0)
            } else {
                0.0
            }
        };
        let minimum = LogicalSize::new(
            component(self.minimum.width),
            component(self.minimum.height),
        );
        Self {
            minimum,
            preferred: LogicalSize::new(
                component(self.preferred.width).max(minimum.width),
                component(self.preferred.height).max(minimum.height),
            ),
        }
    }
}

/// A stable root-to-node branch.  Paths remain valid across focus, resize and
/// divider-ratio changes; a structural split/close intentionally invalidates
/// paths below the changed node, where mutation then fails closed.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, serde::Serialize, serde::Deserialize,
)]
pub(crate) enum SplitBranch {
    First,
    Second,
}

/// Stable topology address used by restore, divider drags and inspection.
#[derive(
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Debug,
    Default,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(transparent)]
pub(crate) struct SplitPath(Vec<SplitBranch>);

impl SplitPath {
    #[must_use]
    pub(crate) fn root() -> Self {
        Self::default()
    }

    #[must_use]
    pub(crate) fn branches(&self) -> &[SplitBranch] {
        &self.0
    }

    #[must_use]
    pub(crate) fn from_branches(branches: Vec<SplitBranch>) -> Self {
        Self(branches)
    }

    fn pushed(&self, branch: SplitBranch) -> Self {
        let mut path = self.clone();
        path.0.push(branch);
        path
    }
}

/// One visible canonical view and its exact host placement.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct VisibleLeaf {
    pub(crate) path: SplitPath,
    pub(crate) view: ViewId,
    pub(crate) rect: LogicalRect,
    pub(crate) sizing: LeafSizing,
    pub(crate) focused: bool,
}

/// One visible divider, including the bounds needed to turn a pointer back into
/// a ratio without re-resolving an index or content kind.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct VisibleDivider {
    pub(crate) path: SplitPath,
    pub(crate) axis: SplitAxis,
    pub(crate) rect: LogicalRect,
    span_origin: f32,
    span_extent: f32,
}

/// A frame-stable, content-agnostic plan.  Paint, pointer hit testing, terminal
/// sizing and accessibility all consume this same artifact.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct VisibleLeafPlan {
    pub(crate) tab: TabId,
    pub(crate) bounds: LogicalRect,
    pub(crate) leaves: Vec<VisibleLeaf>,
    pub(crate) dividers: Vec<VisibleDivider>,
    pub(crate) focused: ViewId,
    pub(crate) zoomed: bool,
    pub(crate) divider: f32,
}

impl VisibleLeafPlan {
    #[must_use]
    pub(crate) fn leaf(&self, view: ViewId) -> Option<&VisibleLeaf> {
        self.leaves.iter().find(|leaf| leaf.view == view)
    }

    #[must_use]
    pub(crate) fn leaf_at(&self, point: LogicalPoint) -> Option<&VisibleLeaf> {
        self.leaves.iter().find(|leaf| leaf.rect.contains(point))
    }

    #[must_use]
    pub(crate) fn divider_at(&self, point: LogicalPoint) -> Option<&VisibleDivider> {
        self.dividers
            .iter()
            // A nested divider is visually above its ancestor at intersections.
            .rev()
            .find(|divider| divider.rect.contains(point))
    }

    #[must_use]
    pub(crate) fn ratio_for_pointer(
        &self,
        divider: &VisibleDivider,
        point: LogicalPoint,
    ) -> Option<f32> {
        let pointer = match divider.axis {
            SplitAxis::Horizontal => point.x,
            SplitAxis::Vertical => point.y,
        };
        let splittable = (divider.span_extent - self.divider.max(0.0)).max(0.0);
        (splittable > f32::EPSILON)
            .then(|| ((pointer - divider.span_origin) / splittable).clamp(0.0, 1.0))
    }
}

/// Root-to-split path used as a stable divider-drag handle while the tree shape
/// remains unchanged.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct LogicalDividerHit {
    path: SplitPath,
    pub(crate) axis: SplitAxis,
    span_origin: f32,
    span_extent: f32,
}

/// Direction for geometry-based neighboring-view focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

impl<T: Copy + Eq> SplitTree<T> {
    /// Resolve every leaf in logical pixels.  `divider` is a pixel-width gap;
    /// `min_leaf` is the nonzero ergonomic floor along the divided axis.
    #[must_use]
    pub(crate) fn layout(
        &self,
        bounds: LogicalRect,
        divider: f32,
        min_leaf: f32,
    ) -> Vec<LayoutLeaf<T>> {
        let mut out = Vec::with_capacity(self.len());
        layout_into(self, sanitize_rect(bounds), divider, min_leaf, &mut out);
        out
    }

    #[must_use]
    #[allow(
        dead_code,
        reason = "generic primitive retained for PaneTree compatibility and standalone geometry tests"
    )]
    pub(crate) fn leaf_at(
        &self,
        point: LogicalPoint,
        bounds: LogicalRect,
        divider: f32,
        min_leaf: f32,
    ) -> Option<T> {
        self.layout(bounds, divider, min_leaf)
            .into_iter()
            .find_map(|leaf| leaf.rect.contains(point).then_some(leaf.value))
    }

    #[must_use]
    pub(crate) fn neighbor(
        &self,
        focused: T,
        direction: FocusDirection,
        bounds: LogicalRect,
        divider: f32,
        min_leaf: f32,
    ) -> Option<T> {
        let layout = self.layout(bounds, divider, min_leaf);
        let current = layout.iter().find(|leaf| leaf.value == focused)?.rect;
        let cur_right = current.origin.x + current.size.width;
        let cur_bottom = current.origin.y + current.size.height;
        let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| (a1.min(b1) - a0.max(b0)).max(0.0);

        let mut best: Option<(T, f32, f32, f32)> = None;
        for leaf in layout {
            if leaf.value == focused {
                continue;
            }
            let rect = leaf.rect;
            let right = rect.origin.x + rect.size.width;
            let bottom = rect.origin.y + rect.size.height;
            let (on_side, distance, shared, offset) = match direction {
                FocusDirection::Left => (
                    right <= current.origin.x,
                    current.origin.x - right,
                    overlap(current.origin.y, cur_bottom, rect.origin.y, bottom),
                    rect.origin.y,
                ),
                FocusDirection::Right => (
                    rect.origin.x >= cur_right,
                    rect.origin.x - cur_right,
                    overlap(current.origin.y, cur_bottom, rect.origin.y, bottom),
                    rect.origin.y,
                ),
                FocusDirection::Up => (
                    bottom <= current.origin.y,
                    current.origin.y - bottom,
                    overlap(current.origin.x, cur_right, rect.origin.x, right),
                    rect.origin.x,
                ),
                FocusDirection::Down => (
                    rect.origin.y >= cur_bottom,
                    rect.origin.y - cur_bottom,
                    overlap(current.origin.x, cur_right, rect.origin.x, right),
                    rect.origin.x,
                ),
            };
            if !on_side || shared <= 0.0 {
                continue;
            }
            let better = best.is_none_or(|(_, best_distance, best_shared, best_offset)| {
                distance < best_distance
                    || (distance == best_distance && shared > best_shared)
                    || (distance == best_distance && shared == best_shared && offset < best_offset)
            });
            if better {
                best = Some((leaf.value, distance, shared, offset));
            }
        }
        best.map(|(value, _, _, _)| value)
    }

    #[must_use]
    pub(crate) fn divider_at(
        &self,
        point: LogicalPoint,
        bounds: LogicalRect,
        divider: f32,
        min_leaf: f32,
    ) -> Option<LogicalDividerHit> {
        let mut path = SplitPath::root();
        divider_at_in(
            self,
            sanitize_rect(bounds),
            point,
            divider.max(0.0),
            min_leaf.max(0.0),
            &mut path,
        )
    }

    /// Convert a pointer on the divided axis into an unclamped split ratio.
    #[must_use]
    pub(crate) fn ratio_for_pointer(
        hit: &LogicalDividerHit,
        point: LogicalPoint,
        divider: f32,
    ) -> Option<f32> {
        let pointer = match hit.axis {
            SplitAxis::Horizontal => point.x,
            SplitAxis::Vertical => point.y,
        };
        let splittable = (hit.span_extent - divider.max(0.0)).max(0.0);
        if splittable <= f32::EPSILON {
            return None;
        }
        Some(((pointer - hit.span_origin) / splittable).clamp(0.0, 1.0))
    }

    /// Apply a ratio through a still-live divider path.  Stale paths fail closed.
    pub(crate) fn set_divider_ratio(&mut self, hit: &LogicalDividerHit, ratio: f32) -> bool {
        let mut node = self;
        for branch in hit.path.branches() {
            let Self::Split { first, second, .. } = node else {
                return false;
            };
            node = if *branch == SplitBranch::Second {
                second
            } else {
                first
            };
        }
        let Self::Split { ratio: current, .. } = node else {
            return false;
        };
        *current = ratio.clamp(0.05, 0.95);
        true
    }
}

fn sanitize_rect(mut rect: LogicalRect) -> LogicalRect {
    if !rect.origin.x.is_finite() {
        rect.origin.x = 0.0;
    }
    if !rect.origin.y.is_finite() {
        rect.origin.y = 0.0;
    }
    if !rect.size.width.is_finite() || rect.size.width < 0.0 {
        rect.size.width = 0.0;
    }
    if !rect.size.height.is_finite() || rect.size.height < 0.0 {
        rect.size.height = 0.0;
    }
    rect
}

fn split_extent(extent: f32, divider: f32, ratio: f32, min_leaf: f32) -> (f32, f32) {
    let divider = divider.clamp(0.0, extent.max(0.0));
    let splittable = (extent - divider).max(0.0);
    let min_leaf = min_leaf.max(0.0);
    if splittable < min_leaf * 2.0 {
        let first = (splittable * 0.5).round();
        return (first, splittable - first);
    }
    let first = (splittable * ratio.clamp(0.0, 1.0))
        .round()
        .clamp(min_leaf, splittable - min_leaf);
    (first, splittable - first)
}

fn child_rects(
    bounds: LogicalRect,
    axis: SplitAxis,
    ratio: f32,
    divider: f32,
    min_leaf: f32,
) -> (LogicalRect, LogicalRect, LogicalRect) {
    let divider = divider.max(0.0);
    match axis {
        SplitAxis::Horizontal => {
            let (first, second) = split_extent(bounds.size.width, divider, ratio, min_leaf);
            let gap = divider.min((bounds.size.width - first - second).max(0.0));
            (
                LogicalRect::new(bounds.origin.x, bounds.origin.y, first, bounds.size.height),
                LogicalRect::new(
                    bounds.origin.x + first + gap,
                    bounds.origin.y,
                    second,
                    bounds.size.height,
                ),
                LogicalRect::new(
                    bounds.origin.x + first,
                    bounds.origin.y,
                    gap,
                    bounds.size.height,
                ),
            )
        }
        SplitAxis::Vertical => {
            let (first, second) = split_extent(bounds.size.height, divider, ratio, min_leaf);
            let gap = divider.min((bounds.size.height - first - second).max(0.0));
            (
                LogicalRect::new(bounds.origin.x, bounds.origin.y, bounds.size.width, first),
                LogicalRect::new(
                    bounds.origin.x,
                    bounds.origin.y + first + gap,
                    bounds.size.width,
                    second,
                ),
                LogicalRect::new(
                    bounds.origin.x,
                    bounds.origin.y + first,
                    bounds.size.width,
                    gap,
                ),
            )
        }
    }
}

fn layout_into<T: Copy + Eq>(
    tree: &SplitTree<T>,
    bounds: LogicalRect,
    divider: f32,
    min_leaf: f32,
    out: &mut Vec<LayoutLeaf<T>>,
) {
    match tree {
        SplitTree::Leaf(value) => out.push(LayoutLeaf {
            value: *value,
            rect: bounds,
        }),
        SplitTree::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let (first_rect, second_rect, _) =
                child_rects(bounds, *axis, *ratio, divider, min_leaf);
            layout_into(first, first_rect, divider, min_leaf, out);
            layout_into(second, second_rect, divider, min_leaf, out);
        }
    }
}

fn divider_at_in<T: Copy + Eq>(
    tree: &SplitTree<T>,
    bounds: LogicalRect,
    point: LogicalPoint,
    divider: f32,
    min_leaf: f32,
    path: &mut SplitPath,
) -> Option<LogicalDividerHit> {
    let SplitTree::Split {
        axis,
        ratio,
        first,
        second,
    } = tree
    else {
        return None;
    };
    let (first_rect, second_rect, divider_rect) =
        child_rects(bounds, *axis, *ratio, divider, min_leaf);
    if divider_rect.contains(point) {
        return Some(LogicalDividerHit {
            path: path.clone(),
            axis: *axis,
            span_origin: match axis {
                SplitAxis::Horizontal => bounds.origin.x,
                SplitAxis::Vertical => bounds.origin.y,
            },
            span_extent: match axis {
                SplitAxis::Horizontal => bounds.size.width,
                SplitAxis::Vertical => bounds.size.height,
            },
        });
    }
    if first_rect.contains(point) {
        path.0.push(SplitBranch::First);
        let hit = divider_at_in(first, first_rect, point, divider, min_leaf, path);
        path.0.pop();
        hit
    } else if second_rect.contains(point) {
        path.0.push(SplitBranch::Second);
        let hit = divider_at_in(second, second_rect, point, divider, min_leaf, path);
        path.0.pop();
        hit
    } else {
        None
    }
}

#[derive(Clone, Copy)]
struct SubtreeSizing {
    minimum: LogicalSize,
    preferred: LogicalSize,
}

fn subtree_sizing<T: Copy>(
    tree: &SplitTree<T>,
    divider: f32,
    sizing: &impl Fn(T) -> LeafSizing,
) -> SubtreeSizing {
    match tree {
        SplitTree::Leaf(value) => {
            let sizing = sizing(*value).sanitized();
            SubtreeSizing {
                minimum: sizing.minimum,
                preferred: sizing.preferred,
            }
        }
        SplitTree::Split {
            axis,
            first,
            second,
            ..
        } => {
            let first = subtree_sizing(first, divider, sizing);
            let second = subtree_sizing(second, divider, sizing);
            let gap = divider.max(0.0);
            match axis {
                SplitAxis::Horizontal => SubtreeSizing {
                    minimum: LogicalSize::new(
                        first.minimum.width + gap + second.minimum.width,
                        first.minimum.height.max(second.minimum.height),
                    ),
                    preferred: LogicalSize::new(
                        first.preferred.width + gap + second.preferred.width,
                        first.preferred.height.max(second.preferred.height),
                    ),
                },
                SplitAxis::Vertical => SubtreeSizing {
                    minimum: LogicalSize::new(
                        first.minimum.width.max(second.minimum.width),
                        first.minimum.height + gap + second.minimum.height,
                    ),
                    preferred: LogicalSize::new(
                        first.preferred.width.max(second.preferred.width),
                        first.preferred.height + gap + second.preferred.height,
                    ),
                },
            }
        }
    }
}

fn constrained_extent(
    extent: f32,
    divider: f32,
    ratio: f32,
    first_minimum: f32,
    second_minimum: f32,
    first_preferred: f32,
    second_preferred: f32,
) -> (f32, f32) {
    let divider = divider.clamp(0.0, extent.max(0.0));
    let available = (extent - divider).max(0.0);
    let first_minimum = first_minimum.max(0.0);
    let second_minimum = second_minimum.max(0.0);
    if available + f32::EPSILON >= first_minimum + second_minimum {
        let target = available * ratio.clamp(0.0, 1.0);
        let first = target.round().clamp(
            first_minimum,
            (available - second_minimum).max(first_minimum),
        );
        return (first, available - first);
    }

    // Impossible fit: keep both subtrees represented.  Preferred extents are a
    // deterministic tie-breaker when minimums are zero; otherwise shortage is
    // shared in proportion to the requested minimums.
    let first_weight = if first_minimum + second_minimum > f32::EPSILON {
        first_minimum
    } else {
        first_preferred.max(0.0)
    };
    let second_weight = if first_minimum + second_minimum > f32::EPSILON {
        second_minimum
    } else {
        second_preferred.max(0.0)
    };
    let total = first_weight + second_weight;
    let share = if total > f32::EPSILON {
        first_weight / total
    } else {
        0.5
    };
    let first = (available * share).round().clamp(0.0, available);
    (first, available - first)
}

#[allow(
    clippy::too_many_arguments,
    reason = "recursive layout carries one canonical tree, geometry, topology path, focus and two output lanes"
)]
fn plan_into(
    tree: &SplitTree<ViewId>,
    bounds: LogicalRect,
    divider: f32,
    sizing: &impl Fn(ViewId) -> LeafSizing,
    focus: ViewId,
    path: &SplitPath,
    leaves: &mut Vec<VisibleLeaf>,
    dividers: &mut Vec<VisibleDivider>,
) {
    match tree {
        SplitTree::Leaf(view) => leaves.push(VisibleLeaf {
            path: path.clone(),
            view: *view,
            rect: bounds,
            sizing: sizing(*view).sanitized(),
            focused: *view == focus,
        }),
        SplitTree::Split {
            axis,
            ratio,
            first,
            second,
        } => {
            let first_sizing = subtree_sizing(first, divider, sizing);
            let second_sizing = subtree_sizing(second, divider, sizing);
            let (first_extent, second_extent) = match axis {
                SplitAxis::Horizontal => constrained_extent(
                    bounds.size.width,
                    divider,
                    *ratio,
                    first_sizing.minimum.width,
                    second_sizing.minimum.width,
                    first_sizing.preferred.width,
                    second_sizing.preferred.width,
                ),
                SplitAxis::Vertical => constrained_extent(
                    bounds.size.height,
                    divider,
                    *ratio,
                    first_sizing.minimum.height,
                    second_sizing.minimum.height,
                    first_sizing.preferred.height,
                    second_sizing.preferred.height,
                ),
            };
            let gap = match axis {
                SplitAxis::Horizontal => {
                    (bounds.size.width - first_extent - second_extent).max(0.0)
                }
                SplitAxis::Vertical => (bounds.size.height - first_extent - second_extent).max(0.0),
            };
            let (first_rect, second_rect, divider_rect) = match axis {
                SplitAxis::Horizontal => (
                    LogicalRect::new(
                        bounds.origin.x,
                        bounds.origin.y,
                        first_extent,
                        bounds.size.height,
                    ),
                    LogicalRect::new(
                        bounds.origin.x + first_extent + gap,
                        bounds.origin.y,
                        second_extent,
                        bounds.size.height,
                    ),
                    LogicalRect::new(
                        bounds.origin.x + first_extent,
                        bounds.origin.y,
                        gap,
                        bounds.size.height,
                    ),
                ),
                SplitAxis::Vertical => (
                    LogicalRect::new(
                        bounds.origin.x,
                        bounds.origin.y,
                        bounds.size.width,
                        first_extent,
                    ),
                    LogicalRect::new(
                        bounds.origin.x,
                        bounds.origin.y + first_extent + gap,
                        bounds.size.width,
                        second_extent,
                    ),
                    LogicalRect::new(
                        bounds.origin.x,
                        bounds.origin.y + first_extent,
                        bounds.size.width,
                        gap,
                    ),
                ),
            };
            dividers.push(VisibleDivider {
                path: path.clone(),
                axis: *axis,
                rect: divider_rect,
                span_origin: match axis {
                    SplitAxis::Horizontal => bounds.origin.x,
                    SplitAxis::Vertical => bounds.origin.y,
                },
                span_extent: match axis {
                    SplitAxis::Horizontal => bounds.size.width,
                    SplitAxis::Vertical => bounds.size.height,
                },
            });
            plan_into(
                first,
                first_rect,
                divider,
                sizing,
                focus,
                &path.pushed(SplitBranch::First),
                leaves,
                dividers,
            );
            plan_into(
                second,
                second_rect,
                divider,
                sizing,
                focus,
                &path.pushed(SplitBranch::Second),
                leaves,
                dividers,
            );
        }
    }
}

fn node_at_path_mut<'a, T>(
    mut tree: &'a mut SplitTree<T>,
    path: &SplitPath,
) -> Option<&'a mut SplitTree<T>> {
    for branch in path.branches() {
        let SplitTree::Split { first, second, .. } = tree else {
            return None;
        };
        tree = match branch {
            SplitBranch::First => first,
            SplitBranch::Second => second,
        };
    }
    Some(tree)
}

/// Independent tab-state flags; no state hides another state.
///
/// The two attention bits are SEPARATE FIELDS, one per owner, because a single
/// shared bool cannot be recomputed. The status classifier refolds its bit from
/// the tab's leaves on every publication, while a failed document shutdown or a
/// settings/update announcement writes its own bit out of band and is not
/// derivable from anything the classifier can see. Merged into one bool, the
/// recomputing owner had to OR the stored value back in to avoid erasing the
/// other — and so ORed back its OWN previous contribution, latching a failure
/// onto the tab for the life of the process. Ask [`Self::wants_attention`] at
/// every render seam; never read one field alone.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub(crate) struct TabIndicators {
    pub(crate) dirty: bool,
    pub(crate) busy: bool,
    /// Raised OUT OF BAND by a native leaf's owner (a failed document shutdown,
    /// an update announcement). Never written by the status path.
    pub(crate) attention: bool,
    /// Raised by the session STATUS classifier — today, a failed last outcome.
    /// Fully recomputed from the tab's terminal leaves on every pass, so it
    /// clears itself when the failure is superseded.
    pub(crate) status_attention: bool,
}

impl TabIndicators {
    /// Whether anything at all wants the user's eye. The chrome shows ONE mark,
    /// so every renderer and the introspection serializer fold here rather than
    /// picking a field.
    #[must_use]
    pub(crate) const fn wants_attention(self) -> bool {
        self.attention || self.status_attention
    }
}

/// Closed icon identity for first-party, non-terminal tab applications.
///
/// A terminal is deliberately absent from this enum: terminal presentation is
/// title-only and expresses that invariant as `TabPresentation::icon == None`.
/// Keeping the distinction typed prevents an unknown app metadata string from
/// accidentally receiving the same chrome policy as a terminal session.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TabIconKind {
    Settings,
    Markdown,
    Editor,
    Recovery,
}

impl TabIconKind {
    /// Stable semantic name used by chrome inspection and assistive help.
    #[must_use]
    pub(crate) const fn semantic_name(self) -> &'static str {
        match self {
            Self::Settings => "settings",
            Self::Markdown => "markdown",
            Self::Editor => "editor",
            Self::Recovery => "recovery",
        }
    }
}

/// Which side(s) of a live session connection (design §4) a tab's sessions sit
/// on: `Outbound` = some session here holds authority INTO a peer, `Inbound` =
/// a peer holds authority over a session here, `Both` = both at once.
///
/// Deliberately NOT a [`TabIndicators`] field: the indicator bits have exactly
/// two recomputing owners (the status classifier and the native leaves), and
/// several seams ASSIGN whole `TabIndicators` values over a presentation —
/// a third owner's bit stored there would be erased by every such write (the
/// latching bug that struct's doc comment exists to prevent). The role is
/// instead stamped per TAB by `App::stamp_tab_connection_roles` inside the one
/// strip-refresh funnel, so it can never fight the indicator owners.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) enum TabConnRole {
    Outbound,
    Inbound,
    Both,
}

impl TabConnRole {
    /// The role the two per-session predicate flags spell; `None` = no mark.
    #[must_use]
    pub(crate) const fn from_flags(outbound: bool, inbound: bool) -> Option<Self> {
        match (outbound, inbound) {
            (true, true) => Some(Self::Both),
            (true, false) => Some(Self::Outbound),
            (false, true) => Some(Self::Inbound),
            (false, false) => None,
        }
    }

    /// This role's directional flags, `(outbound, inbound)` — the inverse of
    /// [`Self::from_flags`], so folds can union in flag space.
    #[must_use]
    pub(crate) const fn flags(self) -> (bool, bool) {
        match self {
            Self::Outbound => (true, false),
            Self::Inbound => (false, true),
            Self::Both => (true, true),
        }
    }

    /// OR-fold two optional roles (the [`aggregate_presentations`] discipline:
    /// one pane's role never hides another's — outbound + inbound = both).
    #[must_use]
    pub(crate) const fn union(a: Option<Self>, b: Option<Self>) -> Option<Self> {
        let (ao, ai) = match a {
            Some(role) => role.flags(),
            None => (false, false),
        };
        let (bo, bi) = match b {
            Some(role) => role.flags(),
            None => (false, false),
        };
        Self::from_flags(ao || bo, ai || bi)
    }

    /// Stable state tokens for the `chrome` introspection line (`format_tab_
    /// chrome`). `Both` reports BOTH directional tokens rather than a third
    /// spelling, so a script greps `conn-out`/`conn-in` and never misses a
    /// direction (§6: marks are never visual-only).
    #[must_use]
    pub(crate) const fn chrome_states(self) -> &'static [&'static str] {
        match self {
            Self::Outbound => &["conn-out"],
            Self::Inbound => &["conn-in"],
            Self::Both => &["conn-out", "conn-in"],
        }
    }
}

/// Chrome metadata computed from tab content rather than from a PTY mirror.
#[derive(Clone, PartialEq, Eq, Debug)]
pub(crate) struct TabPresentation {
    pub(crate) title: String,
    pub(crate) icon: Option<TabIconKind>,
    pub(crate) indicators: TabIndicators,
    /// The tab's connection-mark role (design §4), stamped from the live edge
    /// tables by the strip-refresh funnel — see [`TabConnRole`] for why this is
    /// not an indicator bit. `None` = no live connection touches this tab's
    /// sessions (or the `tab_connection_badge` opt-out quieted the mark).
    pub(crate) conn: Option<TabConnRole>,
    pub(crate) closable: bool,
    pub(crate) tooltip: Option<String>,
}

impl TabPresentation {
    #[must_use]
    pub(crate) fn terminal(title: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            icon: None,
            indicators: TabIndicators::default(),
            conn: None,
            closable: true,
            tooltip: None,
        }
    }
}

/// Aggregate split-leaf presentation without letting one state hide another.
/// The focused leaf supplies the human title/icon; independent dirty/busy/
/// attention bits are ORed across every visible sibling, and the tab is closable
/// only when every leaf is ready to close.
#[must_use]
pub(crate) fn aggregate_presentations(
    focused: ViewId,
    presentations: impl IntoIterator<Item = (ViewId, TabPresentation)>,
) -> Option<TabPresentation> {
    let mut focused_presentation = None;
    let mut fallback = None;
    let mut indicators = TabIndicators::default();
    let mut conn = None;
    let mut closable = true;
    let mut count = 0usize;
    for (view, presentation) in presentations {
        count += 1;
        indicators.dirty |= presentation.indicators.dirty;
        indicators.busy |= presentation.indicators.busy;
        // Both attention owners fold independently, so a native leaf's
        // out-of-band mark and a terminal leaf's failure can neither hide nor
        // erase one another.
        indicators.attention |= presentation.indicators.attention;
        indicators.status_attention |= presentation.indicators.status_attention;
        // Directional union: a background pane's outbound role and the focused
        // pane's inbound role read as `Both`, never as whichever pane won.
        conn = TabConnRole::union(conn, presentation.conn);
        closable &= presentation.closable;
        if fallback.is_none() {
            fallback = Some(presentation.clone());
        }
        if view == focused {
            focused_presentation = Some(presentation);
        }
    }
    let mut result = focused_presentation.or(fallback)?;
    result.indicators = indicators;
    result.conn = conn;
    result.closable = closable;
    if count > 1 {
        result.tooltip = Some(match result.tooltip {
            Some(tooltip) => format!("{tooltip} · {count} views"),
            None => format!("{count} views"),
        });
    }
    Some(result)
}

/// One stable tab and its generic split content.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct Tab {
    pub(crate) id: TabId,
    pub(crate) root: SplitTree<ViewId>,
    pub(crate) focus: ViewId,
    pub(crate) zoomed: bool,
    pub(crate) presentation: TabPresentation,
}

impl Tab {
    #[must_use]
    pub(crate) fn new(id: TabId, view: ViewId, presentation: TabPresentation) -> Self {
        Self {
            id,
            root: SplitTree::leaf(view),
            focus: view,
            zoomed: false,
            presentation,
        }
    }

    #[must_use]
    pub(crate) fn from_root(
        id: TabId,
        root: SplitTree<ViewId>,
        focus: ViewId,
        zoomed: bool,
        presentation: TabPresentation,
    ) -> Self {
        Self {
            id,
            root,
            focus,
            zoomed,
            presentation,
        }
    }

    /// Resolve the canonical visible topology once for every frame consumer.
    /// Zoom is a presentation transform over the unchanged tree: only the
    /// focused leaf is planned, at the root path, and unzoom restores the exact
    /// prior topology and ratios.
    #[must_use]
    pub(crate) fn visible_plan(
        &self,
        bounds: LogicalRect,
        divider: f32,
        sizing: impl Fn(ViewId) -> LeafSizing,
    ) -> VisibleLeafPlan {
        let bounds = sanitize_rect(bounds);
        if self.zoomed && self.root.len() > 1 {
            let leaf_sizing = sizing(self.focus).sanitized();
            return VisibleLeafPlan {
                tab: self.id,
                bounds,
                leaves: vec![VisibleLeaf {
                    path: SplitPath::root(),
                    view: self.focus,
                    rect: bounds,
                    sizing: leaf_sizing,
                    focused: true,
                }],
                dividers: Vec::new(),
                focused: self.focus,
                zoomed: true,
                divider: divider.max(0.0),
            };
        }
        let mut leaves = Vec::with_capacity(self.root.len());
        let mut dividers = Vec::with_capacity(self.root.len().saturating_sub(1));
        plan_into(
            &self.root,
            bounds,
            divider.max(0.0),
            &sizing,
            self.focus,
            &SplitPath::root(),
            &mut leaves,
            &mut dividers,
        );
        VisibleLeafPlan {
            tab: self.id,
            bounds,
            leaves,
            dividers,
            focused: self.focus,
            zoomed: false,
            divider: divider.max(0.0),
        }
    }

    /// Deterministically insert `new_view` as the second (right/bottom) sibling
    /// of the focused leaf and move focus to it.
    pub(crate) fn split_focused(&mut self, axis: SplitAxis, new_view: ViewId) -> bool {
        if self.root.contains(new_view) || !self.root.split_leaf(self.focus, axis, new_view) {
            return false;
        }
        self.focus = new_view;
        self.zoomed = false;
        true
    }

    /// Focus one live leaf by stable identity. Stale delayed requests fail closed.
    pub(crate) fn set_focus(&mut self, view: ViewId) -> bool {
        if !self.root.contains(view) {
            return false;
        }
        self.focus = view;
        true
    }

    /// Remove one leaf and repair focus to the nearest deterministic survivor.
    /// The final leaf is left in place so the tab owner can run its close
    /// transaction before removing the whole tab.
    pub(crate) fn remove_view(&mut self, view: ViewId) -> RemoveLeaf {
        let removed = self.root.remove_leaf(view);
        if removed == RemoveLeaf::Removed {
            if self.focus == view || !self.root.contains(self.focus) {
                self.focus = self.root.first_leaf();
            }
            self.zoomed = false;
        }
        removed
    }

    /// Toggle focused-leaf zoom. Single-leaf tabs remain unzoomed.
    pub(crate) fn toggle_zoom(&mut self) -> bool {
        self.zoomed = !self.zoomed && self.root.len() > 1;
        self.zoomed
    }

    /// Move focus geometrically, independent of leaf content kind.
    pub(crate) fn focus_neighbor(
        &mut self,
        direction: FocusDirection,
        plan: &VisibleLeafPlan,
    ) -> bool {
        if plan.zoomed || plan.leaves.len() <= 1 {
            return false;
        }
        let Some(current) = plan.leaf(self.focus).map(|leaf| leaf.rect) else {
            return false;
        };
        let cur_right = current.origin.x + current.size.width;
        let cur_bottom = current.origin.y + current.size.height;
        let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| (a1.min(b1) - a0.max(b0)).max(0.0);
        let mut best: Option<(ViewId, f32, f32, f32)> = None;
        for leaf in &plan.leaves {
            if leaf.view == self.focus {
                continue;
            }
            let rect = leaf.rect;
            let right = rect.origin.x + rect.size.width;
            let bottom = rect.origin.y + rect.size.height;
            let (on_side, distance, shared, offset) = match direction {
                FocusDirection::Left => (
                    right <= current.origin.x,
                    current.origin.x - right,
                    overlap(current.origin.y, cur_bottom, rect.origin.y, bottom),
                    rect.origin.y,
                ),
                FocusDirection::Right => (
                    rect.origin.x >= cur_right,
                    rect.origin.x - cur_right,
                    overlap(current.origin.y, cur_bottom, rect.origin.y, bottom),
                    rect.origin.y,
                ),
                FocusDirection::Up => (
                    bottom <= current.origin.y,
                    current.origin.y - bottom,
                    overlap(current.origin.x, cur_right, rect.origin.x, right),
                    rect.origin.x,
                ),
                FocusDirection::Down => (
                    rect.origin.y >= cur_bottom,
                    rect.origin.y - cur_bottom,
                    overlap(current.origin.x, cur_right, rect.origin.x, right),
                    rect.origin.x,
                ),
            };
            if !on_side || shared <= 0.0 {
                continue;
            }
            let better = best.is_none_or(|(_, best_distance, best_shared, best_offset)| {
                distance < best_distance
                    || (distance == best_distance && shared > best_shared)
                    || (distance == best_distance && shared == best_shared && offset < best_offset)
            });
            if better {
                best = Some((leaf.view, distance, shared, offset));
            }
        }
        best.is_some_and(|(view, _, _, _)| self.set_focus(view))
    }

    /// Apply a divider ratio only while its stable path still names the same
    /// split. A structural mutation invalidates the path and leaves the tree
    /// unchanged.
    pub(crate) fn set_divider_ratio(&mut self, path: &SplitPath, ratio: f32) -> bool {
        let Some(SplitTree::Split { ratio: current, .. }) = node_at_path_mut(&mut self.root, path)
        else {
            return false;
        };
        *current = ratio.clamp(0.05, 0.95);
        true
    }

    /// Structural invariant consumed by window-level debug assertions and tests.
    #[must_use]
    pub(crate) fn invariant_holds(&self, views: &ViewStore) -> bool {
        self.root.contains(self.focus)
            && self
                .root
                .leaves()
                .into_iter()
                .all(|view| views.contains(view))
    }
}

/// Ordered stable tabs with active identity.  Vector positions remain a chrome
/// concern and never escape as durable identity.
#[derive(Clone, PartialEq, Debug, Default)]
pub(crate) struct TabSet {
    active: Option<TabId>,
    tabs: Vec<Tab>,
}

impl TabSet {
    #[must_use]
    pub(crate) fn new(first: Tab) -> Self {
        Self {
            active: Some(first.id),
            tabs: vec![first],
        }
    }

    #[must_use]
    pub(crate) fn len(&self) -> usize {
        self.tabs.len()
    }

    #[must_use]
    #[allow(dead_code, reason = "native-only window lifecycle query")]
    pub(crate) fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    #[must_use]
    #[allow(dead_code, reason = "stable control/restore identity query")]
    pub(crate) fn active_id(&self) -> Option<TabId> {
        self.active
    }

    #[must_use]
    pub(crate) fn active_index(&self) -> Option<usize> {
        let active = self.active?;
        self.tabs.iter().position(|tab| tab.id == active)
    }

    #[must_use]
    pub(crate) fn active(&self) -> Option<&Tab> {
        self.active_index().and_then(|index| self.tabs.get(index))
    }

    #[allow(dead_code, reason = "native presentation refresh mutation seam")]
    pub(crate) fn active_mut(&mut self) -> Option<&mut Tab> {
        let index = self.active_index()?;
        self.tabs.get_mut(index)
    }

    #[must_use]
    pub(crate) fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    #[must_use]
    pub(crate) fn tab_at(&self, index: usize) -> Option<&Tab> {
        self.tabs.get(index)
    }

    pub(crate) fn tab_at_mut(&mut self, index: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(index)
    }

    #[must_use]
    pub(crate) fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// Append a tab and focus it.
    pub(crate) fn push(&mut self, tab: Tab) -> Result<usize, DuplicateTabId> {
        if self.get(tab.id).is_some() {
            return Err(DuplicateTabId);
        }
        let id = tab.id;
        self.tabs.push(tab);
        self.active = Some(id);
        Ok(self.tabs.len() - 1)
    }

    pub(crate) fn switch_to(&mut self, id: TabId) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        self.active = Some(id);
        true
    }

    pub(crate) fn switch_to_index(&mut self, index: usize) -> bool {
        let Some(id) = self.tabs.get(index).map(|tab| tab.id) else {
            return false;
        };
        self.active = Some(id);
        true
    }

    pub(crate) fn cycle(&mut self, forward: bool) -> Option<TabId> {
        let count = self.tabs.len();
        if count == 0 {
            self.active = None;
            return None;
        }
        let current = self.active_index().unwrap_or(0);
        let next = if count == 1 {
            current
        } else if forward {
            (current + 1) % count
        } else {
            (current + count - 1) % count
        };
        let id = self.tabs[next].id;
        self.active = Some(id);
        Some(id)
    }

    /// Remove a tab and keep the same logical active tab when possible.  When
    /// removing the active tab, select the tab now at that position, else the
    /// preceding final tab.
    pub(crate) fn remove(&mut self, id: TabId) -> Option<Tab> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        let was_active = self.active == Some(id);
        let removed = self.tabs.remove(index);
        if self.tabs.is_empty() {
            self.active = None;
        } else if was_active {
            self.active = Some(self.tabs[index.min(self.tabs.len() - 1)].id);
        }
        Some(removed)
    }

    /// Move a stable tab to a chrome index without changing which tab is active.
    pub(crate) fn reorder(&mut self, id: TabId, to: usize) -> bool {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        if to >= self.tabs.len() || from == to {
            return to < self.tabs.len();
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        true
    }

    #[must_use]
    pub(crate) fn invariant_holds(&self, views: &ViewStore) -> bool {
        if self.tabs.is_empty() {
            return self.active.is_none();
        }
        let Some(active) = self.active else {
            return false;
        };
        self.tabs.iter().filter(|tab| tab.id == active).count() == 1
            && self.tabs.iter().all(|tab| tab.invariant_holds(views))
            && self.tabs.iter().enumerate().all(|(index, tab)| {
                self.tabs[..index]
                    .iter()
                    .all(|earlier| earlier.id != tab.id)
            })
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct DuplicateTabId;

#[cfg(test)]
mod tests {
    use super::*;

    fn view_store_with(count: usize) -> (ViewStore, Vec<ViewId>) {
        let mut store = ViewStore::default();
        let ids = (0..count)
            .map(|session| {
                store
                    .insert_terminal(session as u64)
                    .expect("finite test id space")
            })
            .collect();
        (store, ids)
    }

    #[test]
    fn view_ids_are_stable_and_never_reused() {
        let mut store = ViewStore::default();
        let first = store.insert_terminal(9).expect("id");
        assert_eq!(
            store.remove(first).and_then(View::terminal_session),
            Some(9)
        );
        let second = store.insert_terminal(9).expect("id");
        assert_ne!(
            first, second,
            "a delayed event must not alias a replacement"
        );
        assert_eq!(
            store.get(second).copied().and_then(View::terminal_session),
            Some(9)
        );
    }

    #[test]
    fn restore_rejects_duplicates_and_advances_allocator() {
        let mut store = ViewStore::default();
        let restored = ViewId::from_stored(41);
        store
            .restore(restored, View::Terminal(TerminalView { session: 7 }))
            .expect("restore");
        assert_eq!(
            store.restore(restored, View::Terminal(TerminalView { session: 8 })),
            Err(RestoreIdError::Duplicate)
        );
        let next = store.insert_terminal(9).expect("id after restore");
        assert_eq!(next.get(), 42);
        assert_eq!(
            store
                .get(restored)
                .copied()
                .and_then(View::terminal_session),
            Some(7)
        );
    }

    #[test]
    fn split_tree_layout_hit_and_neighbor_share_logical_rects() {
        let mut tree = SplitTree::leaf(1u64);
        assert!(tree.split_leaf(1, SplitAxis::Horizontal, 2));
        assert!(tree.split_leaf(2, SplitAxis::Vertical, 3));
        let bounds = LogicalRect::new(0.0, 0.0, 801.0, 601.0);
        let leaves = tree.layout(bounds, 1.0, 1.0);
        assert_eq!(leaves.len(), 3);
        for leaf in &leaves {
            let centre = LogicalPoint {
                x: leaf.rect.origin.x + leaf.rect.size.width * 0.5,
                y: leaf.rect.origin.y + leaf.rect.size.height * 0.5,
            };
            assert_eq!(tree.leaf_at(centre, bounds, 1.0, 1.0), Some(leaf.value));
        }
        assert_eq!(
            tree.neighbor(3, FocusDirection::Up, bounds, 1.0, 1.0),
            Some(2)
        );
        assert_eq!(
            tree.neighbor(3, FocusDirection::Left, bounds, 1.0, 1.0),
            Some(1)
        );
    }

    #[test]
    fn divider_hit_drives_the_same_split_and_stale_path_fails_closed() {
        let mut tree = SplitTree::leaf(1u64);
        assert!(tree.split_leaf(1, SplitAxis::Horizontal, 2));
        let bounds = LogicalRect::new(0.0, 0.0, 80.0, 24.0);
        let hit = tree
            .divider_at(LogicalPoint { x: 40.0, y: 5.0 }, bounds, 1.0, 1.0)
            .expect("root divider");
        let ratio =
            SplitTree::<u64>::ratio_for_pointer(&hit, LogicalPoint { x: 20.0, y: 5.0 }, 1.0)
                .expect("ratio");
        assert!(tree.set_divider_ratio(&hit, ratio));
        let layout = tree.layout(bounds, 1.0, 1.0);
        assert_eq!(layout[0].rect.size.width, 20.0);

        assert_eq!(tree.remove_leaf(2), RemoveLeaf::Removed);
        assert!(!tree.set_divider_ratio(&hit, 0.8));
    }

    #[test]
    fn collapsed_leaf_placement_round_trips_exact_nested_topology() {
        let mut tree = SplitTree::leaf(1u64);
        assert!(tree.split_leaf(1, SplitAxis::Horizontal, 2));
        assert!(tree.split_leaf(2, SplitAxis::Vertical, 3));
        let expected = tree.clone();
        let (path, branch, axis, ratio) = tree.leaf_placement(3).expect("placement");
        assert_eq!(path.branches(), &[SplitBranch::Second]);
        assert_eq!(branch, SplitBranch::Second);
        assert_eq!(tree.remove_leaf(3), RemoveLeaf::Removed);
        assert!(tree.restore_collapsed_leaf(&path, branch, axis, ratio, 3));
        assert_eq!(tree, expected);
        assert!(!tree.restore_collapsed_leaf(&path, branch, axis, ratio, 3));
    }

    #[test]
    fn tab_set_tracks_stable_active_identity_through_reorder_and_close() {
        let (store, views) = view_store_with(3);
        let mut tab_ids = IdAllocator::<TabId>::default();
        let make = |id, view| Tab::new(id, view, TabPresentation::terminal("shell"));
        let first = make(tab_ids.allocate().expect("id"), views[0]);
        let first_id = first.id;
        let mut tabs = TabSet::new(first);
        let second = make(tab_ids.allocate().expect("id"), views[1]);
        let second_id = second.id;
        tabs.push(second).expect("unique");
        let third = make(tab_ids.allocate().expect("id"), views[2]);
        let third_id = third.id;
        tabs.push(third).expect("unique");
        assert_eq!(tabs.active_id(), Some(third_id));

        assert!(tabs.reorder(third_id, 0));
        assert_eq!(tabs.active_id(), Some(third_id));
        assert_eq!(tabs.active_index(), Some(0));
        assert!(tabs.switch_to(second_id));
        assert_eq!(tabs.remove(first_id).map(|tab| tab.id), Some(first_id));
        assert_eq!(tabs.active_id(), Some(second_id));
        assert!(tabs.invariant_holds(&store));
    }

    #[test]
    fn visible_plan_keeps_heterogeneous_leaves_paths_and_hits_in_lockstep() {
        let mut store = ViewStore::default();
        let terminal = store.insert_terminal(11).expect("terminal view");
        let native = store
            .insert_native(AppInstanceId::from_stored(7))
            .expect("native view");
        let second_terminal = store.insert_terminal(12).expect("terminal view");
        let mut tab = Tab::new(
            TabId::from_stored(3),
            terminal,
            TabPresentation::terminal("shell"),
        );
        assert!(tab.split_focused(SplitAxis::Horizontal, native));
        assert!(tab.split_focused(SplitAxis::Vertical, second_terminal));
        let plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 121.0, 61.0), 1.0, |view| {
            if view == native {
                LeafSizing::new(LogicalSize::new(30.0, 12.0), LogicalSize::new(72.0, 36.0))
            } else {
                LeafSizing::new(LogicalSize::new(2.0, 1.0), LogicalSize::new(80.0, 24.0))
            }
        });
        assert_eq!(plan.leaves.len(), 3);
        assert_eq!(plan.dividers.len(), 2);
        assert_eq!(plan.leaves[0].path.branches(), &[SplitBranch::First]);
        assert_eq!(
            plan.leaves[1].path.branches(),
            &[SplitBranch::Second, SplitBranch::First]
        );
        assert_eq!(
            plan.leaves[2].path.branches(),
            &[SplitBranch::Second, SplitBranch::Second]
        );
        for leaf in &plan.leaves {
            let point = LogicalPoint {
                x: leaf.rect.origin.x + leaf.rect.size.width * 0.5,
                y: leaf.rect.origin.y + leaf.rect.size.height * 0.5,
            };
            assert_eq!(plan.leaf_at(point).map(|hit| hit.view), Some(leaf.view));
        }
        assert!(plan.leaf(second_terminal).is_some_and(|leaf| leaf.focused));
        assert!(tab.invariant_holds(&store));
    }

    #[test]
    fn impossible_minimum_fit_preserves_every_leaf_and_stale_divider_fails_closed() {
        let (_, views) = view_store_with(3);
        let mut tab = Tab::new(
            TabId::from_stored(1),
            views[0],
            TabPresentation::terminal("one"),
        );
        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
        let tiny = tab.visible_plan(LogicalRect::new(0.0, 0.0, 3.0, 2.0), 1.0, |_| {
            LeafSizing::new(LogicalSize::new(20.0, 10.0), LogicalSize::new(80.0, 24.0))
        });
        assert_eq!(tiny.leaves.len(), 3, "shortage never drops a view");
        let nested = tiny
            .dividers
            .iter()
            .find(|divider| !divider.path.branches().is_empty())
            .expect("nested divider")
            .path
            .clone();
        assert_eq!(tab.remove_view(views[2]), RemoveLeaf::Removed);
        let before = tab.root.clone();
        assert!(!tab.set_divider_ratio(&nested, 0.8));
        assert_eq!(tab.root, before, "stale topology handles fail closed");
        assert_eq!(tab.focus, views[0], "close repairs focus deterministically");
    }

    #[test]
    fn zoom_focus_and_presentation_are_content_agnostic() {
        let (_, views) = view_store_with(2);
        let mut tab = Tab::new(
            TabId::from_stored(1),
            views[0],
            TabPresentation::terminal("left"),
        );
        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        let plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 80.0, 24.0), 1.0, |_| {
            LeafSizing::new(LogicalSize::new(1.0, 1.0), LogicalSize::new(10.0, 10.0))
        });
        assert!(tab.focus_neighbor(FocusDirection::Left, &plan));
        assert_eq!(tab.focus, views[0]);
        assert!(tab.toggle_zoom());
        let zoom = tab.visible_plan(LogicalRect::new(0.0, 0.0, 80.0, 24.0), 1.0, |_| {
            LeafSizing::new(LogicalSize::new(1.0, 1.0), LogicalSize::new(10.0, 10.0))
        });
        assert_eq!(zoom.leaves.len(), 1);
        assert_eq!(zoom.leaves[0].view, views[0]);
        assert_eq!(zoom.leaves[0].rect, zoom.bounds);

        let aggregate = aggregate_presentations(
            views[0],
            [
                (
                    views[0],
                    TabPresentation {
                        title: "Focused".to_string(),
                        icon: None,
                        indicators: TabIndicators {
                            dirty: false,
                            busy: true,
                            attention: false,
                            status_attention: false,
                        },
                        conn: Some(TabConnRole::Inbound),
                        closable: true,
                        tooltip: Some("primary".to_string()),
                    },
                ),
                (
                    views[1],
                    TabPresentation {
                        title: "Sibling".to_string(),
                        icon: Some(TabIconKind::Editor),
                        indicators: TabIndicators {
                            dirty: true,
                            busy: false,
                            attention: true,
                            // The classifier's own bit folds independently, so a
                            // sibling's terminal failure cannot be hidden by, or
                            // hide, this leaf's out-of-band mark.
                            status_attention: true,
                        },
                        // Unions with the focused pane's Inbound to Both below.
                        conn: Some(TabConnRole::Outbound),
                        closable: false,
                        tooltip: None,
                    },
                ),
            ],
        )
        .expect("aggregate");
        assert_eq!(aggregate.title, "Focused");
        assert_eq!(
            aggregate.indicators,
            TabIndicators {
                dirty: true,
                busy: true,
                attention: true,
                status_attention: true,
            }
        );
        assert_eq!(aggregate.conn, Some(TabConnRole::Both));
        assert!(!aggregate.closable);
    }

    #[test]
    fn heterogeneous_tab_mutators_refine_pane_tree_model_with_negative_control() {
        use aterm_spec::derive::pane_tree_model;
        use aterm_spec::interp::{State, admits};

        fn project(tab: &Tab) -> State {
            let leaves = tab.root.leaves();
            let focused = leaves
                .iter()
                .position(|view| *view == tab.focus)
                .expect("focused leaf is live");
            State::from([
                ("leaf_count", leaves.len() as i64),
                ("focused", focused as i64),
            ])
        }

        let (_, views) = view_store_with(3);
        let model = pane_tree_model();
        let mut tab = Tab::new(
            TabId::from_stored(1),
            views[0],
            TabPresentation::terminal("one"),
        );
        let initial = project(&tab);
        assert_eq!(initial, model.init_state());

        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        let split_once = project(&tab);
        assert_eq!(admits(&model, &initial, &split_once), Some("Split"));
        assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
        let split_twice = project(&tab);
        assert_eq!(admits(&model, &split_once, &split_twice), Some("Split"));
        assert_eq!(tab.remove_view(views[2]), RemoveLeaf::Removed);
        let closed = project(&tab);
        assert!(
            model.successors("Close", &split_twice).contains(&closed),
            "real generic close is one admitted nondeterministic focus repair"
        );

        let mut dangling = closed.clone();
        dangling.insert("focused", dangling["leaf_count"]);
        assert_eq!(admits(&model, &split_twice, &dangling), None);
        assert!(!model.check_invariant("FocusInRange", &dangling));
    }

    /// `any_leaf` replaces `leaves().into_iter().any(..)` on the wake/present paths, so it
    /// has to agree with it on EVERY leaf of a nested tree — including the short-circuit
    /// point, which is what makes the two orderings observationally the same.
    ///
    /// The reference side is spelled `leaves.contains(&probe)`: for a slice that IS
    /// `iter().any(|leaf| leaf == &probe)` by definition, so it is the same oracle written
    /// the way the standard library wants an equality search written.
    #[test]
    fn any_leaf_agrees_with_leaves_any_and_stops_at_the_first_match() {
        let (_, views) = view_store_with(3);
        let mut tab = Tab::new(
            TabId::from_stored(1),
            views[0],
            TabPresentation::terminal("one"),
        );
        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
        let leaves = tab.root.leaves();
        assert_eq!(leaves.len(), 3, "a nested three-pane tree, not a lone leaf");

        for probe in views.iter().copied().chain([ViewId::from_stored(9999)]) {
            assert_eq!(
                tab.root.any_leaf(&mut |view| *view == probe),
                leaves.contains(&probe),
                "any_leaf disagrees with leaves().contains() on {probe}"
            );
        }

        // Short-circuit: a predicate true for the FIRST visited leaf must not visit a second.
        let mut seen = Vec::new();
        assert!(tab.root.any_leaf(&mut |view| {
            seen.push(*view);
            true
        }));
        assert_eq!(
            seen,
            leaves[..1],
            "stopped at the first match, in visit order"
        );
    }

    #[test]
    fn tab_and_view_id_types_are_not_interchangeable() {
        let tab = TabId::from_stored(7);
        let view = ViewId::from_stored(7);
        assert_eq!(tab.get(), view.get());
        // The compile-time distinction is the property; this runtime assertion
        // documents that equal wire values do not imply equal domain identity.
        assert_eq!(tab.to_string(), view.to_string());
    }
}
