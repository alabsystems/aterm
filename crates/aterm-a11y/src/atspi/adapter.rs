// Copyright 2022, 2023 The AccessKit Authors.
// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0
//
// Derived from accesskit_unix 0.22.0 (MIT OR Apache-2.0, used under
// Apache-2.0) src/adapter.rs and the adapter half of src/context.rs, and
// rewritten by the aterm project in 2026: the channel is std's, the
// publisher thread is woken through a socket, and a poisoned lock is
// recovered rather than unwound.

//! Window-side [`Adapter`] state and publisher callback.

use std::fmt;
use std::sync::{Arc, Mutex};

use crate::model::{ActionHandler, ActivationHandler, DeactivationHandler, Rect, TreeUpdate};

use aterm_types::MutexExt as _;

use super::publisher::{Change, Link, Msg};
use super::semantics::{
    ActionHandlerNoMut, ActionHandlerWrapper, Adapter as Semantics, AdapterCallback, CacheEvent,
    Event, InterfaceSet, NodeIdentity, WindowBounds, next_adapter_id,
};

/// Where a window's accessibility stands.
pub(crate) enum AdapterState {
    /// No assistive technology is listening: nothing is built.
    Inactive {
        is_window_focused: bool,
        root_window_bounds: WindowBounds,
        action_handler: Arc<dyn ActionHandlerNoMut + Send + Sync>,
    },
    /// One is, and the window owes its first full tree, for the publisher's
    /// `epoch`th accessibility bus.
    Pending {
        epoch: u64,
        is_window_focused: bool,
        root_window_bounds: WindowBounds,
        action_handler: Arc<dyn ActionHandlerNoMut + Send + Sync>,
    },
    /// The tree is published.
    Active(Semantics),
}

impl fmt::Debug for AdapterState {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Inactive {
                is_window_focused, ..
            } => f
                .debug_struct("Inactive")
                .field("is_window_focused", is_window_focused)
                .finish_non_exhaustive(),
            Self::Pending {
                epoch,
                is_window_focused,
                ..
            } => f
                .debug_struct("Pending")
                .field("epoch", epoch)
                .field("is_window_focused", is_window_focused)
                .finish_non_exhaustive(),
            Self::Active(semantics) => f.debug_tuple("Active").field(semantics).finish(),
        }
    }
}

/// Hands the tree's registrations and events to the publisher thread,
/// marked with the accessibility bus the tree was built for.
pub(crate) struct Callback {
    link: Link,
    epoch: u64,
}

impl Callback {
    pub(crate) fn new(link: Link, epoch: u64) -> Self {
        Self { link, epoch }
    }

    fn send(&self, change: Change) {
        self.link.send(Msg::Tree {
            epoch: self.epoch,
            change,
        });
    }
}

impl AdapterCallback for Callback {
    fn register_interfaces(&self, adapter: &Semantics, id: NodeIdentity, interfaces: InterfaceSet) {
        self.send(Change::RegisterInterfaces {
            node: adapter.platform_node(id),
            interfaces,
        });
    }

    fn unregister_interfaces(
        &self,
        adapter: &Semantics,
        id: NodeIdentity,
        interfaces: InterfaceSet,
    ) {
        self.send(Change::UnregisterInterfaces {
            adapter: adapter.id(),
            node: id,
            interfaces,
        });
    }

    fn emit_event(&self, adapter: &Semantics, event: Event) {
        self.send(match event {
            Event::Cache(CacheEvent::Added(id)) => Change::CacheAdd {
                node: adapter.platform_node(id),
            },
            Event::Cache(CacheEvent::Removed(id)) => Change::CacheRemove {
                adapter: adapter.id(),
                node: id,
            },
            event => Change::Event {
                adapter: adapter.id(),
                event,
            },
        });
    }
}

/// One window's accessibility tree, published while an assistive technology listens.
#[derive(Debug)]
pub struct Adapter {
    id: usize,
    link: Link,
    state: Arc<Mutex<AdapterState>>,
}

impl Adapter {
    pub(crate) fn new(
        link: Link,
        activation_handler: impl 'static + ActivationHandler + Send,
        action_handler: impl 'static + ActionHandler + Send,
        deactivation_handler: impl 'static + DeactivationHandler + Send,
    ) -> Self {
        let id = next_adapter_id();
        let state = Arc::new(Mutex::new(AdapterState::Inactive {
            is_window_focused: false,
            root_window_bounds: WindowBounds::default(),
            action_handler: Arc::new(ActionHandlerWrapper::new(action_handler)),
        }));
        link.send(Msg::AddAdapter {
            id,
            activation_handler: Box::new(activation_handler),
            deactivation_handler: Box::new(deactivation_handler),
            state: Arc::clone(&state),
        });
        Self { id, link, state }
    }

    /// Whether an assistive technology listens: its first tree owed or published.
    pub fn is_listened(&self) -> bool {
        !matches!(*self.state.lock_or_recover(), AdapterState::Inactive { .. })
    }

    /// Sets outer (with decorations) and inner window bounds in screen coordinates.
    pub fn set_root_window_bounds(&mut self, outer: Rect, inner: Rect) {
        let bounds = WindowBounds::new(outer, inner);
        match &mut *self.state.lock_or_recover() {
            AdapterState::Inactive {
                root_window_bounds, ..
            }
            | AdapterState::Pending {
                root_window_bounds, ..
            } => *root_window_bounds = bounds,
            AdapterState::Active(semantics) => semantics.set_root_window_bounds(bounds),
        }
    }

    /// Applies `update_factory`'s update if an assistive technology listens.
    pub fn update_if_active(&mut self, update_factory: impl FnOnce() -> TreeUpdate) {
        let mut state = self.state.lock_or_recover();
        match &mut *state {
            AdapterState::Inactive { .. } => {}
            AdapterState::Pending {
                epoch,
                is_window_focused,
                root_window_bounds,
                action_handler,
            } => {
                let semantics = first_tree(
                    self.id,
                    &self.link,
                    *epoch,
                    update_factory(),
                    *is_window_focused,
                    *root_window_bounds,
                    Arc::clone(action_handler),
                );
                *state = AdapterState::Active(semantics);
            }
            AdapterState::Active(semantics) => semantics.update(update_factory()),
        }
    }

    /// Whether the window has the keyboard focus.
    pub fn update_window_focus_state(&mut self, is_focused: bool) {
        match &mut *self.state.lock_or_recover() {
            AdapterState::Inactive {
                is_window_focused, ..
            }
            | AdapterState::Pending {
                is_window_focused, ..
            } => *is_window_focused = is_focused,
            AdapterState::Active(semantics) => semantics.update_window_focus_state(is_focused),
        }
    }
}

impl Drop for Adapter {
    fn drop(&mut self) {
        self.link.send(Msg::RemoveAdapter { id: self.id });
    }
}

fn first_tree(
    id: usize,
    link: &Link,
    epoch: u64,
    tree: TreeUpdate,
    is_window_focused: bool,
    bounds: WindowBounds,
    handler: Arc<dyn ActionHandlerNoMut + Send + Sync>,
) -> Semantics {
    let mut semantics = Semantics::with_wrapped_action_handler(
        id,
        link.app(),
        Callback::new(link.clone(), epoch),
        tree,
        false,
        bounds,
        handler,
    );
    if is_window_focused {
        semantics.update_window_focus_state(true);
    }
    semantics
}

/// The publisher thread's record of one adapter.
pub(crate) struct Entry {
    pub(crate) id: usize,
    pub(crate) activation_handler: Box<dyn ActivationHandler + Send>,
    pub(crate) deactivation_handler: Box<dyn DeactivationHandler + Send>,
    pub(crate) state: Arc<Mutex<AdapterState>>,
}

impl Entry {
    /// An assistive technology is listening on the publisher's `epoch`th
    /// accessibility bus: ask the window for its tree. An initial tree given
    /// at once is published at once; else the window owes it through
    /// [`Adapter::update_if_active`].
    pub(crate) fn activate(&mut self, link: &Link, epoch: u64) {
        let mut state = self.state.lock_or_recover();
        let AdapterState::Inactive {
            is_window_focused,
            root_window_bounds,
            action_handler,
        } = &*state
        else {
            return;
        };
        let (focused, bounds, handler) = (
            *is_window_focused,
            *root_window_bounds,
            Arc::clone(action_handler),
        );
        *state = match self.activation_handler.request_initial_tree() {
            Some(initial) => AdapterState::Active(first_tree(
                self.id, link, epoch, initial, focused, bounds, handler,
            )),
            None => AdapterState::Pending {
                epoch,
                is_window_focused: focused,
                root_window_bounds: bounds,
                action_handler: handler,
            },
        };
    }

    /// Nobody listens any more: drop the tree and tell the window.
    pub(crate) fn deactivate(&mut self) {
        let mut state = self.state.lock_or_recover();
        let next = match &*state {
            AdapterState::Inactive { .. } => return,
            AdapterState::Pending {
                is_window_focused,
                root_window_bounds,
                action_handler,
                ..
            } => AdapterState::Inactive {
                is_window_focused: *is_window_focused,
                root_window_bounds: *root_window_bounds,
                action_handler: Arc::clone(action_handler),
            },
            AdapterState::Active(semantics) => AdapterState::Inactive {
                is_window_focused: semantics.is_window_focused(),
                root_window_bounds: semantics.root_window_bounds(),
                action_handler: semantics.wrapped_action_handler(),
            },
        };
        let previous = std::mem::replace(&mut *state, next);
        drop(state);
        drop(previous);
        self.deactivation_handler.deactivate_accessibility();
    }
}
