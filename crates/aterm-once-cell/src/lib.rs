// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! `once_cell` — first-party replacement for the crates.io package of the same
//! name. The package is `once_cell`; the directory is `crates/aterm-once-cell`
//! (see the manifest for why the two names differ).
//!
//! Upstream is a hand-written set of single-assignment cells built on
//! `AtomicPtr`, `UnsafeCell` and a bespoke parking queue — the crate that
//! existed because `std` had no `OnceLock` or `LazyLock`. `std` has both now,
//! so this replacement is those cells expressed in terms of
//! [`std::sync::OnceLock`], [`std::sync::Mutex`] and [`core::cell::Cell`], with
//! no dependency and **no `unsafe` at all**.
//!
//! # What it retires
//!
//! ```text
//! all five cells   -1 package, -3,950 lines, -53 unsafe tokens
//!                  (no build script, no proc macro — a leaf)
//! ```
//!
//! `once_cell` is a LEAF: it has no dependencies of its own, so its dominator
//! is itself in every cell, and — this is the part worth stating out loud —
//! that was as true before the W6b flip as after it. The flip cut mac-arm's
//! parent set from three to one (`rustls` alone, after `naga` and `wgpu-core`
//! left with wgpu), which changes who is to blame but cannot change what
//! carving the package removes. This row was always buyable and simply was not
//! on anyone's list.
//!
//! Every one of its thirteen edges is third-party — `grep -rn once_cell
//! crates/ tools/ apps/ --include='*.rs'` returns ZERO — so a call-site census
//! reports nothing to rewrite and the patch table is the only lever that
//! reaches it. Same shape as `cfg-if`, `profiling` and `log` before it.
//!
//! # THIS ONE IS LIVE CODE, and that is the whole difference from `core_maths`
//!
//! `crates/aterm-core-maths` replaces a crate that is linked and never called;
//! its correctness argument is "no body of it runs". **That argument is not
//! available here and must not be borrowed.** Measured against the resolved
//! sources of all thirteen consumers:
//!
//! ```text
//! DEAD  (import sits under a `cfg` that is off in every cell)
//!   rustls 0.23.41       race::OnceBox    #[cfg(not(feature = "std"))], std ON
//!   naga 29.0.3          race::OnceBox    #[cfg(no_std)]; naga's build.rs sets
//!                                         `std` from `wgsl-in`, which is ON
//!   read-fonts 0.43.2    race::OnceBox    #[cfg(not(feature = "std"))], std ON
//!
//! LIVE  (called on the cells named)
//!   ahash 0.8.12         race::OnceBox    linux            get_or_init, set
//!   wgpu-core 29.0.3     sync::OnceCell   linux win wasm-gpu   get_or_try_init
//!   x11-dl 2.21.0        sync::OnceCell   linux            get_or_try_init
//!   xkbcommon-dl 0.4.2   sync::OnceCell   linux            get_or_init
//!   wayland-sys 0.31.11  sync::Lazy       linux            8 statics, Deref
//!   x11rb 0.13.2         sync::Lazy       linux            force, Deref
//!   wgpu-hal 29.0.3      sync::Lazy       win              Deref, Debug
//!   js-sys 0.3.85        unsync::Lazy     wasm-gpu         Deref
//!   wasm-bindgen 0.2.108 unsync::Lazy     wasm-cpu wasm-gpu   force, Deref
//!   wasm-bindgen-futures unsync::Lazy     wasm-gpu         Deref
//!
//! DEV-ONLY (outside every `cargo forge` cell, but `cargo test` builds them)
//!   criterion 0.5.1      sync::Lazy       5 statics
//!   tempfile 3.27.0      sync::OnceCell   get_or_init, get
//! ```
//!
//! So mac-arm — where `rustls` is the sole parent — is the ONLY cell on which
//! this package is dead weight. On the other four it is running code, and the
//! obligation is to be right, not merely to be present.
//!
//! ## The contract that has teeth
//!
//! `wgpu-core`'s `ResourcePool::get_or_init` (src/pool.rs) leans on
//! [`sync::OnceCell::get_or_try_init`] calling its closure **exactly once**
//! across threads:
//!
//! ```text
//! let mut strong = None;
//! let weak = entry.get_or_try_init(|| { strong = Some(constructor(..)?); Ok(weak) })?;
//! if let Some(strong) = strong { return Ok(strong); }   // "I initialised it"
//! ```
//!
//! `strong` being `Some` is how the caller decides it won the race. A
//! best-effort implementation — check, compute, `set`, re-read — would let two
//! threads both run the closure, both see `strong == Some`, and both return a
//! DIFFERENT `Arc` for one key: two bind group layouts where the pool exists to
//! guarantee one. wgpu-core's own `concurrent_creation_2_threads` test asserts
//! the counter reaches 1. [`sync::OnceCell`] therefore serialises
//! initialisation through a private [`Mutex`](std::sync::Mutex) rather than
//! racing, and `tests/behaviour.rs` asserts it directly — against a
//! deliberately racy control cell that the same test must catch.
//!
//! # Divergences, stated first because they are the risk
//!
//! Each is deliberate, each is unreachable in this graph, and each fails
//! CLOSED — as a compile error or a test failure, never as a quiet behaviour
//! change.
//!
//! 1. **Upstream is `#![no_std]`-capable; this is not.** Upstream exists so
//!    that code without `std` can have a `OnceCell`; this shim answers with
//!    `std`. Every target aterm builds has `std`, `wasm32-unknown-unknown`
//!    included. A genuine `no_std` build would fail to compile — loudly. Same
//!    trade, and same justification, as `crates/aterm-core-maths`.
//!
//! 2. **`race::OnceBox` blocks where upstream races.** Upstream's `race` module
//!    is lock-free and documents that "if several threads concurrently run
//!    `get_or_init`, more than one `f` can be called; however, all threads will
//!    return the same value". Ours calls `f` exactly once. That is a STRICTER
//!    guarantee which still satisfies the documented contract, and it also
//!    never allocates a `Box` only to drop it. The one thing upstream permits
//!    and this does not is REENTRANT initialisation: upstream's recursive
//!    `get_or_init` merely wastes an allocation, ours deadlocks — exactly as
//!    upstream's own `sync::OnceCell` does, and as its
//!    `examples/reentrant_init_deadlocks.rs` documents for that type. No
//!    consumer reenters; `ahash`'s closure is a `Box::new`.
//!
//! 3. **`with_value` is not `const`.** Upstream's `sync::OnceCell::with_value`
//!    and `unsync::OnceCell::with_value` are `const fn`, which needs
//!    `UnsafeCell` field construction; `std`'s `OnceLock` and
//!    `core::cell::OnceCell` expose no const initialiser. Ours are plain `fn`.
//!    Both are used only by this crate's own `Clone`/`From` impls; no consumer
//!    calls either, and a `static` built from one would fail to compile.
//!
//! 4. **Sizes differ.** `sync::OnceCell<T>` is `OnceLock<T>` plus a `Mutex<()>`
//!    gate where upstream is an `AtomicPtr` plus an `UnsafeCell<Option<T>>`;
//!    `sync::Lazy` holds `Mutex<Option<F>>` where upstream holds
//!    `Cell<Option<F>>` behind an `unsafe impl Sync`. Nothing in the graph
//!    takes `size_of` of either. The `Mutex` is what buys divergence 2's
//!    exactly-once guarantee and, with it, the `Sync` bound upstream spells by
//!    hand: `Mutex<Option<F>>: Sync` requires `F: Send`, which is precisely
//!    upstream's `unsafe impl<T, F: Send> Sync for Lazy<T, F>`.
//!
//! 5. **Five upstream items are ABSENT, deliberately.**
//!    `race::OnceNonZeroUsize`, `race::OnceBool`, `race::OnceRef`,
//!    `sync::OnceCell::wait` and `sync::OnceCell::get_unchecked`. The first
//!    three are `AtomicUsize`/`AtomicPtr` cells no consumer mentions; `wait`
//!    has no `OnceLock` equivalent; `get_unchecked` is `unsafe` by signature
//!    and this crate forbids `unsafe`. Using any of them is a COMPILE ERROR,
//!    which is the fail-closed direction — and
//!    `tests/consumers.rs::no_consumer_reaches_for_an_item_this_shim_omits`
//!    walks the live consumer sources so the tripwire fires at test time rather
//!    than at somebody else's build time.
//!
//! 6. **Feature flags gate nothing.** All nine upstream feature names are
//!    declared (a `[patch]` target must accept what its consumers ask for) and
//!    all nine are inert. Four of them — `critical-section`,
//!    `atomic-polyfill`, `parking_lot`, `portable-atomic` — would change
//!    upstream's implementation, so `tests/consumers.rs` proves no cell enables
//!    one.
//!
//! # The auto-trait surface is IDENTICAL, and two lines of it are hand-written
//!
//! `Send`, `Sync`, `UnwindSafe` and `RefUnwindSafe` were measured over both
//! crates side by side across sixteen instantiations. Fourteen fall out of the
//! fields and match upstream exactly. TWO DID NOT: `RefUnwindSafe` has a
//! negative impl on `UnsafeCell`, which both `core::cell::OnceCell` and `Cell`
//! contain, so [`unsync::OnceCell`] and [`unsync::Lazy`] came out
//! `!RefUnwindSafe` where upstream's — which spell the impls out by hand at
//! `once_cell-1.21.4/src/lib.rs:430` and `:729` — are not. That is a NARROWING
//! of the public surface: a consumer sending `&unsync::OnceCell<T>` through
//! `catch_unwind` compiles against upstream and not against a shim without
//! those lines. Nothing asks for it today (`wasm-bindgen` hands
//! `maybe_catch_unwind` an `AssertUnwindSafe`), and nothing here could have
//! CAUGHT it either: all three `unsync` consumers are wasm-only and no wasm
//! target is installed on the machine that runs this suite. Both impls are
//! restored verbatim, and
//! `tests/behaviour.rs::the_unwind_safety_surface_matches_upstream` pins all
//! ten bounds so the next wrapper change cannot lose one quietly.
//!
//! [`sync::Lazy`] is the one place this crate is deliberately WIDER than
//! upstream: its `Mutex<Option<F>>` is unconditionally `RefUnwindSafe`, so it
//! does not need `F: RefUnwindSafe`. Wider accepts every program upstream
//! accepts, so it cannot break a consumer, and narrowing it back would cost a
//! `PhantomData` for nobody.
//!
//! # Poisoning
//!
//! Upstream has no locks and therefore no poisoning. The two `Mutex`es here are
//! private and every acquisition ignores poisoning
//! ([`PoisonError::into_inner`](std::sync::PoisonError::into_inner)), so a
//! panic inside an initialiser leaves the cell empty and retryable — upstream's
//! behaviour — instead of turning every later call into a panic of its own.

#![deny(missing_docs)]
#![deny(rust_2018_idioms)]
// THE HEADLINE OF THIS ROW. Upstream carries 53 `unsafe` tokens; the whole
// point of expressing these cells over `OnceLock`/`Mutex`/`Cell` is that the
// replacement carries none, and this attribute is what keeps it that way.
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

    // UPSTREAM'S IMPL, KEPT VERBATIM, AND IT IS NOT DECORATION.
    //
    // `RefUnwindSafe` is an auto trait with a NEGATIVE impl on `UnsafeCell`,
    // which `core::cell::OnceCell` contains — so wrapping std's cell silently
    // makes this type `!RefUnwindSafe` where upstream's is, and a consumer that
    // sends `&OnceCell<T>` through `catch_unwind` compiles against `once_cell`
    // 1.21.4 and does NOT compile against the shim. Measured, not argued: a
    // probe asserting the same sixteen bounds over both crates exits 0 on
    // upstream and 101 here without these two impls, naming exactly
    // `unsync::OnceCell` and `unsync::Lazy`. Restored so the two surfaces
    // match; `tests/behaviour.rs::the_unwind_safety_surface_matches_upstream`
    // is the tripwire.
    //
    // `UnwindSafe` needs no impl here and MUST NOT HAVE ONE: `UnsafeCell` is
    // only `!RefUnwindSafe`, so the auto impl already gives exactly upstream's
    // `T: UnwindSafe`, and a manual copy would be a coherence error. Same for
    // both `Lazy`s and for everything in `sync` and `race`, all of which the
    // probe found already identical.
    impl<T: RefUnwindSafe + UnwindSafe> RefUnwindSafe for OnceCell<T> {}

    impl<T> Default for OnceCell<T> {
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
        fn from(value: T) -> Self {
            OnceCell::with_value(value)
        }
    }

    impl<T> OnceCell<T> {
        /// Creates a new empty cell.
        pub const fn new() -> OnceCell<T> {
            OnceCell(CoreOnceCell::new())
        }

        /// Creates a new initialised cell.
        ///
        /// NOT `const`, where upstream's is — see divergence 3 in the crate
        /// documentation.
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
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> T,
        {
            self.0.get_or_init(f)
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        ///
        /// If `f` fails, the cell stays empty and the error is returned.
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<T, E>,
        {
            if let Some(val) = self.get() {
                return Ok(val);
            }
            let val = f()?;
            // Single-threaded, and the cell was empty one statement ago: `f`
            // cannot have filled it without a `&self` this function did not
            // hand out.
            let _ = self.0.set(val);
            Ok(self
                .get()
                .unwrap_or_else(|| unreachable!("just initialised")))
        }

        /// Takes the value out, leaving the cell empty.
        pub fn take(&mut self) -> Option<T> {
            self.0.take()
        }

        /// Consumes the cell, returning the wrapped value.
        pub fn into_inner(self) -> Option<T> {
            self.0.into_inner()
        }
    }

    /// A value which is initialised on first access.
    ///
    /// `const fn new` carries NO bound on `F`, matching upstream. That is
    /// load-bearing rather than cosmetic: `wasm-bindgen 0.2.108` builds its own
    /// `LazyCell` inside an `impl<T, F>` with no `F: FnOnce() -> T` bound, so a
    /// thin wrapper over [`core::cell::LazyCell`] — whose `new` IS bounded —
    /// would fail to compile on both wasm cells.
    pub struct Lazy<T, F = fn() -> T> {
        cell: OnceCell<T>,
        init: Cell<Option<F>>,
    }

    // Upstream's impl, for the reason given above `unsync::OnceCell`: `Cell`
    // contains an `UnsafeCell` too, so without this line the type is
    // `!RefUnwindSafe` where upstream's is not.
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
        pub fn get(this: &Lazy<T, F>) -> Option<&T> {
            this.cell.get()
        }

        /// Gets a mutable reference to the result, or `None` if not yet forced.
        pub fn get_mut(this: &mut Lazy<T, F>) -> Option<&mut T> {
            this.cell.get_mut()
        }
    }

    impl<T, F: FnOnce() -> T> Deref for Lazy<T, F> {
        type Target = T;
        fn deref(&self) -> &T {
            Lazy::force(self)
        }
    }

    impl<T, F: FnOnce() -> T> DerefMut for Lazy<T, F> {
        fn deref_mut(&mut self) -> &mut T {
            Lazy::force_mut(self)
        }
    }

    impl<T: Default> Default for Lazy<T> {
        /// Creates a new lazy value using `Default` as the initialising function.
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
        /// `OnceLock::get_or_init` already does this for the infallible case,
        /// but there is no stable fallible form, and `wgpu-core` relies on the
        /// fallible one being exactly-once (see the crate documentation). One
        /// gate serves both entry points so the two cannot race each other.
        gate: Mutex<()>,
    }

    impl<T> Default for OnceCell<T> {
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
            // POISONING IS IGNORED ON PURPOSE. A panic inside somebody else's
            // `f` must leave this cell empty and retryable — upstream's
            // behaviour, which has no locks to poison — not convert every
            // later call into a panic of our own.
            let _guard = self.gate.lock().unwrap_or_else(PoisonError::into_inner);
            // Re-check under the gate: another thread may have initialised the
            // cell between the fast path and the lock.
            if let Some(val) = self.get() {
                return Ok(val);
            }
            let val = f()?;
            // The gate is held and the cell was empty under it, so the only way
            // this can fail is a `set` that bypassed the gate — and there is
            // none: `set`/`try_insert` both route through `get_or_init`.
            let _ = self.inner.set(val);
            Ok(self
                .get()
                .unwrap_or_else(|| unreachable!("just initialised under the gate")))
        }

        /// Takes the value out, leaving the cell empty.
        pub fn take(&mut self) -> Option<T> {
            self.inner.take()
        }

        /// Consumes the cell, returning the wrapped value.
        pub fn into_inner(self) -> Option<T> {
            self.inner.into_inner()
        }
    }

    /// A thread-safe value which is initialised on first access.
    ///
    /// `Sync` is DERIVED rather than asserted: `Mutex<Option<F>>` is `Sync`
    /// exactly when `F: Send`, and `OnceCell<T>` is `Sync` exactly when
    /// `T: Send + Sync` — together, precisely the bound upstream spells out by
    /// hand as `unsafe impl<T, F: Send> Sync for Lazy<T, F> where OnceCell<T>:
    /// Sync`. Holding the initialiser in a `Mutex` instead of a `Cell` is what
    /// lets this crate forbid `unsafe` and still keep `new` unbounded in `F`.
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
        pub fn get(this: &Lazy<T, F>) -> Option<&T> {
            this.cell.get()
        }

        /// Gets a mutable reference to the result, or `None` if not yet forced.
        pub fn get_mut(this: &mut Lazy<T, F>) -> Option<&mut T> {
            this.cell.get_mut()
        }
    }

    impl<T, F: FnOnce() -> T> Deref for Lazy<T, F> {
        type Target = T;
        fn deref(&self) -> &T {
            Lazy::force(self)
        }
    }

    impl<T, F: FnOnce() -> T> DerefMut for Lazy<T, F> {
        fn deref_mut(&mut self) -> &mut T {
            Lazy::force_mut(self)
        }
    }

    impl<T: Default> Default for Lazy<T> {
        /// Creates a new lazy value using `Default` as the initialising function.
        fn default() -> Lazy<T> {
            Lazy::new(T::default)
        }
    }
}

/// Thread-safe cells for `no_std` code, in upstream's naming.
///
/// Upstream's `race` module is lock-free — "racy" — and its cells may run an
/// initialiser more than once. This replacement layers [`OnceBox`] over
/// [`sync::OnceCell`] instead, which is a stricter guarantee that still meets
/// upstream's documented contract; see divergence 2 in the crate documentation.
///
/// `OnceNonZeroUsize`, `OnceBool` and `OnceRef` are NOT provided — see
/// divergence 5.
pub mod race {
    use core::fmt;
    use core::ptr;

    /// A thread-safe cell which can be written to only once, holding a `Box`.
    pub struct OnceBox<T>(crate::sync::OnceCell<Box<T>>);

    impl<T> fmt::Debug for OnceBox<T> {
        fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
            // Upstream prints the raw `AtomicPtr` it stores. The address of the
            // boxed value is the same fact by a different route, and `null` is
            // what upstream prints for an empty cell.
            let ptr = self.get().map_or(ptr::null(), |v| v as *const T);
            write!(f, "OnceBox({ptr:?})")
        }
    }

    impl<T> Default for OnceBox<T> {
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
        pub const fn new() -> Self {
            OnceBox(crate::sync::OnceCell::new())
        }

        /// Creates a new cell with the given value.
        pub fn with_value(value: Box<T>) -> Self {
            OnceBox(crate::sync::OnceCell::with_value(value))
        }

        /// Gets a reference to the underlying value, or `None` if empty.
        pub fn get(&self) -> Option<&T> {
            self.0.get().map(|b| &**b)
        }

        /// Sets the contents of this cell.
        ///
        /// Returns `Ok(())` if the cell was empty and `Err(value)` if it was
        /// full.
        pub fn set(&self, value: Box<T>) -> Result<(), Box<T>> {
            self.0.set(value)
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        ///
        /// Unlike upstream, `f` runs at most once — a stricter guarantee than
        /// the "more than one `f` can be called" upstream documents, and one
        /// its "all threads return the same value" promise still holds under.
        pub fn get_or_init<F>(&self, f: F) -> &T
        where
            F: FnOnce() -> Box<T>,
        {
            self.0.get_or_init(f)
        }

        /// Gets the contents, initialising them with `f` if the cell was empty.
        ///
        /// If `f` fails, the cell stays empty and the error is returned.
        pub fn get_or_try_init<F, E>(&self, f: F) -> Result<&T, E>
        where
            F: FnOnce() -> Result<Box<T>, E>,
        {
            self.0.get_or_try_init(f).map(|b| &**b)
        }
    }
}
