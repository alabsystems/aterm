// Copyright 2023 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_atspi_common 0.19.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/context.rs, and changed by the aterm project in 2026: a
// poisoned lock is recovered rather than unwound.

//! Shared tree, window and application context for AT-SPI nodes.

use std::fmt::{Debug, Formatter};
use std::sync::{Arc, Mutex, PoisonError, RwLock, RwLockReadGuard, RwLockWriteGuard};

use crate::model::{ActionHandler, ActionRequest};
use crate::tree::Tree;
use aterm_types::MutexExt as _;

use super::WindowBounds;

/// A window's action handler behind a shared reference.
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

/// A read-write lock whose poisoning is recovered.
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

/// The application.
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
#[path = "context_tests.rs"]
mod tests;
