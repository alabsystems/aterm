// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! UIA semantic deltas from the shared consumer's changed-node callbacks.

use std::collections::HashSet;

use crate::model::{TextSelection, Toggled};
use crate::tree::{ChangeHandler, Node, NodeId};

#[derive(PartialEq)]
pub(crate) struct Observed {
    pub(crate) name: Option<String>,
    pub(crate) description: Option<String>,
    pub(crate) value: Option<String>,
    pub(crate) text: Option<String>,
    pub(crate) selection: Option<TextSelection>,
    pub(crate) enabled: bool,
    pub(crate) toggled: Option<Toggled>,
    pub(crate) selected: Option<bool>,
}

impl Observed {
    fn new(node: Node<'_>) -> Self {
        Self {
            name: node.label(),
            description: node.description(),
            value: node.value(),
            text: node
                .supports_text_ranges()
                .then(|| node.document_range().text()),
            selection: node.data().text_selection().copied(),
            enabled: !node.is_disabled(),
            toggled: node.toggled(),
            selected: node.is_selected(),
        }
    }
}

pub(crate) struct NodeChange {
    pub(crate) id: NodeId,
    pub(crate) old: Observed,
    pub(crate) new: Observed,
}

#[derive(Default)]
pub(crate) struct Events {
    pub(crate) nodes: Vec<NodeChange>,
    seen: HashSet<NodeId>,
}

impl Events {
    fn observe(&mut self, old: Node<'_>, new: Node<'_>) {
        if !self.seen.insert(new.id()) {
            return;
        }
        let id = new.id();
        let old = Observed::new(old);
        let new = Observed::new(new);
        if old != new {
            self.nodes.push(NodeChange { id, old, new });
        }
    }
}

impl ChangeHandler for Events {
    fn node_added(&mut self, _: &Node) {}
    fn node_removed(&mut self, _: &Node) {}
    fn focus_moved(&mut self, _: Option<&Node>, _: Option<&Node>) {}

    fn node_updated(&mut self, old: &Node, new: &Node) {
        // Focus-only callbacks have their own shared event; the semantic
        // properties collected here depend on node data and descendants.
        if old.data() == new.data() && old.parent_id() == new.parent_id() {
            return;
        }
        self.observe(*old, *new);
        for node in [old, new] {
            let mut parent = node.parent();
            while let Some(node) = parent {
                if let (Some(before), Some(after)) = (
                    old.tree_state.node_by_id(node.id()),
                    new.tree_state.node_by_id(node.id()),
                ) {
                    self.observe(before, after);
                }
                parent = node.parent();
            }
        }
    }
}

#[cfg(test)]
#[path = "uia_changes_tests.rs"]
mod tests;
