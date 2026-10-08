// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! A thread-affinity container, [`MainThreadBound<T>`], built from the
//! [`MainThread`] witness and [`run_on_main`].
//!
//! `vendor/winit`'s `Window` must be `Send + Sync` while the `Retained`s it
//! owns (`WinitWindow`, `WindowDelegate`) are `!Send`; this container lets a
//! main-thread value live in a `Send` struct.
//!
//! # Obligations
//!
//! `Send` and `Sync` are implemented unconditionally in `T`, which is sound
//! only because:
//!
//! 1. The value is born on the main thread: [`MainThreadBound::new`] takes a
//!    [`MainThread`].
//! 2. It is read only on the main thread: [`MainThreadBound::get`],
//!    [`MainThreadBound::get_mut`] and [`MainThreadBound::into_inner`] take a
//!    witness, and [`MainThread`] is `!Send`.
//! 3. It is dropped only on the main thread: [`Drop`] reschedules through
//!    [`run_on_main`]. No type-system check backs this one;
//!    `tests/main_thread_bound_drive.rs` exercises it.
//! 4. A `T` whose address is load-bearing (e.g. [`crate::WeakSlot`]) must
//!    still not be moved, as with any move; [`crate::WeakObj`] boxes its slot.
//!
//! A `T: Send` bound would make `MainThreadBound<Retained<_>>` `!Send`,
//! defeating the purpose; `Sync` without `Send` would forbid building
//! `Window` by value.
//!
//! **`Send`:** moving the container moves `T`'s bytes, which nothing on the
//! far thread can read, write or drop (2, 3). A `!Send` value can only have
//! reached the main thread through a container like this, whose `new` needs a
//! witness; a `Send` value is fine anywhere. A witness minted with the
//! `unsafe` [`MainThread::new_unchecked`] off the main thread breaks 1-3 at
//! once, which is that constructor's obligation.
//!
//! **`Sync`:** `&T` is reachable only through [`MainThreadBound::get`]
//! (witness) or [`MainThreadBound::get_on_main`] (runs on the main thread), so
//! every `&T` exists on the one main thread; `&mut T` needs `&mut self`.
//!
//! [`MainThreadBound::get_on_main`] returns `R: Send` from a closure over a
//! higher-ranked `&T`, so a borrow of the value cannot cross back (the same
//! rule as [`run_on_main`]; no [`crate::AutoreleasePool`] is handed in either).
//!
//! # Drop
//!
//! Dropping a `Retained` may be the last release, which runs the declared
//! class's `-dealloc` and its Rust ivar destructor. The rescheduled `Drop`
//! makes that happen on the main thread even when the container is dropped
//! elsewhere (closing `S3`'s release end for values held here only; AppKit
//! still releases its own references wherever it likes).
//! `tests/main_thread_bound_drive.rs` contrasts a `NaiveBound<T>` with the
//! same `unsafe impl Send` and an ordinary drop.
//!
//! A `T` without drop glue is dropped in place and never blocks. Otherwise an
//! off-main drop blocks on the main queue, so it hangs silently if the main
//! thread runs no event loop (e.g. libtest) or is inside another
//! [`MainThreadBound::get_on_main`] closure — `dispatch_sync` semantics, as in
//! [`run_on_main`].

use std::mem::ManuallyDrop;

use crate::declare::MainThread;
use crate::dispatch::run_on_main;

/// A value that may only be touched on the process main thread, in a container
/// that may travel anywhere. See the module docs for the obligations behind the
/// `Send`/`Sync` impls.
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

// SAFETY: the value is born on the main thread (`new` takes a `MainThread`),
// read only there (`get`/`get_mut`/`into_inner` take a `!Send` witness), and
// dropped only there (`Drop` reschedules through `run_on_main`). What crosses
// threads is an inert copy of `T`'s bytes. See the module docs for why a
// `!Send` `T` cannot have originated elsewhere.
unsafe impl<T> Send for MainThreadBound<T> {}

// SAFETY: `&T` is reachable only through `get` (witness) or `get_on_main` (runs
// on the main thread), so every shared reference exists on the main thread;
// `&mut T` needs `&mut self`. That is what `T: Sync` would otherwise supply.
unsafe impl<T> Sync for MainThreadBound<T> {}

impl<T> MainThreadBound<T> {
    /// Put `inner` in the container. The witness guarantees the caller is on
    /// the thread the value belongs to (obligation 1).
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
        // Stop this type's own `Drop` from running: the value is being moved
        // out, so there is nothing left for it to reschedule.
        let mut this = ManuallyDrop::new(self);
        // SAFETY: `self` was consumed by this call and is wrapped in a
        // `ManuallyDrop`, so `this.0` is never read or dropped again.
        unsafe { ManuallyDrop::take(&mut this.0) }
    }

    /// Run `f` against the value on the main thread and bring its result back.
    ///
    /// Works from any thread (winit's `maybe_wait_on_main` uses it with a `!Send`
    /// `WindowDelegate`).
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
    /// # Blocking
    ///
    /// Off the main thread this blocks until the main thread services its
    /// queue — see [`run_on_main`].
    ///
    /// # Panics
    ///
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
            // Rebind `self` so the closure captures the whole container
            // (`Send` by this module's impl) rather than the disjoint field
            // `&mut ManuallyDrop<T>`, which is `Send` only if `T` is.
            let this = &mut *self;
            // SAFETY: this is the container's own `Drop`, so the value is
            // dropped once and never read again; `run_on_main` put us on the
            // main thread, where `new`'s witness says it belongs.
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
