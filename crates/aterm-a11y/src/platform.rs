// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A native publisher's retained model, without calls into AppKit. The shared
//! consumer validates updates, resolves inherited geometry and joins text runs.

use std::sync::{Arc, Weak};

#[cfg(test)]
use crate::model::NodeId as ModelId;
use crate::model::{Action, ActionData, ActionRequest, Live, TextSelection, TreeId, TreeUpdate};
use crate::tree::{ChangeHandler, Node, NodeId, Tree};

/// A native element retains the consumer's exact node lifetime and a weak
/// publisher identity. Reusing an id, or replacing the publisher, cannot revive it.
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

    /// Observe the consumer's complete old/new node pairs while retaining the
    /// shared structure, focus and live-announcement collection.
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

    /// Only actions advertised by this exact live element are admitted. The
    /// host independently validates the captured lossless owner/lifecycle context.
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
mod tests {
    use super::*;
    use crate::model::{Node as Data, Role, TextPosition, Tree as TreeData};

    fn update(children: bool) -> TreeUpdate {
        let mut root = Data::new(Role::Group);
        let mut nodes = vec![];
        if children {
            root.push_child(ModelId(2));
            let mut button = Data::new(Role::Button);
            button.set_label("Run");
            button.add_action(Action::Click);
            nodes.push((ModelId(2), button));
        }
        nodes.push((ModelId(1), root));
        TreeUpdate {
            nodes,
            tree: Some(TreeData::new(ModelId(1))),
            tree_id: TreeId::ROOT,
            focus: ModelId(1),
        }
    }

    #[test]
    fn actions_follow_live_support_and_a_retired_element_stays_retired() {
        let mut state = Publication::default();
        let changes = state.update(update(true));
        assert_eq!(changes.added.len(), 2);
        let handle = state.handle(model_id(2)).unwrap();
        assert_eq!(
            state
                .action(&handle, Action::Click, None)
                .unwrap()
                .target_node,
            ModelId(2)
        );
        assert!(state.action(&handle, Action::SetValue, None).is_none());
        let mut disabled = update(true);
        disabled.nodes[0].1.set_disabled();
        state.update(disabled);
        assert!(state.action(&handle, Action::Click, None).is_none());
        state.update(update(false));
        assert!(state.node(&handle).is_none());
        state.update(update(true));
        assert!(state.action(&handle, Action::Click, None).is_none());
        let new = state.handle(model_id(2)).unwrap();
        assert!(state.action(&new, Action::Click, None).is_some());
        let mut other = Publication::default();
        other.update(update(true));
        assert!(
            other.node(&new).is_none(),
            "a native object cannot migrate to a different publisher"
        );
        assert!(
            state.update(update(true)).updated.is_empty(),
            "unchanged data stays unchanged"
        );
    }

    #[test]
    fn a_held_element_cannot_cross_an_owner_lifecycle_replacement() {
        let scoped = |generation| {
            let mut update = update(true);
            update.nodes[0]
                .1
                .set_action_context(crate::model::ActionContext::new(41, generation));
            update
        };
        let mut state = Publication::default();
        state.update(scoped(7));
        let held = state.handle(model_id(2)).unwrap();
        let queued = state.action(&held, Action::Click, None).unwrap();
        let changes = state.update(scoped(8));
        assert_eq!(changes.replaced, vec![model_id(2)]);
        assert!(
            state.node(&held).is_none(),
            "the old OS element is retired despite equal node ids"
        );
        assert!(state.action(&held, Action::Click, None).is_none());
        assert_eq!(
            queued.context,
            Some(crate::model::ActionContext::new(41, 7))
        );
        let current = state.handle(model_id(2)).unwrap();
        assert_eq!(
            state.action(&current, Action::Click, None).unwrap().context,
            Some(crate::model::ActionContext::new(41, 8))
        );
        assert!(state.update(scoped(8)).replaced.is_empty());
        assert!(
            state.action(&current, Action::Click, None).is_some(),
            "ordinary same-owner frames preserve identity"
        );
    }

    #[test]
    fn astral_selection_round_trips_and_half_surrogates_are_refused() {
        let mut root = Data::new(Role::TextInput);
        root.push_child(ModelId(2));
        root.set_text_selection(TextSelection {
            anchor: TextPosition {
                node: ModelId(2),
                character_index: 1,
            },
            focus: TextPosition {
                node: ModelId(2),
                character_index: 2,
            },
        });
        root.add_action(Action::SetTextSelection);
        let mut run = Data::new(Role::TextRun);
        run.set_value("a🦀e\u{301}z");
        run.set_character_lengths(vec![1, 4, 3, 1]);
        let mut state = Publication::default();
        state.update(TreeUpdate {
            nodes: vec![(ModelId(1), root), (ModelId(2), run)],
            tree: Some(TreeData::new(ModelId(1))),
            tree_id: TreeId::ROOT,
            focus: ModelId(1),
        });
        let handle = state.handle(model_id(1)).unwrap();
        let node = state.node(&handle).unwrap();
        assert_eq!(text(node).as_deref(), Some("a🦀e\u{301}z"));
        assert_eq!(selection(node), Some((1, 2)));
        assert_eq!(
            text_selection(node, 1, 2).unwrap(),
            *node.data().text_selection().unwrap()
        );
        assert!(text_selection(node, 2, 0).is_none());
        assert!(
            text_selection(node, 4, 0).is_none(),
            "a combining cluster is one model character"
        );
        assert!(text_selection(node, 3, 2).is_some());
        assert!(text_selection(node, usize::MAX, 2).is_none());
    }
}
