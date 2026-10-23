// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A thread-affinity container, [`MainThreadBound<T>`], built from the [`MainThread`] witness and [`run_on_main`].

use std::mem::ManuallyDrop;

use crate::declare::MainThread;
use crate::dispatch::run_on_main;

/// A value that may only be touched on the process main thread, in a container that may travel anywhere.
///
/// # Examples
///
/// ```no_run
/// # use aterm_objc::{MainThread, MainThreadBound};
/// # use std::rc::Rc;
/// let mt = MainThread::new().expect("must be on the main thread");
/// // `Rc` is `!Send`. The container is not.
/// let bound = MainThreadBound::new(Rc::new(7_u8), mt);
///
/// fn assert_send<T: Send + Sync>(_: &T) {}
/// assert_send(&bound);
///
/// // Reading it needs a witness, wherever you are.
/// let seven = bound.get_on_main(|rc| **rc);
/// assert_eq!(seven, 7);
/// ```
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
    ///
    /// The witness is `!Send`, so it cannot follow the container to a worker
    /// (the error names the marker field, "`*const u8` cannot be sent between
    /// threads safely"):
    ///
    /// ```compile_fail
    /// # use aterm_objc::{MainThread, MainThreadBound};
    /// # use std::rc::Rc;
    /// let mt = MainThread::new().unwrap();
    /// let bound = MainThreadBound::new(Rc::new(7_u8), mt);
    /// std::thread::spawn(move || {
    ///     // The container is `Send` and arrived. The witness is not.
    ///     let inner = bound.into_inner(mt);
    ///     drop(inner);
    /// });
    /// ```
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
    ///
    /// `R: Send`, and `f` takes a higher-ranked `&T` with no lifetime `R` can
    /// unify with, so a borrow of the value cannot come back:
    ///
    /// ```compile_fail
    /// # use aterm_objc::{MainThread, MainThreadBound};
    /// # use std::rc::Rc;
    /// let mt = MainThread::new().unwrap();
    /// let bound = MainThreadBound::new(Rc::new(7_u8), mt);
    /// // Smuggling the main thread's value out as a borrow.
    /// let escaped: &Rc<u8> = bound.get_on_main(|rc| rc);
    /// ```
    ///
    /// For a `Sync` `T`, `&T` is `Send`, and the lifetime alone refuses it:
    ///
    /// ```compile_fail
    /// # use aterm_objc::{MainThread, MainThreadBound};
    /// # use std::sync::Mutex;
    /// let mt = MainThread::new().unwrap();
    /// let bound = MainThreadBound::new(Mutex::new(7_u8), mt);
    /// // `&Mutex<u8>` IS `Send`, so `R: Send` is satisfied and proves nothing.
    /// let escaped: &Mutex<u8> = bound.get_on_main(|m| m);
    /// ```
    ///
    /// Writing the borrow into a captured slot is refused too (`E0521`):
    ///
    /// ```compile_fail
    /// # use aterm_objc::{MainThread, MainThreadBound};
    /// # use std::sync::Mutex;
    /// let mt = MainThread::new().unwrap();
    /// let bound = MainThreadBound::new(Mutex::new(7_u8), mt);
    /// let slot: Mutex<Option<&Mutex<u8>>> = Mutex::new(None);
    /// bound.get_on_main(|r| {
    ///     *slot.lock().unwrap() = Some(r);
    /// });
    /// ```
    ///
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
    /// blocks; anything else is dropped on the main thread.
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
    /// `Debug` must not block on the main queue.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MainThreadBound").finish_non_exhaustive()
    }
}
