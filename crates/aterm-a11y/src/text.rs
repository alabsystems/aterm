// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_consumer 0.37.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/text.rs, and cut by the aterm project in 2026 to what the
// AT-SPI Text interface reads (aterm's model has no text attributes, so no
// format runs, and no lines spanning runs), with one fix: a blank line no
// longer stalls or misplaces a paragraph search (see
// `Position::backward_to_line_start` and `Position::is_paragraph_start`).

//! Text documents: a node whose text runs are its text, positions in it,
//! ranges between them, and the navigation by character, word, line and
//! paragraph that AT-SPI's `GetStringAtOffset` and its kin answer from.
//!
//! A document is a text input, a label, a document or a terminal with at
//! least one `Role::TextRun` under it. Each run is one line; a run whose
//! text ends in a line break ends a paragraph. A run's characters are the
//! model's `character_lengths` (a character may be several code points), and
//! offsets on the bus count Unicode scalar values.

use std::cmp::Ordering;
use std::fmt;

use crate::model::{Point, Rect, Role, TextDirection, TextPosition as WeakPosition, TextSelection};
use crate::tree::{FilterResult, Node, NodeId};

/// A position in a document: a run, and a character index in it (its length
/// when at its end).
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
            || self
                .inner
                .node
                .data()
                .word_starts()
                .binary_search(&(self.inner.character_index as u8))
                .is_ok()
    }

    #[inline]
    pub(crate) const fn is_line_start(&self) -> bool {
        self.inner.is_line_start()
    }

    /// A line start with no text before it, or whose text before it ends in
    /// a line break. accesskit_consumer looked only at the line just before,
    /// so a line after an empty one never started a paragraph, and a
    /// paragraph search from it went on past the break to the paragraph
    /// before, one that does not hold the offset.
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

    /// The position's offset in Unicode scalar values from the document's
    /// start: AT-SPI's text offset.
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
        Self {
            root_node: self.root_node,
            inner: InnerPosition {
                node: pos.node,
                character_index: pos.character_index + 1,
            },
        }
    }

    pub(crate) fn forward_to_word_start(&self) -> Self {
        let pos = self.inner.biased_to_start(&self.root_node);
        // Wrap the following in a scope to make sure we can't misuse the
        // `word_starts` local later.
        {
            let word_starts = pos.node.data().word_starts();
            let index = match word_starts.binary_search(&(pos.character_index as u8)) {
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
        // Wrap the following in a scope to make sure we can't misuse the
        // `word_starts` local later.
        {
            let word_starts = self.inner.node.data().word_starts();
            let index = match word_starts.binary_search(&(self.inner.character_index as u8)) {
                Ok(index) => index,
                Err(index) => index,
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

    /// The start of the line before this position (this line's, when inside
    /// it): a position strictly before this one, or the document's start.
    /// An empty line's start biases forward to the line after it, so when
    /// the walk lands where it began it goes on to the line before, and with
    /// no line before it answers the first line's own start, the document's
    /// start, which every paragraph search stops at. accesskit_consumer
    /// returned the starting point in both cases, and its paragraph search
    /// looped forever on a blank line anywhere in a document.
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
        F: FnMut(&Node<'a>) -> Option<T>,
    {
        // If the range is degenerate, we don't want to normalize it.
        // This is important e.g. when getting the bounding rectangle
        // of the caret range when the caret is at the end of a wrapped line.
        let (start, end) = if self.is_degenerate() {
            (self.start, self.start)
        } else {
            let start = self.start.biased_to_start(&self.node);
            let end = self.end.biased_to_end(&self.node);
            (start, end)
        };
        if let Some(result) = f(&start.node) {
            return Some(result);
        }
        if start.node.id() == end.node.id() {
            return None;
        }
        for node in start.node.following_text_runs(&self.node) {
            if let Some(result) = f(&node) {
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
        self.walk(|node| {
            let character_lengths = node.data().character_lengths();
            let start_index = if node.id() == self.start.node.id() {
                self.start.character_index
            } else {
                0
            };
            let end_index = if node.id() == self.end.node.id() {
                self.end.character_index
            } else {
                character_lengths.len()
            };
            let value = node.data().value().unwrap_or_default();
            let s = if start_index == end_index {
                ""
            } else if start_index == 0 && end_index == character_lengths.len() {
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
        // Writing to a String cannot fail.
        let _ = self.write_text(&mut result);
        result
    }

    /// Returns the range's transformed bounding boxes relative to the tree's
    /// container (e.g. window).
    ///
    /// If the return value is empty, it means that the source tree doesn't
    /// provide enough information to calculate bounding boxes. Otherwise,
    /// there will always be at least one box, even if it's zero-width,
    /// as it is for a degenerate range.
    pub(crate) fn bounding_boxes(&self) -> Vec<Rect> {
        let mut result = Vec::new();
        self.walk(|node| {
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
            let start_index = if node.id() == self.start.node.id() {
                self.start.character_index
            } else {
                0
            };
            let end_index = if node.id() == self.end.node.id() {
                self.end.character_index
            } else {
                character_lengths.len()
            };
            if start_index != 0 || end_index != character_lengths.len() {
                let pixel_start = if start_index < character_lengths.len() {
                    positions[start_index]
                } else {
                    positions[start_index - 1] + widths[start_index - 1]
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
                    // Note: The following directions assume that the rectangle,
                    // in the node's coordinate space, is y-down. TBD: Will we
                    // ever encounter a case where this isn't true?
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
        // We use `>=` here because if the two endpoints are equivalent
        // but with a different bias, we want to normalize the bias.
        if self.start.comparable(&self.node) >= self.end.comparable(&self.node) {
            self.start = self.end;
        }
        self.fix_start_bias();
    }

    pub(crate) fn to_text_selection(self) -> TextSelection {
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

/// A document's own node and its runs; any other node between them is
/// looked through.
fn text_node_filter(root_id: NodeId, node: &Node) -> FilterResult {
    if node.id() == root_id || node.role() == Role::TextRun {
        FilterResult::Include
    } else {
        FilterResult::ExcludeNode
    }
}

fn character_index_at_point(node: &Node, point: Point) -> usize {
    // A hit test returned the node, so it has bounds.
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
    for (i, (position, width)) in positions.iter().zip(widths.iter()).enumerate().rev() {
        let relative_pos = match direction {
            TextDirection::LeftToRight => point.x - rect.x0,
            TextDirection::RightToLeft => rect.x1 - point.x,
            // Note: The following directions assume that the rectangle,
            // in the node's coordinate space, is y-down. TBD: Will we
            // ever encounter a case where this isn't true?
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
        Some(Range::new(*self, anchor, focus))
    }

    pub(crate) fn text_selection_focus(&self) -> Option<Position<'a>> {
        let selection = self.data().text_selection()?;
        let focus = InnerPosition::clamped_upgrade(self.tree_state, selection.focus)?;
        Some(Position {
            root_node: *self,
            inner: focus,
        })
    }

    /// Returns the nearest text position to the given point
    /// in this node's coordinate space.
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

        // The following tests can assume that the point is not within
        // any text run.

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
                    // Note: The following directions assume that the rectangle,
                    // in the root node's coordinate space, is y-down. TBD: Will we
                    // ever encounter a case where this isn't true?
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

    /// The position `index` Unicode scalar values into the document, or its
    /// end when `index` is its length; `None` past that.
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
