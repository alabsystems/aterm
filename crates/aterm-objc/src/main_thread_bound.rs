// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A thread-affinity container, [`MainThreadBound<T>`], built from the [`MainThread`] witness and [`run_on_main`].

use std::mem::ManuallyDrop;

use crate::declare::MainThread;
use crate::dispatch::run_on_main;

/// A value that may only be touched on the process main thread, in a container that may travel anywhere.
pub struct MainThreadBound<T>(ManuallyDrop<T>);

// SAFETY: obligations 1-3 of the module docs: born, read and dropped only on the main thread, so what crosses threads is an inert copy of `T`'s bytes.
unsafe impl<T> Send for MainThreadBound<T> {}

// SAFETY: every `&T` exists on the main thread and `&mut T` needs `&mut self`; see the module docs.
unsafe impl<T> Sync for MainThreadBound<T> {}

impl<T> MainThreadBound<T> {
    /// Put `inner` in the container.
    #[inline]
    #[must_use]
    pub const fn new(inner: T, _mt: MainThread) -> Self {
        Self(ManuallyDrop::new(inner))
    }

    /// The value, borrowed, on the thread it belongs to.
    #[inline]
    #[must_use]
    pub fn get(&self, _mt: MainThread) -> &T {
        &self.0
    }

    /// The value, mutably, on the thread it belongs to.
    #[inline]
    #[must_use]
    pub fn get_mut(&mut self, _mt: MainThread) -> &mut T {
        &mut self.0
    }

    /// Take the value out, onto the main thread where it may live.
    #[inline]
    #[must_use]
    pub fn into_inner(self, _mt: MainThread) -> T {
        let mut this = ManuallyDrop::new(self);
        // SAFETY: `self` was consumed by this call and is wrapped in a `ManuallyDrop`, so `this.0` is never read or dropped again.
        unsafe { ManuallyDrop::take(&mut this.0) }
    }

    /// Run `f` against the value on the main thread and bring its result back.
    /// # Panics
    /// If `f` panics, the panic is re-raised here with its original payload.
    #[inline]
    pub fn get_on_main<F, R>(&self, f: F) -> R
    where
        F: Send + FnOnce(&T) -> R,
        R: Send,
    {
        run_on_main(|mt| f(self.get(mt)))
    }
}

impl<T> Drop for MainThreadBound<T> {
    /// Obligation 3. A `T` with no destructor is dropped in place, so it never
    fn drop(&mut self) {
        if !std::mem::needs_drop::<T>() {
            return;
        }
        run_on_main(|_mt| {
            let this = &mut *self;
            // SAFETY: this is the container's own `Drop`, so the value is dropped once and never read again; `run_on_main` put us on the main thread, where `new`'s witness says it belongs.
            unsafe { ManuallyDrop::drop(&mut this.0) };
        });
    }
}

impl<T> std::fmt::Debug for MainThreadBound<T> {
    /// Opaque: printing the value needs a witness `Debug` cannot have, and
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainThreadBound").finish_non_exhaustive()
    }
}
