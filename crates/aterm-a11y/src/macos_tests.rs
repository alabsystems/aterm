// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

use super::*;
use crate::model::ActionRequest;

struct Inert;
impl ActivationHandler for Inert {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        panic!("offscreen activation");
    }
}
impl ActionHandler for Inert {
    fn do_action(&mut self, _: ActionRequest) {
        panic!("offscreen action");
    }
}

#[test]
fn a_meter_is_a_level_indicator_a_grid_a_grid_and_a_heading_a_heading() {
    assert_eq!(role(Role::Meter), "AXLevelIndicator");
    assert_eq!(role(Role::Grid), "AXGrid");
    assert_eq!(role(Role::ProgressIndicator), "AXProgressIndicator");
    assert_eq!(role(Role::Heading), "AXHeading");
    assert_eq!(role(Role::Label), "AXStaticText");
}

#[test]
fn a_label_says_its_words_once_and_a_heading_by_its_label() {
    use crate::model::{Node as ModelNode, NodeId as ModelId, Tree, TreeId};
    let node = |role, label: Option<&str>, value: Option<&str>| {
        let mut node = ModelNode::new(role);
        if let Some(label) = label {
            node.set_label(label.to_owned());
        }
        if let Some(value) = value {
            node.set_value(value.to_owned());
        }
        node
    };
    let cases = [
        (Role::Label, None, Some("Changes apply at once")),
        (Role::Label, Some("aterm · ALab"), Some("aterm · ALab")),
        (Role::Label, Some("Version"), None),
        (Role::Label, Some("Version"), Some("1.2.3")),
        (Role::Heading, Some("System State"), None),
        (Role::Button, Some("Stop"), None),
    ];
    let mut root = ModelNode::new(Role::Window);
    root.set_children((1..=cases.len() as u64).map(ModelId).collect::<Vec<_>>());
    let mut nodes = vec![(ModelId(0), root)];
    nodes.extend(
        (1..).map(ModelId).zip(
            cases
                .iter()
                .map(|(role, label, value)| node(*role, *label, *value)),
        ),
    );
    let mut publication = Publication::default();
    publication.update(TreeUpdate {
        nodes,
        tree: Some(Tree::new(ModelId(0))),
        tree_id: TreeId::ROOT,
        focus: ModelId(0),
    });
    let read = |id: u64| {
        let node = publication
            .tree()
            .unwrap()
            .node_by_id(NodeId(ModelId(id)))
            .unwrap();
        (description(node), state::text(node))
    };
    let words = |words: &str| Some(words.to_owned());
    assert_eq!(read(1), (None, words("Changes apply at once")));
    assert_eq!(read(2), (None, words("aterm · ALab")), "once, as its value");
    assert_eq!(
        read(3),
        (words("Version"), None),
        "a label alone is not dropped"
    );
    assert_eq!(read(4), (words("Version"), words("1.2.3")));
    assert_eq!(read(5), (words("System State"), None));
    assert_eq!(read(6), (words("Stop"), None));
}

#[test]
fn no_native_view_is_inert_without_initializing_appkit() {
    // SAFETY: no view is supplied, so no native precondition is required.
    assert!(unsafe { Adapter::new(None, 1., Inert, Inert) }.is_none());
}

#[test]
fn retired_elements_cannot_read_replacement_focus_hits_or_window() {
    use crate::model::{
        ActionContext, Node as ModelNode, NodeId as ModelId, Point, Rect, Tree, TreeId,
    };
    let update = |generation, present| {
        let mut root = ModelNode::new(Role::Window);
        root.set_bounds(Rect::new(0., 0., 100., 100.));
        root.set_children(if present { vec![ModelId(1)] } else { vec![] });
        let mut nodes = vec![(ModelId(0), root)];
        if present {
            let mut button = ModelNode::new(Role::Button);
            button.set_bounds(Rect::new(0., 0., 40., 40.));
            button.set_action_context(ActionContext::new(11, generation));
            nodes.push((ModelId(1), button));
        }
        TreeUpdate {
            nodes,
            tree: Some(Tree::new(ModelId(0))),
            tree_id: TreeId::ROOT,
            focus: if present { ModelId(1) } else { ModelId(0) },
        }
    };
    let mut cx = Context {
        publication: Publication::default(),
        // SAFETY: nil initializes an empty weak runtime slot; no AppKit call.
        view: unsafe { WeakObj::new(Id::NIL) },
        host: None,
        elements: HashMap::new(),
        activation: Rc::new(RefCell::new(Box::new(Inert))),
        actions: Rc::new(RefCell::new(Box::new(Inert))),
        active: true,
        closed: false,
        scale: 1.,
    };
    cx.publication.update(update(7, true));
    cx.publication.set_focused(true);
    let old = cx.publication.handle(NodeId(ModelId(1))).unwrap();
    let focus = |cx: &Context, _: Option<&Handle>| cx.publication.tree()?.focus_id();
    let hit = |cx: &Context, _: Option<&Handle>| {
        cx.publication
            .tree()?
            .root()
            .node_at_point(Point::new(10., 10.), &common_filter)
            .map(|node| node.id())
    };
    assert_eq!(
        cx.read_live(Some(&old), focus),
        Some(Some(NodeId(ModelId(1))))
    );
    assert_eq!(
        cx.read_live(Some(&old), hit),
        Some(Some(NodeId(ModelId(1))))
    );
    cx.publication.update(update(8, true));
    assert!(cx.read_live(Some(&old), focus).is_none());
    assert!(cx.read_live(Some(&old), hit).is_none());
    let window_read = std::cell::Cell::new(false);
    assert!(
        cx.read_live(Some(&old), |_, _| window_read.set(true))
            .is_none()
    );
    assert!(
        !window_read.get(),
        "a retired element never reaches native window reads"
    );
    let fresh = cx.publication.handle(NodeId(ModelId(1))).unwrap();
    assert!(cx.read_live(Some(&fresh), focus).is_some());
    cx.publication.update(update(8, true));
    assert!(
        cx.read_live(Some(&fresh), hit).is_some(),
        "ordinary updates retain identity"
    );
    cx.publication.update(update(8, false));
    assert!(cx.read_live(Some(&fresh), focus).is_none());
    cx.publication.update(update(8, true));
    assert!(
        cx.read_live(Some(&fresh), hit).is_none(),
        "reinsertion is a new object"
    );
    assert!(
        cx.read_live(None, focus).is_some(),
        "the live lazy host may read"
    );
    cx.closed = true;
    assert!(
        cx.read_live(None, focus).is_none(),
        "a closed host may not read"
    );
}

#[test]
fn text_ranges_use_utf16_and_reject_partial_surrogates() {
    let value = "a🦀\nz";
    assert_eq!(
        utf16_slice(
            value,
            NSRange {
                location: 1,
                length: 2
            }
        )
        .as_deref(),
        Some("🦀")
    );
    assert!(
        utf16_slice(
            value,
            NSRange {
                location: 2,
                length: 1
            }
        )
        .is_none()
    );
    assert!(
        utf16_slice(
            value,
            NSRange {
                location: usize::MAX,
                length: 1
            }
        )
        .is_none()
    );
    let first = line_range(value, 0);
    assert_eq!((first.location, first.length), (0, 4));
    let second = line_range(value, 1);
    assert_eq!((second.location, second.length), (4, 1));
    assert_eq!(line_range(value, 2).location, isize::MAX as usize);
}
