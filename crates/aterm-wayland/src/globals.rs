// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! The registry: the globals a compositor advertises, collected on a private
//! queue in one round trip, and bound by name and version.

use std::ffi::{CStr, CString};
use std::sync::{Arc, Mutex, PoisonError};

use crate::iface::Interface;
use crate::protocols::wayland::{wl_display, wl_registry};
use crate::proxy::{Error, EventArg, Listener, Proxy, Queue};

/// One advertised global.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Global {
    /// The registry's name for it, what `bind` takes.
    pub name: u32,
    /// Its interface's name.
    pub interface: CString,
    /// The highest version the compositor offers.
    pub version: u32,
}

/// What the registry's listener and the caller share: the globals, and how
/// many times the list changed (a leaf of the lock graph: nothing but the
/// list's own methods runs while it is held).
#[derive(Default)]
struct RegistryBook {
    globals: Vec<Global>,
    generation: u64,
}

/// A registry on a private queue, kept current as globals come and go
/// whenever that queue is dispatched.
pub struct Registry {
    proxy: Proxy,
    registry_book: Arc<Mutex<RegistryBook>>,
    /// Freed after `proxy` is destroyed (Drop).
    _listener: Listener,
}

impl Registry {
    /// `wl_display.get_registry` through `queue`'s display wrapper, then a
    /// round trip, after which every global the compositor had is listed.
    pub fn new(queue: &Queue) -> Result<Registry, Error> {
        let wrapper = queue.display_wrapper()?;
        // SAFETY: the wrapper is live for the call; the registry it makes is
        // on `queue`.
        let proxy = unsafe { wl_display::get_registry(wrapper.proxy()) }.ok_or(Error::NoObject)?;
        drop(wrapper);
        let registry_book = Arc::new(Mutex::new(RegistryBook::default()));
        // SAFETY: a registry this crate made, without a listener; the
        // Listener lives in the returned value and drops after the proxy.
        let listener = unsafe { registry_listener(proxy, &registry_book) };
        let listener = match listener {
            Ok(l) => l,
            Err(e) => {
                // SAFETY: the registry made above, not used again.
                unsafe { proxy.destroy() };
                return Err(e);
            }
        };
        let registry = Registry {
            proxy,
            registry_book,
            _listener: listener,
        };
        queue.roundtrip()?;
        Ok(registry)
    }

    /// The globals as of the last dispatch of the queue.
    pub fn globals(&self) -> Vec<Global> {
        self.registry_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .globals
            .clone()
    }

    /// How many times a global came or went since the registry was made: a
    /// caller that binds globals as they come compares it with the last one
    /// it saw, and syncs when it moved.
    pub fn generation(&self) -> u64 {
        self.registry_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .generation
    }

    /// The first global of `interface`.
    pub fn find(&self, interface: &CStr) -> Option<Global> {
        self.registry_book
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .globals
            .iter()
            .find(|g| g.interface.as_c_str() == interface)
            .cloned()
    }

    /// Bind `global` as `iface` at `version` (at most what it offers); the
    /// object lands on the registry's queue.
    pub fn bind(&self, global: &Global, iface: &'static Interface, version: u32) -> Option<Proxy> {
        let version = version.min(global.version).max(1);
        // SAFETY: the registry is live (owned by self); the interface table
        // is static and names the global's interface.
        unsafe { wl_registry::bind(self.proxy, global.name, iface, version) }
    }
}

/// A registry event, decoded.
enum Announced {
    Global(Global),
    Removed(u32),
}

/// Listen to a registry: each global announced or removed, decoded with the
/// book unlocked, then recorded (a leaf of the lock graph: nothing but the
/// list's own methods runs while it is held).
///
/// # Safety
/// `proxy` is a live registry this crate made, without a listener, destroyed
/// before the listener is freed.
unsafe fn registry_listener(
    proxy: Proxy,
    registry_book: &Arc<Mutex<RegistryBook>>,
) -> Result<Listener, Error> {
    let registry_book = Arc::clone(registry_book);
    // SAFETY: the caller's contract.
    unsafe {
        proxy.listen(move |_, opcode, args| {
            let announced = match (opcode, args) {
                (
                    wl_registry::evt::GLOBAL,
                    [
                        EventArg::Uint(name),
                        EventArg::Str(Some(iface)),
                        EventArg::Uint(version),
                    ],
                ) => Announced::Global(Global {
                    name: *name,
                    interface: (*iface).to_owned(),
                    version: *version,
                }),
                (wl_registry::evt::GLOBAL_REMOVE, [EventArg::Uint(name)]) => {
                    Announced::Removed(*name)
                }
                _ => return,
            };
            let mut book = registry_book.lock().unwrap_or_else(PoisonError::into_inner);
            match announced {
                Announced::Global(g) => book.globals.push(g),
                Announced::Removed(name) => book.globals.retain(|g| g.name != name),
            }
            book.generation += 1;
        })
    }
}

impl Drop for Registry {
    fn drop(&mut self) {
        // SAFETY: the registry this value made; wl_registry has no destructor
        // request, so the proxy is destroyed client-side. The listener drops
        // after this, with the other fields.
        unsafe { self.proxy.destroy() }
    }
}
