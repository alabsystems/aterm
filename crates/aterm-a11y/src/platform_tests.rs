// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

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
fn a_held_element_cannot_cross_an_owner_lifecycle_or_review_replacement() {
    use crate::model::ActionContext;
    let scoped = |context| {
        let mut update = update(true);
        update.nodes[0].1.set_action_context(context);
        update
    };
    for next in [
        ActionContext::new(41, 8),
        ActionContext::native_review(41, 7, 1),
    ] {
        let mut state = Publication::default();
        state.update(scoped(ActionContext::new(41, 7)));
        let held = state.handle(model_id(2)).unwrap();
        let queued = state.action(&held, Action::Click, None).unwrap();
        let changes = state.update(scoped(next));
        assert_eq!(changes.replaced, vec![model_id(2)]);
        assert!(
            state.node(&held).is_none(),
            "the old OS element is retired despite equal node ids"
        );
        assert!(state.action(&held, Action::Click, None).is_none());
        assert_eq!(queued.context, Some(ActionContext::new(41, 7)));
        let current = state.handle(model_id(2)).unwrap();
        assert_eq!(
            state.action(&current, Action::Click, None).unwrap().context,
            Some(next)
        );
        assert!(state.update(scoped(next)).replaced.is_empty());
        assert!(
            state.action(&current, Action::Click, None).is_some(),
            "ordinary same-owner frames preserve identity"
        );
    }
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
