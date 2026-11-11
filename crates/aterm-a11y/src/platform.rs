// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Retained accessibility model without AppKit dependencies.

use std::sync::{Arc, Weak};

#[cfg(test)]
use crate::model::NodeId as ModelId;
use crate::model::{Action, ActionData, ActionRequest, Live, TextSelection, TreeId, TreeUpdate};
use crate::tree::{ChangeHandler, Node, NodeId, Tree};

/// Node handle bound to a node's incarnation and publisher identity.
#[derive(Clone)]
pub(crate) struct Handle {
    pub(crate) id: NodeId,
    incarnation: u64,
    publisher: Weak<()>,
}

#[derive(Default)]
pub(crate) struct Changes {
    pub(crate) added: Vec<NodeId>,
    pub(crate) updated: Vec<NodeId>,
    pub(crate) removed: Vec<NodeId>,
    pub(crate) replaced: Vec<NodeId>,
    pub(crate) focus: bool,
    pub(crate) announcements: Vec<(NodeId, Live, String)>,
}

impl ChangeHandler for Changes {
    fn node_added(&mut self, node: &Node) {
        self.added.push(node.id());
        self.live_changed(node);
    }

    fn node_updated(&mut self, old: &Node, node: &Node) {
        self.updated.push(node.id());
        if old.data().action_context() != node.data().action_context() {
            self.replaced.push(node.id());
        }
        if old.data().value() != node.data().value() || old.data().label() != node.data().label() {
            self.live_changed(node);
        }
    }

    fn node_removed(&mut self, node: &Node) {
        self.removed.push(node.id());
    }

    fn focus_moved(&mut self, _old: Option<&Node>, _new: Option<&Node>) {
        self.focus = true;
    }
}

impl Changes {
    fn live_changed(&mut self, node: &Node<'_>) {
        let Some(live) = node.data().live().filter(|live| *live != Live::Off) else {
            return;
        };
        if let Some(value) = node.value().or_else(|| node.label()) {
            self.announcements.push((node.id(), live, value));
        }
    }
}

struct IgnoreChanges;

impl ChangeHandler for IgnoreChanges {
    fn node_added(&mut self, _: &Node) {}
    fn node_updated(&mut self, _: &Node, _: &Node) {}
    fn node_removed(&mut self, _: &Node) {}
    fn focus_moved(&mut self, _: Option<&Node>, _: Option<&Node>) {}
}

struct ForwardChanges<'a, T> {
    changes: &'a mut Changes,
    observer: &'a mut T,
}

impl<T: ChangeHandler> ChangeHandler for ForwardChanges<'_, T> {
    fn node_added(&mut self, node: &Node) {
        self.changes.node_added(node);
        self.observer.node_added(node);
    }

    fn node_updated(&mut self, old: &Node, new: &Node) {
        self.changes.node_updated(old, new);
        self.observer.node_updated(old, new);
    }

    fn node_removed(&mut self, node: &Node) {
        self.changes.node_removed(node);
        self.observer.node_removed(node);
    }

    fn focus_moved(&mut self, old: Option<&Node>, new: Option<&Node>) {
        self.changes.focus_moved(old, new);
        self.observer.focus_moved(old, new);
    }
}

#[derive(Default)]
pub(crate) struct Publication {
    tree: Option<Tree>,
    lifetime: Arc<()>,
    focused: bool,
}

impl Publication {
    pub(crate) fn update(&mut self, update: TreeUpdate) -> Changes {
        self.update_with_changes(update, &mut IgnoreChanges)
    }

    /// Apply `update` and report changes to both the returned [`Changes`] and `observer`.
    pub(crate) fn update_with_changes(
        &mut self,
        update: TreeUpdate,
        observer: &mut impl ChangeHandler,
    ) -> Changes {
        let mut changes = Changes::default();
        let mut handler = ForwardChanges {
            changes: &mut changes,
            observer,
        };
        match self.tree.as_mut() {
            Some(tree) => tree.update_and_process_changes(update, &mut handler),
            None => {
                let tree = Tree::new(update, self.focused);
                let mut ids = tree.state().nodes.keys().copied().collect::<Vec<_>>();
                ids.sort();
                for id in ids {
                    if let Some(node) = tree.state().node_by_id(id) {
                        handler.node_added(&node);
                    }
                }
                handler.changes.focus = true;
                handler
                    .observer
                    .focus_moved(None, tree.state().focus().as_ref());
                self.tree = Some(tree);
            }
        }
        changes
    }

    pub(crate) fn set_focused(&mut self, focused: bool) -> Changes {
        self.focused = focused;
        let mut changes = Changes::default();
        if let Some(tree) = self.tree.as_mut() {
            tree.update_host_focus_state_and_process_changes(focused, &mut changes);
        }
        changes
    }

    pub(crate) fn tree(&self) -> Option<&crate::tree::State> {
        self.tree.as_ref().map(Tree::state)
    }

    pub(crate) fn handle(&self, id: NodeId) -> Option<Handle> {
        Some(Handle {
            id,
            incarnation: self.tree()?.node_by_id(id)?.incarnation(),
            publisher: Arc::downgrade(&self.lifetime),
        })
    }

    pub(crate) fn node(&self, handle: &Handle) -> Option<Node<'_>> {
        if !Weak::ptr_eq(&handle.publisher, &Arc::downgrade(&self.lifetime)) {
            return None;
        }
        let node = self.tree()?.node_by_id(handle.id)?;
        (node.incarnation() == handle.incarnation).then_some(node)
    }

    /// Build an [`ActionRequest`] if `handle` is live, enabled, and supports `action`.
    pub(crate) fn action(
        &self,
        handle: &Handle,
        action: Action,
        data: Option<ActionData>,
    ) -> Option<ActionRequest> {
        let node = self.node(handle)?;
        if node.is_disabled() || !node.data().supports_action(action) {
            return None;
        }
        Some(ActionRequest {
            context: node.data().action_context(),
            action,
            target_tree: TreeId::ROOT,
            target_node: handle.id.0,
            data,
        })
    }
}

pub(crate) fn text(node: Node<'_>) -> Option<String> {
    if node.supports_text_ranges() {
        Some(node.document_range().text())
    } else {
        node.value()
    }
}

/// NSRange counts UTF-16 units; model positions name a text run's characters.
pub(crate) fn selection(node: Node<'_>) -> Option<(usize, usize)> {
    let value = text(node)?;
    let raw = node.data().text_selection()?;
    let (start, end) = if raw.anchor.node == node.id().0 && raw.focus.node == node.id().0 {
        let start = raw.anchor.character_index.min(raw.focus.character_index);
        let end = raw.anchor.character_index.max(raw.focus.character_index);
        if end > value.chars().count() {
            return None;
        }
        (start, end)
    } else {
        let range = node.text_selection()?;
        (
            range.start().to_global_usv_index(),
            range.end().to_global_usv_index(),
        )
    };
    let offset = value.chars().take(start).map(char::len_utf16).sum();
    let len = value
        .chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .map(char::len_utf16)
        .sum();
    Some((offset, len))
}

fn scalar_offset(value: &str, utf16: usize) -> Option<usize> {
    let mut units = 0usize;
    for (offset, ch) in value.chars().enumerate() {
        if units == utf16 {
            return Some(offset);
        }
        units += ch.len_utf16();
        if units > utf16 {
            return None;
        }
    }
    (units == utf16).then(|| value.chars().count())
}

pub(crate) fn text_range(
    node: Node<'_>,
    offset: usize,
    length: usize,
) -> Option<crate::text::Range<'_>> {
    if !node.supports_text_ranges() {
        return None;
    }
    let value = text(node)?;
    let start = scalar_offset(&value, offset)?;
    let end = scalar_offset(&value, offset.checked_add(length)?)?;
    let start_position = node.text_position_from_global_usv_index(start)?;
    let end_position = node.text_position_from_global_usv_index(end)?;
    // A model character can include a combining cluster. Do not silently turn
    // a partial-character selection into a different range at its leading edge.
    if start_position.to_global_usv_index() != start || end_position.to_global_usv_index() != end {
        return None;
    }
    let mut range = start_position.to_degenerate_range();
    range.set_end(end_position);
    Some(range)
}

pub(crate) fn text_selection(
    node: Node<'_>,
    offset: usize,
    length: usize,
) -> Option<TextSelection> {
    Some(text_range(node, offset, length)?.to_text_selection())
}

#[cfg(test)]
pub(crate) fn model_id(id: u64) -> NodeId {
    NodeId(ModelId(id))
}

#[cfg(test)]
#[path = "platform_tests.rs"]
mod tests;
