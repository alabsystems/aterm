// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `once_cell` — first-party replacement for the crates.io package of the same
//! name, backed by [`std::sync::OnceLock`], [`std::sync::Mutex`], and
//! [`core::cell::Cell`] with no dependencies and no `unsafe`.
//!
//! # Exactly-once initialization contract
//!
//! `wgpu-core`'s `ResourcePool::get_or_init` relies on
//! [`sync::OnceCell::get_or_try_init`] calling its closure **exactly once**
//! across threads:
//!
//! ```text
//! let mut strong = None;
//! let weak = entry.get_or_try_init(|| { strong = Some(constructor(..)?); Ok(weak) })?;
//! if let Some(strong) = strong { return Ok(strong); }   // "I initialised it"
//! ```
//!
//! A check-compute-set implementation would let two threads both run the
//! closure and return two different `Arc`s for one key. [`sync::OnceCell`]
//! therefore serialises initialisation through a private
//! [`Mutex`](std::sync::Mutex); `tests/behaviour.rs` asserts this with a racy
//! control cell.
//!
//! # Divergences
//!
//! 1. **Not `no_std`-capable.** Every target aterm builds has `std`,
//!    `wasm32-unknown-unknown` included; a `no_std` build fails to compile.
//!
//! 2. **`race::OnceBox` blocks where upstream races.** Upstream documents that
//!    concurrent `get_or_init` may call more than one `f` but returns one value
//!    to all; ours calls `f` exactly once, which meets that contract. Reentrant
//!    initialisation deadlocks here (as in upstream's `sync::OnceCell`) where
//!    upstream's `race` wastes an allocation.
//!
//! 3. **`with_value` is not `const`.** `std` exposes no const initialiser for
//!    a filled `OnceLock` or `core::cell::OnceCell`. Only this crate's own
//!    `Clone`/`From` impls call it.
//!
//! 4. **Sizes differ.** `sync::OnceCell<T>` adds a `Mutex<()>` gate;
//!    `sync::Lazy` holds `Mutex<Option<F>>` where upstream holds a `Cell` behind
//!    `unsafe impl Sync`. The `Mutex` provides divergence 2's exactly-once
//!    guarantee and derives upstream's `Sync` bound (`Mutex<Option<F>>: Sync`
//!    iff `F: Send`).
//!
//! 5. **Five upstream items are absent.** `race::OnceNonZeroUsize`,
//!    `race::OnceBool`, `race::OnceRef`, `sync::OnceCell::wait` (no `OnceLock`
//!    equivalent) and `sync::OnceCell::get_unchecked` (`unsafe` by signature).
//!
//! 6. **Feature flags gate nothing.** The five upstream names that change
//!    nothing here are declared so `[patch.crates-io]` resolves them, and are
//!    inert. `critical-section`, `atomic-polyfill`, `parking_lot` and
//!    `portable-atomic` are omitted so any consumer requesting them fails to
//!    resolve.
//!
//! # Auto traits match upstream
//!
//! `UnsafeCell` (inside both `core::cell::OnceCell` and `Cell`) is
//! `!RefUnwindSafe`, so [`unsync::OnceCell`] and [`unsync::Lazy`] carry
//! explicit `RefUnwindSafe` impls matching upstream (`tests/behaviour.rs` pins
//! the auto-trait surface). [`sync::Lazy`] is wider (`Mutex<Option<F>>` is
//! `RefUnwindSafe` without `F: RefUnwindSafe`).
//!
//! # Poisoning
//!
//! Every acquisition of the two private `Mutex`es ignores poisoning
//! ([`PoisonError::into_inner`](std::sync::PoisonError::into_inner)), so a
//! panic inside an initialiser leaves the cell empty and retryable.

#![warn(missing_docs)]
#![warn(rust_2018_idioms)]
#![forbid(unsafe_code)]

/// Single-threaded, non-blocking cells.
///
/// The `Sync`-free half of the crate: [`OnceCell`](unsync::OnceCell) over
/// [`core::cell::OnceCell`] and [`Lazy`](unsync::Lazy) over that plus a
/// [`Cell`](core::cell::Cell), which is upstream's own shape.
pub mod unsync {
    use core::cell::{Cell, OnceCell as CoreOnceCell};
    use core::fmt;
    use core::ops::{Deref, DerefMut};
    use core::panic::{RefUnwindSafe, UnwindSafe};

    /// A cell which can be written to only once, without thread synchronisation.
    pub struct OnceCell<T>(CoreOnceCell<T>);

    // Upstream's impl, verbatim. `core::cell::OnceCell` contains an
    // `UnsafeCell`, which is `!RefUnwindSafe`, so without this the type is
    // narrower than upstream's (see "Auto traits" in the crate docs).
    // `UnwindSafe` already matches through the auto impl; a manual one would be
    // a coherence error.
    impl<T: RefUnwindSafe + UnwindSafe> RefUnwindSafe for OnceCell<T> {}

    impl<T> Default for OnceCell<T> {
        #[inline]
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T: fmt::Debug> fmt::Debug for OnceCell<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.get() {
                Some(v) => f.debug_tuple("OnceCell").field(v).finish(),
                None => f.write_str("OnceCell(Uninit)"),
            }
        }
    }

    impl<T: Clone> Clone for OnceCell<T> {
        fn clone(&self) -> OnceCell<T> {
            match self.get() {
                Some(value) => OnceCell::with_value(value.clone()),
                None => OnceCell::new(),
            }
        }

        fn clone_from(&mut self, source: &Self) {
            match (self.get_mut(), source.get()) {
                (Some(this), Some(source)) => this.clone_from(source),
                _ => *self = source.clone(),
            }
        }
    }

    impl<T: PartialEq> PartialEq for OnceCell<T> {
        fn eq(&self, other: &Self) -> bool {
            self.get() == other.get()
        }
    }

    impl<T: Eq> Eq for OnceCell<T> {}

    impl<T> From<T> for OnceCell<T> {
        #[inline]
        fn from(value: T) -> Self {
            OnceCell::with_value(value)
        }
    }

    impl<T> OnceCell<T> {
        /// Creates a new empty cell.
        #[inline]
        pub const fn new() -> OnceCell<T> {
            OnceCell(CoreOnceCell::new())
        }

        /// Creates a new initialised cell.
        ///
        /// NOT `const`, where upstream's is — see divergence 3 in the crate
        /// documentation.
        #[inline]
        pub fn with_value(value: T) -> OnceCell<T> {
            let cell = CoreOnceCell::new();
            // The cell was just created empty, so this cannot fail.
            let _ = cell.set(value);
            OnceCell(cell)
        }

        /// Gets a reference to the underlying value, or `None` if empty.
        #[inline]
        pub fn get(&self) -> Option<&T> {
            self.0.get()
        }

        /// Gets a mutable reference to the underlying value, or `None` if empty.
        #[inline]
        pub fn get_mut(&mut self) -> Option<&mut T> {
            self.0.get_mut()
        }

        /// Sets the contents of this cell.
        ///
        /// Returns `Ok(())` if the cell was empty and `Err(value)` if it was full.
        #[inline]
        pub fn set(&self, value: T) -> Result<(), T> {
            match self.try_insert(value) {
                Ok(_) => Ok(()),
                Err((_, value)) => Err(value),
            }
        }

        /// Like [`set`](Self::set), but also returns a reference to the
        /// contents — the new one on success, the existing one on failure.
        pub fn try_insert(&self, value: T) -> Result<&T, (&T, T)> {
            let mut value = Some(value);
            let res = self.get_or_init(|| {
                value
                    .take()
                    .unwrap_or_else(|| unreachable!("the initialiser runs at most once"))
            });
            match value {
                None => Ok(res),
                Some(value) => Err((res, value)),
            }
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        #[inline]
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> T,
        {
            self.0.get_or_init(f)
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        ///
        /// If `f` fails, the cell stays empty and the error is returned.
        ///
        /// # Panics
        ///
        /// If `f` panics, the panic propagates and the cell stays empty. It is
        /// an error to initialise the cell reentrantly from `f`; doing so
        /// panics with "reentrant init", as upstream does and as
        /// [`get_or_init`](Self::get_or_init) does.
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<T, E>,
        {
            if let Some(val) = self.get() {
                return Ok(val);
            }
            let val = f()?;
            // `f` may capture `&self` and fill the cell reentrantly; assert
            // empty before storing so `f`'s return value is never silently
            // dropped in favour of an earlier reentrant write.
            assert!(self.0.set(val).is_ok(), "reentrant init");
            Ok(self
                .get()
                .unwrap_or_else(|| unreachable!("just initialised")))
        }

        /// Takes the value out, leaving the cell empty.
        #[inline]
        pub fn take(&mut self) -> Option<T> {
            self.0.take()
        }

        /// Consumes the cell, returning the wrapped value.
        #[inline]
        pub fn into_inner(self) -> Option<T> {
            self.0.into_inner()
        }
    }

    /// A value which is initialised on first access.
    ///
    /// `const fn new` carries no bound on `F`, matching upstream:
    /// `wasm-bindgen 0.2.108` calls it inside an unbounded `impl<T, F>`, so a
    /// wrapper over [`core::cell::LazyCell`] (whose `new` is bounded) would not
    /// compile on wasm-gpu.
    pub struct Lazy<T, F = fn() -> T> {
        cell: OnceCell<T>,
        init: Cell<Option<F>>,
    }

    // Upstream's impl, for the reason given above `unsync::OnceCell`.
    impl<T, F: RefUnwindSafe> RefUnwindSafe for Lazy<T, F> where OnceCell<T>: RefUnwindSafe {}

    impl<T: fmt::Debug, F> fmt::Debug for Lazy<T, F> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Lazy")
                .field("cell", &self.cell)
                .field("init", &"..")
                .finish()
        }
    }

    impl<T, F> Lazy<T, F> {
        /// Creates a new lazy value with the given initialising function.
        #[inline]
        pub const fn new(init: F) -> Lazy<T, F> {
            Lazy {
                cell: OnceCell::new(),
                init: Cell::new(Some(init)),
            }
        }

        /// Consumes this `Lazy`, returning the stored value.
        ///
        /// Returns `Ok(value)` if it was initialised and `Err(f)` otherwise.
        pub fn into_value(this: Lazy<T, F>) -> Result<T, F> {
            let cell = this.cell;
            let init = this.init;
            cell.into_inner().ok_or_else(|| {
                init.take()
                    .unwrap_or_else(|| panic!("Lazy instance has previously been poisoned"))
            })
        }
    }

    impl<T, F: FnOnce() -> T> Lazy<T, F> {
        /// Forces evaluation and returns a reference to the result.
        ///
        /// Equivalent to the [`Deref`] impl, but explicit.
        pub fn force(this: &Lazy<T, F>) -> &T {
            this.cell.get_or_init(|| match this.init.take() {
                Some(f) => f(),
                None => panic!("Lazy instance has previously been poisoned"),
            })
        }

        /// Forces evaluation and returns a mutable reference to the result.
        pub fn force_mut(this: &mut Lazy<T, F>) -> &mut T {
            if this.cell.get_mut().is_none() {
                let value = match this.init.get_mut().take() {
                    Some(f) => f(),
                    None => panic!("Lazy instance has previously been poisoned"),
                };
                let _ = this.cell.set(value);
            }
            this.cell
                .get_mut()
                .unwrap_or_else(|| unreachable!("just initialised"))
        }

        /// Gets the reference to the result, or `None` if not yet forced.
        #[inline]
        pub fn get(this: &Lazy<T, F>) -> Option<&T> {
            this.cell.get()
        }

        /// Gets a mutable reference to the result, or `None` if not yet forced.
        #[inline]
        pub fn get_mut(this: &mut Lazy<T, F>) -> Option<&mut T> {
            this.cell.get_mut()
        }
    }

    impl<T, F: FnOnce() -> T> Deref for Lazy<T, F> {
        type Target = T;
        #[inline]
        fn deref(&self) -> &T {
            Lazy::force(self)
        }
    }

    impl<T, F: FnOnce() -> T> DerefMut for Lazy<T, F> {
        #[inline]
        fn deref_mut(&mut self) -> &mut T {
            Lazy::force_mut(self)
        }
    }

    impl<T: Default> Default for Lazy<T> {
        /// Creates a new lazy value using `Default` as the initialising function.
        #[inline]
        fn default() -> Lazy<T> {
            Lazy::new(T::default)
        }
    }
}

/// Thread-safe, blocking cells.
///
/// [`OnceCell`](sync::OnceCell) is [`std::sync::OnceLock`] plus a private
/// [`Mutex`](std::sync::Mutex) that serialises initialisation, which is what
/// gives the fallible [`get_or_try_init`](sync::OnceCell::get_or_try_init)
/// `std` has no stable equivalent for. [`Lazy`](sync::Lazy) is that cell plus a
/// `Mutex<Option<F>>` holding the initialiser.
pub mod sync {
    use core::convert::Infallible;
    use core::fmt;
    use core::ops::{Deref, DerefMut};
    use std::sync::{Mutex, OnceLock, PoisonError};

    /// A thread-safe cell which can be written to only once.
    pub struct OnceCell<T> {
        inner: OnceLock<T>,
        /// Held across a user initialiser so that exactly one runs.
        ///
        /// `OnceLock` has no stable fallible `get_or_init`, and `wgpu-core`
        /// needs the fallible one exactly-once. One gate serves both entry
        /// points so they cannot race each other.
        gate: Mutex<()>,
    }

    impl<T> Default for OnceCell<T> {
        #[inline]
        fn default() -> OnceCell<T> {
            OnceCell::new()
        }
    }

    impl<T: fmt::Debug> fmt::Debug for OnceCell<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            match self.get() {
                Some(v) => f.debug_tuple("OnceCell").field(v).finish(),
                None => f.write_str("OnceCell(Uninit)"),
            }
        }
    }

    impl<T: Clone> Clone for OnceCell<T> {
        fn clone(&self) -> OnceCell<T> {
            match self.get() {
                Some(value) => Self::with_value(value.clone()),
                None => Self::new(),
            }
        }

        fn clone_from(&mut self, source: &Self) {
            match (self.get_mut(), source.get()) {
                (Some(this), Some(source)) => this.clone_from(source),
                _ => *self = source.clone(),
            }
        }
    }

    impl<T> From<T> for OnceCell<T> {
        #[inline]
        fn from(value: T) -> Self {
            Self::with_value(value)
        }
    }

    impl<T: PartialEq> PartialEq for OnceCell<T> {
        fn eq(&self, other: &OnceCell<T>) -> bool {
            self.get() == other.get()
        }
    }

    impl<T: Eq> Eq for OnceCell<T> {}

    impl<T> OnceCell<T> {
        /// Creates a new empty cell.
        #[inline]
        pub const fn new() -> OnceCell<T> {
            OnceCell {
                inner: OnceLock::new(),
                gate: Mutex::new(()),
            }
        }

        /// Creates a new initialised cell.
        ///
        /// NOT `const`, where upstream's is — see divergence 3 in the crate
        /// documentation.
        #[inline]
        pub fn with_value(value: T) -> OnceCell<T> {
            let cell = OnceCell::new();
            // Just created, so nothing can have filled it.
            let _ = cell.inner.set(value);
            cell
        }

        /// Gets the reference to the underlying value.
        ///
        /// Returns `None` if the cell is empty or being initialised. Never
        /// blocks.
        #[inline]
        pub fn get(&self) -> Option<&T> {
            self.inner.get()
        }

        /// Gets a mutable reference to the underlying value, or `None` if empty.
        #[inline]
        pub fn get_mut(&mut self) -> Option<&mut T> {
            self.inner.get_mut()
        }

        /// Sets the contents of this cell.
        ///
        /// Returns `Ok(())` if the cell was empty and `Err(value)` if it was
        /// full. Like upstream, this runs through the same initialisation path
        /// as [`get_or_init`](Self::get_or_init), so it blocks while another
        /// thread is initialising rather than racing past it.
        #[inline]
        pub fn set(&self, value: T) -> Result<(), T> {
            match self.try_insert(value) {
                Ok(_) => Ok(()),
                Err((_, value)) => Err(value),
            }
        }

        /// Like [`set`](Self::set), but also returns a reference to the
        /// contents — the new one on success, the existing one on failure.
        pub fn try_insert(&self, value: T) -> Result<&T, (&T, T)> {
            let mut value = Some(value);
            let res = self.get_or_init(|| {
                value
                    .take()
                    .unwrap_or_else(|| unreachable!("the initialiser runs at most once"))
            });
            match value {
                None => Ok(res),
                Some(value) => Err((res, value)),
            }
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        ///
        /// `f` is called at most once per cell, even under contention: a second
        /// thread blocks until the first finishes and then observes its value.
        #[inline]
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> T,
        {
            match self.get_or_try_init(|| Ok::<T, Infallible>(f())) {
                Ok(val) => val,
                Err(never) => match never {},
            }
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        ///
        /// If `f` fails, the cell stays empty, the error is returned, and a
        /// later call may try again. `f` is called at most once at a time; on
        /// success it is called exactly once for the life of the cell.
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<T, E>,
        {
            // Fast path: fully initialised, no lock taken. `OnceLock::get`
            // carries the acquire ordering that makes the value visible.
            if let Some(val) = self.get() {
                return Ok(val);
            }
            // Poisoning is ignored: a panic in another caller's `f` leaves the
            // cell empty and retryable, as upstream does.
            let _guard = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
            // Re-check under the gate: another thread may have initialised the
            // cell between the fast path and the lock.
            if let Some(val) = self.get() {
                return Ok(val);
            }
            let val = f()?;
            // Cannot fail: the gate is held, the cell was empty under it, and
            // every writer (`set`, `try_insert`) goes through the gate. That
            // includes a reentrant one called from `f`, which blocks on the
            // gate this thread holds (upstream's sync cell deadlocks the same
            // way; divergence 2 says so for `race`).
            let _ = self.inner.set(val);
            Ok(self
                .get()
                .unwrap_or_else(|| unreachable!("just initialised under the gate")))
        }

        /// Takes the value out, leaving the cell empty.
        #[inline]
        pub fn take(&mut self) -> Option<T> {
            self.inner.take()
        }

        /// Consumes the cell, returning the wrapped value.
        #[inline]
        pub fn into_inner(self) -> Option<T> {
            self.inner.into_inner()
        }
    }

    /// A thread-safe value which is initialised on first access.
    ///
    /// `Sync` is derived, not asserted: `Mutex<Option<F>>` is `Sync` iff
    /// `F: Send` and `OnceCell<T>` iff `T: Send + Sync`, which is the bound
    /// upstream writes as `unsafe impl<T, F: Send> Sync`.
    pub struct Lazy<T, F = fn() -> T> {
        cell: OnceCell<T>,
        init: Mutex<Option<F>>,
    }

    impl<T: fmt::Debug, F> fmt::Debug for Lazy<T, F> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            f.debug_struct("Lazy")
                .field("cell", &self.cell)
                .field("init", &"..")
                .finish()
        }
    }

    impl<T, F> Lazy<T, F> {
        /// Creates a new lazy value with the given initialising function.
        #[inline]
        pub const fn new(f: F) -> Lazy<T, F> {
            Lazy {
                cell: OnceCell::new(),
                init: Mutex::new(Some(f)),
            }
        }

        /// Consumes this `Lazy`, returning the stored value.
        ///
        /// Returns `Ok(value)` if it was initialised and `Err(f)` otherwise.
        pub fn into_value(this: Lazy<T, F>) -> Result<T, F> {
            let cell = this.cell;
            let init = this.init;
            cell.into_inner().ok_or_else(|| {
                init.into_inner()
                    .unwrap_or_else(PoisonError::into_inner)
                    .unwrap_or_else(|| panic!("Lazy instance has previously been poisoned"))
            })
        }
    }

    impl<T, F: FnOnce() -> T> Lazy<T, F> {
        /// Forces evaluation and returns a reference to the result.
        ///
        /// Equivalent to the [`Deref`] impl, but explicit.
        pub fn force(this: &Lazy<T, F>) -> &T {
            this.cell.get_or_init(|| {
                let f = this
                    .init
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                match f {
                    Some(f) => f(),
                    None => panic!("Lazy instance has previously been poisoned"),
                }
            })
        }

        /// Forces evaluation and returns a mutable reference to the result.
        pub fn force_mut(this: &mut Lazy<T, F>) -> &mut T {
            if this.cell.get_mut().is_none() {
                let f = this
                    .init
                    .get_mut()
                    .unwrap_or_else(PoisonError::into_inner)
                    .take();
                let value = match f {
                    Some(f) => f(),
                    None => panic!("Lazy instance has previously been poisoned"),
                };
                let _ = this.cell.set(value);
            }
            this.cell
                .get_mut()
                .unwrap_or_else(|| unreachable!("just initialised"))
        }

        /// Gets the reference to the result, or `None` if not yet forced.
        #[inline]
        pub fn get(this: &Lazy<T, F>) -> Option<&T> {
            this.cell.get()
        }

        /// Gets a mutable reference to the result, or `None` if not yet forced.
        #[inline]
        pub fn get_mut(this: &mut Lazy<T, F>) -> Option<&mut T> {
            this.cell.get_mut()
        }
    }

    impl<T, F: FnOnce() -> T> Deref for Lazy<T, F> {
        type Target = T;
        #[inline]
        fn deref(&self) -> &T {
            Lazy::force(self)
        }
    }

    impl<T, F: FnOnce() -> T> DerefMut for Lazy<T, F> {
        #[inline]
        fn deref_mut(&mut self) -> &mut T {
            Lazy::force_mut(self)
        }
    }

    impl<T: Default> Default for Lazy<T> {
        /// Creates a new lazy value using `Default` as the initialising function.
        #[inline]
        fn default() -> Lazy<T> {
            Lazy::new(T::default)
        }
    }
}

/// Thread-safe cells for `no_std` code, in upstream's naming.
///
/// [`OnceBox`](race::OnceBox) is layered over [`sync::OnceCell`], so it runs its initialiser
/// exactly once where upstream's lock-free cell may run it more than once
/// (divergence 2). `OnceNonZeroUsize`, `OnceBool` and `OnceRef` are absent
/// (divergence 5).
pub mod race {
    use core::fmt;
    use core::ptr;

    /// A thread-safe cell which can be written to only once, holding a `Box`.
    pub struct OnceBox<T>(crate::sync::OnceCell<Box<T>>);

    impl<T> fmt::Debug for OnceBox<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            // Upstream prints its raw `AtomicPtr`: the box's address, or null.
            let ptr = self.get().map_or(ptr::null(), |v| v as *const T);
            write!(f, "OnceBox({ptr:?})")
        }
    }

    impl<T> Default for OnceBox<T> {
        #[inline]
        fn default() -> Self {
            Self::new()
        }
    }

    impl<T: Clone> Clone for OnceBox<T> {
        fn clone(&self) -> Self {
            match self.get() {
                Some(value) => OnceBox::with_value(Box::new(value.clone())),
                None => OnceBox::new(),
            }
        }
    }

    impl<T> OnceBox<T> {
        /// Creates a new empty cell.
        #[inline]
        pub const fn new() -> Self {
            OnceBox(crate::sync::OnceCell::new())
        }

        /// Creates a new cell with the given value.
        #[inline]
        pub fn with_value(value: Box<T>) -> Self {
            OnceBox(crate::sync::OnceCell::with_value(value))
        }

        /// Gets a reference to the underlying value, or `None` if empty.
        #[inline]
        pub fn get(&self) -> Option<&T> {
            self.0.get().map(|b| &**b)
        }

        /// Sets the contents of this cell.
        ///
        /// Returns `Ok(())` if the cell was empty and `Err(value)` if it was
        /// full.
        #[inline]
        pub fn set(&self, value: Box<T>) -> Result<(), Box<T>> {
            self.0.set(value)
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        ///
        /// `f` runs at most once, where upstream allows several (divergence 2).
        #[inline]
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> Box<T>,
        {
            self.0.get_or_init(f)
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        ///
        /// If `f` fails, the cell stays empty and the error is returned.
        #[inline]
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<Box<T>, E>,
        {
            self.0.get_or_try_init(f).map(|b| &**b)
        }
    }
}
