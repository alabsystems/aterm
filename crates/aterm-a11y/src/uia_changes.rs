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
mod tests {
    use super::*;
    use crate::model::{Node as Data, NodeId as Id, Role, Tree, TreeId, TreeUpdate};
    use crate::platform::Publication;

    fn run(id: u64, value: &str) -> (Id, Data) {
        let mut node = Data::new(Role::TextRun);
        node.set_value(value);
        node.set_character_lengths(vec![1; value.len()]);
        (Id(id), node)
    }

    fn document(extra: bool) -> TreeUpdate {
        let mut root = Data::new(Role::Terminal);
        root.set_children(if extra {
            vec![Id(2), Id(3), Id(4), Id(5)]
        } else {
            vec![Id(2), Id(3), Id(4)]
        });
        let mut nodes = vec![(Id(1), root), run(2, "one"), run(3, "two"), run(4, "fixed")];
        if extra {
            nodes.push(run(5, "new"));
        }
        TreeUpdate {
            nodes,
            tree: Some(Tree::new(Id(1))),
            tree_id: TreeId::ROOT,
            focus: Id(1),
        }
    }

    #[test]
    fn unchanged_publications_observe_no_nodes_and_child_changes_keep_whole_text() {
        let mut publication = Publication::default();
        publication.update(document(false));
        for update in [
            document(false),
            TreeUpdate {
                nodes: vec![],
                tree: None,
                tree_id: TreeId::ROOT,
                focus: Id(1),
            },
        ] {
            let mut events = Events::default();
            let changes = publication.update_with_changes(update, &mut events);
            assert!(changes.updated.is_empty());
            assert!(
                events.seen.is_empty(),
                "unchanged publication takes no semantic snapshots"
            );
            assert!(events.nodes.is_empty());
        }
        publication.set_focused(true);
        let mut focus_only = Events::default();
        let changes = publication.update_with_changes(
            TreeUpdate {
                nodes: vec![],
                tree: None,
                tree_id: TreeId::ROOT,
                focus: Id(2),
            },
            &mut focus_only,
        );
        assert!(changes.focus);
        assert!(
            focus_only.seen.is_empty(),
            "focus has its own event without semantic snapshots"
        );
        let mut events = Events::default();
        let changes = publication.update_with_changes(
            TreeUpdate {
                nodes: vec![run(2, "ONE"), run(3, "TWO")],
                tree: None,
                tree_id: TreeId::ROOT,
                focus: Id(2),
            },
            &mut events,
        );
        assert_eq!(changes.updated, [NodeId(Id(2)), NodeId(Id(3))]);
        let roots = events
            .nodes
            .iter()
            .filter(|change| change.id == NodeId(Id(1)))
            .collect::<Vec<_>>();
        assert_eq!(roots.len(), 1, "one document event for the complete update");
        assert_eq!(roots[0].old.text.as_deref(), Some("onetwofixed"));
        assert_eq!(roots[0].new.text.as_deref(), Some("ONETWOfixed"));
        assert!(
            !events.seen.contains(&NodeId(Id(4))),
            "unchanged sibling is never snapshotted"
        );
        assert_eq!(
            publication
                .tree()
                .unwrap()
                .node_by_id(NodeId(Id(4)))
                .unwrap()
                .data()
                .value(),
            Some("fixed")
        );
    }

    #[test]
    fn child_insertions_and_removals_notify_the_surviving_document() {
        let mut publication = Publication::default();
        publication.update(document(false));
        for (extra, before, after) in [
            (true, "onetwofixed", "onetwofixednew"),
            (false, "onetwofixednew", "onetwofixed"),
        ] {
            let mut events = Events::default();
            publication.update_with_changes(document(extra), &mut events);
            let roots = events
                .nodes
                .iter()
                .filter(|change| change.id == NodeId(Id(1)))
                .collect::<Vec<_>>();
            assert_eq!(roots.len(), 1);
            assert_eq!(roots[0].old.text.as_deref(), Some(before));
            assert_eq!(roots[0].new.text.as_deref(), Some(after));
        }
    }
}
