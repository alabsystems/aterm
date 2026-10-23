// Copyright 2021 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_consumer 0.37.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/node.rs, modified by the aterm project in 2026.
//
// Derived from Chromium's accessibility abstraction.
// Copyright 2021 The Chromium Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-CHROMIUM file.

//! One node of a [`State`]: its tree position, model semantics, and geometry.

use std::fmt;

use crate::model::{
    Action, Affine, Live, Node as NodeData, NodeId as LocalNodeId, Orientation, Point, Rect, Role,
    TextSelection, Toggled,
};

use super::State;
use super::filters::FilterResult;
use super::iterators::{FilteredChildren, FollowingFilteredSiblings, PrecedingFilteredSiblings};

/// A node's id in its window's tree.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Debug)]
pub(crate) struct NodeId(pub(crate) LocalNodeId);

impl From<NodeId> for u128 {
    fn from(id: NodeId) -> Self {
        u128::from(id.0.0) << 64
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct ParentAndIndex(pub(crate) NodeId, pub(crate) usize);

#[derive(Clone, Debug)]
pub(crate) struct NodeState {
    pub(crate) parent_and_index: Option<ParentAndIndex>,
    pub(crate) incarnation: u64,
    pub(crate) data: NodeData,
}

#[derive(Copy, Clone, Debug)]
pub(crate) struct Node<'a> {
    pub(crate) tree_state: &'a State,
    pub(crate) id: NodeId,
    pub(crate) state: &'a NodeState,
}

impl<'a> Node<'a> {
    #[inline]
    pub(crate) const fn incarnation(&self) -> u64 {
        self.state.incarnation
    }

    #[inline]
    pub(crate) const fn data(&self) -> &'a NodeData {
        &self.state.data
    }

    #[inline]
    pub(crate) fn is_focused(&self) -> bool {
        self.tree_state.focus_id() == Some(self.id())
    }

    #[inline]
    pub(crate) fn is_focused_in_tree(&self) -> bool {
        self.tree_state.focus == self.id()
    }

    #[inline]
    pub(crate) fn is_focusable(&self, parent_filter: &impl Fn(&Node) -> FilterResult) -> bool {
        self.supports_action(Action::Focus, parent_filter) || self.is_focused_in_tree()
    }

    #[inline]
    pub(crate) fn is_root(&self) -> bool {
        self.id() == self.tree_state.root_id()
    }

    #[inline]
    pub(crate) fn parent_id(&self) -> Option<NodeId> {
        self.state
            .parent_and_index
            .as_ref()
            .map(|ParentAndIndex(id, _)| *id)
    }

    #[inline]
    pub(crate) fn parent(&self) -> Option<Node<'a>> {
        self.parent_id()
            .and_then(|id| self.tree_state.node_by_id(id))
    }

    pub(crate) fn filtered_parent(
        &self,
        filter: &impl Fn(&Node) -> FilterResult,
    ) -> Option<Node<'a>> {
        let mut current = self.parent()?;
        loop {
            if filter(&current) == FilterResult::Include {
                return Some(current);
            }
            current = current.parent()?;
        }
    }

    #[inline]
    pub(crate) fn parent_and_index(self) -> Option<(Node<'a>, usize)> {
        let ParentAndIndex(parent, index) = self.state.parent_and_index?;
        Some((self.tree_state.node_by_id(parent)?, index))
    }

    pub(crate) fn children(
        &self,
    ) -> impl DoubleEndedIterator<Item = Node<'a>> + ExactSizeIterator + use<'a> {
        let state = self.tree_state;
        let data = self.data();
        data.children().iter().map(move |id| {
            state
                .node_by_id(NodeId(*id))
                .expect("a node's children are in its tree")
        })
    }

    pub(crate) fn filtered_children<F: Fn(&Node) -> FilterResult>(
        &self,
        filter: F,
    ) -> impl DoubleEndedIterator<Item = Node<'a>> + use<'a, F> {
        FilteredChildren::new(*self, filter)
    }

    /// The sibling after this node, filtered or not.
    pub(crate) fn next_sibling(&self) -> Option<Node<'a>> {
        let (parent, index) = self.parent_and_index()?;
        let id = parent.data().children().get(index + 1)?;
        self.tree_state.node_by_id(NodeId(*id))
    }

    /// The sibling before this node, filtered or not.
    pub(crate) fn previous_sibling(&self) -> Option<Node<'a>> {
        let (parent, index) = self.parent_and_index()?;
        let id = parent.data().children().get(index.checked_sub(1)?)?;
        self.tree_state.node_by_id(NodeId(*id))
    }

    pub(crate) fn following_filtered_siblings<F: Fn(&Node) -> FilterResult>(
        &self,
        filter: F,
    ) -> impl DoubleEndedIterator<Item = Node<'a>> + use<'a, F> {
        FollowingFilteredSiblings::new(*self, filter)
    }

    pub(crate) fn preceding_filtered_siblings<F: Fn(&Node) -> FilterResult>(
        &self,
        filter: F,
    ) -> impl DoubleEndedIterator<Item = Node<'a>> + use<'a, F> {
        PrecedingFilteredSiblings::new(*self, filter)
    }

    /// Returns the transform defined directly on this node, or the identity transform.
    #[inline]
    pub(crate) fn direct_transform(&self) -> Affine {
        self.data()
            .transform()
            .map_or(Affine::IDENTITY, |value| *value)
    }

    /// Returns the combined affine transform of this node and its ancestors up to the tree root.
    pub(crate) fn transform(&self) -> Affine {
        let mut acc = self.direct_transform();
        let mut current = *self;
        while let Some(parent) = current.parent() {
            acc = parent.direct_transform() * acc;
            current = parent;
        }
        acc
    }

    fn relative_transform(&self, stop_at: &Node) -> Affine {
        let mut acc = self.direct_transform();
        let mut current = *self;
        while let Some(parent) = current.parent() {
            if parent.id() == stop_at.id() {
                break;
            }
            acc = parent.direct_transform() * acc;
            current = parent;
        }
        acc
    }

    #[inline]
    pub(crate) const fn raw_bounds(&self) -> Option<Rect> {
        self.data().bounds()
    }

    /// Returns the node's transformed bounding box relative to the tree's container.
    pub(crate) fn bounding_box(&self) -> Option<Rect> {
        self.raw_bounds()
            .as_ref()
            .map(|rect| self.transform().transform_rect_bbox(*rect))
    }

    pub(crate) fn bounding_box_in_coordinate_space(&self, other: &Node) -> Option<Rect> {
        self.raw_bounds()
            .as_ref()
            .map(|rect| self.relative_transform(other).transform_rect_bbox(*rect))
    }

    pub(crate) fn hit_test(
        &self,
        point: Point,
        filter: &impl Fn(&Node) -> FilterResult,
    ) -> Option<(Node<'a>, Point)> {
        enum Frame<'n> {
            Visit(Node<'n>, Point),
            Test(Node<'n>, Point),
        }

        let mut stack = Vec::with_capacity(self.children().len() + 1);
        stack.push(Frame::Visit(*self, point));
        while let Some(frame) = stack.pop() {
            match frame {
                Frame::Test(node, point) => {
                    if let Some(rect) = &node.raw_bounds()
                        && rect.contains(point)
                    {
                        return Some((node, point));
                    }
                }
                Frame::Visit(node, point) => {
                    let filter_result = filter(&node);
                    if filter_result == FilterResult::ExcludeSubtree || node.is_offscreen() {
                        continue;
                    }
                    if filter_result == FilterResult::Include {
                        stack.push(Frame::Test(node, point));
                    }
                    for child in node.children() {
                        let child_point = child.direct_transform().inverse() * point;
                        stack.push(Frame::Visit(child, child_point));
                    }
                }
            }
        }

        None
    }

    /// Returns the deepest filtered node at `point` in this node's coordinate space.
    pub(crate) fn node_at_point(
        &self,
        point: Point,
        filter: &impl Fn(&Node) -> FilterResult,
    ) -> Option<Node<'a>> {
        self.hit_test(point, filter).map(|(node, _)| node)
    }

    #[inline]
    pub(crate) const fn id(&self) -> NodeId {
        self.id
    }

    #[inline]
    pub(crate) const fn role(&self) -> Role {
        self.data().role()
    }

    #[inline]
    pub(crate) const fn is_disabled(&self) -> bool {
        self.data().is_disabled()
    }

    #[inline]
    pub(crate) const fn is_offscreen(&self) -> bool {
        self.data().is_offscreen()
    }

    #[inline]
    pub(crate) fn is_read_only(&self) -> bool {
        self.data().is_read_only()
            || self.should_have_read_only_state_by_default()
            || !self.is_read_only_supported()
    }

    #[inline]
    pub(crate) fn is_read_only_or_disabled(&self) -> bool {
        self.is_read_only() || self.is_disabled()
    }

    #[inline]
    pub(crate) const fn toggled(&self) -> Option<Toggled> {
        self.data().toggled()
    }

    #[inline]
    pub(crate) const fn numeric_value(&self) -> Option<f64> {
        self.data().numeric_value()
    }

    #[inline]
    pub(crate) const fn min_numeric_value(&self) -> Option<f64> {
        self.data().min_numeric_value()
    }

    #[inline]
    pub(crate) const fn max_numeric_value(&self) -> Option<f64> {
        self.data().max_numeric_value()
    }

    #[inline]
    pub(crate) const fn numeric_value_step(&self) -> Option<f64> {
        self.data().numeric_value_step()
    }

    /// The first `Some` of `getter` on this node and its ancestors.
    pub(crate) fn fetch_inherited_property<T>(
        &self,
        getter: fn(&'a NodeData) -> Option<T>,
    ) -> Option<T> {
        let mut node = *self;
        loop {
            let value = getter(node.data());
            if value.is_some() {
                return value;
            }
            node = node.parent()?;
        }
    }

    #[inline]
    pub(crate) const fn is_text_input(&self) -> bool {
        matches!(
            self.role(),
            Role::TextInput | Role::MultilineTextInput | Role::SearchInput
        )
    }

    #[inline]
    pub(crate) fn is_multiline(&self) -> bool {
        self.role() == Role::MultilineTextInput
    }

    /// The orientation a list box and a tab list have by their role.
    #[inline]
    pub(crate) const fn orientation(&self) -> Option<Orientation> {
        match self.role() {
            Role::ListBox => Some(Orientation::Vertical),
            Role::TabList => Some(Orientation::Horizontal),
            _ => None,
        }
    }

    #[inline]
    pub(crate) fn is_dialog(&self) -> bool {
        self.role() == Role::AlertDialog
    }

    #[inline]
    pub(crate) const fn is_clickable(
        &self,
        parent_filter: &impl Fn(&Node) -> FilterResult,
    ) -> bool {
        self.supports_action(Action::Click, parent_filter)
    }

    #[inline]
    pub(crate) const fn is_selectable(&self) -> bool {
        self.is_selected().is_some() && !self.is_disabled()
    }

    /// Whether this node supports `action`.
    #[inline]
    pub(crate) const fn supports_action(
        &self,
        action: Action,
        _parent_filter: &impl Fn(&Node) -> FilterResult,
    ) -> bool {
        self.data().supports_action(action)
    }
}

const fn descendant_label_filter(node: &Node) -> FilterResult {
    match node.role() {
        Role::Label => FilterResult::Include,
        _ => FilterResult::ExcludeSubtree,
    }
}

impl<'a> Node<'a> {
    /// The label children that name a control without a label of its own.
    pub(crate) fn labelled_by(&self) -> impl DoubleEndedIterator<Item = Node<'a>> + use<'a> {
        let from_children = matches!(
            self.role(),
            Role::Button | Role::Link | Role::MenuItem | Role::MenuItemRadio
        );
        from_children
            .then(|| FilteredChildren::new(*self, &descendant_label_filter))
            .into_iter()
            .flatten()
    }

    #[inline]
    pub(crate) fn label_comes_from_value(&self) -> bool {
        self.role() == Role::Label
    }

    pub(crate) fn label(&self) -> Option<String> {
        let mut result = String::new();
        self.write_label(&mut result)
            .unwrap_or_default()
            .then_some(result)
    }

    fn write_label_direct<W: fmt::Write>(&self, mut writer: W) -> Result<bool, fmt::Error> {
        if let Some(label) = &self.data().label() {
            writer.write_str(label)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    pub(crate) fn write_label<W: fmt::Write>(&self, mut writer: W) -> Result<bool, fmt::Error> {
        if self.write_label_direct(&mut writer)? {
            Ok(true)
        } else {
            let mut wrote_one = false;
            for node in self.labelled_by() {
                let writer = SpacePrefixingWriter {
                    inner: &mut writer,
                    need_prefix: wrote_one,
                };
                let wrote_this_time = if node.label_comes_from_value() {
                    node.write_value(writer)
                } else {
                    node.write_label_direct(writer)
                }?;
                wrote_one = wrote_one || wrote_this_time;
            }
            Ok(wrote_one)
        }
    }

    pub(crate) fn description(&self) -> Option<String> {
        self.data()
            .description()
            .map(|description| description.to_string())
    }

    #[inline]
    pub(crate) fn url(&self) -> Option<&str> {
        self.data().url()
    }

    #[inline]
    pub(crate) fn supports_url(&self) -> bool {
        self.role() == Role::Link && self.url().is_some()
    }

    pub(crate) fn value(&self) -> Option<String> {
        let mut result = String::new();
        self.write_value(&mut result)
            .unwrap_or_default()
            .then_some(result)
    }

    pub(crate) fn write_value<W: fmt::Write>(&self, mut writer: W) -> Result<bool, fmt::Error> {
        if let Some(value) = &self.data().value() {
            writer.write_str(value)?;
            Ok(true)
        } else if self.supports_text_ranges() && !self.is_multiline() {
            self.document_range().write_text(writer)?;
            Ok(true)
        } else {
            Ok(false)
        }
    }

    #[inline]
    pub(crate) const fn is_read_only_supported(&self) -> bool {
        self.is_text_input()
            || matches!(
                self.role(),
                Role::ListBox | Role::MenuItemRadio | Role::Slider | Role::Switch
            )
    }

    #[inline]
    pub(crate) const fn should_have_read_only_state_by_default(&self) -> bool {
        matches!(
            self.role(),
            Role::Document | Role::List | Role::ProgressIndicator | Role::Meter | Role::Grid
        )
    }

    pub(crate) fn live(&self) -> Live {
        self.data()
            .live()
            .unwrap_or_else(|| self.parent().map_or(Live::Off, |parent| parent.live()))
    }

    #[inline]
    pub(crate) const fn is_selected(&self) -> Option<bool> {
        self.data().is_selected()
    }

    #[inline]
    pub(crate) const fn is_item_like(&self) -> bool {
        matches!(
            self.role(),
            Role::MenuItem | Role::MenuItemRadio | Role::ListBoxOption | Role::Tab
        )
    }

    #[inline]
    pub(crate) const fn is_container_with_selectable_children(&self) -> bool {
        matches!(self.role(), Role::ListBox | Role::Menu | Role::TabList)
    }

    #[inline]
    pub(crate) fn raw_text_selection(&self) -> Option<&TextSelection> {
        self.data().text_selection()
    }

    pub(crate) fn relative_index_path(&self, ancestor_id: NodeId) -> Vec<usize> {
        let mut result = Vec::new();
        let mut current = *self;
        while current.id() != ancestor_id {
            let Some((parent, index)) = current.parent_and_index() else {
                break;
            };
            result.push(index);
            current = parent;
        }
        result.reverse();
        result
    }

    pub(crate) fn first_filtered_child(
        &self,
        filter: &impl Fn(&Node) -> FilterResult,
    ) -> Option<Node<'a>> {
        let mut stack = vec![self.children()];
        while let Some(iter) = stack.last_mut() {
            if let Some(child) = iter.next() {
                match filter(&child) {
                    FilterResult::Include => return Some(child),
                    FilterResult::ExcludeNode => stack.push(child.children()),
                    FilterResult::ExcludeSubtree => {}
                }
            } else {
                stack.pop();
            }
        }
        None
    }

    pub(crate) fn last_filtered_child(
        &self,
        filter: &impl Fn(&Node) -> FilterResult,
    ) -> Option<Node<'a>> {
        let mut stack = vec![self.children().rev()];
        while let Some(iter) = stack.last_mut() {
            if let Some(child) = iter.next() {
                match filter(&child) {
                    FilterResult::Include => return Some(child),
                    FilterResult::ExcludeNode => stack.push(child.children().rev()),
                    FilterResult::ExcludeSubtree => {}
                }
            } else {
                stack.pop();
            }
        }
        None
    }

    pub(crate) fn selection_container(
        &self,
        filter: &impl Fn(&Node) -> FilterResult,
    ) -> Option<Node<'a>> {
        self.filtered_parent(&|parent| match filter(parent) {
            FilterResult::Include if parent.is_container_with_selectable_children() => {
                FilterResult::Include
            }
            FilterResult::Include => FilterResult::ExcludeNode,
            filter_result => filter_result,
        })
    }

    pub(crate) fn items<F: Fn(&Node) -> FilterResult>(
        &self,
        filter: F,
    ) -> impl DoubleEndedIterator<Item = Node<'a>> + use<'a, F> {
        self.filtered_children(move |child| match filter(child) {
            FilterResult::Include if child.is_item_like() => FilterResult::Include,
            FilterResult::Include => FilterResult::ExcludeNode,
            filter_result => filter_result,
        })
    }
}

struct SpacePrefixingWriter<W: fmt::Write> {
    inner: W,
    need_prefix: bool,
}

impl<W: fmt::Write> SpacePrefixingWriter<W> {
    fn write_prefix_if_needed(&mut self) -> fmt::Result {
        if self.need_prefix {
            self.inner.write_char(' ')?;
            self.need_prefix = false;
        }
        Ok(())
    }
}

impl<W: fmt::Write> fmt::Write for SpacePrefixingWriter<W> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        self.write_prefix_if_needed()?;
        self.inner.write_str(s)
    }

    fn write_char(&mut self, c: char) -> fmt::Result {
        self.write_prefix_if_needed()?;
        self.inner.write_char(c)
    }
}
