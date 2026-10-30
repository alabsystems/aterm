// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Wayland compositor global registry.

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

#[derive(Default)]
struct RegistryBook {
    globals: Vec<Global>,
    generation: u64,
}

/// A Wayland global registry on a private queue.
pub struct Registry {
    proxy: Proxy,
    registry_book: Arc<Mutex<RegistryBook>>,
    _listener: Listener,
}

impl Registry {
    /// Creates a registry on `queue` and collects advertised globals in one round trip.
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

    /// Returns how many times globals were added or removed since creation.
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

    /// Binds `global` as `iface` at up to `version` on the registry's queue.
    pub fn bind(&self, global: &Global, iface: &'static Interface, version: u32) -> Option<Proxy> {
        let version = bind_version(global.version, iface.version(), version)?;
        // SAFETY: the registry is live (owned by self); the interface table
        // is static and names the global's interface.
        unsafe { wl_registry::bind(self.proxy, global.name, iface, version) }
    }
}

fn bind_version(advertised: u32, table: u32, requested: u32) -> Option<u32> {
    let version = requested.min(advertised).min(table);
    (version > 0).then_some(version)
}

enum Announced {
    Global(Global),
    Removed(u32),
}

/// Attaches a listener that records announced and removed globals in `registry_book`.
///
/// # Safety
/// `proxy` is a live registry this crate made, without a listener, destroyed before the listener
/// is freed.
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

#[cfg(test)]
mod tests {
    use super::bind_version;

    #[test]
    fn bind_version_caps_to_advertised_and_table_and_rejects_zero() {
        assert_eq!(bind_version(0, 6, 4), None);
        assert_eq!(bind_version(6, 0, 4), None);
        assert_eq!(bind_version(6, 6, 0), None);
        assert_eq!(bind_version(3, 6, 5), Some(3));
        assert_eq!(bind_version(8, 4, 6), Some(4));
        assert_eq!(bind_version(8, 6, 2), Some(2));
    }
}
