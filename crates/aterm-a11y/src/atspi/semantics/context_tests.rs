// Copyright 2023 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/context.rs, and changed by the aterm project in 2026: a
// poisoned lock is recovered rather than unwound.

use super::*;
use crate::atspi::semantics::{Adapter, AdapterCallback, Event, InterfaceSet, WindowBounds};
use crate::model::{Node, NodeId, Role, Tree as ModelTree, TreeId, TreeUpdate};

struct NoActions;
impl ActionHandlerNoMut for NoActions {
    fn do_action(&self, _: ActionRequest) {}
}
struct NoBus;
impl AdapterCallback for NoBus {
    fn register_interfaces(&self, _: &Adapter, _: super::super::NodeIdentity, _: InterfaceSet) {}
    fn unregister_interfaces(&self, _: &Adapter, _: super::super::NodeIdentity, _: InterfaceSet) {}
    fn emit_event(&self, _: &Adapter, _: Event) {}
}
fn window(app: &Arc<SharedLock<AppContext>>, id: usize) -> Adapter {
    let root = NodeId(1);
    Adapter::with_wrapped_action_handler(
        id,
        app,
        NoBus,
        TreeUpdate {
            nodes: vec![(root, Node::new(Role::Window))],
            tree: Some(ModelTree::new(root)),
            tree_id: TreeId::ROOT,
            focus: root,
        },
        false,
        WindowBounds::default(),
        Arc::new(NoActions),
    )
}

#[test]
fn reverse_registration_retires_the_exact_window_context() {
    let app = AppContext::new(None);
    let first = window(&app, 0);
    let second = window(&app, 1);
    assert_eq!(app.read().adapter_index(0), Ok(0));
    assert_eq!(app.read().adapter_index(1), Ok(1));
    drop(first);
    assert_eq!(app.read().adapter_index(1), Ok(0));
    drop(second);
    assert!(app.read().adapters.is_empty());

    let later = window(&app, 3);
    let earlier = window(&app, 2);
    let weak_later = {
        let state = app.read();
        Arc::downgrade(
            &state
                .adapters
                .iter()
                .find(|(id, _)| *id == 3)
                .expect("registered3")
                .1,
        )
    };
    let sorted = app
        .read()
        .adapters
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>()
        == [2, 3];
    let indexed = app.read().adapter_index(2) == Ok(0) && app.read().adapter_index(3) == Ok(1);
    drop(later); // real Drop must remove3, not retain its Arc cycle in app
    let retired = weak_later.upgrade().is_none();
    let survivor = app
        .read()
        .adapters
        .iter()
        .map(|(id, _)| *id)
        .collect::<Vec<_>>()
        == [2]
        && app.read().adapter_index(2) == Ok(0);
    drop(earlier);
    let empty = app.read().adapters.is_empty();
    assert!(
        sorted && indexed && retired && survivor && empty,
        "A11Y_REVERSE_REGISTRATION_RETAINED_WINDOW: sorted={sorted} indexed={indexed} retired={retired} survivor={survivor} empty={empty}"
    );
}
