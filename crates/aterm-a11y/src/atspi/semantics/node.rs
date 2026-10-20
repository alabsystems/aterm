// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/node.rs and src/text_attributes.rs, and cut by the aterm
// project in 2026 to aterm's model: its roles only, no text attributes, no
// relations, author ids, role descriptions, placeholders or set positions
// (so no object attributes), and a poisoned lock recovered rather than
// unwound.
//
// Derived from Chromium's accessibility abstraction.
// Copyright 2017 The Chromium Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-CHROMIUM file.

//! What one node is to an assistive technology, and how it answers each
//! AT-SPI method: [`PlatformNode`] for a window's node, [`PlatformRoot`] for
//! the application. Both hold their tree weakly; a node of a window that is
//! gone answers [`Error::Defunct`].

use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Weak};

use crate::model::{
    Action, ActionData, ActionRequest, Affine, Live, NodeId as LocalNodeId, Orientation, Point,
    Rect, Role, Toggled, TreeId,
};
use crate::tree::{FilterResult, Node, NodeId, State as TreeState, Tree, common_filter as filter};

use super::adapter::Adapter;
use super::context::{AppContext, Context, SharedLock};
use super::util::{
    WindowBounds, atspi_scroll_type_to_scroll_hint, text_position_from_offset,
    text_range_bounds_from_offsets, text_range_from_offset, text_range_from_offsets,
};
use super::{
    Action as AtspiAction, CoordType, Error, Granularity, Interface, InterfaceSet, Layer,
    ObjectEvent, Politeness, Property, Rect as AtspiRect, RelationType, Result, Role as AtspiRole,
    ScrollType, State, StateSet,
};

/// One exported lifetime of a model node. Content updates keep this identity;
/// removal/reinsertion and action-owner changes retire it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub(crate) struct NodeIdentity {
    pub(crate) id: NodeId,
    pub(crate) incarnation: u64,
}

impl NodeIdentity {
    pub(crate) fn of(node: &Node<'_>) -> Self {
        Self {
            id: node.id(),
            incarnation: node.incarnation(),
        }
    }
}

impl From<NodeIdentity> for u128 {
    fn from(node: NodeIdentity) -> Self {
        u128::from(node.id) | u128::from(node.incarnation)
    }
}

pub(crate) struct NodeWrapper<'a>(pub(crate) &'a Node<'a>);

impl NodeWrapper<'_> {
    pub(crate) fn name(&self) -> Option<String> {
        if self.0.label_comes_from_value() {
            self.0.value()
        } else {
            self.0.label()
        }
    }

    pub(crate) fn description(&self) -> Option<String> {
        self.0.description()
    }

    pub(crate) fn parent_id(&self) -> Option<NodeId> {
        self.0.parent_id()
    }

    pub(crate) fn id(&self) -> NodeIdentity {
        NodeIdentity::of(self.0)
    }

    pub(crate) fn cache_node(
        &self,
        adapter_id: usize,
        index_in_parent: i32,
        child_count: i32,
        window_focused: bool,
    ) -> CacheNode {
        let parent = self
            .0
            .filtered_parent(&filter)
            .map_or(NodeIdOrRoot::Root, |node| {
                NodeIdOrRoot::Node(NodeIdentity::of(&node))
            });
        CacheNode {
            adapter_id,
            id: self.id(),
            parent,
            index_in_parent,
            child_count,
            interfaces: self.interfaces(),
            name: self.name().unwrap_or_default(),
            description: self.description().unwrap_or_default(),
            role: self.role(),
            states: self.state(window_focused),
        }
    }

    fn filtered_child_ids(&self) -> impl DoubleEndedIterator<Item = NodeIdentity> {
        self.0
            .filtered_children(&filter)
            .map(|child| NodeIdentity::of(&child))
    }

    pub(crate) fn role(&self) -> AtspiRole {
        match self.0.role() {
            Role::Alert => AtspiRole::Notification,
            Role::AlertDialog => AtspiRole::Alert,
            // TODO: See how to represent ARIA role="application"
            Role::Application => AtspiRole::Embedded,
            Role::Button => {
                if self.0.toggled().is_some() {
                    AtspiRole::ToggleButton
                } else {
                    AtspiRole::Button
                }
            }
            Role::Switch => AtspiRole::ToggleButton,
            Role::Document => AtspiRole::DocumentFrame,
            Role::Group => AtspiRole::Panel,
            Role::Heading => AtspiRole::Heading,
            Role::TextRun => AtspiRole::Static,
            Role::Link => AtspiRole::Link,
            Role::List => AtspiRole::List,
            Role::ListBox => AtspiRole::ListBox,
            Role::ListBoxOption => AtspiRole::ListItem,
            Role::Menu => AtspiRole::Menu,
            Role::MenuItem => AtspiRole::MenuItem,
            Role::MenuItemRadio => AtspiRole::RadioMenuItem,
            Role::Navigation => AtspiRole::Landmark,
            Role::ProgressIndicator => AtspiRole::ProgressBar,
            Role::Slider => AtspiRole::Slider,
            Role::Label => AtspiRole::Label,
            Role::Status => AtspiRole::StatusBar,
            Role::Tab => AtspiRole::PageTab,
            Role::TabList => AtspiRole::PageTabList,
            Role::TextInput | Role::MultilineTextInput | Role::SearchInput => AtspiRole::Entry,
            // In AT-SPI, elements with `AtspiRole::Frame` are windows with titles and
            // buttons, while those with `AtspiRole::Window` are windows without those
            // elements.
            Role::Window => AtspiRole::Frame,
            Role::Terminal => AtspiRole::Terminal,
            // TODO: Are there special cases to consider?
            Role::Unknown => AtspiRole::Unknown,
        }
    }

    fn is_focused(&self) -> bool {
        self.0.is_focused()
    }

    pub(crate) fn state(&self, is_window_focused: bool) -> StateSet {
        let state = self.0;
        let atspi_role = self.role();
        let mut atspi_state = StateSet::empty();
        if is_window_focused
            && ((state.parent_id().is_none() && state.role() == Role::Window)
                || (state.is_dialog()
                    && state.tree_state.active_dialog().map(|d| d.id()) == Some(state.id())))
        {
            atspi_state.insert(State::Active);
        }
        if state.is_text_input() && !state.is_read_only() {
            atspi_state.insert(State::Editable);
        }
        // TODO: Focus and selection.
        if state.is_focusable(&filter) {
            atspi_state.insert(State::Focusable);
        }
        let filter_result = filter(self.0);
        if filter_result == FilterResult::Include {
            atspi_state.insert(State::Visible);
            // Off screen in its scrolling container: there, not showing.
            if !state.is_offscreen() {
                atspi_state.insert(State::Showing);
            }
        }
        if let Some(orientation) = state.orientation() {
            atspi_state.insert(if orientation == Orientation::Horizontal {
                State::Horizontal
            } else {
                State::Vertical
            });
        }
        if atspi_role != AtspiRole::ToggleButton && state.toggled().is_some() {
            atspi_state.insert(State::Checkable);
        }
        if let Some(selected) = state.is_selected() {
            if !state.is_disabled() {
                atspi_state.insert(State::Selectable);
            }
            if selected {
                atspi_state.insert(State::Selected);
            }
        }
        if state.is_text_input() {
            atspi_state.insert(State::SelectableText);
            atspi_state.insert(match state.is_multiline() {
                true => State::MultiLine,
                false => State::SingleLine,
            });
        }

        // Special case for indeterminate progressbar.
        if state.role() == Role::ProgressIndicator && state.numeric_value().is_none() {
            atspi_state.insert(State::Indeterminate);
        }

        // Toggled state
        match state.toggled() {
            Some(Toggled::Mixed) => atspi_state.insert(State::Indeterminate),
            Some(Toggled::True) if atspi_role == AtspiRole::ToggleButton => {
                atspi_state.insert(State::Pressed)
            }
            Some(Toggled::True) => atspi_state.insert(State::Checked),
            _ => {}
        }

        if state.is_read_only_supported() && state.is_read_only_or_disabled() {
            atspi_state.insert(State::ReadOnly);
        } else {
            atspi_state.insert(State::Enabled | State::Sensitive);
        }

        if self.is_focused() {
            atspi_state.insert(State::Focused);
        }

        atspi_state
    }

    fn is_root(&self) -> bool {
        self.0.is_root()
    }

    fn supports_action(&self) -> bool {
        self.0.is_clickable(&filter)
    }

    fn supports_component(&self) -> bool {
        self.0.raw_bounds().is_some() || self.is_root()
    }

    fn supports_hyperlink(&self) -> bool {
        self.0.supports_url()
    }

    fn supports_selection(&self) -> bool {
        self.0.is_container_with_selectable_children()
    }

    fn supports_text(&self) -> bool {
        self.0.supports_text_ranges()
    }

    fn supports_value(&self) -> bool {
        self.current_value().is_some()
    }

    pub(crate) fn interfaces(&self) -> InterfaceSet {
        let mut interfaces = InterfaceSet::new(Interface::Accessible);
        if self.supports_action() {
            interfaces.insert(Interface::Action);
        }
        if self.supports_component() {
            interfaces.insert(Interface::Component);
        }
        if self.supports_hyperlink() {
            interfaces.insert(Interface::Hyperlink);
        }
        if self.supports_selection() {
            interfaces.insert(Interface::Selection);
        }
        if self.supports_text() {
            interfaces.insert(Interface::Text);
        }
        if self.supports_value() {
            interfaces.insert(Interface::Value);
        }
        interfaces
    }

    pub(crate) fn live(&self) -> Politeness {
        match self.0.live() {
            Live::Off => Politeness::None,
            Live::Polite => Politeness::Polite,
            Live::Assertive => Politeness::Assertive,
        }
    }

    fn n_actions(&self) -> i32 {
        if self.0.is_clickable(&filter) { 1 } else { 0 }
    }

    fn get_action_name(&self, index: i32) -> String {
        if index != 0 {
            return String::new();
        }
        String::from(if self.0.is_clickable(&filter) {
            "click"
        } else {
            ""
        })
    }

    fn raw_bounds_and_transform(&self) -> (Option<Rect>, Affine) {
        let state = self.0;
        (state.raw_bounds(), state.direct_transform())
    }

    fn extents(&self, window_bounds: &WindowBounds, coord_type: CoordType) -> Option<Rect> {
        let mut bounds = self.0.bounding_box();
        if self.is_root() {
            let window_bounds = window_bounds.inner.with_origin(Point::ZERO);
            if !window_bounds.is_empty() {
                if let Some(bounds) = &mut bounds {
                    bounds.x0 = bounds.x0.min(window_bounds.x1);
                    bounds.y0 = bounds.y0.min(window_bounds.y1);
                    bounds.x1 = bounds.x1.min(window_bounds.x1);
                    bounds.y1 = bounds.y1.min(window_bounds.y1);
                } else {
                    bounds = Some(window_bounds);
                }
            }
        }
        bounds.map(|bounds| {
            let new_origin = window_bounds.accesskit_point_to_atspi_point(
                bounds.origin(),
                self.0.filtered_parent(&filter),
                coord_type,
            );
            bounds.with_origin(new_origin)
        })
    }

    fn current_value(&self) -> Option<f64> {
        self.0.numeric_value()
    }

    pub(crate) fn notify_changes(
        &self,
        window_bounds: &WindowBounds,
        adapter: &Adapter,
        old: &NodeWrapper<'_>,
    ) {
        self.notify_state_changes(adapter, old);
        self.notify_property_changes(adapter, old);
        self.notify_bounds_changes(window_bounds, adapter, old);
        self.notify_children_changes(adapter, old);
    }

    fn notify_state_changes(&self, adapter: &Adapter, old: &NodeWrapper<'_>) {
        let old_state = old.state(true);
        let new_state = self.state(true);
        let changed_states = old_state ^ new_state;
        for state in changed_states.iter() {
            if state == State::Focused {
                // This is handled specially in `focus_moved`.
                continue;
            }
            adapter.emit_object_event(
                self.id(),
                ObjectEvent::StateChanged(state, new_state.contains(state)),
            );
        }
    }

    fn notify_property_changes(&self, adapter: &Adapter, old: &NodeWrapper<'_>) {
        let name = self.name();
        if name != old.name() {
            let name = name.unwrap_or_default();
            adapter.emit_object_event(
                self.id(),
                ObjectEvent::PropertyChanged(Property::Name(name.clone())),
            );

            let live = self.live();
            if live != Politeness::None {
                adapter.emit_object_event(self.id(), ObjectEvent::Announcement(name, live));
            }
        }
        let description = self.description();
        if description != old.description() {
            adapter.emit_object_event(
                self.id(),
                ObjectEvent::PropertyChanged(Property::Description(
                    description.unwrap_or_default(),
                )),
            );
        }
        let parent_id = self.parent_id();
        if parent_id != old.parent_id() {
            let parent = self
                .0
                .filtered_parent(&filter)
                .map_or(NodeIdOrRoot::Root, |node| {
                    NodeIdOrRoot::Node(NodeIdentity::of(&node))
                });
            adapter.emit_object_event(
                self.id(),
                ObjectEvent::PropertyChanged(Property::Parent(parent)),
            );
        }
        let role = self.role();
        if role != old.role() {
            adapter.emit_object_event(
                self.id(),
                ObjectEvent::PropertyChanged(Property::Role(role)),
            );
        }
        if let Some(value) = self.current_value()
            && Some(value) != old.current_value()
        {
            adapter.emit_object_event(
                self.id(),
                ObjectEvent::PropertyChanged(Property::Value(value)),
            );
        }
    }

    fn notify_bounds_changes(
        &self,
        window_bounds: &WindowBounds,
        adapter: &Adapter,
        old: &NodeWrapper<'_>,
    ) {
        if self.raw_bounds_and_transform() != old.raw_bounds_and_transform()
            && let Some(extents) = self.extents(window_bounds, CoordType::Window)
        {
            adapter.emit_object_event(self.id(), ObjectEvent::BoundsChanged(extents.into()));
        }
    }

    fn notify_children_changes(&self, adapter: &Adapter, old: &NodeWrapper<'_>) {
        let old_filtered_children = old.filtered_child_ids().collect::<Vec<NodeIdentity>>();
        let new_filtered_children = self.filtered_child_ids().collect::<Vec<NodeIdentity>>();
        for (index, child) in new_filtered_children.iter().enumerate() {
            if !old_filtered_children.contains(child) {
                adapter.emit_object_event(self.id(), ObjectEvent::ChildAdded(index, *child));
            }
        }
        for child in old_filtered_children.into_iter() {
            if !new_filtered_children.contains(&child) {
                adapter.emit_object_event(self.id(), ObjectEvent::ChildRemoved(child));
            }
        }
    }
}

/// A node of a window, as the publisher serves it.
#[derive(Clone)]
pub(crate) struct PlatformNode {
    context: Weak<Context>,
    adapter_id: usize,
    identity: NodeIdentity,
}

impl PlatformNode {
    pub(crate) fn new(context: &Arc<Context>, adapter_id: usize, identity: NodeIdentity) -> Self {
        Self {
            context: Arc::downgrade(context),
            adapter_id,
            identity,
        }
    }

    fn upgrade_context(&self) -> Result<Arc<Context>> {
        self.context.upgrade().ok_or(Error::Defunct)
    }

    fn with_tree_and_context<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&Tree, &Context) -> Result<T>,
    {
        let context = self.upgrade_context()?;
        let tree = context.tree.read();
        f(&tree, &context)
    }

    fn resolve_with_context<F, T>(&self, f: F) -> Result<T>
    where
        for<'a> F: FnOnce(Node<'a>, &'a Tree, &Context) -> Result<T>,
    {
        self.with_tree_and_context(|tree, context| {
            if let Some(node) = tree
                .state()
                .node_by_id(self.identity.id)
                .filter(|node| NodeIdentity::of(node) == self.identity)
            {
                f(node, tree, context)
            } else {
                Err(Error::Defunct)
            }
        })
    }

    fn resolve_for_selection_with_context<F, T>(&self, f: F) -> Result<T>
    where
        for<'a> F: FnOnce(Node<'a>, &'a Tree, &Context) -> Result<T>,
    {
        self.resolve_with_context(|node, tree, context| {
            let wrapper = NodeWrapper(&node);
            if wrapper.supports_selection() {
                f(node, tree, context)
            } else {
                Err(Error::UnsupportedInterface)
            }
        })
    }

    fn resolve_for_text_with_context<F, T>(&self, f: F) -> Result<T>
    where
        for<'a> F: FnOnce(Node<'a>, &'a Tree, &Context) -> Result<T>,
    {
        self.resolve_with_context(|node, tree, context| {
            let wrapper = NodeWrapper(&node);
            if wrapper.supports_text() {
                f(node, tree, context)
            } else {
                Err(Error::UnsupportedInterface)
            }
        })
    }

    fn resolve<F, T>(&self, f: F) -> Result<T>
    where
        for<'a> F: FnOnce(Node<'a>) -> Result<T>,
    {
        self.resolve_with_context(|node, _, _| f(node))
    }

    fn resolve_for_selection<F, T>(&self, f: F) -> Result<T>
    where
        for<'a> F: FnOnce(Node<'a>) -> Result<T>,
    {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            if wrapper.supports_selection() {
                f(node)
            } else {
                Err(Error::UnsupportedInterface)
            }
        })
    }

    fn resolve_for_text<F, T>(&self, f: F) -> Result<T>
    where
        for<'a> F: FnOnce(Node<'a>) -> Result<T>,
    {
        self.resolve_for_text_with_context(|node, _, _| f(node))
    }

    /// Hands the window the request `make` builds for `target`, if it is
    /// still in the tree.
    fn request(
        context: &Context,
        state: &TreeState,
        target: NodeId,
        make: impl FnOnce(LocalNodeId) -> ActionRequest,
    ) -> Result<()> {
        let local = state.locate_node(target).ok_or(Error::Defunct)?;
        let mut request = make(local);
        request.context = state
            .node_by_id(target)
            .ok_or(Error::Defunct)?
            .data()
            .action_context();
        context.do_action(request);
        Ok(())
    }

    fn do_action_internal<F>(&self, f: F) -> Result<()>
    where
        F: FnOnce(LocalNodeId) -> ActionRequest,
    {
        let context = self.upgrade_context()?;
        let tree = context.tree.read();
        let node = tree
            .state()
            .node_by_id(self.identity.id)
            .filter(|node| NodeIdentity::of(node) == self.identity)
            .ok_or(Error::Defunct)?;
        let mut request = f(self.identity.id.0);
        request.context = node.data().action_context();
        drop(tree);
        context.do_action(request);
        Ok(())
    }

    pub(crate) fn name(&self) -> Result<String> {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.name().unwrap_or_default())
        })
    }

    pub(crate) fn description(&self) -> Result<String> {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.description().unwrap_or_default())
        })
    }

    pub(crate) fn parent(&self) -> Result<NodeIdOrRoot> {
        self.resolve(|node| {
            let parent = node
                .filtered_parent(&filter)
                .map_or(NodeIdOrRoot::Root, |node| {
                    NodeIdOrRoot::Node(NodeIdentity::of(&node))
                });
            Ok(parent)
        })
    }

    pub(crate) fn child_count(&self) -> Result<i32> {
        self.resolve(|node| {
            i32::try_from(node.filtered_children(&filter).count())
                .map_err(|_| Error::TooManyChildren)
        })
    }

    pub(crate) fn adapter_id(&self) -> usize {
        self.adapter_id
    }

    pub(crate) fn id(&self) -> NodeIdentity {
        self.identity
    }

    /// AT-SPI's `AccessibleId`: aterm's model names none.
    pub(crate) fn accessible_id(&self) -> Result<String> {
        self.resolve(|_| Ok(String::new()))
    }

    pub(crate) fn child_at_index(&self, index: usize) -> Result<Option<NodeIdentity>> {
        self.resolve(|node| {
            let child = node
                .filtered_children(&filter)
                .nth(index)
                .map(|child| NodeIdentity::of(&child));
            Ok(child)
        })
    }

    pub(crate) fn map_children<T, I>(&self, f: impl Fn(NodeIdentity) -> I) -> Result<T>
    where
        T: FromIterator<I>,
    {
        self.resolve(|node| {
            let children = node
                .filtered_children(&filter)
                .map(|child| NodeIdentity::of(&child))
                .map(f)
                .collect();
            Ok(children)
        })
    }

    pub(crate) fn index_in_parent(&self) -> Result<i32> {
        self.resolve_with_context(|node, _tree, context| {
            if node.filtered_parent(&filter).is_some() {
                i32::try_from(node.preceding_filtered_siblings(&filter).count())
                    .map_err(|_| Error::IndexOutOfRange)
            } else {
                let index = context
                    .app_context
                    .read()
                    .adapter_index(self.adapter_id)
                    .map_err(|_| Error::Defunct)?;
                i32::try_from(index).map_err(|_| Error::IndexOutOfRange)
            }
        })
    }

    pub(crate) fn cache_node(&self) -> Result<CacheNode> {
        self.resolve_with_context(|node, tree, context| {
            let index_in_parent = if node.filtered_parent(&filter).is_some() {
                i32::try_from(node.preceding_filtered_siblings(&filter).count())
                    .map_err(|_| Error::IndexOutOfRange)?
            } else {
                let index = context
                    .app_context
                    .read()
                    .adapter_index(self.adapter_id)
                    .map_err(|_| Error::Defunct)?;
                i32::try_from(index).map_err(|_| Error::IndexOutOfRange)?
            };
            let child_count = i32::try_from(node.filtered_children(&filter).count())
                .map_err(|_| Error::TooManyChildren)?;
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.cache_node(
                self.adapter_id,
                index_in_parent,
                child_count,
                tree.state().focus_id().is_some(),
            ))
        })
    }

    /// AT-SPI's relation set: always empty, aterm's model has no relations.
    pub(crate) fn relation_set<T>(
        &self,
        _f: impl Fn(NodeIdentity) -> T,
    ) -> Result<HashMap<RelationType, Vec<T>>> {
        self.resolve(|_| Ok(HashMap::new()))
    }

    pub(crate) fn role(&self) -> Result<AtspiRole> {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.role())
        })
    }

    /// AT-SPI's localized role name: aterm's model has no role descriptions.
    pub(crate) fn localized_role_name(&self) -> Result<String> {
        self.resolve(|_| Ok(String::new()))
    }

    pub(crate) fn state(&self) -> StateSet {
        self.resolve_with_context(|node, tree, _| {
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.state(tree.state().focus_id().is_some()))
        })
        .unwrap_or(State::Defunct.into())
    }

    /// AT-SPI's object attributes: aterm's model sets none of the properties
    /// accesskit_atspi_common read them from (placeholders, set positions
    /// and sizes, braille labels).
    pub(crate) fn attributes(&self) -> Result<HashMap<&'static str, String>> {
        self.resolve(|_| Ok(HashMap::new()))
    }

    pub(crate) fn interfaces(&self) -> Result<InterfaceSet> {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.interfaces())
        })
    }

    pub(crate) fn n_actions(&self) -> Result<i32> {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.n_actions())
        })
    }

    pub(crate) fn action_name(&self, index: i32) -> Result<String> {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.get_action_name(index))
        })
    }

    pub(crate) fn actions(&self) -> Result<Vec<AtspiAction>> {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            let n_actions = wrapper.n_actions();
            Ok((0..n_actions)
                .map(|i| AtspiAction {
                    localized_name: wrapper.get_action_name(i),
                    description: String::new(),
                    key_binding: String::new(),
                })
                .collect())
        })
    }

    pub(crate) fn do_action(&self, index: i32) -> Result<bool> {
        if index != 0 {
            return Ok(false);
        }
        self.do_action_internal(|target_node| ActionRequest {
            context: None,
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node,
            data: None,
        })?;
        Ok(true)
    }

    pub(crate) fn contains(&self, x: i32, y: i32, coord_type: CoordType) -> Result<bool> {
        self.resolve_with_context(|node, _, context| {
            let window_bounds = context.root_window_bounds.read();
            let wrapper = NodeWrapper(&node);
            if let Some(extents) = wrapper.extents(&window_bounds, coord_type) {
                Ok(extents.contains(Point::new(x.into(), y.into())))
            } else {
                Ok(false)
            }
        })
    }

    pub(crate) fn accessible_at_point(
        &self,
        x: i32,
        y: i32,
        coord_type: CoordType,
    ) -> Result<Option<NodeIdentity>> {
        self.resolve_with_context(|node, _, context| {
            let window_bounds = context.root_window_bounds.read();
            let point = window_bounds.atspi_point_to_accesskit_point(
                Point::new(x.into(), y.into()),
                Some(node),
                coord_type,
            );
            let point = node.transform().inverse() * point;
            Ok(node
                .node_at_point(point, &filter)
                .map(|node| NodeIdentity::of(&node)))
        })
    }

    pub(crate) fn extents(&self, coord_type: CoordType) -> Result<AtspiRect> {
        self.resolve_with_context(|node, _, context| {
            let window_bounds = context.root_window_bounds.read();
            let wrapper = NodeWrapper(&node);
            Ok(wrapper
                .extents(&window_bounds, coord_type)
                .map_or(AtspiRect::INVALID, AtspiRect::from))
        })
    }

    /// AT-SPI's layer: a window's layer is AT-SPI's `Window` role's, which
    /// none of aterm's roles maps to, so every node is a widget.
    pub(crate) fn layer(&self) -> Result<Layer> {
        self.resolve(|_| Ok(Layer::Widget))
    }

    pub(crate) fn grab_focus(&self) -> Result<bool> {
        self.do_action_internal(|target_node| ActionRequest {
            context: None,
            action: Action::Focus,
            target_tree: TreeId::ROOT,
            target_node,
            data: None,
        })?;
        Ok(true)
    }

    pub(crate) fn scroll_to(&self, scroll_type: ScrollType) -> Result<bool> {
        self.do_action_internal(|target_node| ActionRequest {
            context: None,
            action: Action::ScrollIntoView,
            target_tree: TreeId::ROOT,
            target_node,
            data: atspi_scroll_type_to_scroll_hint(scroll_type).map(ActionData::ScrollHint),
        })?;
        Ok(true)
    }

    pub(crate) fn scroll_to_point(&self, coord_type: CoordType, x: i32, y: i32) -> Result<bool> {
        self.resolve_with_context(|node, tree, context| {
            let window_bounds = context.root_window_bounds.read();
            let point = window_bounds.atspi_point_to_accesskit_point(
                Point::new(x.into(), y.into()),
                node.filtered_parent(&filter),
                coord_type,
            );
            Self::request(context, tree.state(), self.identity.id, |target_node| {
                ActionRequest {
                    context: None,
                    action: Action::ScrollToPoint,
                    target_tree: TreeId::ROOT,
                    target_node,
                    data: Some(ActionData::ScrollToPoint(point)),
                }
            })
        })?;
        Ok(true)
    }

    pub(crate) fn n_anchors(&self) -> Result<i32> {
        self.resolve(|node| if node.url().is_some() { Ok(1) } else { Ok(0) })
    }

    pub(crate) fn hyperlink_start_index(&self) -> Result<i32> {
        self.resolve(|_| {
            // TODO: Support rich text
            Ok(-1)
        })
    }

    pub(crate) fn hyperlink_end_index(&self) -> Result<i32> {
        self.resolve(|_| {
            // TODO: Support rich text
            Ok(-1)
        })
    }

    pub(crate) fn hyperlink_object(&self, index: i32) -> Result<Option<NodeIdentity>> {
        self.resolve(|_| {
            if index == 0 {
                Ok(Some(self.identity))
            } else {
                Ok(None)
            }
        })
    }

    pub(crate) fn uri(&self, index: i32) -> Result<String> {
        self.resolve(|node| {
            if index == 0 {
                Ok(node.url().map(|s| s.to_string()).unwrap_or_default())
            } else {
                Ok(String::new())
            }
        })
    }

    pub(crate) fn hyperlink_is_valid(&self) -> Result<bool> {
        self.resolve(|node| Ok(node.url().is_some()))
    }

    pub(crate) fn n_selected_children(&self) -> Result<i32> {
        self.resolve_for_selection(|node| {
            node.items(filter)
                .filter(|item| item.is_selected() == Some(true))
                .count()
                .try_into()
                .map_err(|_| Error::TooManyChildren)
        })
    }

    pub(crate) fn selected_child(
        &self,
        selected_child_index: usize,
    ) -> Result<Option<NodeIdentity>> {
        self.resolve_for_selection(|node| {
            Ok(node
                .items(filter)
                .filter(|item| item.is_selected() == Some(true))
                .nth(selected_child_index)
                .map(|node| NodeIdentity::of(&node)))
        })
    }

    /// Clicks child `child_index` (a filtered child) to select it, unless it
    /// is selected already.
    pub(crate) fn select_child(&self, child_index: usize) -> Result<bool> {
        self.resolve_for_selection_with_context(|node, tree, context| {
            let child = node
                .filtered_children(filter)
                .nth(child_index)
                .ok_or(Error::Defunct)?;
            if let Some(true) = child.is_selected() {
                Ok(true)
            } else if child.is_selectable() && child.is_clickable(&filter) {
                Self::click(context, tree.state(), child.id())?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    pub(crate) fn deselect_selected_child(&self, selected_child_index: usize) -> Result<bool> {
        self.resolve_for_selection_with_context(|node, tree, context| {
            let child = node
                .items(filter)
                .filter(|c| c.is_selected() == Some(true))
                .nth(selected_child_index)
                .ok_or(Error::Defunct)?;
            if child.is_clickable(&filter) {
                Self::click(context, tree.state(), child.id())?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    pub(crate) fn is_child_selected(&self, child_index: usize) -> Result<bool> {
        self.resolve_for_selection(|node| {
            node.filtered_children(filter)
                .nth(child_index)
                .map(|child| child.is_item_like() && child.is_selected() == Some(true))
                .ok_or(Error::Defunct)
        })
    }

    pub(crate) fn select_all(&self) -> Result<bool> {
        // We don't support selecting all children at once.
        Ok(false)
    }

    pub(crate) fn clear_selection(&self) -> Result<bool> {
        // We don't support deselecting all children at once.
        Ok(false)
    }

    /// Clicks child `child_index` (a filtered child) to deselect it, unless
    /// it is unselected already.
    pub(crate) fn deselect_child(&self, child_index: usize) -> Result<bool> {
        self.resolve_for_selection_with_context(|node, tree, context| {
            let child = node
                .filtered_children(filter)
                .nth(child_index)
                .ok_or(Error::Defunct)?;
            if let Some(false) = child.is_selected() {
                Ok(true)
            } else if child.is_selectable() && child.is_clickable(&filter) {
                Self::click(context, tree.state(), child.id())?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    fn click(context: &Context, state: &TreeState, target: NodeId) -> Result<()> {
        Self::request(context, state, target, |target_node| ActionRequest {
            context: None,
            action: Action::Click,
            target_tree: TreeId::ROOT,
            target_node,
            data: None,
        })
    }

    pub(crate) fn character_count(&self) -> Result<i32> {
        self.resolve_for_text(|node| {
            node.document_range()
                .end()
                .to_global_usv_index()
                .try_into()
                .map_err(|_| Error::TooManyCharacters)
        })
    }

    pub(crate) fn caret_offset(&self) -> Result<i32> {
        self.resolve_for_text(|node| {
            node.text_selection_focus().map_or(Ok(-1), |focus| {
                focus
                    .to_global_usv_index()
                    .try_into()
                    .map_err(|_| Error::TooManyCharacters)
            })
        })
    }

    pub(crate) fn string_at_offset(
        &self,
        offset: i32,
        granularity: Granularity,
    ) -> Result<(String, i32, i32)> {
        self.resolve_for_text(|node| {
            let range = text_range_from_offset(&node, offset, granularity)?;
            let text = range.text();
            let start = range
                .start()
                .to_global_usv_index()
                .try_into()
                .map_err(|_| Error::TooManyCharacters)?;
            let end = range
                .end()
                .to_global_usv_index()
                .try_into()
                .map_err(|_| Error::TooManyCharacters)?;

            Ok((text, start, end))
        })
    }

    pub(crate) fn text(&self, start_offset: i32, end_offset: i32) -> Result<String> {
        self.resolve_for_text(|node| {
            let range = text_range_from_offsets(&node, start_offset, end_offset)
                .ok_or(Error::IndexOutOfRange)?;
            Ok(range.text())
        })
    }

    pub(crate) fn set_caret_offset(&self, offset: i32) -> Result<bool> {
        self.resolve_for_text_with_context(|node, tree, context| {
            let offset = text_position_from_offset(&node, offset).ok_or(Error::IndexOutOfRange)?;
            Self::request(context, tree.state(), node.id(), |target_node| {
                ActionRequest {
                    context: None,
                    action: Action::SetTextSelection,
                    target_tree: TreeId::ROOT,
                    target_node,
                    data: Some(ActionData::SetTextSelection(
                        offset.to_degenerate_range().to_text_selection(),
                    )),
                }
            })?;
            Ok(true)
        })
    }

    /// One text attribute at `offset`: aterm's model has none, so it is
    /// empty for every name, once the offset is checked.
    pub(crate) fn text_attribute_value(
        &self,
        offset: i32,
        _attribute_name: &str,
    ) -> Result<String> {
        self.resolve_for_text(|node| {
            text_position_from_offset(&node, offset).ok_or(Error::IndexOutOfRange)?;
            Ok(String::new())
        })
    }

    pub(crate) fn text_attributes(
        &self,
        offset: i32,
    ) -> Result<(HashMap<&'static str, String>, i32, i32)> {
        self.text_attribute_run(offset, false)
    }

    /// The document's default text attributes: none.
    pub(crate) fn default_text_attributes(&self) -> Result<HashMap<&'static str, String>> {
        self.resolve_for_text(|_| Ok(HashMap::new()))
    }

    pub(crate) fn character_extents(
        &self,
        offset: i32,
        coord_type: CoordType,
    ) -> Result<AtspiRect> {
        self.resolve_for_text_with_context(|node, _, context| {
            let range = text_range_from_offset(&node, offset, Granularity::Char)?;
            if let Some(bounds) = range.bounding_boxes().first() {
                let window_bounds = context.root_window_bounds.read();
                let new_origin = window_bounds.accesskit_point_to_atspi_point(
                    bounds.origin(),
                    Some(node),
                    coord_type,
                );
                Ok(bounds.with_origin(new_origin).into())
            } else {
                Ok(AtspiRect::INVALID)
            }
        })
    }

    pub(crate) fn offset_at_point(&self, x: i32, y: i32, coord_type: CoordType) -> Result<i32> {
        self.resolve_for_text_with_context(|node, _, context| {
            let window_bounds = context.root_window_bounds.read();
            let point = window_bounds.atspi_point_to_accesskit_point(
                Point::new(x.into(), y.into()),
                Some(node),
                coord_type,
            );
            let point = node.transform().inverse() * point;
            node.text_position_at_point(point)
                .to_global_usv_index()
                .try_into()
                .map_err(|_| Error::TooManyCharacters)
        })
    }

    pub(crate) fn n_selections(&self) -> Result<i32> {
        self.resolve_for_text(|node| {
            match node.text_selection().filter(|range| !range.is_degenerate()) {
                Some(_) => Ok(1),
                None => Ok(0),
            }
        })
    }

    pub(crate) fn selection(&self, selection_num: i32) -> Result<(i32, i32)> {
        if selection_num != 0 {
            return Ok((-1, -1));
        }

        self.resolve_for_text(|node| {
            node.text_selection()
                .filter(|range| !range.is_degenerate())
                .map_or(Ok((-1, -1)), |range| {
                    let start = range
                        .start()
                        .to_global_usv_index()
                        .try_into()
                        .map_err(|_| Error::TooManyCharacters)?;
                    let end = range
                        .end()
                        .to_global_usv_index()
                        .try_into()
                        .map_err(|_| Error::TooManyCharacters)?;

                    Ok((start, end))
                })
        })
    }

    pub(crate) fn add_selection(&self, start_offset: i32, end_offset: i32) -> Result<bool> {
        // We only support one selection.
        self.set_selection(0, start_offset, end_offset)
    }

    pub(crate) fn remove_selection(&self, selection_num: i32) -> Result<bool> {
        if selection_num != 0 {
            return Ok(false);
        }

        self.resolve_for_text_with_context(|node, tree, context| {
            // Simply collapse the selection to the position of the caret if a caret is
            // visible, otherwise set the selection to 0.
            let selection_end = node
                .text_selection_focus()
                .unwrap_or_else(|| node.document_range().start());
            Self::request(context, tree.state(), node.id(), |target_node| {
                ActionRequest {
                    context: None,
                    action: Action::SetTextSelection,
                    target_tree: TreeId::ROOT,
                    target_node,
                    data: Some(ActionData::SetTextSelection(
                        selection_end.to_degenerate_range().to_text_selection(),
                    )),
                }
            })?;
            Ok(true)
        })
    }

    pub(crate) fn set_selection(
        &self,
        selection_num: i32,
        start_offset: i32,
        end_offset: i32,
    ) -> Result<bool> {
        if selection_num != 0 {
            return Ok(false);
        }

        self.resolve_for_text_with_context(|node, tree, context| {
            let range = text_range_from_offsets(&node, start_offset, end_offset)
                .ok_or(Error::IndexOutOfRange)?;
            Self::request(context, tree.state(), node.id(), |target_node| {
                ActionRequest {
                    context: None,
                    action: Action::SetTextSelection,
                    target_tree: TreeId::ROOT,
                    target_node,
                    data: Some(ActionData::SetTextSelection(range.to_text_selection())),
                }
            })?;
            Ok(true)
        })
    }

    pub(crate) fn range_extents(
        &self,
        start_offset: i32,
        end_offset: i32,
        coord_type: CoordType,
    ) -> Result<AtspiRect> {
        self.resolve_for_text_with_context(|node, _, context| {
            if let Some(rect) = text_range_bounds_from_offsets(&node, start_offset, end_offset) {
                let window_bounds = context.root_window_bounds.read();
                let new_origin = window_bounds.accesskit_point_to_atspi_point(
                    rect.origin(),
                    Some(node),
                    coord_type,
                );
                Ok(rect.with_origin(new_origin).into())
            } else {
                Ok(AtspiRect::INVALID)
            }
        })
    }

    /// The attributes at `offset` and the run of text that shares them:
    /// aterm's model has no text attributes, so the run is the whole
    /// document (accesskit_atspi_common's format run is bounded only where
    /// two runs' attributes differ).
    pub(crate) fn text_attribute_run(
        &self,
        offset: i32,
        _include_defaults: bool,
    ) -> Result<(HashMap<&'static str, String>, i32, i32)> {
        self.resolve_for_text(|node| {
            text_position_from_offset(&node, offset).ok_or(Error::IndexOutOfRange)?;
            let range = node.document_range();
            Ok((
                HashMap::new(),
                range
                    .start()
                    .to_global_usv_index()
                    .try_into()
                    .map_err(|_| Error::TooManyCharacters)?,
                range
                    .end()
                    .to_global_usv_index()
                    .try_into()
                    .map_err(|_| Error::TooManyCharacters)?,
            ))
        })
    }

    pub(crate) fn scroll_substring_to(
        &self,
        start_offset: i32,
        end_offset: i32,
        scroll_type: ScrollType,
    ) -> Result<bool> {
        self.resolve_for_text_with_context(|node, tree, context| {
            if let Some(range) = text_range_from_offsets(&node, start_offset, end_offset) {
                let position = if matches!(
                    scroll_type,
                    ScrollType::BottomRight | ScrollType::BottomEdge | ScrollType::RightEdge
                ) {
                    range.end()
                } else {
                    range.start()
                };
                Self::request(
                    context,
                    tree.state(),
                    position.inner_node().id(),
                    |target_node| ActionRequest {
                        context: None,
                        action: Action::ScrollIntoView,
                        target_tree: TreeId::ROOT,
                        target_node,
                        data: atspi_scroll_type_to_scroll_hint(scroll_type)
                            .map(ActionData::ScrollHint),
                    },
                )?;
                Ok(true)
            } else {
                Ok(false)
            }
        })
    }

    pub(crate) fn scroll_substring_to_point(
        &self,
        start_offset: i32,
        end_offset: i32,
        coord_type: CoordType,
        x: i32,
        y: i32,
    ) -> Result<bool> {
        self.resolve_for_text_with_context(|node, tree, context| {
            let window_bounds = context.root_window_bounds.read();
            let target_point = window_bounds.atspi_point_to_accesskit_point(
                Point::new(x.into(), y.into()),
                Some(node),
                coord_type,
            );

            if let Some(rect) = text_range_bounds_from_offsets(&node, start_offset, end_offset) {
                let point = Point::new(target_point.x - rect.x0, target_point.y - rect.y0);
                Self::request(context, tree.state(), node.id(), |target_node| {
                    ActionRequest {
                        context: None,
                        action: Action::ScrollToPoint,
                        target_tree: TreeId::ROOT,
                        target_node,
                        data: Some(ActionData::ScrollToPoint(point)),
                    }
                })?;
                return Ok(true);
            }
            Ok(false)
        })
    }

    pub(crate) fn minimum_value(&self) -> Result<f64> {
        self.resolve(|node| Ok(node.min_numeric_value().unwrap_or(f64::MIN)))
    }

    pub(crate) fn maximum_value(&self) -> Result<f64> {
        self.resolve(|node| Ok(node.max_numeric_value().unwrap_or(f64::MAX)))
    }

    pub(crate) fn minimum_increment(&self) -> Result<f64> {
        self.resolve(|node| Ok(node.numeric_value_step().unwrap_or(0.0)))
    }

    pub(crate) fn current_value(&self) -> Result<f64> {
        self.resolve(|node| {
            let wrapper = NodeWrapper(&node);
            Ok(wrapper.current_value().unwrap_or(0.0))
        })
    }

    pub(crate) fn set_current_value(&self, value: f64) -> Result<()> {
        self.do_action_internal(|target_node| ActionRequest {
            context: None,
            action: Action::SetValue,
            target_tree: TreeId::ROOT,
            target_node,
            data: Some(ActionData::NumericValue(value)),
        })
    }
}

impl PartialEq for PlatformNode {
    fn eq(&self, other: &Self) -> bool {
        self.adapter_id == other.adapter_id && self.identity == other.identity
    }
}

impl Eq for PlatformNode {}

impl Hash for PlatformNode {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.adapter_id.hash(state);
        self.identity.hash(state);
    }
}

/// The application's root, as the publisher serves it.
#[derive(Clone)]
pub(crate) struct PlatformRoot {
    app_context: Weak<SharedLock<AppContext>>,
}

impl PlatformRoot {
    pub(crate) fn new(app_context: &Arc<SharedLock<AppContext>>) -> Self {
        Self {
            app_context: Arc::downgrade(app_context),
        }
    }

    fn resolve_app_context<F, T>(&self, f: F) -> Result<T>
    where
        F: FnOnce(&AppContext) -> Result<T>,
    {
        let app_context = self.app_context.upgrade().ok_or(Error::Defunct)?;
        let app_context = app_context.read();
        f(&app_context)
    }

    pub(crate) fn name(&self) -> Result<String> {
        self.resolve_app_context(|context| Ok(context.name.clone().unwrap_or_default()))
    }

    pub(crate) fn description(&self) -> Result<String> {
        Ok(String::new())
    }

    pub(crate) fn child_count(&self) -> Result<i32> {
        self.resolve_app_context(|context| {
            i32::try_from(context.adapters.len()).map_err(|_| Error::TooManyChildren)
        })
    }

    pub(crate) fn interfaces(&self) -> InterfaceSet {
        InterfaceSet::new(Interface::Accessible | Interface::Application)
    }

    pub(crate) fn role(&self) -> AtspiRole {
        AtspiRole::Application
    }

    pub(crate) fn index_in_parent(&self) -> i32 {
        -1
    }

    pub(crate) fn state(&self) -> StateSet {
        StateSet::empty()
    }

    pub(crate) fn child_id_at_index(&self, index: usize) -> Result<Option<(usize, NodeIdentity)>> {
        self.resolve_app_context(|context| {
            let child = context.adapters.get(index).map(|(adapter_id, context)| {
                (
                    *adapter_id,
                    NodeIdentity::of(&context.tree.read().state().root()),
                )
            });
            Ok(child)
        })
    }

    pub(crate) fn map_child_ids<T, I>(&self, f: impl Fn((usize, NodeIdentity)) -> I) -> Result<T>
    where
        T: FromIterator<I>,
    {
        self.resolve_app_context(|context| {
            let children = context
                .adapters
                .iter()
                .map(|(adapter_id, context)| {
                    (
                        *adapter_id,
                        NodeIdentity::of(&context.tree.read().state().root()),
                    )
                })
                .map(f)
                .collect();
            Ok(children)
        })
    }

    pub(crate) fn toolkit_name(&self) -> Result<String> {
        self.resolve_app_context(|context| Ok(context.toolkit_name.clone().unwrap_or_default()))
    }

    pub(crate) fn toolkit_version(&self) -> Result<String> {
        self.resolve_app_context(|context| Ok(context.toolkit_version.clone().unwrap_or_default()))
    }

    pub(crate) fn id(&self) -> Result<i32> {
        self.resolve_app_context(|context| Ok(context.id.unwrap_or(-1)))
    }

    pub(crate) fn set_id(&mut self, id: i32) -> Result<()> {
        let app_context = self.app_context.upgrade().ok_or(Error::Defunct)?;
        app_context.write().id = Some(id);
        Ok(())
    }

    pub(crate) fn map_descendant_cache_nodes<T, I>(&self, f: impl Fn(CacheNode) -> I) -> Result<T>
    where
        T: FromIterator<I>,
    {
        fn collect_descendants<I>(
            node: Node<'_>,
            index_in_parent: i32,
            adapter_id: usize,
            window_focused: bool,
            f: &impl Fn(CacheNode) -> I,
            results: &mut Vec<Result<I>>,
        ) {
            let children: Vec<_> = node.filtered_children(&filter).collect();
            let child_count = match i32::try_from(children.len()) {
                Ok(count) => count,
                Err(_) => {
                    results.push(Err(Error::TooManyChildren));
                    return;
                }
            };
            let wrapper = NodeWrapper(&node);
            let cache_node =
                wrapper.cache_node(adapter_id, index_in_parent, child_count, window_focused);
            results.push(Ok(f(cache_node)));
            for (child_index, child) in children.into_iter().enumerate() {
                let child_index = match i32::try_from(child_index) {
                    Ok(index) => index,
                    Err(_) => {
                        results.push(Err(Error::IndexOutOfRange));
                        return;
                    }
                };
                collect_descendants(child, child_index, adapter_id, window_focused, f, results);
            }
        }

        self.resolve_app_context(|context| {
            let mut results = Vec::new();

            for (adapter_slot, (adapter_id, adapter_context)) in context.adapters.iter().enumerate()
            {
                let tree = adapter_context.tree.read();
                let state = tree.state();
                let root = state.root();
                let index_in_parent =
                    i32::try_from(adapter_slot).map_err(|_| Error::IndexOutOfRange)?;
                let window_focused = state.focus_id().is_some();

                collect_descendants(
                    root,
                    index_in_parent,
                    *adapter_id,
                    window_focused,
                    &f,
                    &mut results,
                );
            }

            results.into_iter().collect()
        })
    }
}

impl PartialEq for PlatformRoot {
    fn eq(&self, other: &Self) -> bool {
        self.app_context.ptr_eq(&other.app_context)
    }
}

impl Hash for PlatformRoot {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.app_context.as_ptr().hash(state);
    }
}

/// A node of a window, or the application root.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NodeIdOrRoot {
    Node(NodeIdentity),
    Root,
}

/// A node's `Cache.GetItems` entry.
pub(crate) struct CacheNode {
    pub(crate) adapter_id: usize,
    pub(crate) id: NodeIdentity,
    pub(crate) parent: NodeIdOrRoot,
    pub(crate) index_in_parent: i32,
    pub(crate) child_count: i32,
    pub(crate) interfaces: InterfaceSet,
    pub(crate) name: String,
    pub(crate) description: String,
    pub(crate) role: AtspiRole,
    pub(crate) states: StateSet,
}
