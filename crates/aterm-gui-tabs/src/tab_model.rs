// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Content-agnostic tab, view, and split-tree identities and logical-pixel split geometry.

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
        pub struct $name(u64);

        impl $name {
            /// The stable integer representation used by restore and control serialization.
            #[must_use]
            #[inline]
            pub const fn get(self) -> u64 {
                self.0
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

/// Reconstitute an identity at a validated restore boundary.
macro_rules! from_stored {
    ($name:ident) => {
        impl $name {
            #[must_use]
            #[inline]
            pub const fn from_stored(raw: u64) -> Self {
                Self(raw)
            }
        }
    };
}

from_stored!(ViewId);
#[cfg(any(target_os = "macos", test, debug_assertions))]
from_stored!(TabId);
#[cfg(any(test, debug_assertions))]
from_stored!(AppInstanceId);

/// The stable-id space has been exhausted.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct IdExhausted;

/// Monotonic, never-reusing identity source.
#[derive(Clone, Debug)]
pub struct IdAllocator<I> {
    next: u64,
    _id: PhantomData<fn() -> I>,
}

impl<I> Default for IdAllocator<I> {
    fn default() -> Self {
        Self {
            next: 1,
            _id: PhantomData,
        }
    }
}

pub trait StableId: Copy {
    fn from_raw(raw: u64) -> Self;
}

macro_rules! stable_id_impl {
    ($name:ident) => {
        impl StableId for $name {
            #[inline]
            fn from_raw(raw: u64) -> Self {
                Self(raw)
            }
        }
    };
}

stable_id_impl!(TabId);
stable_id_impl!(ViewId);
stable_id_impl!(AppInstanceId);

impl<I: StableId> IdAllocator<I> {
    /// Mint the next identity.
    #[inline]
    pub fn allocate(&mut self) -> Result<I, IdExhausted> {
        let raw = self.next;
        self.next = raw.checked_add(1).ok_or(IdExhausted)?;
        Ok(I::from_raw(raw))
    }
}

/// A terminal view's typed link into the process-wide session pool.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct TerminalView {
    pub session: u64,
}

/// The core-owned link for a native view.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct NativeViewRef {
    pub instance: AppInstanceId,
}

/// Content of one split leaf.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum View {
    Terminal(TerminalView),
    Native(NativeViewRef),
}

impl View {
    /// Resolve the PTY session only for a terminal view.
    #[must_use]
    #[inline]
    pub const fn terminal_session(self) -> Option<u64> {
        match self {
            Self::Terminal(view) => Some(view.session),
            Self::Native(_) => None,
        }
    }
}

/// Process-wide owner of view identities and their typed content links.
#[derive(Debug, Default)]
pub struct ViewStore {
    ids: IdAllocator<ViewId>,
    views: HashMap<ViewId, View>,
}

impl ViewStore {
    /// Insert a new terminal view over `session`.
    #[inline]
    pub fn insert_terminal(&mut self, session: u64) -> Result<ViewId, IdExhausted> {
        self.insert(View::Terminal(TerminalView { session }))
    }

    /// Insert a new native view linked to `instance`.
    #[inline]
    pub fn insert_native(&mut self, instance: AppInstanceId) -> Result<ViewId, IdExhausted> {
        self.insert(View::Native(NativeViewRef { instance }))
    }

    fn insert(&mut self, view: View) -> Result<ViewId, IdExhausted> {
        let id = self.ids.allocate()?;
        let previous = self.views.insert(id, view);
        debug_assert!(previous.is_none(), "monotonic ViewId collided");
        Ok(id)
    }

    #[must_use]
    #[inline]
    pub fn get(&self, id: ViewId) -> Option<&View> {
        self.views.get(&id)
    }

    #[must_use]
    #[inline]
    pub fn contains(&self, id: ViewId) -> bool {
        self.views.contains_key(&id)
    }

    /// Remove a view.
    #[inline]
    pub fn remove(&mut self, id: ViewId) -> Option<View> {
        self.views.remove(&id)
    }

    #[inline]
    pub fn iter(&self) -> impl Iterator<Item = (ViewId, View)> + '_ {
        self.views.iter().map(|(id, view)| (*id, *view))
    }

    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub fn len(&self) -> usize {
        self.views.len()
    }

    /// Whether the store holds no view.
    #[cfg(any(test, debug_assertions))]
    #[must_use]
    #[inline]
    pub fn is_empty(&self) -> bool {
        self.views.is_empty()
    }
}

/// Split orientation in content coordinates.
#[derive(Clone, Copy, PartialEq, Eq, Debug, serde::Serialize, serde::Deserialize)]
pub enum SplitAxis {
    /// First child left, second child right.
    Horizontal,
    /// First child above, second child below.
    Vertical,
}

/// One node in a content-agnostic split tree.
#[derive(Clone, PartialEq, Debug)]
pub enum SplitTree<T> {
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
    #[inline]
    pub const fn leaf(value: T) -> Self {
        Self::Leaf(value)
    }

    #[must_use]
    #[inline]
    #[allow(
        clippy::len_without_is_empty,
        reason = "a split tree always holds at least one leaf"
    )]
    pub fn len(&self) -> usize {
        match self {
            Self::Leaf(_) => 1,
            Self::Split { first, second, .. } => first.len() + second.len(),
        }
    }

    pub fn visit(&self, out: &mut impl FnMut(&T)) {
        match self {
            Self::Leaf(value) => out(value),
            Self::Split { first, second, .. } => {
                first.visit(out);
                second.visit(out);
            }
        }
    }

    /// Allocation-free, short-circuiting predicate over leaves in [`Self::visit`] order.
    pub fn any_leaf(&self, pred: &mut impl FnMut(&T) -> bool) -> bool {
        match self {
            Self::Leaf(value) => pred(value),
            Self::Split { first, second, .. } => first.any_leaf(pred) || second.any_leaf(pred),
        }
    }

    /// Project leaf payloads while preserving split structure and ratios.
    #[must_use]
    pub fn map<U>(&self, map: &mut impl FnMut(&T) -> U) -> SplitTree<U> {
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
    pub fn contains(&self, target: T) -> bool {
        match self {
            Self::Leaf(value) => *value == target,
            Self::Split { first, second, .. } => first.contains(target) || second.contains(target),
        }
    }

    #[must_use]
    pub fn leaves(&self) -> Vec<T> {
        let mut leaves = Vec::with_capacity(self.len());
        self.visit(&mut |value| leaves.push(*value));
        leaves
    }

    /// Replace `target` with a split of the original and `new`, or return false if stale.
    pub fn split_leaf(&mut self, target: T, axis: SplitAxis, new: T) -> bool {
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

    /// Describe the parent split that collapses when `target` is removed.
    #[must_use]
    pub fn leaf_placement(&self, target: T) -> Option<(SplitPath, SplitBranch, SplitAxis, f32)> {
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

    /// Recreate a split whose removed leaf was collapsed onto its sibling.
    pub fn restore_collapsed_leaf(
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
            ratio: if ratio.is_nan() {
                0.5
            } else {
                ratio.clamp(0.05, 0.95)
            },
            first,
            second,
        };
        true
    }

    /// Remove `target` and collapse its parent, leaving the sole leaf in place.
    pub fn remove_leaf(&mut self, target: T) -> RemoveLeaf {
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
    #[inline]
    pub fn first_leaf(&self) -> T {
        match self {
            Self::Leaf(value) => *value,
            Self::Split { first, .. } => first.first_leaf(),
        }
    }

    /// The last leaf in tree order (bottom/right-most).
    #[must_use]
    #[inline]
    pub fn last_leaf(&self) -> T {
        match self {
            Self::Leaf(value) => *value,
            Self::Split { second, .. } => second.last_leaf(),
        }
    }

    /// The sibling leaf that inherits `target`'s space when its parent collapses.
    #[must_use]
    pub fn absorbing_neighbor(&self, target: T) -> Option<T> {
        let Self::Split { first, second, .. } = self else {
            return None;
        };
        if matches!(&**first, Self::Leaf(value) if *value == target) {
            return Some(second.first_leaf());
        }
        if matches!(&**second, Self::Leaf(value) if *value == target) {
            return Some(first.last_leaf());
        }
        first
            .absorbing_neighbor(target)
            .or_else(|| second.absorbing_neighbor(target))
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RemoveLeaf {
    Removed,
    OnlyLeaf,
    NotFound,
}

/// Logical point in a tab's content coordinate space.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LogicalPoint {
    pub x: f32,
    pub y: f32,
}

/// Logical extent in a tab's content coordinate space.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LogicalSize {
    pub width: f32,
    pub height: f32,
}

impl LogicalSize {
    #[must_use]
    #[inline]
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// Half-open logical rectangle.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
pub struct LogicalRect {
    pub origin: LogicalPoint,
    pub size: LogicalSize,
}

impl LogicalRect {
    #[must_use]
    #[inline]
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            origin: LogicalPoint { x, y },
            size: LogicalSize { width, height },
        }
    }

    #[must_use]
    #[inline]
    pub const fn contains(self, point: LogicalPoint) -> bool {
        point.x >= self.origin.x
            && point.y >= self.origin.y
            && point.x < self.origin.x + self.size.width
            && point.y < self.origin.y + self.size.height
    }
}

/// One laid-out split leaf.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LayoutLeaf<T> {
    pub value: T,
    pub rect: LogicalRect,
}

/// One leaf's sizing contract in the tab's logical coordinate space.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct LeafSizing {
    pub minimum: LogicalSize,
    pub preferred: LogicalSize,
}

impl LeafSizing {
    #[must_use]
    #[inline]
    pub const fn new(minimum: LogicalSize, preferred: LogicalSize) -> Self {
        Self { minimum, preferred }
    }

    #[must_use]
    #[inline]
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

/// A stable root-to-node branch.
#[derive(
    Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug, serde::Serialize, serde::Deserialize,
)]
pub enum SplitBranch {
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
pub struct SplitPath(Vec<SplitBranch>);

impl SplitPath {
    #[must_use]
    #[inline]
    pub fn root() -> Self {
        Self::default()
    }

    #[must_use]
    #[inline]
    pub fn branches(&self) -> &[SplitBranch] {
        &self.0
    }

    #[must_use]
    #[inline]
    pub const fn from_branches(branches: Vec<SplitBranch>) -> Self {
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
pub struct VisibleLeaf {
    pub path: SplitPath,
    pub view: ViewId,
    /// Host-owned subtab title row, outside the content's coordinate space.
    pub header: Option<LogicalRect>,
    pub rect: LogicalRect,
    pub sizing: LeafSizing,
    pub focused: bool,
}

impl VisibleLeaf {
    /// The complete pane allocation, including its optional subtab header.
    #[must_use]
    #[inline]
    pub fn outer_rect(&self) -> LogicalRect {
        self.header.map_or(self.rect, |header| {
            LogicalRect::new(
                self.rect.origin.x,
                header.origin.y,
                self.rect.size.width,
                header.size.height + self.rect.size.height,
            )
        })
    }
}

/// Reserve one cell row for a split pane's title when at least one content row remains.
#[must_use]
#[inline]
pub fn pane_header_rect(rect: LogicalRect) -> Option<LogicalRect> {
    (rect.size.height >= 2.0 && rect.size.width > 0.0)
        .then(|| LogicalRect::new(rect.origin.x, rect.origin.y, rect.size.width, 1.0))
}

/// One visible divider and the span bounds needed to map pointer position to ratio.
#[derive(Clone, PartialEq, Debug)]
pub struct VisibleDivider {
    pub path: SplitPath,
    pub axis: SplitAxis,
    pub rect: LogicalRect,
    span_origin: f32,
    span_extent: f32,
}

/// A frame-stable, content-agnostic layout plan.
#[derive(Clone, PartialEq, Debug)]
pub struct VisibleLeafPlan {
    pub tab: TabId,
    pub bounds: LogicalRect,
    pub leaves: Vec<VisibleLeaf>,
    pub dividers: Vec<VisibleDivider>,
    pub focused: ViewId,
    pub zoomed: bool,
    pub divider: f32,
}

impl VisibleLeafPlan {
    /// Reserve host chrome only when siblings are visible.
    pub fn reserve_pane_headers(&mut self) {
        if self.leaves.len() <= 1 {
            return;
        }
        for leaf in &mut self.leaves {
            if leaf.header.is_none()
                && let Some(header) = pane_header_rect(leaf.rect)
            {
                leaf.rect.origin.y += header.size.height;
                leaf.rect.size.height -= header.size.height;
                leaf.header = Some(header);
            }
        }
    }

    #[must_use]
    #[inline]
    pub fn leaf(&self, view: ViewId) -> Option<&VisibleLeaf> {
        self.leaves.iter().find(|leaf| leaf.view == view)
    }

    #[must_use]
    #[inline]
    pub fn leaf_at(&self, point: LogicalPoint) -> Option<&VisibleLeaf> {
        self.leaves
            .iter()
            .find(|leaf| leaf.outer_rect().contains(point))
    }

    #[must_use]
    #[inline]
    pub fn header_at(&self, point: LogicalPoint) -> Option<&VisibleLeaf> {
        self.leaves
            .iter()
            .find(|leaf| leaf.header.is_some_and(|header| header.contains(point)))
    }

    #[must_use]
    #[inline]
    pub fn divider_at(&self, point: LogicalPoint) -> Option<&VisibleDivider> {
        self.dividers
            .iter()
            .rev()
            .find(|divider| divider.rect.contains(point))
    }

    #[must_use]
    #[inline]
    pub fn ratio_for_pointer(&self, divider: &VisibleDivider, point: LogicalPoint) -> Option<f32> {
        let pointer = match divider.axis {
            SplitAxis::Horizontal => point.x,
            SplitAxis::Vertical => point.y,
        };
        if !pointer.is_finite()
            || !divider.span_origin.is_finite()
            || !divider.span_extent.is_finite()
        {
            return None;
        }
        let gap = sanitize_nonneg_finite(self.divider);
        let splittable = (divider.span_extent - gap).max(0.0);
        (splittable > f32::EPSILON)
            .then(|| ((pointer - divider.span_origin) / splittable).clamp(0.0, 1.0))
    }
}

/// Root-to-split path used as a stable divider-drag handle.
#[derive(Clone, PartialEq, Debug)]
pub struct LogicalDividerHit {
    path: SplitPath,
    pub axis: SplitAxis,
    span_origin: f32,
    span_extent: f32,
}

/// Direction for geometry-based neighboring-view focus.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FocusDirection {
    Left,
    Right,
    Up,
    Down,
}

impl<T: Copy + Eq> SplitTree<T> {
    /// Resolve every leaf in logical pixels.
    #[must_use]
    pub fn layout(&self, bounds: LogicalRect, divider: f32, min_leaf: f32) -> Vec<LayoutLeaf<T>> {
        let mut out = Vec::with_capacity(self.len());
        layout_into(self, sanitize_rect(bounds), divider, min_leaf, &mut out);
        out
    }

    #[must_use]
    pub fn neighbor(
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
    pub fn divider_at(
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
            sanitize_nonneg_finite(divider),
            sanitize_nonneg_finite(min_leaf),
            &mut path,
        )
    }

    /// Convert a pointer on the divided axis into an unclamped split ratio.
    #[must_use]
    pub fn ratio_for_pointer(
        hit: &LogicalDividerHit,
        point: LogicalPoint,
        divider: f32,
    ) -> Option<f32> {
        let pointer = match hit.axis {
            SplitAxis::Horizontal => point.x,
            SplitAxis::Vertical => point.y,
        };
        if !pointer.is_finite() || !hit.span_origin.is_finite() || !hit.span_extent.is_finite() {
            return None;
        }
        let splittable = (hit.span_extent - sanitize_nonneg_finite(divider)).max(0.0);
        if splittable <= f32::EPSILON {
            return None;
        }
        Some(((pointer - hit.span_origin) / splittable).clamp(0.0, 1.0))
    }

    /// Apply a ratio through a still-live divider path. Stale paths fail closed.
    pub fn set_divider_ratio(&mut self, hit: &LogicalDividerHit, ratio: f32) -> bool {
        if ratio.is_nan() {
            return false;
        }
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

#[inline]
fn sanitize_nonneg_finite(value: f32) -> f32 {
    if value.is_finite() {
        value.max(0.0)
    } else {
        0.0
    }
}

#[inline]
fn sanitize_ratio(ratio: f32) -> f32 {
    if ratio.is_nan() {
        0.5
    } else {
        ratio.clamp(0.0, 1.0)
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
    let extent = sanitize_nonneg_finite(extent);
    let divider = sanitize_nonneg_finite(divider).min(extent);
    let splittable = (extent - divider).max(0.0);
    let min_leaf = sanitize_nonneg_finite(min_leaf);
    if splittable < min_leaf * 2.0 {
        let first = (splittable * 0.5).round();
        return (first, splittable - first);
    }
    let first = (splittable * sanitize_ratio(ratio))
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
    let divider = sanitize_nonneg_finite(divider);
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
            let gap = sanitize_nonneg_finite(divider);
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
    let extent = sanitize_nonneg_finite(extent);
    let divider = sanitize_nonneg_finite(divider).min(extent);
    let available = (extent - divider).max(0.0);
    let first_minimum = sanitize_nonneg_finite(first_minimum);
    let second_minimum = sanitize_nonneg_finite(second_minimum);
    if available + f32::EPSILON >= first_minimum + second_minimum {
        let target = available * sanitize_ratio(ratio);
        let first = target.round().clamp(
            first_minimum,
            (available - second_minimum).max(first_minimum),
        );
        return (first, available - first);
    }

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
            header: None,
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
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub struct TabIndicators {
    pub dirty: bool,
    pub busy: bool,
    /// Raised out of band by a native leaf's owner.
    pub attention: bool,
    /// Raised by the session status classifier and recomputed on every pass.
    pub status_attention: bool,
}

impl TabIndicators {
    /// Whether any indicator wants the user's attention.
    #[must_use]
    #[inline]
    pub const fn wants_attention(self) -> bool {
        self.attention || self.status_attention
    }
}

/// Closed icon identity for first-party tabs and recognized terminal programs.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TabIconKind {
    Mail,
    Codex,
    About,
    Settings,
    SystemState,
    Markdown,
    Editor,
    Recovery,
    Workers,
}

/// What kind of surface a tab's icon stands for: its session's terminal, or an application of aterm's own.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TabSurface {
    Terminal,
    App,
}

impl TabIconKind {
    /// Every kind, for the sweeps that must cover them all.
    pub const ALL: [Self; 9] = [
        Self::Mail,
        Self::Codex,
        Self::About,
        Self::Settings,
        Self::SystemState,
        Self::Markdown,
        Self::Editor,
        Self::Recovery,
        Self::Workers,
    ];

    /// The surface this icon stands for. `Codex` names a program in a terminal, so an icon's presence never means an application: a new kind answers here.
    #[must_use]
    #[inline]
    pub const fn surface(self) -> TabSurface {
        match self {
            Self::Codex => TabSurface::Terminal,
            Self::Mail
            | Self::About
            | Self::Settings
            | Self::SystemState
            | Self::Markdown
            | Self::Editor
            | Self::Recovery
            | Self::Workers => TabSurface::App,
        }
    }

    /// Stable semantic name used by chrome inspection (`toolbar-tabs icons=`).
    #[must_use]
    #[inline]
    pub const fn semantic_name(self) -> &'static str {
        match self {
            Self::Mail => "mail",
            Self::Codex => "codex",
            Self::About => "about",
            Self::Settings => "settings",
            Self::SystemState => "system-state",
            Self::Markdown => "markdown",
            Self::Editor => "editor",
            Self::Recovery => "recovery",
            Self::Workers => "workers",
        }
    }

    /// The app's name in words, for hover help.
    #[must_use]
    #[inline]
    pub const fn display_name(self) -> &'static str {
        match self {
            Self::Mail => "Mail",
            Self::Codex => "Codex",
            Self::About => "About aterm",
            Self::Settings => "Settings",
            Self::SystemState => "System State",
            Self::Markdown => "Markdown viewer",
            Self::Editor => "Editor",
            Self::Recovery => "Recovery",
            Self::Workers => "Workers",
        }
    }
}

/// Which side(s) of a live session connection a tab's sessions sit on.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum TabConnRole {
    Outbound,
    Inbound,
    Both,
}

impl TabConnRole {
    /// The role the two per-session predicate flags spell; `None` = no mark.
    #[must_use]
    #[inline]
    pub const fn from_flags(outbound: bool, inbound: bool) -> Option<Self> {
        match (outbound, inbound) {
            (true, true) => Some(Self::Both),
            (true, false) => Some(Self::Outbound),
            (false, true) => Some(Self::Inbound),
            (false, false) => None,
        }
    }

    /// Directional `(outbound, inbound)` flags for this role.
    #[must_use]
    #[inline]
    pub const fn flags(self) -> (bool, bool) {
        match self {
            Self::Outbound => (true, false),
            Self::Inbound => (false, true),
            Self::Both => (true, true),
        }
    }

    /// OR-fold two optional roles.
    #[must_use]
    #[inline]
    pub const fn union(a: Option<Self>, b: Option<Self>) -> Option<Self> {
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

    /// Stable state tokens for the `chrome` introspection line.
    #[must_use]
    #[inline]
    pub const fn chrome_states(self) -> &'static [&'static str] {
        match self {
            Self::Outbound => &["conn-out"],
            Self::Inbound => &["conn-in"],
            Self::Both => &["conn-out", "conn-in"],
        }
    }
}

/// Chrome metadata computed from tab content rather than from a PTY mirror.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct TabPresentation {
    pub title: String,
    pub icon: Option<TabIconKind>,
    pub indicators: TabIndicators,
    /// The tab's connection-mark role, or `None` when no live connection applies.
    pub conn: Option<TabConnRole>,
    pub closable: bool,
    pub tooltip: Option<String>,
}

impl TabPresentation {
    #[must_use]
    pub fn terminal(title: impl Into<String>) -> Self {
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

/// Aggregate split-leaf presentations without letting one state hide another.
#[must_use]
pub fn aggregate_presentations(
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
        indicators.attention |= presentation.indicators.attention;
        indicators.status_attention |= presentation.indicators.status_attention;
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
pub struct Tab {
    pub id: TabId,
    pub root: SplitTree<ViewId>,
    pub focus: ViewId,
    pub zoomed: bool,
    pub presentation: TabPresentation,
    /// A worker's tab: not on the strip, reached through the window's Workers tab. Parked tabs
    /// are a suffix of their [`TabSet`], which keeps that across every change; write this only
    /// before a [`TabSet::push`].
    pub parked: bool,
}

impl Tab {
    #[must_use]
    pub const fn new(id: TabId, view: ViewId, presentation: TabPresentation) -> Self {
        Self {
            id,
            root: SplitTree::leaf(view),
            focus: view,
            zoomed: false,
            presentation,
            parked: false,
        }
    }

    #[must_use]
    pub const fn from_root(
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
            parked: false,
        }
    }

    /// Resolve the canonical visible topology once for every frame consumer.
    #[must_use]
    pub fn visible_plan(
        &self,
        bounds: LogicalRect,
        divider: f32,
        sizing: impl Fn(ViewId) -> LeafSizing,
    ) -> VisibleLeafPlan {
        let bounds = sanitize_rect(bounds);
        let divider = sanitize_nonneg_finite(divider);
        if self.zoomed && self.root.len() > 1 {
            let leaf_sizing = sizing(self.focus).sanitized();
            return VisibleLeafPlan {
                tab: self.id,
                bounds,
                leaves: vec![VisibleLeaf {
                    path: SplitPath::root(),
                    view: self.focus,
                    header: None,
                    rect: bounds,
                    sizing: leaf_sizing,
                    focused: true,
                }],
                dividers: Vec::new(),
                focused: self.focus,
                zoomed: true,
                divider,
            };
        }
        let mut leaves = Vec::with_capacity(self.root.len());
        let mut dividers = Vec::with_capacity(self.root.len().saturating_sub(1));
        plan_into(
            &self.root,
            bounds,
            divider,
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
            divider,
        }
    }

    /// Insert `new_view` as the second sibling of the focused leaf and focus it.
    pub fn split_focused(&mut self, axis: SplitAxis, new_view: ViewId) -> bool {
        if self.root.contains(new_view) || !self.root.split_leaf(self.focus, axis, new_view) {
            return false;
        }
        self.focus = new_view;
        self.zoomed = false;
        true
    }

    /// Focus one live leaf by stable identity. Stale delayed requests fail closed.
    pub fn set_focus(&mut self, view: ViewId) -> bool {
        if !self.root.contains(view) {
            return false;
        }
        self.focus = view;
        true
    }

    /// Remove one leaf and re-seat focus on [`SplitTree::absorbing_neighbor`].
    pub fn remove_view(&mut self, view: ViewId) -> RemoveLeaf {
        let heir = self.root.absorbing_neighbor(view);
        let removed = self.root.remove_leaf(view);
        if removed == RemoveLeaf::Removed {
            if self.focus == view || !self.root.contains(self.focus) {
                self.focus = heir.unwrap_or_else(|| self.root.first_leaf());
            }
            self.zoomed = false;
        }
        removed
    }

    /// Toggle focused-leaf zoom. Single-leaf tabs remain unzoomed.
    pub fn toggle_zoom(&mut self) -> bool {
        self.zoomed = !self.zoomed && self.root.len() > 1;
        self.zoomed
    }

    /// The leaf a geometric focus move from the focused leaf lands on.
    pub fn neighbor(&self, direction: FocusDirection, plan: &VisibleLeafPlan) -> Option<ViewId> {
        if plan.zoomed || plan.leaves.len() <= 1 {
            return None;
        }
        let current = plan.leaf(self.focus).map(VisibleLeaf::outer_rect)?;
        let cur_right = current.origin.x + current.size.width;
        let cur_bottom = current.origin.y + current.size.height;
        let overlap = |a0: f32, a1: f32, b0: f32, b1: f32| (a1.min(b1) - a0.max(b0)).max(0.0);
        let mut best: Option<(ViewId, f32, f32, f32)> = None;
        for leaf in &plan.leaves {
            if leaf.view == self.focus {
                continue;
            }
            let rect = leaf.outer_rect();
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
        best.map(|(view, _, _, _)| view)
    }

    /// Apply a divider ratio while its stable path still names the same split.
    pub fn set_divider_ratio(&mut self, path: &SplitPath, ratio: f32) -> bool {
        if ratio.is_nan() {
            return false;
        }
        let Some(SplitTree::Split { ratio: current, .. }) = node_at_path_mut(&mut self.root, path)
        else {
            return false;
        };
        *current = ratio.clamp(0.05, 0.95);
        true
    }

    /// Structural invariant consumed by window-level debug assertions and tests.
    #[must_use]
    pub fn invariant_holds(&self, views: &ViewStore) -> bool {
        self.root.contains(self.focus)
            && self
                .root
                .leaves()
                .into_iter()
                .all(|view| views.contains(view))
    }
}

/// Ordered stable tabs with active identity.
///
/// `tabs[..shown_len()]` is what the strip draws and `tabs[shown_len()..]` are the parked tabs, so
/// a canonical index is a strip index for every visible tab. Any tab may be active, parked or not.
#[derive(Clone, PartialEq, Debug, Default)]
pub struct TabSet {
    active: Option<TabId>,
    tabs: Vec<Tab>,
    /// The shown tab that stands for the parked ones: the strip selects it while a parked tab is
    /// active, and closing that parked tab lands on it.
    home: Option<TabId>,
}

impl TabSet {
    #[must_use]
    #[inline]
    pub fn new(first: Tab) -> Self {
        Self {
            active: Some(first.id),
            tabs: vec![first],
            home: None,
        }
    }

    #[must_use]
    #[inline]
    pub const fn len(&self) -> usize {
        self.tabs.len()
    }

    #[must_use]
    #[inline]
    pub const fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    #[must_use]
    #[inline]
    pub const fn active_id(&self) -> Option<TabId> {
        self.active
    }

    #[must_use]
    #[inline]
    pub fn active_index(&self) -> Option<usize> {
        let active = self.active?;
        self.tabs.iter().position(|tab| tab.id == active)
    }

    #[must_use]
    #[inline]
    pub fn active(&self) -> Option<&Tab> {
        self.active_index().and_then(|index| self.tabs.get(index))
    }

    #[inline]
    pub fn active_mut(&mut self) -> Option<&mut Tab> {
        let index = self.active_index()?;
        self.tabs.get_mut(index)
    }

    #[must_use]
    #[inline]
    pub fn get(&self, id: TabId) -> Option<&Tab> {
        self.tabs.iter().find(|tab| tab.id == id)
    }

    #[must_use]
    #[inline]
    pub fn tab_at(&self, index: usize) -> Option<&Tab> {
        self.tabs.get(index)
    }

    #[inline]
    pub fn tab_at_mut(&mut self, index: usize) -> Option<&mut Tab> {
        self.tabs.get_mut(index)
    }

    #[must_use]
    #[inline]
    pub fn tabs(&self) -> &[Tab] {
        &self.tabs
    }

    /// How many leading tabs the strip draws; the rest are parked.
    #[must_use]
    #[inline]
    pub fn shown_len(&self) -> usize {
        self.tabs.partition_point(|tab| !tab.parked)
    }

    /// The tabs the strip draws, in order.
    #[must_use]
    #[inline]
    pub fn shown(&self) -> &[Tab] {
        &self.tabs[..self.shown_len()]
    }

    /// The parked tabs, in order; the first sits at canonical index [`Self::shown_len`].
    #[must_use]
    #[inline]
    pub fn parked(&self) -> &[Tab] {
        &self.tabs[self.shown_len()..]
    }

    /// The shown tab that stands for the parked ones, when the window has one.
    #[must_use]
    #[inline]
    pub const fn home(&self) -> Option<TabId> {
        self.home
    }

    /// Name the shown tab that stands for the parked ones. A parked or absent tab is refused.
    pub fn set_home(&mut self, home: Option<TabId>) -> bool {
        if let Some(id) = home
            && !self.get(id).is_some_and(|tab| !tab.parked)
        {
            return false;
        }
        self.home = home;
        true
    }

    /// Whether the active tab is parked.
    #[must_use]
    #[inline]
    pub fn active_is_parked(&self) -> bool {
        self.active().is_some_and(|tab| tab.parked)
    }

    /// The canonical index the strip selects: the active tab's, or while a parked tab is active
    /// the home tab's, else the last shown tab's. `None` when nothing is shown.
    #[must_use]
    pub fn strip_active(&self) -> Option<usize> {
        let index = self.active_index()?;
        let shown = self.shown_len();
        if index < shown {
            return Some(index);
        }
        self.home_index().or_else(|| shown.checked_sub(1))
    }

    /// The shown tab whose chip stands for tab `id`: itself, or for a parked tab the home tab.
    #[must_use]
    pub fn chip_of(&self, id: TabId) -> Option<TabId> {
        let tab = self.get(id)?;
        if tab.parked { self.home } else { Some(id) }
    }

    fn home_index(&self) -> Option<usize> {
        let home = self.home?;
        self.shown().iter().position(|tab| tab.id == home)
    }

    /// Append a tab and focus it: a shown tab at the end of the shown ones, a parked tab last.
    pub fn push(&mut self, tab: Tab) -> Result<usize, DuplicateTabId> {
        if self.get(tab.id).is_some() {
            return Err(DuplicateTabId);
        }
        let id = tab.id;
        let at = if tab.parked {
            self.tabs.len()
        } else {
            self.shown_len()
        };
        self.tabs.insert(at, tab);
        self.active = Some(id);
        Ok(at)
    }

    /// Take a tab off the suffix: it becomes the last shown tab, keeping which tab is active.
    /// Its new canonical index, or `None` when it is absent or not parked.
    pub fn unpark(&mut self, id: TabId) -> Option<usize> {
        let from = self
            .tabs
            .iter()
            .position(|tab| tab.id == id && tab.parked)?;
        let mut tab = self.tabs.remove(from);
        tab.parked = false;
        let to = self.shown_len();
        self.tabs.insert(to, tab);
        Some(to)
    }

    pub fn switch_to(&mut self, id: TabId) -> bool {
        if self.get(id).is_none() {
            return false;
        }
        self.active = Some(id);
        true
    }

    pub fn switch_to_index(&mut self, index: usize) -> bool {
        let Some(id) = self.tabs.get(index).map(|tab| tab.id) else {
            return false;
        };
        self.active = Some(id);
        true
    }

    pub fn cycle(&mut self, forward: bool) -> Option<TabId> {
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

    /// Next or previous among the tabs the strip draws, wrapping, from the one it selects: a
    /// parked active tab steps from its home. With one shown tab, a parked active tab goes home
    /// and a shown one stays. `None` when nothing is shown.
    pub fn cycle_shown(&mut self, forward: bool) -> Option<TabId> {
        let count = self.shown_len();
        if count == 0 {
            return None;
        }
        let current = self.strip_active()?;
        let next = if self.active_is_parked() && count == 1 {
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

    /// Remove a tab and update the active tab selection. A closing tab never hands the front to a
    /// parked one while a shown tab stands: a parked active tab lands on its home, else the last
    /// shown tab, and a shown one on the shown tab in its place, else the one before it.
    pub fn remove(&mut self, id: TabId) -> Option<Tab> {
        let index = self.tabs.iter().position(|tab| tab.id == id)?;
        let was_active = self.active == Some(id);
        let removed = self.tabs.remove(index);
        if self.home == Some(id) {
            self.home = None;
        }
        if self.tabs.is_empty() {
            self.active = None;
        } else if was_active {
            let shown = self.shown_len();
            let landing = if shown == 0 {
                index.min(self.tabs.len() - 1)
            } else if removed.parked {
                self.home_index().unwrap_or(shown - 1)
            } else {
                index.min(shown - 1)
            };
            self.active = Some(self.tabs[landing].id);
        }
        Some(removed)
    }

    /// Move a stable tab to a chrome index without changing which tab is active. A move that
    /// would carry a tab across the boundary between the shown and parked tabs is refused.
    pub fn reorder(&mut self, id: TabId, to: usize) -> bool {
        let Some(from) = self.tabs.iter().position(|tab| tab.id == id) else {
            return false;
        };
        if to >= self.tabs.len() || from == to {
            return to < self.tabs.len();
        }
        let shown = self.shown_len();
        if (from < shown) != (to < shown) {
            return false;
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(to, tab);
        true
    }

    /// Move a tab as near canonical index `to` as its own side of the boundary allows, keeping
    /// which tab is active. Its index now, or `None` when it is absent.
    pub fn place(&mut self, id: TabId, to: usize) -> Option<usize> {
        let tab = self.get(id)?;
        let (shown, len) = (self.shown_len(), self.tabs.len());
        let to = if tab.parked {
            to.clamp(shown, len - 1)
        } else {
            to.min(shown - 1)
        };
        let moved = self.reorder(id, to);
        debug_assert!(moved);
        Some(to)
    }

    #[must_use]
    pub fn invariant_holds(&self, views: &ViewStore) -> bool {
        if self.tabs.is_empty() {
            return self.active.is_none();
        }
        let Some(active) = self.active else {
            return false;
        };
        self.tabs.iter().filter(|tab| tab.id == active).count() == 1
            && self.parked_is_suffix()
            && self
                .home
                .is_none_or(|home| self.shown().iter().any(|tab| tab.id == home))
            && self.tabs.iter().all(|tab| tab.invariant_holds(views))
            && self.tabs.iter().enumerate().all(|(index, tab)| {
                self.tabs[..index]
                    .iter()
                    .all(|earlier| earlier.id != tab.id)
            })
    }

    /// No shown tab follows a parked one.
    #[must_use]
    pub fn parked_is_suffix(&self) -> bool {
        self.tabs
            .iter()
            .skip_while(|tab| !tab.parked)
            .all(|tab| tab.parked)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct DuplicateTabId;

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

    fn parked_set(shown: usize, parked: usize) -> (TabSet, ViewStore, Vec<TabId>) {
        let (store, views) = view_store_with(shown + parked);
        let mut ids = IdAllocator::<TabId>::default();
        let mut set = TabSet::default();
        let mut tabs = Vec::new();
        for (index, view) in views.into_iter().enumerate() {
            let id = ids.allocate().expect("finite test id space");
            let mut tab = Tab::new(id, view, TabPresentation::terminal(format!("t{index}")));
            tab.parked = index >= shown;
            assert_eq!(set.push(tab).expect("fresh id"), index);
            tabs.push(id);
        }
        (set, store, tabs)
    }

    #[test]
    fn a_shown_tab_lands_before_the_parked_suffix_and_a_parked_one_after_it() {
        let (mut set, store, tabs) = parked_set(2, 2);
        assert_eq!(set.shown_len(), 2);
        assert_eq!(set.shown().len() + set.parked().len(), 4);

        let view = set.tabs()[0].focus;
        let mut ids = IdAllocator::<TabId>::default();
        for _ in 0..tabs.len() {
            ids.allocate().expect("finite test id space");
        }
        let shown = Tab::new(
            ids.allocate().expect("finite test id space"),
            view,
            TabPresentation::terminal("new"),
        );
        let shown_id = shown.id;
        assert_eq!(set.push(shown).expect("fresh id"), 2, "at the boundary");
        assert_eq!(set.active_id(), Some(shown_id));
        assert_eq!(set.shown_len(), 3);
        assert!(set.invariant_holds(&store));

        let mut parked = Tab::new(
            ids.allocate().expect("finite test id space"),
            view,
            TabPresentation::terminal("worker"),
        );
        parked.parked = true;
        let parked_id = parked.id;
        assert_eq!(set.push(parked).expect("fresh id"), 5, "last of all");
        assert_eq!(set.tabs()[5].id, parked_id);
        assert_eq!(set.shown_len(), 3);
        assert!(set.invariant_holds(&store));
    }

    #[test]
    fn a_broken_suffix_fails_the_invariant() {
        let (mut set, store, _) = parked_set(2, 1);
        assert!(set.invariant_holds(&store));
        // The negative control: a shown tab behind a parked one is the state nothing may build.
        set.tab_at_mut(0).expect("a tab").parked = true;
        assert!(!set.parked_is_suffix());
        assert!(!set.invariant_holds(&store));
    }

    #[test]
    fn home_must_stand_among_the_shown_tabs() {
        let (mut set, store, tabs) = parked_set(2, 1);
        assert!(!set.set_home(Some(tabs[2])), "a parked tab is no home");
        assert_eq!(set.home(), None);
        assert!(set.set_home(Some(tabs[1])));
        assert!(set.invariant_holds(&store));
        // A home that became parked behind the model's back fails the invariant.
        set.tab_at_mut(1).expect("a tab").parked = true;
        assert!(!set.invariant_holds(&store));
    }

    #[test]
    fn the_strip_selects_home_while_a_parked_tab_is_active() {
        let (mut set, _store, tabs) = parked_set(3, 2);
        assert!(set.set_home(Some(tabs[1])));
        assert!(set.switch_to(tabs[0]));
        assert_eq!(set.strip_active(), Some(0), "a shown tab is its own");
        assert!(!set.active_is_parked());
        assert!(set.switch_to(tabs[4]));
        assert!(set.active_is_parked());
        assert_eq!(set.active_index(), Some(4), "canonical stays canonical");
        assert_eq!(set.strip_active(), Some(1), "the chip stands for it");
        assert!(set.set_home(None));
        assert_eq!(set.strip_active(), Some(2), "no home: the last shown");
    }

    #[test]
    fn next_and_previous_walk_only_the_shown_tabs() {
        let (mut set, store, tabs) = parked_set(3, 2);
        assert!(set.set_home(Some(tabs[2])));
        assert!(set.switch_to(tabs[0]));
        assert_eq!(
            set.cycle_shown(false),
            Some(tabs[2]),
            "wraps past the suffix"
        );
        assert_eq!(
            set.cycle_shown(true),
            Some(tabs[0]),
            "wraps past the suffix"
        );
        assert_eq!(set.cycle_shown(true), Some(tabs[1]));
        assert!(set.switch_to(tabs[3]));
        assert_eq!(set.cycle_shown(true), Some(tabs[0]), "steps from home");
        assert!(set.switch_to(tabs[4]));
        assert_eq!(set.cycle_shown(false), Some(tabs[1]), "steps from home");
        assert!(set.invariant_holds(&store));

        let (mut one, _, only) = parked_set(1, 1);
        assert!(one.set_home(Some(only[0])));
        assert_eq!(
            one.cycle_shown(true),
            Some(only[0]),
            "a parked tab goes home"
        );
        assert_eq!(one.cycle_shown(true), Some(only[0]), "and home stays");
    }

    #[test]
    fn closing_never_hands_the_front_to_a_parked_tab_while_a_shown_one_stands() {
        // The last shown tab closes: what follows it is parked, so the one before it takes over.
        let (mut set, store, tabs) = parked_set(2, 2);
        assert!(set.switch_to(tabs[1]));
        assert!(set.remove(tabs[1]).is_some());
        assert_eq!(set.active_id(), Some(tabs[0]));
        assert!(set.invariant_holds(&store));

        // A parked active tab closes: it lands on home, not its next sibling.
        let (mut set, store, tabs) = parked_set(3, 3);
        assert!(set.set_home(Some(tabs[1])));
        assert!(set.switch_to(tabs[3]));
        assert!(set.remove(tabs[3]).is_some());
        assert_eq!(set.active_id(), Some(tabs[1]));
        assert!(set.invariant_holds(&store));

        // A parked tab closes behind the front: the front stays.
        assert!(set.switch_to(tabs[0]));
        assert!(set.remove(tabs[5]).is_some());
        assert_eq!(set.active_id(), Some(tabs[0]));

        // With no home the last shown tab takes it.
        let (mut set, _store, tabs) = parked_set(2, 2);
        assert!(set.switch_to(tabs[2]));
        assert!(set.remove(tabs[2]).is_some());
        assert_eq!(set.active_id(), Some(tabs[1]));

        // Removing the home clears it, so no stale identity outlives the tab.
        let (mut set, store, tabs) = parked_set(2, 1);
        assert!(set.set_home(Some(tabs[1])));
        assert!(set.remove(tabs[1]).is_some());
        assert_eq!(set.home(), None);
        assert!(set.invariant_holds(&store));

        // Only parked tabs remain: one of them still takes the front, never nothing.
        let (mut set, store, tabs) = parked_set(1, 2);
        assert!(set.remove(tabs[0]).is_some());
        assert!(set.active_is_parked());
        assert!(set.invariant_holds(&store));
    }

    #[test]
    fn a_move_never_crosses_the_boundary() {
        let (mut set, store, tabs) = parked_set(3, 2);
        assert!(!set.reorder(tabs[0], 3), "a shown tab stays shown");
        assert!(!set.reorder(tabs[4], 2), "a parked tab stays parked");
        assert_eq!(set.tabs()[0].id, tabs[0]);
        assert!(set.reorder(tabs[0], 2), "within the shown tabs");
        assert!(set.reorder(tabs[4], 3), "within the parked tabs");
        assert!(set.invariant_holds(&store));
        assert_eq!(set.shown_len(), 3);
    }

    #[test]
    fn placing_a_tab_stops_at_the_boundary() {
        let (mut set, store, tabs) = parked_set(3, 2);
        assert_eq!(set.place(tabs[0], 4), Some(2), "a shown tab stays shown");
        assert_eq!(set.tabs()[2].id, tabs[0]);
        assert_eq!(set.place(tabs[4], 0), Some(3), "a parked tab stays parked");
        assert_eq!(set.tabs()[3].id, tabs[4]);
        assert!(set.invariant_holds(&store));
    }

    #[test]
    fn unparking_makes_the_worker_the_last_shown_tab_and_keeps_the_front() {
        let (mut set, store, tabs) = parked_set(2, 3);
        assert!(set.switch_to(tabs[0]));
        assert_eq!(set.unpark(tabs[3]), Some(2));
        assert_eq!(set.tabs()[2].id, tabs[3]);
        assert!(!set.tabs()[2].parked);
        assert_eq!(set.shown_len(), 3);
        assert_eq!(set.active_id(), Some(tabs[0]));
        assert!(set.invariant_holds(&store));
        assert_eq!(set.unpark(tabs[0]), None, "only a parked tab comes out");
    }

    #[test]
    fn a_parked_tabs_chip_is_home_and_a_shown_tabs_is_its_own() {
        let (mut set, _store, tabs) = parked_set(2, 1);
        assert_eq!(set.chip_of(tabs[1]), Some(tabs[1]));
        assert_eq!(
            set.chip_of(tabs[2]),
            None,
            "no home yet: no chip stands for it"
        );
        assert!(set.set_home(Some(tabs[0])));
        assert_eq!(set.chip_of(tabs[2]), Some(tabs[0]));
    }

    #[test]
    fn split_tree_layout_and_neighbor_share_logical_rects() {
        let mut tree = SplitTree::leaf(1u64);
        assert!(tree.split_leaf(1, SplitAxis::Horizontal, 2));
        assert!(tree.split_leaf(2, SplitAxis::Vertical, 3));
        let bounds = LogicalRect::new(0.0, 0.0, 801.0, 601.0);
        let leaves = tree.layout(bounds, 1.0, 1.0);
        assert_eq!(leaves.len(), 3);
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
    fn pane_headers_reserve_content_and_hit_their_own_leaf_in_nested_splits() {
        let (_, views) = view_store_with(3);
        let mut tab = Tab::new(
            TabId::from_stored(1),
            views[0],
            TabPresentation::terminal("shell"),
        );
        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
        let mut plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 80.0, 24.0), 1.0, |_| {
            LeafSizing::new(LogicalSize::new(2.0, 1.0), LogicalSize::new(80.0, 24.0))
        });
        let before = plan.clone();
        plan.reserve_pane_headers();
        assert_eq!(
            plan.dividers, before.dividers,
            "headers never move split handles"
        );
        for (leaf, original) in plan.leaves.iter().zip(&before.leaves) {
            let header = leaf.header.expect("roomy split has a title row");
            assert_eq!(leaf.outer_rect(), original.rect);
            assert_eq!(header.size.height, 1.0);
            assert_eq!(leaf.rect.origin.y, original.rect.origin.y + 1.0);
            assert_eq!(leaf.rect.size.height, original.rect.size.height - 1.0);
            let header_point = LogicalPoint {
                x: header.origin.x,
                y: header.origin.y,
            };
            assert_eq!(
                plan.header_at(header_point).map(|hit| hit.view),
                Some(leaf.view)
            );
            assert_eq!(
                plan.leaf_at(header_point).map(|hit| hit.view),
                Some(leaf.view)
            );
            assert!(
                !leaf.rect.contains(header_point),
                "chrome is outside terminal input"
            );
            assert!(
                plan.header_at(leaf.rect.origin).is_none(),
                "first content row is not chrome"
            );
        }
        for divider in &plan.dividers {
            assert!(plan.header_at(divider.rect.origin).is_none());
            assert!(plan.leaf_at(divider.rect.origin).is_none());
        }
        for leaf in &plan.leaves {
            for direction in [
                FocusDirection::Left,
                FocusDirection::Right,
                FocusDirection::Up,
                FocusDirection::Down,
            ] {
                let mut from_leaf = tab.clone();
                from_leaf.focus = leaf.view;
                assert_eq!(
                    from_leaf.neighbor(direction, &plan),
                    from_leaf.neighbor(direction, &before),
                    "title rows must not change keyboard focus navigation"
                );
            }
        }
        let once = plan.clone();
        plan.reserve_pane_headers();
        assert_eq!(
            plan, once,
            "reserving twice never steals a second content row"
        );
    }

    #[test]
    fn pane_headers_disappear_for_single_zoomed_and_one_row_leaves() {
        let (_, views) = view_store_with(2);
        let mut tab = Tab::new(
            TabId::from_stored(1),
            views[0],
            TabPresentation::terminal("shell"),
        );
        let plan_for = |tab: &Tab, rows| {
            let mut plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 80.0, rows), 1.0, |_| {
                LeafSizing::new(LogicalSize::new(2.0, 1.0), LogicalSize::new(80.0, 24.0))
            });
            plan.reserve_pane_headers();
            plan
        };
        let single = plan_for(&tab, 24.0);
        assert!(single.leaves[0].header.is_none());
        assert_eq!(single.leaves[0].rect, single.bounds);
        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        let tiny = plan_for(&tab, 1.0);
        assert!(tiny.leaves.iter().all(|leaf| leaf.header.is_none()));
        assert!(tiny.leaves.iter().all(|leaf| leaf.rect.size.height == 1.0));
        let just_fits = plan_for(&tab, 2.0);
        assert!(just_fits.leaves.iter().all(|leaf| leaf.header.is_some()));
        assert!(
            just_fits
                .leaves
                .iter()
                .all(|leaf| leaf.rect.size.height == 1.0)
        );
        tab.zoomed = true;
        let zoomed = plan_for(&tab, 24.0);
        assert_eq!(zoomed.leaves.len(), 1);
        assert!(zoomed.leaves[0].header.is_none());
        assert_eq!(zoomed.leaves[0].rect, zoomed.bounds);
        tab.zoomed = false;
        assert!(
            plan_for(&tab, 24.0)
                .leaves
                .iter()
                .all(|leaf| leaf.header.is_some())
        );
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
        assert_eq!(
            tab.focus, views[1],
            "close repairs focus deterministically onto the leaf that inherits the \
             removed leaf's space, not the tree's first leaf"
        );
    }

    /// Shrinking clamps to bounds without dropping leaves, and regrowing restores the geometry.
    #[test]
    fn shrinking_a_canonical_plan_stays_inside_the_bounds_and_drops_no_leaf() {
        let (_, views) = view_store_with(4);
        let mut tab = Tab::new(
            TabId::from_stored(1),
            views[0],
            TabPresentation::terminal("one"),
        );
        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
        assert!(tab.split_focused(SplitAxis::Horizontal, views[3]));
        let sizing = |_| LeafSizing::new(LogicalSize::new(16.0, 3.0), LogicalSize::new(80.0, 24.0));
        let roomy = tab.visible_plan(LogicalRect::new(0.0, 0.0, 200.0, 48.0), 1.0, sizing);
        assert_eq!(roomy.leaves.len(), 4);

        for w in 0u16..=8 {
            for h in 0u16..=8 {
                let bounds = LogicalRect::new(0.0, 0.0, f32::from(w), f32::from(h));
                let plan = tab.visible_plan(bounds, 1.0, sizing);
                assert_eq!(plan.leaves.len(), 4, "shrinking never drops a leaf");
                for leaf in &plan.leaves {
                    let r = leaf.rect;
                    assert!(
                        r.size.width >= 0.0 && r.size.height >= 0.0,
                        "no negative extent at {w}x{h}: {r:?}"
                    );
                    assert!(
                        r.origin.x >= plan.bounds.origin.x - f32::EPSILON
                            && r.origin.y >= plan.bounds.origin.y - f32::EPSILON,
                        "origin inside the {w}x{h} bounds: {r:?}"
                    );
                    assert!(
                        r.origin.x + r.size.width
                            <= plan.bounds.origin.x + plan.bounds.size.width + f32::EPSILON
                            && r.origin.y + r.size.height
                                <= plan.bounds.origin.y + plan.bounds.size.height + f32::EPSILON,
                        "extent inside the {w}x{h} bounds: {r:?}"
                    );
                }
            }
        }

        let regrown = tab.visible_plan(LogicalRect::new(0.0, 0.0, 200.0, 48.0), 1.0, sizing);
        assert_eq!(
            regrown.leaves, roomy.leaves,
            "shrink -> grow restores the exact geometry"
        );
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
        assert_eq!(tab.neighbor(FocusDirection::Left, &plan), Some(views[0]));
        assert!(tab.set_focus(views[0]));
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
                            status_attention: true,
                        },
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

    /// `remove_view` re-seats focus onto the leaf that inherits the removed leaf's space.
    #[test]
    fn remove_view_repairs_focus_onto_the_leaf_that_inherits_the_space() {
        let (_, views) = view_store_with(4);
        let mut tab = Tab::new(
            TabId::from_stored(1),
            views[0],
            TabPresentation::terminal("one"),
        );
        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
        assert_eq!(tab.focus, views[2]);
        assert_eq!(tab.remove_view(views[2]), RemoveLeaf::Removed);
        assert_eq!(
            tab.focus, views[1],
            "focus follows the space to view 1, not back to the first leaf 0"
        );

        let (_, views) = view_store_with(3);
        let mut tab = Tab::new(
            TabId::from_stored(2),
            views[0],
            TabPresentation::terminal("one"),
        );
        assert!(tab.split_focused(SplitAxis::Horizontal, views[1]));
        assert!(tab.split_focused(SplitAxis::Vertical, views[2]));
        assert!(tab.set_focus(views[0]));
        assert_eq!(tab.remove_view(views[2]), RemoveLeaf::Removed);
        assert_eq!(
            tab.focus, views[0],
            "a background leaf closing never steals the keyboard"
        );
    }

    #[test]
    fn heterogeneous_tab_mutators_refine_pane_tree_model_with_negative_control() {
        use aterm_models_gui::pane_tree_model;
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

    /// `any_leaf` agrees with `leaves().contains()` and short-circuits on the first match.
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
        assert!(!tab.set_divider_ratio(&SplitPath::root(), f32::NAN));
        let plan = tab.visible_plan(LogicalRect::new(0.0, 0.0, 200.0, 100.0), f32::NAN, |_| {
            LeafSizing::new(LogicalSize::default(), LogicalSize::default())
        });
        assert!(plan.divider.is_finite());
        assert!(plan.leaves.iter().all(|l| l.rect.size.width.is_finite()
            && l.rect.size.height.is_finite()
            && l.rect.origin.x.is_finite()
            && l.rect.origin.y.is_finite()));
        assert_eq!(
            plan.ratio_for_pointer(
                &plan.dividers[0],
                LogicalPoint {
                    x: f32::NAN,
                    y: 10.0,
                },
            ),
            None
        );
    }
}
