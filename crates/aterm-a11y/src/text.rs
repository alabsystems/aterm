// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_consumer 0.37.0 (MIT OR Apache-2.0, used under Apache-2.0)
// src/text.rs, modified by the aterm project in 2026.

//! Text documents: positions, ranges, and navigation by character, word, line, and paragraph.

use std::cmp::Ordering;
use std::fmt;

use crate::model::{Point, Rect, Role, TextDirection, TextPosition as WeakPosition, TextSelection};
use crate::tree::{FilterResult, Node, NodeId};

/// A position in a document: a run, and a character index in it (its length when at its end).
#[derive(Clone, Copy, Debug)]
pub(crate) struct InnerPosition<'a> {
    pub(crate) node: Node<'a>,
    pub(crate) character_index: usize,
}

impl<'a> InnerPosition<'a> {
    fn clamped_upgrade(tree_state: &'a crate::tree::State, weak: WeakPosition) -> Option<Self> {
        let node = tree_state.node_by_id(NodeId(weak.node))?;
        if node.role() != Role::TextRun {
            return None;
        }
        let character_index = weak
            .character_index
            .min(node.data().character_lengths().len());
        Some(Self {
            node,
            character_index,
        })
    }

    #[inline]
    const fn is_run_start(&self) -> bool {
        self.character_index == 0
    }

    #[inline]
    const fn is_line_start(&self) -> bool {
        self.is_run_start()
    }

    #[inline]
    fn is_run_end(&self) -> bool {
        self.character_index == self.node.data().character_lengths().len()
    }

    #[inline]
    fn is_line_end(&self) -> bool {
        self.is_run_end()
    }

    fn is_paragraph_end(&self) -> bool {
        self.is_line_end()
            && self
                .node
                .data()
                .value()
                .is_some_and(|value| value.ends_with('\n'))
    }

    fn is_document_end(&self, root_node: &Node) -> bool {
        self.is_run_end() && self.node.following_text_runs(root_node).next().is_none()
    }

    fn biased_to_start(&self, root_node: &Node) -> Self {
        if self.is_run_end()
            && let Some(node) = self.node.following_text_runs(root_node).next()
        {
            return Self {
                node,
                character_index: 0,
            };
        }
        *self
    }

    fn biased_to_end(&self, root_node: &Node) -> Self {
        if self.is_run_start()
            && let Some(node) = self.node.preceding_text_runs(root_node).next()
        {
            return Self {
                node,
                character_index: node.data().character_lengths().len(),
            };
        }
        *self
    }

    fn comparable(&self, root_node: &Node) -> (Vec<usize>, usize) {
        let normalized = self.biased_to_start(root_node);
        (
            normalized.node.relative_index_path(root_node.id()),
            normalized.character_index,
        )
    }

    #[inline]
    const fn line_start(&self) -> Self {
        Self {
            node: self.node,
            character_index: 0,
        }
    }

    #[inline]
    fn line_end(&self) -> Self {
        Self {
            node: self.node,
            character_index: self.node.data().character_lengths().len(),
        }
    }

    #[inline]
    pub(crate) const fn downgrade(&self) -> WeakPosition {
        WeakPosition {
            node: self.node.id().0,
            character_index: self.character_index,
        }
    }
}

impl PartialEq for InnerPosition<'_> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.node.id() == other.node.id() && self.character_index == other.character_index
    }
}

impl Eq for InnerPosition<'_> {}

/// A position in the document `root_node`.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Position<'a> {
    root_node: Node<'a>,
    pub(crate) inner: InnerPosition<'a>,
}

impl<'a> Position<'a> {
    #[inline]
    pub(crate) const fn inner_node(&self) -> &Node<'a> {
        &self.inner.node
    }

    pub(crate) fn is_word_start(&self) -> bool {
        self.is_paragraph_start()
            || u8::try_from(self.inner.character_index).is_ok_and(|idx| {
                self.inner
                    .node
                    .data()
                    .word_starts()
                    .binary_search(&idx)
                    .is_ok()
            })
    }

    #[inline]
    pub(crate) const fn is_line_start(&self) -> bool {
        self.inner.is_line_start()
    }

    /// A line start with no text before it, or whose text before it ends in a line break.
    pub(crate) fn is_paragraph_start(&self) -> bool {
        self.is_line_start()
            && self
                .inner
                .node
                .preceding_text_runs(&self.root_node)
                .find(|node| !node.data().character_lengths().is_empty())
                .is_none_or(|node| {
                    node.data()
                        .value()
                        .is_some_and(|value| value.ends_with('\n'))
                })
    }

    #[inline]
    pub(crate) fn is_document_end(&self) -> bool {
        self.inner.is_document_end(&self.root_node)
    }

    #[inline]
    pub(crate) fn to_degenerate_range(self) -> Range<'a> {
        Range::new(self.root_node, self.inner, self.inner)
    }

    /// The position's offset in Unicode scalar values from the document's start.
    pub(crate) fn to_global_usv_index(self) -> usize {
        let mut total_length = 0usize;
        for node in self.root_node.text_runs() {
            let node_text = node.data().value().unwrap_or_default();
            if node.id() == self.inner.node.id() {
                let character_lengths = node.data().character_lengths();
                let slice_end = character_lengths[..self.inner.character_index]
                    .iter()
                    .copied()
                    .map(usize::from)
                    .sum::<usize>();
                return total_length + node_text[..slice_end].chars().count();
            }
            total_length += node_text.chars().count();
        }
        panic!("invalid position")
    }

    pub(crate) fn forward_to_character_end(&self) -> Self {
        let pos = self.inner.biased_to_start(&self.root_node);
        let max_len = pos.node.data().character_lengths().len();
        Self {
            root_node: self.root_node,
            inner: InnerPosition {
                node: pos.node,
                character_index: (pos.character_index + 1).min(max_len),
            },
        }
    }

    pub(crate) fn forward_to_word_start(&self) -> Self {
        let pos = self.inner.biased_to_start(&self.root_node);
        if let Ok(char_idx) = u8::try_from(pos.character_index) {
            let word_starts = pos.node.data().word_starts();
            let index = match word_starts.binary_search(&char_idx) {
                Ok(index) => index + 1,
                Err(index) => index,
            };
            if let Some(start) = word_starts.get(index) {
                return Self {
                    root_node: self.root_node,
                    inner: InnerPosition {
                        node: pos.node,
                        character_index: *start as usize,
                    },
                };
            }
        }
        for node in pos.node.following_text_runs(&self.root_node) {
            let start_pos = Self {
                root_node: self.root_node,
                inner: InnerPosition {
                    node,
                    character_index: 0,
                },
            };
            if start_pos.is_paragraph_start() {
                return start_pos;
            }
            if let Some(start) = node.data().word_starts().first() {
                return Self {
                    root_node: self.root_node,
                    inner: InnerPosition {
                        node,
                        character_index: *start as usize,
                    },
                };
            }
        }
        self.document_end()
    }

    pub(crate) fn forward_to_word_end(&self) -> Self {
        self.forward_to_word_start().biased_to_end()
    }

    pub(crate) fn backward_to_word_start(&self) -> Self {
        {
            let word_starts = self.inner.node.data().word_starts();
            let index = match u8::try_from(self.inner.character_index) {
                Ok(char_idx) => match word_starts.binary_search(&char_idx) {
                    Ok(index) | Err(index) => index,
                },
                Err(_) => word_starts.len(),
            };
            if let Some(index) = index.checked_sub(1) {
                return Self {
                    root_node: self.root_node,
                    inner: InnerPosition {
                        node: self.inner.node,
                        character_index: word_starts[index] as usize,
                    },
                };
            }
        }
        if self.inner.character_index != 0 {
            let start_pos = Self {
                root_node: self.root_node,
                inner: InnerPosition {
                    node: self.inner.node,
                    character_index: 0,
                },
            };
            if start_pos.is_paragraph_start() {
                return start_pos;
            }
        }
        for node in self.inner.node.preceding_text_runs(&self.root_node) {
            if let Some(start) = node.data().word_starts().last() {
                return Self {
                    root_node: self.root_node,
                    inner: InnerPosition {
                        node,
                        character_index: *start as usize,
                    },
                };
            }
            let start_pos = Self {
                root_node: self.root_node,
                inner: InnerPosition {
                    node,
                    character_index: 0,
                },
            };
            if start_pos.is_paragraph_start() {
                return start_pos;
            }
        }
        self.document_start()
    }

    pub(crate) fn forward_to_line_end(&self) -> Self {
        let pos = self.inner.biased_to_start(&self.root_node);
        Self {
            root_node: self.root_node,
            inner: pos.line_end(),
        }
    }

    /// The start of the line before this position (this line's, when inside it).
    pub(crate) fn backward_to_line_start(&self) -> Self {
        let here = self.inner.comparable(&self.root_node);
        let mut pos = self.inner.biased_to_end(&self.root_node);
        loop {
            let start = pos.line_start();
            let found = start.biased_to_start(&self.root_node);
            if found.comparable(&self.root_node) < here {
                return Self {
                    root_node: self.root_node,
                    inner: found,
                };
            }
            let before = start.biased_to_end(&self.root_node);
            if before == start {
                return Self {
                    root_node: self.root_node,
                    inner: start,
                };
            }
            pos = before;
        }
    }

    pub(crate) fn forward_to_paragraph_end(&self) -> Self {
        let mut current = *self;
        loop {
            current = current.forward_to_line_end();
            if current.is_document_end() || current.inner.is_paragraph_end() {
                break;
            }
        }
        current
    }

    pub(crate) fn backward_to_paragraph_start(&self) -> Self {
        let mut current = *self;
        loop {
            current = current.backward_to_line_start();
            if current.is_paragraph_start() {
                break;
            }
        }
        current
    }

    #[inline]
    pub(crate) fn biased_to_end(&self) -> Self {
        Self {
            root_node: self.root_node,
            inner: self.inner.biased_to_end(&self.root_node),
        }
    }

    #[inline]
    pub(crate) fn document_end(&self) -> Self {
        self.root_node.document_end()
    }

    #[inline]
    pub(crate) fn document_start(&self) -> Self {
        self.root_node.document_start()
    }
}

impl PartialEq for Position<'_> {
    #[inline]
    fn eq(&self, other: &Self) -> bool {
        self.root_node.id() == other.root_node.id() && self.inner == other.inner
    }
}

impl Eq for Position<'_> {}

impl PartialOrd for Position<'_> {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.root_node.id() != other.root_node.id() {
            return None;
        }
        let self_comparable = self.inner.comparable(&self.root_node);
        let other_comparable = other.inner.comparable(&self.root_node);
        Some(self_comparable.cmp(&other_comparable))
    }
}

/// A range of a document, its start never after its end.
#[derive(Clone, Copy)]
pub(crate) struct Range<'a> {
    pub(crate) node: Node<'a>,
    pub(crate) start: InnerPosition<'a>,
    pub(crate) end: InnerPosition<'a>,
}

impl<'a> Range<'a> {
    fn new(node: Node<'a>, mut start: InnerPosition<'a>, mut end: InnerPosition<'a>) -> Self {
        if start.comparable(&node) > end.comparable(&node) {
            std::mem::swap(&mut start, &mut end);
        }
        Self { node, start, end }
    }

    #[inline]
    pub(crate) const fn start(&self) -> Position<'a> {
        Position {
            root_node: self.node,
            inner: self.start,
        }
    }

    #[inline]
    pub(crate) const fn end(&self) -> Position<'a> {
        Position {
            root_node: self.node,
            inner: self.end,
        }
    }

    pub(crate) fn is_degenerate(&self) -> bool {
        self.start.comparable(&self.node) == self.end.comparable(&self.node)
    }

    fn walk<F, T>(&self, mut f: F) -> Option<T>
    where
        F: FnMut(&Node<'a>, InnerPosition<'a>, InnerPosition<'a>) -> Option<T>,
    {
        let (start, end) = if self.is_degenerate() {
            (self.start, self.start)
        } else {
            let start = self.start.biased_to_start(&self.node);
            let end = self.end.biased_to_end(&self.node);
            (start, end)
        };
        if let Some(result) = f(&start.node, start, end) {
            return Some(result);
        }
        if start.node.id() == end.node.id() {
            return None;
        }
        for node in start.node.following_text_runs(&self.node) {
            if let Some(result) = f(&node, start, end) {
                return Some(result);
            }
            if node.id() == end.node.id() {
                break;
            }
        }
        None
    }

    fn traverse_text<F, T>(&self, mut f: F) -> Option<T>
    where
        F: FnMut(&Node<'a>, &str) -> Option<T>,
    {
        self.walk(|node, start, end| {
            let character_lengths = node.data().character_lengths();
            let len = character_lengths.len();
            let start_index = if node.id() == start.node.id() {
                start.character_index.min(len)
            } else {
                0
            };
            let end_index = if node.id() == end.node.id() {
                end.character_index.min(len)
            } else {
                len
            }
            .max(start_index);
            let value = node.data().value().unwrap_or_default();
            let s = if start_index == end_index {
                ""
            } else if start_index == 0 && end_index == len {
                value
            } else {
                let slice_start = character_lengths[..start_index]
                    .iter()
                    .copied()
                    .map(usize::from)
                    .sum::<usize>();
                let slice_end = slice_start
                    + character_lengths[start_index..end_index]
                        .iter()
                        .copied()
                        .map(usize::from)
                        .sum::<usize>();
                &value[slice_start..slice_end]
            };
            f(node, s)
        })
    }

    pub(crate) fn write_text<W: fmt::Write>(&self, mut writer: W) -> fmt::Result {
        if let Some(err) = self.traverse_text(|_, s| writer.write_str(s).err()) {
            Err(err)
        } else {
            Ok(())
        }
    }

    pub(crate) fn text(&self) -> String {
        let mut result = String::new();
        let _ = self.write_text(&mut result);
        result
    }

    /// Returns the range's transformed bounding boxes relative to the tree's container.
    pub(crate) fn bounding_boxes(&self) -> Vec<Rect> {
        let mut result = Vec::new();
        self.walk(|node, start, end| {
            let mut rect = match node.data().bounds() {
                Some(rect) => rect,
                None => {
                    return Some(Vec::new());
                }
            };
            let positions = match node.data().character_positions() {
                Some(positions) => positions,
                None => {
                    return Some(Vec::new());
                }
            };
            let widths = match node.data().character_widths() {
                Some(widths) => widths,
                None => {
                    return Some(Vec::new());
                }
            };
            let direction = match node.text_direction() {
                Some(direction) => direction,
                None => {
                    return Some(Vec::new());
                }
            };
            let character_lengths = node.data().character_lengths();
            let len = character_lengths.len();
            if positions.len() < len || widths.len() < len {
                return Some(Vec::new());
            }
            let start_index = if node.id() == start.node.id() {
                start.character_index.min(len)
            } else {
                0
            };
            let end_index = if node.id() == end.node.id() {
                end.character_index.min(len)
            } else {
                len
            }
            .max(start_index);
            if start_index != 0 || end_index != len {
                let pixel_start = if start_index < len {
                    positions[start_index]
                } else if len > 0 {
                    positions[start_index - 1] + widths[start_index - 1]
                } else {
                    0.0
                };
                let pixel_end = if end_index == start_index {
                    pixel_start
                } else {
                    positions[end_index - 1] + widths[end_index - 1]
                };
                let pixel_start = f64::from(pixel_start);
                let pixel_end = f64::from(pixel_end);
                match direction {
                    TextDirection::LeftToRight => {
                        let orig_left = rect.x0;
                        rect.x0 = orig_left + pixel_start;
                        rect.x1 = orig_left + pixel_end;
                    }
                    TextDirection::RightToLeft => {
                        let orig_right = rect.x1;
                        rect.x1 = orig_right - pixel_start;
                        rect.x0 = orig_right - pixel_end;
                    }
                    TextDirection::TopToBottom => {
                        let orig_top = rect.y0;
                        rect.y0 = orig_top + pixel_start;
                        rect.y1 = orig_top + pixel_end;
                    }
                    TextDirection::BottomToTop => {
                        let orig_bottom = rect.y1;
                        rect.y1 = orig_bottom - pixel_start;
                        rect.y0 = orig_bottom - pixel_end;
                    }
                }
            }
            result.push(node.transform().transform_rect_bbox(rect));
            None
        })
        .unwrap_or(result)
    }

    fn fix_start_bias(&mut self) {
        if !self.is_degenerate() {
            self.start = self.start.biased_to_start(&self.node);
        }
    }

    pub(crate) fn set_end(&mut self, pos: Position<'a>) {
        assert_eq!(pos.root_node.id(), self.node.id());
        self.end = pos.inner;
        if self.start.comparable(&self.node) >= self.end.comparable(&self.node) {
            self.start = self.end;
        }
        self.fix_start_bias();
    }

    pub(crate) const fn to_text_selection(self) -> TextSelection {
        TextSelection {
            anchor: self.start.downgrade(),
            focus: self.end.downgrade(),
        }
    }
}

impl PartialEq for Range<'_> {
    fn eq(&self, other: &Self) -> bool {
        self.node.id() == other.node.id() && self.start == other.start && self.end == other.end
    }
}

impl Eq for Range<'_> {}

fn text_node_filter(root_id: NodeId, node: &Node) -> FilterResult {
    if node.id() == root_id || node.role() == Role::TextRun {
        FilterResult::Include
    } else {
        FilterResult::ExcludeNode
    }
}

fn character_index_at_point(node: &Node, point: Point) -> usize {
    let Some(rect) = node.data().bounds() else {
        return 0;
    };
    let character_lengths = node.data().character_lengths();
    let positions = match node.data().character_positions() {
        Some(positions) => positions,
        None => {
            return 0;
        }
    };
    let widths = match node.data().character_widths() {
        Some(widths) => widths,
        None => {
            return 0;
        }
    };
    let direction = match node.text_direction() {
        Some(direction) => direction,
        None => {
            return 0;
        }
    };
    for (i, (position, width)) in positions
        .iter()
        .zip(widths.iter())
        .take(character_lengths.len())
        .enumerate()
        .rev()
    {
        let relative_pos = match direction {
            TextDirection::LeftToRight => point.x - rect.x0,
            TextDirection::RightToLeft => rect.x1 - point.x,
            TextDirection::TopToBottom => point.y - rect.y0,
            TextDirection::BottomToTop => rect.y1 - point.y,
        };
        if relative_pos >= f64::from(*position) && relative_pos < f64::from(*position + *width) {
            return i;
        }
    }
    character_lengths.len()
}

impl<'a> Node<'a> {
    /// The direction text runs, the node's own or its nearest ancestor's.
    pub(crate) fn text_direction(&self) -> Option<TextDirection> {
        self.fetch_inherited_property(crate::model::Node::text_direction)
    }

    pub(crate) fn text_runs(&self) -> impl DoubleEndedIterator<Item = Node<'a>> + use<'a> {
        let id = self.id();
        self.filtered_children(move |node| text_node_filter(id, node))
    }

    fn following_text_runs(
        &self,
        root_node: &Node,
    ) -> impl DoubleEndedIterator<Item = Node<'a>> + use<'a> {
        let id = root_node.id();
        self.following_filtered_siblings(move |node| text_node_filter(id, node))
    }

    fn preceding_text_runs(
        &self,
        root_node: &Node,
    ) -> impl DoubleEndedIterator<Item = Node<'a>> + use<'a> {
        let id = root_node.id();
        self.preceding_filtered_siblings(move |node| text_node_filter(id, node))
    }

    pub(crate) fn supports_text_ranges(&self) -> bool {
        (self.is_text_input()
            || matches!(self.role(), Role::Label | Role::Document | Role::Terminal))
            && self.text_runs().next().is_some()
    }

    fn document_start_inner(&self) -> InnerPosition<'a> {
        let node = self.text_runs().next().expect("a text document has a run");
        InnerPosition {
            node,
            character_index: 0,
        }
    }

    pub(crate) fn document_start(&self) -> Position<'a> {
        Position {
            root_node: *self,
            inner: self.document_start_inner(),
        }
    }

    fn document_end_inner(&self) -> InnerPosition<'a> {
        let node = self
            .text_runs()
            .next_back()
            .expect("a text document has a run");
        InnerPosition {
            node,
            character_index: node.data().character_lengths().len(),
        }
    }

    pub(crate) fn document_end(&self) -> Position<'a> {
        Position {
            root_node: *self,
            inner: self.document_end_inner(),
        }
    }

    pub(crate) fn document_range(&self) -> Range<'a> {
        let start = self.document_start_inner();
        let end = self.document_end_inner();
        Range::new(*self, start, end)
    }

    pub(crate) fn text_selection(&self) -> Option<Range<'a>> {
        let selection = self.data().text_selection()?;
        let anchor = InnerPosition::clamped_upgrade(self.tree_state, selection.anchor)?;
        let focus = InnerPosition::clamped_upgrade(self.tree_state, selection.focus)?;
        if !self.text_runs().any(|run| run.id() == anchor.node.id())
            || !self.text_runs().any(|run| run.id() == focus.node.id())
        {
            return None;
        }
        Some(Range::new(*self, anchor, focus))
    }

    pub(crate) fn text_selection_focus(&self) -> Option<Position<'a>> {
        let selection = self.data().text_selection()?;
        let focus = InnerPosition::clamped_upgrade(self.tree_state, selection.focus)?;
        if !self.text_runs().any(|run| run.id() == focus.node.id()) {
            return None;
        }
        Some(Position {
            root_node: *self,
            inner: focus,
        })
    }

    /// Returns the nearest text position to the given point in this node's coordinate space.
    pub(crate) fn text_position_at_point(&self, point: Point) -> Position<'a> {
        let id = self.id();
        if let Some((node, point)) = self.hit_test(point, &move |node| text_node_filter(id, node))
            && node.role() == Role::TextRun
        {
            let pos = InnerPosition {
                node,
                character_index: character_index_at_point(&node, point),
            };
            return Position {
                root_node: *self,
                inner: pos,
            };
        }

        if let Some(node) = self.text_runs().next()
            && let Some(rect) = node.bounding_box_in_coordinate_space(self)
        {
            let origin = rect.origin();
            if point.x < origin.x || point.y < origin.y {
                return self.document_start();
            }
        }

        for node in self.text_runs().rev() {
            if let Some(rect) = node.bounding_box_in_coordinate_space(self)
                && let Some(direction) = node.text_direction()
            {
                let is_past_end = match direction {
                    TextDirection::LeftToRight => {
                        point.y >= rect.y0 && point.y < rect.y1 && point.x >= rect.x1
                    }
                    TextDirection::RightToLeft => {
                        point.y >= rect.y0 && point.y < rect.y1 && point.x < rect.x0
                    }
                    TextDirection::TopToBottom => {
                        point.x >= rect.x0 && point.x < rect.x1 && point.y >= rect.y1
                    }
                    TextDirection::BottomToTop => {
                        point.x >= rect.x0 && point.x < rect.x1 && point.y < rect.y0
                    }
                };
                if is_past_end {
                    return Position {
                        root_node: *self,
                        inner: InnerPosition {
                            node,
                            character_index: node.data().character_lengths().len(),
                        },
                    };
                }
            }
        }

        self.document_end()
    }

    /// The position `index` Unicode scalar values into the document (`None` past its length).
    pub(crate) fn text_position_from_global_usv_index(&self, index: usize) -> Option<Position<'a>> {
        let mut total_length = 0usize;
        for node in self.text_runs() {
            let node_text = node.data().value().unwrap_or_default();
            let node_text_length = node_text.chars().count();
            let new_total_length = total_length + node_text_length;
            if index >= total_length && index < new_total_length {
                let index = index - total_length;
                let mut utf8_length = 0usize;
                let mut usv_length = 0usize;
                for (character_index, utf8_char_length) in
                    node.data().character_lengths().iter().enumerate()
                {
                    let new_utf8_length = utf8_length + (*utf8_char_length as usize);
                    let char_str = &node_text[utf8_length..new_utf8_length];
                    let usv_char_length = char_str.chars().count();
                    let new_usv_length = usv_length + usv_char_length;
                    if index >= usv_length && index < new_usv_length {
                        return Some(Position {
                            root_node: *self,
                            inner: InnerPosition {
                                node,
                                character_index,
                            },
                        });
                    }
                    utf8_length = new_utf8_length;
                    usv_length = new_usv_length;
                }
                panic!("index out of range");
            }
            total_length = new_total_length;
        }
        if index == total_length {
            return Some(self.document_end());
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{
        Node as Data, NodeId as LocalNodeId, Point, Rect, Role, TextDirection,
        TextPosition as WeakPosition, TextSelection, Tree, TreeId, TreeUpdate,
    };
    use crate::platform::Publication;

    #[test]
    fn word_navigation_past_255_characters_does_not_wrap_u8() {
        let mut root = Data::new(Role::Document);
        root.set_children(vec![LocalNodeId(2), LocalNodeId(3)]);

        let mut long_run = Data::new(Role::TextRun);
        let text = format!("word {}tail", "a".repeat(260));
        long_run.set_value(text.as_str());
        long_run.set_character_lengths(vec![1; text.len()]);
        long_run.set_word_starts(vec![0, 5]);

        let mut next_run = Data::new(Role::TextRun);
        next_run.set_value("next");
        next_run.set_character_lengths(vec![1; 4]);
        next_run.set_word_starts(vec![0]);

        let mut pub_state = Publication::default();
        pub_state.update(TreeUpdate {
            nodes: vec![
                (LocalNodeId(1), root),
                (LocalNodeId(2), long_run),
                (LocalNodeId(3), next_run),
            ],
            tree: Some(Tree::new(LocalNodeId(1))),
            tree_id: TreeId::ROOT,
            focus: LocalNodeId(1),
        });
        let tree = pub_state.tree().unwrap();
        let doc = tree.root();

        let pos_256 = doc.text_position_from_global_usv_index(256).unwrap();
        assert_eq!(pos_256.inner.character_index, 256);
        assert!(
            !pos_256.is_word_start(),
            "index 256 must not wrap to u8 index 0"
        );

        let forward = pos_256.forward_to_word_start();
        assert_eq!(forward.inner.node.id(), NodeId(LocalNodeId(3)));
        assert_eq!(forward.inner.character_index, 0);

        let backward = pos_256.backward_to_word_start();
        assert_eq!(backward.inner.node.id(), NodeId(LocalNodeId(2)));
        assert_eq!(backward.inner.character_index, 5);
    }

    #[test]
    fn forward_to_character_end_clamps_at_document_end_and_degenerate_cross_run_range_is_empty() {
        let mut root = Data::new(Role::Document);
        root.set_children(vec![LocalNodeId(2), LocalNodeId(3)]);

        let mut run1 = Data::new(Role::TextRun);
        run1.set_value("abc");
        run1.set_character_lengths(vec![1, 1, 1]);

        let mut run2 = Data::new(Role::TextRun);
        run2.set_value("def");
        run2.set_character_lengths(vec![1, 1, 1]);

        let mut pub_state = Publication::default();
        pub_state.update(TreeUpdate {
            nodes: vec![
                (LocalNodeId(1), root),
                (LocalNodeId(2), run1),
                (LocalNodeId(3), run2),
            ],
            tree: Some(Tree::new(LocalNodeId(1))),
            tree_id: TreeId::ROOT,
            focus: LocalNodeId(1),
        });
        let tree = pub_state.tree().unwrap();
        let doc = tree.root();

        let end = doc.document_end();
        let stepped = end.forward_to_character_end();
        assert_eq!(stepped, end);
        assert_eq!(stepped.to_global_usv_index(), 6);

        let r1 = tree.node_by_id(NodeId(LocalNodeId(2))).unwrap();
        let r2 = tree.node_by_id(NodeId(LocalNodeId(3))).unwrap();
        let cross_degenerate = Range::new(
            doc,
            InnerPosition {
                node: r1,
                character_index: 3,
            },
            InnerPosition {
                node: r2,
                character_index: 0,
            },
        );
        assert!(cross_degenerate.is_degenerate());
        assert_eq!(cross_degenerate.text(), "");
    }

    #[test]
    fn mismatched_geometry_lengths_and_foreign_text_selection_stay_in_bounds() {
        let mut window = Data::new(Role::Window);
        window.set_children(vec![LocalNodeId(2), LocalNodeId(4)]);

        let mut doc1 = Data::new(Role::Document);
        doc1.set_children(vec![LocalNodeId(3)]);
        doc1.set_text_direction(TextDirection::LeftToRight);
        doc1.set_text_selection(TextSelection {
            anchor: WeakPosition {
                node: LocalNodeId(5),
                character_index: 0,
            },
            focus: WeakPosition {
                node: LocalNodeId(5),
                character_index: 2,
            },
        });

        let mut short_geom_run = Data::new(Role::TextRun);
        short_geom_run.set_value("abcd");
        short_geom_run.set_character_lengths(vec![1, 1, 1, 1]);
        short_geom_run.set_bounds(Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 40.0,
            y1: 10.0,
        });
        short_geom_run.set_character_positions(vec![0.0]);
        short_geom_run.set_character_widths(vec![10.0]);

        let mut doc2 = Data::new(Role::Document);
        doc2.set_children(vec![LocalNodeId(5)]);
        doc2.set_text_direction(TextDirection::LeftToRight);

        let mut long_geom_run = Data::new(Role::TextRun);
        long_geom_run.set_value("xy");
        long_geom_run.set_character_lengths(vec![1, 1]);
        long_geom_run.set_bounds(Rect {
            x0: 0.0,
            y0: 0.0,
            x1: 40.0,
            y1: 10.0,
        });
        long_geom_run.set_character_positions(vec![0.0, 10.0, 20.0, 30.0]);
        long_geom_run.set_character_widths(vec![10.0, 10.0, 10.0, 10.0]);

        let mut pub_state = Publication::default();
        pub_state.update(TreeUpdate {
            nodes: vec![
                (LocalNodeId(1), window),
                (LocalNodeId(2), doc1),
                (LocalNodeId(3), short_geom_run),
                (LocalNodeId(4), doc2),
                (LocalNodeId(5), long_geom_run),
            ],
            tree: Some(Tree::new(LocalNodeId(1))),
            tree_id: TreeId::ROOT,
            focus: LocalNodeId(2),
        });
        let tree = pub_state.tree().unwrap();
        let d1 = tree.node_by_id(NodeId(LocalNodeId(2))).unwrap();
        assert!(d1.text_selection().is_none());
        assert!(d1.text_selection_focus().is_none());
        assert!(d1.document_range().bounding_boxes().is_empty());

        let d2 = tree.node_by_id(NodeId(LocalNodeId(4))).unwrap();
        let hit = d2.text_position_at_point(Point { x: 35.0, y: 5.0 });
        assert!(hit.inner.character_index <= 2);
        assert_eq!(hit.to_global_usv_index(), 2);
    }
}
