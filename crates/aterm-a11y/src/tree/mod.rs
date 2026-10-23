// Copyright 2021 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_consumer 0.37.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/tree.rs and src/lib.rs, and cut by the aterm project in
// 2026 to what aterm's publisher reads: one tree per window (no grafted
// subtrees), and an update's changes reported in node order, not hash order.

//! A window's accessibility tree as a publisher reads it: the nodes of the
//! model's updates joined into a tree with parents and indices, and what
//! each update changed (docs/AUDIT-third-party-2026-10-15.md, unit a11y,
//! A11Y.5).
//!
//! [`Tree`] holds the published state and applies a [`TreeUpdate`] to a
//! copy, then tells a [`ChangeHandler`] which nodes were added, updated and
//! removed and where the focus moved, old and new node side by side, before
//! it adopts the copy. [`Node`] reads a node in its tree, and [`text`]
//! (crate::text) walks a text document's runs.
//!
//! An update that breaks the model's rules (a node that is nobody's child, a
//! child twice, a focus or root that is not in the tree) panics, as the
//! upstream crate's does: the window that builds the tree is aterm itself.

mod filters;
mod iterators;
mod node;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::fmt;

use crate::model::{Node as NodeData, NodeId as LocalNodeId, Tree as TreeData, TreeUpdate};

pub(crate) use filters::{FilterResult, common_filter};
pub(crate) use node::{Node, NodeId};
use node::{NodeState, ParentAndIndex};

/// A tree's nodes, root, focus and whether its window has the focus.
#[derive(Clone, Debug)]
pub(crate) struct State {
    pub(crate) nodes: HashMap<NodeId, NodeState>,
    pub(crate) data: TreeData,
    pub(crate) focus: NodeId,
    is_host_focused: bool,
    next_incarnation: u64,
}

/// The nodes an update touched, in node order so the events they cause come
/// out the same way every time.
#[derive(Default)]
struct Changes {
    added: BTreeSet<NodeId>,
    updated: BTreeSet<NodeId>,
    removed: BTreeSet<NodeId>,
}

/// Never reuse an exported object incarnation after removal or owner replacement.
const fn fresh_incarnation(next: &mut u64) -> u64 {
    *next = next
        .checked_add(1)
        .expect("accessibility object incarnation exhausted");
    *next
}

impl State {
    fn validate_global(&self) {
        if !self.nodes.contains_key(&self.root_id()) {
            panic!("Root ID {:?} is not in the node list", self.data.root);
        }
        if !self.nodes.contains_key(&self.focus) {
            panic!("Focused ID {:?} is not in the node list", self.focus.0);
        }
    }

    fn update(
        &mut self,
        update: TreeUpdate,
        is_host_focused: bool,
        mut changes: Option<&mut Changes>,
    ) {
        let mut unreachable = HashSet::new();
        let mut seen_child_ids = HashSet::new();

        let new_tree_root = if let Some(tree) = update.tree {
            if tree.root != self.data.root {
                unreachable.insert(self.root_id());
            }
            self.data = tree;
            Some(self.root_id())
        } else {
            None
        };
        let root = self.data.root;

        let mut pending_nodes: HashMap<NodeId, NodeData> = HashMap::new();
        let mut pending_children = HashMap::new();

        fn add_node(
            nodes: &mut HashMap<NodeId, NodeState>,
            changes: &mut Option<&mut Changes>,
            next_incarnation: &mut u64,
            parent_and_index: Option<ParentAndIndex>,
            id: NodeId,
            data: NodeData,
        ) {
            nodes.insert(
                id,
                NodeState {
                    parent_and_index,
                    incarnation: if changes.is_some() {
                        fresh_incarnation(next_incarnation)
                    } else {
                        0
                    },
                    data,
                },
            );
            if let Some(changes) = changes {
                changes.added.insert(id);
            }
        }

        for (local_node_id, node_data) in update.nodes {
            let node_id = NodeId(local_node_id);
            unreachable.remove(&node_id);

            for (child_index, child_id) in node_data.children().iter().enumerate() {
                let child_id = NodeId(*child_id);
                if !seen_child_ids.insert(child_id) {
                    panic!("TreeUpdate includes duplicate child {:?}", child_id.0);
                }
                unreachable.remove(&child_id);
                let parent_and_index = ParentAndIndex(node_id, child_index);
                if let Some(child_state) = self.nodes.get_mut(&child_id) {
                    if child_state.parent_and_index != Some(parent_and_index) {
                        child_state.parent_and_index = Some(parent_and_index);
                        if let Some(changes) = &mut changes {
                            changes.updated.insert(child_id);
                        }
                    }
                } else if let Some(child_data) = pending_nodes.remove(&child_id) {
                    add_node(
                        &mut self.nodes,
                        &mut changes,
                        &mut self.next_incarnation,
                        Some(parent_and_index),
                        child_id,
                        child_data,
                    );
                } else {
                    pending_children.insert(child_id, parent_and_index);
                }
            }

            if let Some(node_state) = self.nodes.get_mut(&node_id) {
                if local_node_id == root {
                    node_state.parent_and_index = None;
                }
                for child_id in node_state.data.children().iter() {
                    let child_id = NodeId(*child_id);
                    if !seen_child_ids.contains(&child_id) {
                        unreachable.insert(child_id);
                    }
                }
                if node_state.data != node_data {
                    if node_state.data.action_context() != node_data.action_context() {
                        node_state.incarnation = fresh_incarnation(&mut self.next_incarnation);
                    }
                    node_state.data.clone_from(&node_data);
                    if let Some(changes) = &mut changes {
                        changes.updated.insert(node_id);
                    }
                }
            } else if let Some(parent_and_index) = pending_children.remove(&node_id) {
                add_node(
                    &mut self.nodes,
                    &mut changes,
                    &mut self.next_incarnation,
                    Some(parent_and_index),
                    node_id,
                    node_data,
                );
            } else if local_node_id == root {
                add_node(
                    &mut self.nodes,
                    &mut changes,
                    &mut self.next_incarnation,
                    None,
                    node_id,
                    node_data,
                );
            } else {
                pending_nodes.insert(node_id, node_data);
            }
        }

        if !pending_nodes.is_empty() {
            panic!(
                "TreeUpdate includes {} nodes which are neither in the current tree nor a child of another node from the update: {}",
                pending_nodes.len(),
                ShortNodeList(&pending_nodes)
            );
        }
        if !pending_children.is_empty() {
            panic!(
                "TreeUpdate's nodes include {} children ids which are neither in the current tree nor the ID of another node from the update: {}",
                pending_children.len(),
                ShortNodeList(&pending_children)
            );
        }

        self.focus = NodeId(update.focus);
        self.is_host_focused = is_host_focused;

        // What no node holds any more goes, with everything under it that
        // the update did not give a new parent.
        for id in unreachable {
            let mut stack = vec![id];
            while let Some(id) = stack.pop() {
                if let Some(changes) = &mut changes {
                    changes.removed.insert(id);
                }
                let Some(node) = self.nodes.remove(&id) else {
                    continue;
                };
                for child_id in node.data.children().iter() {
                    let child_id = NodeId(*child_id);
                    if !seen_child_ids.contains(&child_id) && new_tree_root != Some(child_id) {
                        stack.push(child_id);
                    }
                }
            }
        }

        self.validate_global();
    }

    fn update_host_focus_state(&mut self, is_host_focused: bool, changes: Option<&mut Changes>) {
        let update = TreeUpdate {
            nodes: vec![],
            tree: None,
            tree_id: crate::model::TreeId::ROOT,
            focus: self.focus.0,
        };
        self.update(update, is_host_focused, changes);
    }

    #[inline]
    pub(crate) fn node_by_id(&self, id: NodeId) -> Option<Node<'_>> {
        self.nodes.get(&id).map(|node_state| Node {
            tree_state: self,
            id,
            state: node_state,
        })
    }

    #[inline]
    pub(crate) const fn root_id(&self) -> NodeId {
        NodeId(self.data.root)
    }

    #[inline]
    pub(crate) fn root(&self) -> Node<'_> {
        self.node_by_id(self.root_id())
            .expect("the root is always in the tree")
    }

    #[inline]
    pub(crate) const fn is_host_focused(&self) -> bool {
        self.is_host_focused
    }

    /// The focused node, while the window has the focus.
    #[inline]
    pub(crate) const fn focus_id(&self) -> Option<NodeId> {
        if self.is_host_focused {
            Some(self.focus)
        } else {
            None
        }
    }

    #[inline]
    pub(crate) fn focus(&self) -> Option<Node<'_>> {
        self.focus_id().and_then(|id| self.node_by_id(id))
    }

    /// The dialog holding the focus, if one does.
    pub(crate) fn active_dialog(&self) -> Option<Node<'_>> {
        let mut node = self.focus();
        while let Some(candidate) = node {
            if candidate.is_dialog() {
                return Some(candidate);
            }
            node = candidate.parent();
        }
        None
    }

    #[inline]
    pub(crate) fn toolkit_name(&self) -> Option<&str> {
        self.data.toolkit_name.as_deref()
    }

    #[inline]
    pub(crate) fn toolkit_version(&self) -> Option<&str> {
        self.data.toolkit_version.as_deref()
    }

    /// The model's id of `node_id`, if it is in the tree: what an action
    /// request names.
    #[inline]
    pub(crate) fn locate_node(&self, node_id: NodeId) -> Option<LocalNodeId> {
        self.nodes.contains_key(&node_id).then_some(node_id.0)
    }
}

/// What a publisher does with an update's changes.
pub(crate) trait ChangeHandler {
    fn node_added(&mut self, node: &Node);
    fn node_updated(&mut self, old_node: &Node, new_node: &Node);
    fn focus_moved(&mut self, old_node: Option<&Node>, new_node: Option<&Node>);
    fn node_removed(&mut self, node: &Node);
}

/// A window's tree: the published state, and the next one an update builds.
#[derive(Debug)]
pub(crate) struct Tree {
    state: State,
    next_state: State,
}

impl Tree {
    pub(crate) fn new(mut initial_state: TreeUpdate, is_host_focused: bool) -> Self {
        let Some(tree) = initial_state.tree.take() else {
            panic!(
                "Tried to initialize the accessibility tree without a root tree. TreeUpdate::tree must be Some."
            );
        };
        let mut state = State {
            nodes: HashMap::new(),
            focus: NodeId(initial_state.focus),
            data: tree,
            is_host_focused,
            next_incarnation: 0,
        };
        state.update(initial_state, is_host_focused, None);
        Self {
            next_state: state.clone(),
            state,
        }
    }

    pub(crate) fn update_and_process_changes(
        &mut self,
        update: TreeUpdate,
        handler: &mut impl ChangeHandler,
    ) {
        let mut changes = Changes::default();
        self.next_state
            .update(update, self.state.is_host_focused, Some(&mut changes));
        self.process_changes(changes, handler);
    }

    pub(crate) fn update_host_focus_state_and_process_changes(
        &mut self,
        is_host_focused: bool,
        handler: &mut impl ChangeHandler,
    ) {
        let mut changes = Changes::default();
        self.next_state
            .update_host_focus_state(is_host_focused, Some(&mut changes));
        self.process_changes(changes, handler);
    }

    fn process_changes(&mut self, changes: Changes, handler: &mut impl ChangeHandler) {
        for id in &changes.added {
            if let Some(node) = self.next_state.node_by_id(*id) {
                handler.node_added(&node);
            }
        }
        for id in &changes.updated {
            if let (Some(old_node), Some(new_node)) =
                (self.state.node_by_id(*id), self.next_state.node_by_id(*id))
            {
                handler.node_updated(&old_node, &new_node);
            }
        }
        let old_focus = self.state.focus();
        let new_focus = self.next_state.focus();
        if old_focus.as_ref().map(|n| (n.id(), n.incarnation()))
            != new_focus.as_ref().map(|n| (n.id(), n.incarnation()))
        {
            if let Some(old_node) = &old_focus {
                let id = old_node.id();
                if !changes.updated.contains(&id)
                    && !changes.removed.contains(&id)
                    && let Some(old_node_new_version) = self.next_state.node_by_id(id)
                {
                    handler.node_updated(old_node, &old_node_new_version);
                }
            }
            if let Some(new_node) = &new_focus {
                let id = new_node.id();
                if !changes.added.contains(&id)
                    && !changes.updated.contains(&id)
                    && let Some(new_node_old_version) = self.state.node_by_id(id)
                {
                    handler.node_updated(&new_node_old_version, new_node);
                }
            }
            handler.focus_moved(old_focus.as_ref(), new_focus.as_ref());
        }
        for id in &changes.removed {
            if let Some(node) = self.state.node_by_id(*id) {
                handler.node_removed(&node);
            }
        }
        for id in changes.added {
            if let Some(node) = self.next_state.nodes.get(&id) {
                self.state.nodes.insert(id, node.clone());
            }
        }
        for id in changes.updated {
            if let (Some(old), Some(new)) = (
                self.state.nodes.get_mut(&id),
                self.next_state.nodes.get(&id),
            ) {
                old.clone_from(new);
            }
        }
        for id in changes.removed {
            self.state.nodes.remove(&id);
        }
        if self.state.data != self.next_state.data {
            self.state.data.clone_from(&self.next_state.data);
        }
        self.state.next_incarnation = self.next_state.next_incarnation;
        self.state.focus = self.next_state.focus;
        self.state.is_host_focused = self.next_state.is_host_focused;
    }

    #[inline]
    pub(crate) const fn state(&self) -> &State {
        &self.state
    }
}

struct ShortNodeList<'a, T>(&'a HashMap<NodeId, T>);

impl<T> fmt::Display for ShortNodeList<'_, T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut ids: Vec<&NodeId> = self.0.keys().collect();
        ids.sort();
        write!(f, "[")?;
        for (i, id) in ids.iter().take(10).enumerate() {
            if i != 0 {
                write!(f, ", ")?;
            }
            write!(f, "{:?}", id.0)?;
        }
        if ids.len() > 10 {
            write!(f, " ...")?;
        }
        write!(f, "]")
    }
}
