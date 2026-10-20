// Copyright 2023 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/context.rs, and changed by the aterm project in 2026: a
// poisoned lock is recovered rather than unwound.

//! The state a window's tree shares with its nodes and the application:
//! the tree, the window's bounds and action handler, and the application's
//! name, toolkit, registry id and windows.

use std::fmt::{Debug, Formatter};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::model::{ActionHandler, ActionRequest};
use crate::tree::Tree;
use aterm_types::MutexExt as _;

use super::WindowBounds;

/// A window's action handler behind a shared reference: the publisher hands
/// requests on from any node.
pub(crate) trait ActionHandlerNoMut {
    fn do_action(&self, request: ActionRequest);
}

/// An [`ActionHandler`] made [`ActionHandlerNoMut`] by a lock.
pub(crate) struct ActionHandlerWrapper<H: ActionHandler + Send>(Mutex<H>);

impl<H: 'static + ActionHandler + Send> ActionHandlerWrapper<H> {
    pub(crate) fn new(inner: H) -> Self {
        Self(Mutex::new(inner))
    }
}

impl<H: ActionHandler + Send> ActionHandlerNoMut for ActionHandlerWrapper<H> {
    fn do_action(&self, request: ActionRequest) {
        self.0.lock_or_recover().do_action(request)
    }
}

/// A read-write lock whose poisoning is recovered: a panic on another
/// thread while it was held leaves the value as it was, and the publisher
/// goes on serving it.
#[derive(Debug, Default)]
pub(crate) struct SharedLock<T>(RwLock<T>);

impl<T> SharedLock<T> {
    pub(crate) fn new(value: T) -> Self {
        Self(RwLock::new(value))
    }

    pub(crate) fn read(&self) -> RwLockReadGuard<'_, T> {
        self.0.read().unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn write(&self) -> RwLockWriteGuard<'_, T> {
        self.0.write().unwrap_or_else(PoisonError::into_inner)
    }
}

/// One window's tree and what its nodes answer from.
pub(crate) struct Context {
    pub(crate) app_context: Arc<SharedLock<AppContext>>,
    pub(crate) tree: SharedLock<Tree>,
    pub(crate) action_handler: Arc<dyn ActionHandlerNoMut + Send + Sync>,
    pub(crate) root_window_bounds: SharedLock<WindowBounds>,
}

impl Debug for Context {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Context")
            .field("app_context", &self.app_context)
            .field("tree", &self.tree)
            .field("action_handler", &"ActionHandler")
            .field("root_window_bounds", &self.root_window_bounds)
            .finish()
    }
}

impl Context {
    pub(crate) fn new(
        app_context: &Arc<SharedLock<AppContext>>,
        tree: Tree,
        action_handler: Arc<dyn ActionHandlerNoMut + Send + Sync>,
        root_window_bounds: WindowBounds,
    ) -> Arc<Self> {
        Arc::new(Self {
            app_context: Arc::clone(app_context),
            tree: SharedLock::new(tree),
            action_handler,
            root_window_bounds: SharedLock::new(root_window_bounds),
        })
    }

    pub(crate) fn do_action(&self, request: ActionRequest) {
        self.action_handler.do_action(request);
    }
}

/// The application: its name, its toolkit's name and version, the id the
/// registry gave it, and its windows' trees, by adapter id.
#[derive(Debug)]
pub(crate) struct AppContext {
    pub(crate) name: Option<String>,
    pub(crate) toolkit_name: Option<String>,
    pub(crate) toolkit_version: Option<String>,
    pub(crate) id: Option<i32>,
    pub(crate) adapters: Vec<(usize, Arc<Context>)>,
}

impl AppContext {
    pub(crate) fn new(name: Option<String>) -> Arc<SharedLock<Self>> {
        Arc::new(SharedLock::new(Self {
            name,
            toolkit_name: None,
            toolkit_version: None,
            id: None,
            adapters: Vec::new(),
        }))
    }

    /// The window's place among the application's children.
    pub(crate) fn adapter_index(&self, id: usize) -> Result<usize, usize> {
        self.adapters.binary_search_by(|adapter| adapter.0.cmp(&id))
    }

    pub(crate) fn push_adapter(&mut self, id: usize, context: &Arc<Context>) {
        let index = self
            .adapter_index(id)
            .expect_err("adapter id already registered");
        self.adapters.insert(index, (id, Arc::clone(context)));
    }

    pub(crate) fn remove_adapter(&mut self, id: usize) {
        if let Ok(index) = self.adapter_index(id) {
            self.adapters.remove(index);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::atspi::semantics::{Adapter, AdapterCallback, Event, InterfaceSet, WindowBounds};
    use crate::model::{Node, NodeId, Role, Tree as ModelTree, TreeId, TreeUpdate};

    struct NoActions;
    impl ActionHandlerNoMut for NoActions {
        fn do_action(&self, _: ActionRequest) {}
    }
    struct NoBus;
    impl AdapterCallback for NoBus {
        fn register_interfaces(&self, _: &Adapter, _: super::super::NodeIdentity, _: InterfaceSet) {
        }
        fn unregister_interfaces(
            &self,
            _: &Adapter,
            _: super::super::NodeIdentity,
            _: InterfaceSet,
        ) {
        }
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
        // Ordinary registration order and each actual Adapter Drop.
        let first = window(&app, 0);
        let second = window(&app, 1);
        assert_eq!(app.read().adapter_index(0), Ok(0));
        assert_eq!(app.read().adapter_index(1), Ok(1));
        drop(first);
        assert_eq!(app.read().adapter_index(1), Ok(0));
        drop(second);
        assert!(app.read().adapters.is_empty());

        // IDs are assigned before activation. Window2 can finish its async
        // initial tree after window3: register3 first, then2, as Pending allows.
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
}
