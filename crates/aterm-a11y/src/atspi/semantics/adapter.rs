// Copyright 2022 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/adapter.rs, and changed by the aterm project in 2026: the
// nodes whose selection changed are visited in node order, and a poisoned
// lock is recovered rather than unwound.
//
// Derived from Chromium's accessibility abstraction.
// Copyright 2017 The Chromium Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE-CHROMIUM file.

//! A window's tree as AT-SPI sees it: its registrations, and the events an
//! update, a focus change or the window's own focus sends.

use std::collections::{BTreeSet, HashSet};
use std::fmt::{Debug, Formatter};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::model::{Role, TreeUpdate};
use crate::tree::{ChangeHandler, FilterResult, Node, NodeId, State as TreeState, Tree};

use super::context::{ActionHandlerNoMut, AppContext, Context, SharedLock};
use super::node::{NodeIdOrRoot, NodeWrapper, PlatformNode};
use super::{
    AdapterCallback, CacheEvent, Event, InterfaceSet, ObjectEvent, Politeness, State, WindowBounds,
    WindowEvent,
};
use crate::tree::common_filter as filter;

struct AdapterChangeHandler<'a> {
    adapter: &'a Adapter,
    added_nodes: HashSet<NodeId>,
    removed_nodes: HashSet<NodeId>,
    checked_text_change: HashSet<NodeId>,
    selection_changed: BTreeSet<NodeId>,
}

impl<'a> AdapterChangeHandler<'a> {
    fn new(adapter: &'a Adapter) -> Self {
        Self {
            adapter,
            added_nodes: HashSet::new(),
            removed_nodes: HashSet::new(),
            checked_text_change: HashSet::new(),
            selection_changed: BTreeSet::new(),
        }
    }

    fn add_node(&mut self, node: &Node) {
        let id = node.id();
        if !self.added_nodes.insert(id) {
            return;
        }

        let role = node.role();
        let is_root = node.is_root();
        let wrapper = NodeWrapper(node);
        let interfaces = wrapper.interfaces();
        self.adapter.register_interfaces(node.id(), interfaces);
        self.adapter.emit_cache_added(node.id());
        if is_root
            && role == Role::Window
            && let Ok(adapter_index) = self
                .adapter
                .context
                .app_context
                .read()
                .adapter_index(self.adapter.id)
        {
            self.adapter.window_created(adapter_index, node.id());
        }

        let live = wrapper.live();
        if live != Politeness::None
            && let Some(name) = wrapper.name()
        {
            self.adapter
                .emit_object_event(node.id(), ObjectEvent::Announcement(name, live));
        }
        if let Some(true) = node.is_selected() {
            self.enqueue_selection_changed_if_needed(node);
        }
    }

    fn add_subtree(&mut self, node: &Node) {
        self.add_node(node);
        for child in node.filtered_children(&filter) {
            self.add_subtree(&child);
        }
    }

    fn remove_node(&mut self, node: &Node) {
        let id = node.id();
        if !self.removed_nodes.insert(id) {
            return;
        }

        let role = node.role();
        let is_root = node.is_root();
        let wrapper = NodeWrapper(node);
        if is_root && role == Role::Window {
            self.adapter.window_destroyed(node.id());
        }
        self.adapter
            .emit_object_event(node.id(), ObjectEvent::StateChanged(State::Defunct, true));
        self.adapter.emit_cache_removed(node.id());
        self.adapter
            .unregister_interfaces(node.id(), wrapper.interfaces());
        if let Some(true) = node.is_selected() {
            self.enqueue_selection_changed_if_needed(node);
        }
    }

    fn remove_subtree(&mut self, node: &Node) {
        for child in node.filtered_children(&filter) {
            self.remove_subtree(&child);
        }
        self.remove_node(node);
    }

    fn emit_text_change_if_needed_parent(&mut self, old_node: &Node, new_node: &Node) {
        if !new_node.supports_text_ranges() || !old_node.supports_text_ranges() {
            return;
        }
        let id = new_node.id();
        if !self.checked_text_change.insert(id) {
            return;
        }
        let old_text = old_node.document_range().text();
        let new_text = new_node.document_range().text();

        let mut old_chars = old_text.chars();
        let mut new_chars = new_text.chars();
        let mut prefix_usv_count = 0;
        let mut prefix_byte_count = 0;
        loop {
            match (old_chars.next(), new_chars.next()) {
                (Some(old_char), Some(new_char)) if old_char == new_char => {
                    prefix_usv_count += 1;
                    prefix_byte_count += new_char.len_utf8();
                }
                (None, None) => return,
                _ => break,
            }
        }

        let suffix_byte_count = old_text[prefix_byte_count..]
            .chars()
            .rev()
            .zip(new_text[prefix_byte_count..].chars().rev())
            .take_while(|(old_char, new_char)| old_char == new_char)
            .fold(0, |count, (c, _)| count + c.len_utf8());

        let old_content = &old_text[prefix_byte_count..old_text.len() - suffix_byte_count];
        if let Ok(length) = old_content.chars().count().try_into()
            && length > 0
        {
            self.adapter.emit_object_event(
                id,
                ObjectEvent::TextRemoved {
                    start_index: prefix_usv_count,
                    length,
                    content: old_content.to_string(),
                },
            );
        }

        let new_content = &new_text[prefix_byte_count..new_text.len() - suffix_byte_count];
        if let Ok(length) = new_content.chars().count().try_into()
            && length > 0
        {
            self.adapter.emit_object_event(
                id,
                ObjectEvent::TextInserted {
                    start_index: prefix_usv_count,
                    length,
                    content: new_content.to_string(),
                },
            );
        }
    }

    fn emit_text_change_if_needed(&mut self, old_node: &Node, new_node: &Node) {
        if new_node.role() == Role::TextRun {
            if let (Some(old_parent), Some(new_parent)) = (
                old_node.filtered_parent(&filter),
                new_node.filtered_parent(&filter),
            ) {
                self.emit_text_change_if_needed_parent(&old_parent, &new_parent);
            }
        } else {
            self.emit_text_change_if_needed_parent(old_node, new_node);
        }
    }

    fn emit_text_selection_change(&self, old_node: Option<&Node>, new_node: &Node) {
        if !new_node.supports_text_ranges() {
            return;
        }
        let Some(old_node) = old_node else {
            if let Some(selection) = new_node.text_selection()
                && !selection.is_degenerate()
            {
                self.adapter
                    .emit_object_event(new_node.id(), ObjectEvent::TextSelectionChanged);
            }
            if let Some(selection_focus) = new_node.text_selection_focus()
                && let Ok(offset) = selection_focus.to_global_usv_index().try_into()
            {
                self.adapter
                    .emit_object_event(new_node.id(), ObjectEvent::CaretMoved(offset));
            }
            return;
        };
        if !old_node.is_focused() || new_node.raw_text_selection() == old_node.raw_text_selection()
        {
            return;
        }

        if let Some(selection) = new_node.text_selection()
            && (!selection.is_degenerate()
                || old_node
                    .text_selection()
                    .map(|selection| !selection.is_degenerate())
                    .unwrap_or(false))
        {
            self.adapter
                .emit_object_event(new_node.id(), ObjectEvent::TextSelectionChanged);
        }

        let old_caret_position = old_node
            .raw_text_selection()
            .map(|selection| selection.focus);
        let new_caret_position = new_node
            .raw_text_selection()
            .map(|selection| selection.focus);
        if old_caret_position != new_caret_position
            && let Some(selection_focus) = new_node.text_selection_focus()
            && let Ok(offset) = selection_focus.to_global_usv_index().try_into()
        {
            self.adapter
                .emit_object_event(new_node.id(), ObjectEvent::CaretMoved(offset));
        }
    }

    fn enqueue_selection_changed_if_needed_parent(&mut self, node: Node) {
        if !node.is_container_with_selectable_children() {
            return;
        }
        self.selection_changed.insert(node.id());
    }

    fn enqueue_selection_changed_if_needed(&mut self, node: &Node) {
        if !node.is_item_like() {
            return;
        }
        if let Some(node) = node.selection_container(&filter) {
            self.enqueue_selection_changed_if_needed_parent(node);
        }
    }

    fn emit_selection_changed(&mut self) {
        for id in self.selection_changed.iter() {
            if self.removed_nodes.contains(id) {
                continue;
            }
            self.adapter
                .emit_object_event(*id, ObjectEvent::SelectionChanged);
        }
    }
}

impl ChangeHandler for AdapterChangeHandler<'_> {
    fn node_added(&mut self, node: &Node) {
        if filter(node) == FilterResult::Include {
            self.add_node(node);
        }
    }

    fn node_updated(&mut self, old_node: &Node, new_node: &Node) {
        self.emit_text_change_if_needed(old_node, new_node);
        let filter_old = filter(old_node);
        let filter_new = filter(new_node);
        if filter_new != filter_old {
            if filter_new == FilterResult::Include {
                if filter_old == FilterResult::ExcludeSubtree {
                    self.add_subtree(new_node);
                } else {
                    self.add_node(new_node);
                }
            } else if filter_old == FilterResult::Include {
                if filter_new == FilterResult::ExcludeSubtree {
                    self.remove_subtree(old_node);
                } else {
                    self.remove_node(old_node);
                }
            }
        } else if filter_new == FilterResult::Include {
            let old_wrapper = NodeWrapper(old_node);
            let new_wrapper = NodeWrapper(new_node);
            let old_interfaces = old_wrapper.interfaces();
            let new_interfaces = new_wrapper.interfaces();
            let kept_interfaces = old_interfaces & new_interfaces;
            self.adapter
                .unregister_interfaces(new_wrapper.id(), old_interfaces ^ kept_interfaces);
            self.adapter
                .register_interfaces(new_node.id(), new_interfaces ^ kept_interfaces);
            let bounds = *self.adapter.context.root_window_bounds.read();
            new_wrapper.notify_changes(&bounds, self.adapter, &old_wrapper);
            self.emit_text_selection_change(Some(old_node), new_node);
            if new_node.is_selected() != old_node.is_selected() {
                self.enqueue_selection_changed_if_needed(new_node);
            }
        }
    }

    fn focus_moved(&mut self, old_node: Option<&Node>, new_node: Option<&Node>) {
        if let (None, Some(new_node)) = (old_node, new_node) {
            if let Some(root_window) = root_window(new_node.tree_state) {
                self.adapter.window_activated(&NodeWrapper(&root_window));
            }
        } else if let (Some(old_node), None) = (old_node, new_node)
            && let Some(root_window) = root_window(old_node.tree_state)
        {
            self.adapter.window_deactivated(&NodeWrapper(&root_window));
        }
        if let Some(node) = new_node {
            self.adapter
                .emit_object_event(node.id(), ObjectEvent::StateChanged(State::Focused, true));
            self.emit_text_selection_change(None, node);
        }
        if let Some(node) = old_node {
            self.adapter
                .emit_object_event(node.id(), ObjectEvent::StateChanged(State::Focused, false));
        }
    }

    fn node_removed(&mut self, node: &Node) {
        if filter(node) == FilterResult::Include {
            self.remove_node(node);
        }
    }
}

static NEXT_ADAPTER_ID: AtomicUsize = AtomicUsize::new(0);

/// A fresh adapter id. Only one adapter at a time may have a given id.
pub(crate) fn next_adapter_id() -> usize {
    NEXT_ADAPTER_ID.fetch_add(1, Ordering::Relaxed)
}

/// One window's tree, registered with the application.
pub(crate) struct Adapter {
    id: usize,
    callback: Box<dyn AdapterCallback + Send + Sync>,
    context: Arc<Context>,
}

impl Debug for Adapter {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Adapter")
            .field("id", &self.id)
            .field("callback", &"AdapterCallback")
            .field("context", &self.context)
            .finish()
    }
}

impl Adapter {
    pub(crate) fn with_wrapped_action_handler(
        id: usize,
        app_context: &Arc<SharedLock<AppContext>>,
        callback: impl 'static + AdapterCallback + Send + Sync,
        initial_state: TreeUpdate,
        is_window_focused: bool,
        root_window_bounds: WindowBounds,
        action_handler: Arc<dyn ActionHandlerNoMut + Send + Sync>,
    ) -> Self {
        let tree = Tree::new(initial_state, is_window_focused);
        let focus_id = tree.state().focus().map(|node| node.id());
        let context = Context::new(app_context, tree, action_handler, root_window_bounds);
        context.app_context.write().push_adapter(id, &context);
        let adapter = Self {
            id,
            callback: Box::new(callback),
            context,
        };
        adapter.register_tree();
        if let Some(id) = focus_id {
            adapter.emit_object_event(id, ObjectEvent::StateChanged(State::Focused, true));
        }
        adapter
    }

    fn register_tree(&self) {
        fn add_children(node: Node<'_>, to_add: &mut Vec<(NodeId, InterfaceSet)>) {
            for child in node.filtered_children(&filter) {
                let child_id = child.id();
                let wrapper = NodeWrapper(&child);
                let interfaces = wrapper.interfaces();
                to_add.push((child_id, interfaces));
                add_children(child, to_add);
            }
        }

        let mut objects_to_add = Vec::new();

        let (adapter_index, root_id) = {
            let tree = self.context.tree.read();
            let tree_state = tree.state();
            let mut app_context = self.context.app_context.write();
            app_context.toolkit_name = tree_state.toolkit_name().map(|s| s.to_string());
            app_context.toolkit_version = tree_state.toolkit_version().map(|s| s.to_string());
            let adapter_index = app_context.adapter_index(self.id);
            let root = tree_state.root();
            let root_id = root.id();
            let wrapper = NodeWrapper(&root);
            objects_to_add.push((root_id, wrapper.interfaces()));
            add_children(root, &mut objects_to_add);
            (adapter_index, root_id)
        };

        for (id, interfaces) in objects_to_add {
            self.register_interfaces(id, interfaces);
            if id == root_id
                && let Ok(adapter_index) = adapter_index
            {
                self.window_created(adapter_index, id);
            }
        }
    }

    pub(crate) fn platform_node(&self, id: NodeId) -> PlatformNode {
        PlatformNode::new(&self.context, self.id, id)
    }

    #[cfg(test)]
    pub(crate) fn root_id(&self) -> NodeId {
        self.context.tree.read().state().root_id()
    }

    fn register_interfaces(&self, id: NodeId, new_interfaces: InterfaceSet) {
        self.callback.register_interfaces(self, id, new_interfaces);
    }

    fn unregister_interfaces(&self, id: NodeId, old_interfaces: InterfaceSet) {
        self.callback
            .unregister_interfaces(self, id, old_interfaces);
    }

    pub(crate) fn emit_object_event(&self, target: NodeId, event: ObjectEvent) {
        let target = NodeIdOrRoot::Node(target);
        self.callback
            .emit_event(self, Event::Object { target, event });
    }

    fn emit_root_object_event(&self, event: ObjectEvent) {
        let target = NodeIdOrRoot::Root;
        self.callback
            .emit_event(self, Event::Object { target, event });
    }

    fn emit_cache_added(&self, target: NodeId) {
        self.callback
            .emit_event(self, Event::Cache(CacheEvent::Added(target)));
    }

    fn emit_cache_removed(&self, target: NodeId) {
        self.callback
            .emit_event(self, Event::Cache(CacheEvent::Removed(target)));
    }

    pub(crate) fn set_root_window_bounds(&mut self, new_bounds: WindowBounds) {
        *self.context.root_window_bounds.write() = new_bounds;
    }

    pub(crate) fn update(&mut self, update: TreeUpdate) {
        let mut handler = AdapterChangeHandler::new(self);
        let mut tree = self.context.tree.write();
        tree.update_and_process_changes(update, &mut handler);
        drop(tree);
        handler.emit_selection_changed();
    }

    pub(crate) fn update_window_focus_state(&mut self, is_focused: bool) {
        let mut handler = AdapterChangeHandler::new(self);
        let mut tree = self.context.tree.write();
        tree.update_host_focus_state_and_process_changes(is_focused, &mut handler);
    }

    fn window_created(&self, adapter_index: usize, window: NodeId) {
        self.emit_root_object_event(ObjectEvent::ChildAdded(adapter_index, window));
    }

    fn window_activated(&self, window: &NodeWrapper<'_>) {
        self.callback.emit_event(
            self,
            Event::Window {
                target: window.id(),
                name: window.name().unwrap_or_default(),
                event: WindowEvent::Activated,
            },
        );
        self.emit_object_event(window.id(), ObjectEvent::StateChanged(State::Active, true));
        self.emit_root_object_event(ObjectEvent::ActiveDescendantChanged(window.id()));
    }

    fn window_deactivated(&self, window: &NodeWrapper<'_>) {
        self.callback.emit_event(
            self,
            Event::Window {
                target: window.id(),
                name: window.name().unwrap_or_default(),
                event: WindowEvent::Deactivated,
            },
        );
        self.emit_object_event(window.id(), ObjectEvent::StateChanged(State::Active, false));
    }

    fn window_destroyed(&self, window: NodeId) {
        self.emit_root_object_event(ObjectEvent::ChildRemoved(window));
    }

    pub(crate) fn id(&self) -> usize {
        self.id
    }

    pub(crate) fn is_window_focused(&self) -> bool {
        self.context.tree.read().state().is_host_focused()
    }

    pub(crate) fn root_window_bounds(&self) -> WindowBounds {
        *self.context.root_window_bounds.read()
    }

    /// The window's action handler, kept when the tree is dropped.
    pub(crate) fn wrapped_action_handler(&self) -> Arc<dyn ActionHandlerNoMut + Send + Sync> {
        Arc::clone(&self.context.action_handler)
    }
}

/// The tree's root when it is a window or a dialog: what the window events
/// name.
fn root_window(current_state: &TreeState) -> Option<Node<'_>> {
    let root = current_state.root();
    matches!(root.role(), Role::AlertDialog | Role::Window).then_some(root)
}

impl Drop for Adapter {
    fn drop(&mut self) {
        let root_id = self.context.tree.read().state().root_id();
        self.window_destroyed(root_id);
        // Note: We deliberately do the following here, not in a Drop
        // implementation on context, because AppContext owns a second
        // strong reference to Context, and we need that to be released.
        self.context.app_context.write().remove_adapter(self.id);
    }
}
