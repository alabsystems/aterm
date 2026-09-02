// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Weak references — the fourth capability `metal/ffi.rs` never needed.
//!
//! Metal's offscreen surface owns everything it touches and owns it in a tree,
//! so nothing in that file ever had a cycle to break. AppKit is a graph: a
//! window retains its content view, and the view wants to name its window back.
//! `vendor/winit/src/platform_impl/macos/view.rs:169` is that exact edge —
//!
//! ```text
//! // Weak reference because the window keeps a strong reference to the view
//! _ns_window: WeakId<NSWindow>,
//! ```
//!
//! — and it is the single largest remaining `objc2` dependency in the fork.
//! `objc_initWeak`, `objc_loadWeak`, `objc_storeWeak`, `objc_destroyWeak` and
//! `objc_copyWeak` appeared NOWHERE in this crate before this module, so the
//! edge could not be expressed at all. That is a missing RUNTIME CAPABILITY,
//! not a port, and it is built here the way [`crate::declare`] built class-pair
//! creation and [`crate::block`] built the block ABI: the real ABI, one
//! ownership rule per function, and a SAFETY comment naming the runtime
//! invariant each relies on.
//!
//! # The one thing that makes this different from every other handle here
//!
//! [`crate::Obj`], [`crate::Retained`] and [`crate::RcBlock`] are all VALUES:
//! the word they hold means the same thing wherever that word is stored, so
//! Rust's move — a `memcpy` and a `mem::forget` of the source — is exactly
//! right for them.
//!
//! **A weak reference is not a value. It is a REGISTRATION, and the thing
//! registered is the ADDRESS OF THE SLOT.** `objc_initWeak(location, obj)`
//! records `location` in the runtime's side table for `obj`, and when `obj`
//! deallocates the runtime walks that table and writes `nil` THROUGH EVERY
//! ADDRESS IT HOLDS. So the storage location is load-bearing in a way no other
//! handle in this crate's storage location is:
//!
//! * `memcpy` the eight bytes to a new address and the runtime still holds the
//!   OLD one. The copy is never zeroed, and the moment the object deallocates
//!   it holds a dangling pointer that still reads non-nil — a use-after-free
//!   with no diagnostic anywhere.
//! * Worse in the other direction: the old address is still registered. If it
//!   was a stack slot the frame will be reused, and the runtime will write
//!   `nil` into whatever occupies those eight bytes at dealloc time. A weak
//!   reference left registered at a dead address is a WILD WRITE the runtime
//!   performs on the program's behalf, at a time nothing in the program
//!   chooses.
//!
//! Rust has no move constructor, so a type with the slot INLINE cannot be made
//! sound — its move is a `memcpy` and no code of ours runs. There are exactly
//! two answers, and this module ships both:
//!
//! 1. **Make the slot's address stable, so the move never touches it.**
//!    [`WeakObj`] and [`Weak<T>`] own a `Box<`[`WeakSlot`]`>`; a move moves the
//!    BOX POINTER and the registered address does not change. This is the safe
//!    API and the one every call site should use. (It is also objc2's answer,
//!    reached independently and for the same reason.)
//! 2. **When the storage genuinely must move, go through the runtime.**
//!    [`WeakSlot::move_from`] (`objc_moveWeak`) re-registers the new address
//!    and unregisters the old one; [`WeakSlot::copy_from`] (`objc_copyWeak`)
//!    registers a second address at the same object. Neither is a `memcpy` and
//!    both are `unsafe`, because their whole contract is about addresses.
//!
//! [`WeakSlot`] has NO safe constructor, and that is the enforcement: safe code
//! cannot get a bare slot to move, and every `unsafe` constructor states the
//! address contract in its `# Safety` block.
//!
//! `tests/weak.rs` PROVES the hazard rather than asserting it — it performs the
//! naive `memcpy` on a real object, releases it, and reads the two slots: the
//! registered one is `nil`, the moved one still holds the dead object's
//! address. `crates/aterm-gui/examples/objc_window_drive.rs` runs the same
//! experiment against a live AppKit `NSView` and then shows the boxed form
//! surviving four real Rust moves.
//!
//! # `load` is +1, and there is NO safe +0 form — WITHDRAWN, and measured twice
//!
//! `objc_loadWeakRetained` returns a **+1** reference, which is the only kind
//! that is safe to hold: a weak reference can go `nil` between the load and the
//! next instruction if another thread releases the last strong one, and the
//! retain inside the runtime's lock is what makes the answer stable. That is
//! [`WeakObj::load`], and it is now the only load this module exports.
//!
//! `objc_loadWeak` returns the same object **autoreleased**, at +0. This module
//! shipped a safe wrapper for it —
//!
//! ```text
//! pub fn load_borrowed<'p>(&self, _pool: &'p AutoreleasePool) -> Option<&'p AutoreleasedId>
//! ```
//!
//! — and advertised it as the form objc2 leaves a `TODO` in `rc/weak_id.rs`.
//! **IT WAS UNSOUND, and it was also SLOWER than the load it was meant to beat.
//! Both halves were measured; it is withdrawn on either one alone.**
//!
//! ## Why the signature cannot be written
//!
//! `objc_loadWeak` hands its +1 to the **INNERMOST pool on the thread**, which
//! is a fact about the runtime stack at the instant of the call. `'p` is a fact
//! about a NAME the caller chose. Nothing relates them, and nesting is where
//! they come apart: inside `autoreleasepool(|inner| …)` a caller may still
//! name `outer`, so the object lands in `inner` and the borrow claims `outer`.
//! `inner` pops, the target deallocates, and safe code is left holding the
//! address. Run against real `NSObject`s, in 100% safe bodies:
//!
//! ```text
//! x12-A:  slot after inner pop = id(nil)          <- the target IS deallocated
//!         borrowed.id() = 0x1014d1e20 = original  <- and safe code still holds it
//! x12-A2: freed block REUSED; borrowed.id() is now a DIFFERENT live object
//! x12-A3: -retainCount on the escaped borrow  ->  SIGSEGV
//! ```
//!
//! ## And no bound on [`crate::autoreleasepool`] closes it
//!
//! The obvious repair is to stop the inner closure returning the borrow, by
//! constraining `R` in `autoreleasepool<R>`. **MEASURED, and it does not work:**
//! the escape does not need the return type. With every closure returning `()`,
//! a `Cell` declared in the outer scope carries the borrow out just as well
//! (`x12-B`). A bound that a `Cell` walks around is not a fix, and `R: 'static`
//! would additionally forbid the honest uses.
//!
//! The only shape that IS sound pushes the pool inside the loader and hands the
//! borrow to a `for<'p>` closure, so the lifetime is minted by the same frame
//! that owns the pool. That form is not built here, because of the second
//! measurement:
//!
//! ```text
//! load()             4.7 – 5.0 ns/op   objc_loadWeakRetained + objc_release
//! borrowed, pooled   8.0 – 8.1 ns/op   objc_loadWeak, pool amortised 1000 deep
//! borrowed, scoped   8.8 – 9.0 ns/op   objc_loadWeak + its own push/pop
//! ```
//!
//! (`--release`, 2×10⁶ loads per arm, four runs, m21.) **The "cheap" load is
//! 1.7x the cost of the +1 load**, and objc4 says why in one line: it defines
//! `objc_loadWeak(location)` as `objc_autorelease(objc_loadWeakRetained(location))`.
//! The borrowed form does not SKIP the retain — it does the same retain and
//! then pays an autorelease, a pool-page write now plus the same release later,
//! for the +1 load's single `objc_release`. The advantage the doc claimed was
//! never there to lose, so there is no borrowed form to rebuild, only one to
//! delete.
//!
//! ## The rule this leaves behind
//!
//! **A `&`[`crate::AutoreleasePool`] PARAMETER MAY NOT MINT A LIFETIME.** A
//! pool reference proves that *a* pool is open, never that *this* pool is the one
//! the runtime will use, so it can gate an operation but must never appear in
//! the return type's lifetime. `tests/weak.rs`'s
//! `a_pool_parameter_never_mints_a_lifetime` reads this crate's own source and
//! enforces it, because the next such signature will look as reasonable as
//! this one did.
//!
//! # Thread safety, stated rather than inherited
//!
//! The runtime's weak table is internally locked, so `objc_loadWeakRetained`
//! from two threads is safe *in the runtime*. Neither type here is `Send` or
//! `Sync` anyway — [`Id`] holds a raw pointer, so the auto-traits are already
//! off — and that is the wanted answer for the same reason
//! [`crate::Retained`] gives: every weak reference in this tree names
//! main-thread AppKit state. This module does NOT restate objc2 0.5's
//! `IsIdCloneable`/`IsRetainable` bounds, because it does not have objc2's
//! problem: those bounds exist to keep `Weak` off `Retained`-as-`Box` types,
//! and this crate has no such type — every [`crate::Retained`] here is
//! `Arc`-shaped.
//!
//! # What is NOT here
//!
//! `objc_initWeakOrNil` / `objc_storeWeakOrNil`. They differ from the bound
//! pair only in what happens when the target is mid-`dealloc`: the plain forms
//! crash with "Cannot form weak reference to instance of class X" and the
//! `OrNil` forms answer nil. Every live site takes its weak reference to an
//! object it is holding a strong reference to at that instant — `view.rs` weak-
//! references the window that owns its own superview — so the mid-dealloc case
//! is unreachable by construction, and binding a symbol nothing can reach is
//! the "constant nothing reads" defect the seam module names.

use std::cell::UnsafeCell;
use std::fmt;
use std::marker::PhantomData;

use crate::retained::{ClassType, Retained};
use crate::runtime::{Id, Obj};

#[link(name = "objc")]
unsafe extern "C" {
    /// `id objc_initWeak(id *location, id value)` — register `location` in
    /// `value`'s weak table and store `value` through it.
    ///
    /// `location` must be UNINITIALISED as a weak slot: this does not read the
    /// old contents and does not unregister anything. A nil `value` is legal
    /// and registers nothing; it just writes nil.
    fn objc_initWeak(location: *mut Id, value: Id) -> Id;
    /// `id objc_loadWeakRetained(id *location)` — the object at +1, or nil.
    ///
    /// The retain happens under the runtime's weak lock, which is what makes
    /// the answer stable in the caller's hand.
    fn objc_loadWeakRetained(location: *mut Id) -> Id;
    /// `id objc_loadWeak(id *location)` — the object AUTORELEASED (+0), or nil.
    fn objc_loadWeak(location: *mut Id) -> Id;
    /// `id objc_storeWeak(id *location, id value)` — re-point an ALREADY
    /// INITIALISED slot: unregister it from its old target, register it with
    /// `value`, store `value` through it.
    fn objc_storeWeak(location: *mut Id, value: Id) -> Id;
    /// `void objc_destroyWeak(id *location)` — unregister an initialised slot.
    /// After this the slot is uninitialised again and the runtime holds no
    /// address into it. Legal on a slot that was initialised with nil.
    fn objc_destroyWeak(location: *mut Id);
    /// `void objc_copyWeak(id *to, id *from)` — register `to` at whatever
    /// `from` names. `to` uninitialised, `from` initialised; both stay valid.
    fn objc_copyWeak(to: *mut Id, from: *mut Id);
    /// `void objc_moveWeak(id *to, id *from)` — register `to` and UNREGISTER
    /// `from`, without touching the target's reference count. This is the
    /// runtime's own "move a weak reference to a new address", and it is the
    /// operation Rust's `memcpy`-move is NOT.
    fn objc_moveWeak(to: *mut Id, from: *mut Id);
}

/// One weak-reference storage location.
///
/// # This type's identity is its ADDRESS
///
/// Every operation below is about where these eight bytes LIVE, not about what
/// they contain. An initialised slot is recorded in the Objective-C runtime's
/// side table BY ADDRESS; the runtime writes `nil` through that address when
/// the target deallocates, and it will do so whether or not the Rust value that
/// used to be there still exists.
///
/// Which is why there is **no safe constructor and no safe way to obtain one by
/// value**. A `WeakSlot` a caller could move is a `WeakSlot` a caller could
/// leave registered at a dead address; [`WeakObj`] and [`Weak`] own theirs
/// behind a `Box` so the address outlives every move of the handle, and that is
/// the only sanctioned way to hold one.
///
/// `#[repr(transparent)]` over the [`Id`], so the address handed to the runtime
/// is the address of the pointer word itself and no offset arithmetic is
/// involved.
#[repr(transparent)]
pub struct WeakSlot(UnsafeCell<Id>);

impl WeakSlot {
    /// An UNINITIALISED slot holding nil.
    ///
    /// Not a weak reference to anything: nothing is registered until
    /// [`WeakSlot::init`], [`WeakSlot::copy_from`] or [`WeakSlot::move_from`]
    /// runs. Safe, because an unregistered slot is just a pointer-sized value
    /// and this crate already treats holding an `id` as safe (see [`Id`]).
    #[must_use]
    pub const fn uninit() -> Self {
        Self(UnsafeCell::new(Id::NIL))
    }

    /// The slot's address, which is the thing the runtime registers.
    #[inline]
    #[must_use]
    pub const fn addr(&self) -> *mut Id {
        self.0.get()
    }

    /// Register this slot as a weak reference to `value` (`objc_initWeak`).
    ///
    /// # Safety
    /// * The slot must be UNINITIALISED — freshly [`WeakSlot::uninit`], or
    ///   [`WeakSlot::destroy`]ed since it was last initialised. Initialising
    ///   twice leaks the first registration, leaving the runtime holding this
    ///   address for an object nothing here will unregister it from.
    /// * The slot must NOT MOVE, and must not be dropped without
    ///   [`WeakSlot::destroy`], until it is destroyed. The runtime holds this
    ///   exact address and will write `nil` through it at an unpredictable
    ///   later time.
    /// * `value` must be nil, or a live object that is not currently
    ///   deallocating (see the module docs on `objc_initWeakOrNil`).
    pub unsafe fn init(&self, value: Id) {
        // SAFETY: the caller pins the slot as uninitialised, immovable until
        // destroyed, and `value` as nil-or-live; `objc_initWeak` writes the
        // pointer word through `addr()` and records that address in `value`'s
        // weak table.
        unsafe { objc_initWeak(self.addr(), value) };
    }

    /// Re-point an already-registered slot at `value` (`objc_storeWeak`).
    ///
    /// # Safety
    /// The slot must be INITIALISED (the runtime already holds its address),
    /// and `value` must be nil or a live, non-deallocating object. The slot's
    /// address is unchanged, so the immovability obligation from
    /// [`WeakSlot::init`] continues to apply.
    pub unsafe fn store(&self, value: Id) {
        // SAFETY: the caller pins the slot as initialised; `objc_storeWeak`
        // unregisters this address from its old target under the weak lock and
        // registers it with `value`.
        unsafe { objc_storeWeak(self.addr(), value) };
    }

    /// The target at **+1**, or nil (`objc_loadWeakRetained`).
    ///
    /// # Safety
    /// The slot must be INITIALISED. The returned reference is +1 and the
    /// caller owns it — wrap it in [`Obj`] or [`Retained`], which is what
    /// [`WeakObj::load`] does.
    #[must_use]
    pub unsafe fn load_retained(&self) -> Id {
        // SAFETY: the caller pins the slot as initialised; the runtime reads it
        // under the weak lock and either retains the target or answers nil, so
        // the pointer cannot be a corpse by the time it is returned.
        unsafe { objc_loadWeakRetained(self.addr()) }
    }

    /// The target **autoreleased** (+0), or nil (`objc_loadWeak`).
    ///
    /// **THERE IS NO SAFE WRAPPER FOR THIS, and that is a decision with two
    /// measurements behind it** — the module docs above carry both. In short:
    /// the pool this lands in is the innermost one on the THREAD, which no
    /// parameter can name, so a borrow tied to a pool argument is unsound
    /// across nesting; and `objc4` defines this call as
    /// `objc_autorelease(objc_loadWeakRetained(…))`, so it is 1.7x the cost of
    /// [`WeakObj::load`] rather than cheaper than it. Reach for `load`.
    ///
    /// It stays bound because it is part of the runtime's weak ABI and the
    /// cost measurement needs both arms; it is not a building block for a
    /// safe API.
    ///
    /// # Safety
    /// The slot must be INITIALISED, and the returned pointer is BORROWED: it
    /// is valid only until the innermost autorelease pool ON THIS THREAD pops
    /// — which is the pool that was open when this call was made, and NOT
    /// necessarily any pool the caller has a name for.
    #[must_use]
    pub unsafe fn load_autoreleased(&self) -> Id {
        // SAFETY: the caller pins the slot as initialised; the runtime retains
        // under the weak lock and hands the +1 straight to the innermost pool,
        // so the +0 pointer that comes back is alive for that pool's scope.
        unsafe { objc_loadWeak(self.addr()) }
    }

    /// Register THIS slot at whatever `from` names (`objc_copyWeak`).
    ///
    /// Both slots are registered afterwards and both are independently valid.
    ///
    /// # Safety
    /// `self` must be UNINITIALISED and `from` INITIALISED, and both carry the
    /// immovability obligation from [`WeakSlot::init`] afterwards.
    pub unsafe fn copy_from(&self, from: &Self) {
        // SAFETY: the caller pins `self` uninitialised and `from` initialised;
        // `objc_copyWeak` reads `from` under the weak lock and registers
        // `self`'s address with the same target, leaving `from` alone.
        unsafe { objc_copyWeak(self.addr(), from.addr()) };
    }

    /// Move the registration from `from` to THIS slot (`objc_moveWeak`).
    ///
    /// **This is what a Rust move of a weak reference would have to be, and
    /// cannot be.** After it, `from` is UNINITIALISED — the runtime no longer
    /// holds its address — and `self` is the registered one. No reference count
    /// changes.
    ///
    /// # Safety
    /// `self` must be UNINITIALISED and `from` INITIALISED. `from` must NOT be
    /// [`WeakSlot::destroy`]ed afterwards (it is already unregistered); `self`
    /// must be.
    pub unsafe fn move_from(&self, from: &Self) {
        // SAFETY: the caller pins `self` uninitialised and `from` initialised;
        // `objc_moveWeak` swaps the registered address under the weak lock,
        // which is the only way to change where a weak reference lives without
        // a window in which the runtime holds a stale address.
        unsafe { objc_moveWeak(self.addr(), from.addr()) };
    }

    /// Unregister this slot (`objc_destroyWeak`). Idempotent only in the sense
    /// that destroying a nil-initialised slot is legal; destroying twice is
    /// not, and neither is destroying one that was never initialised.
    ///
    /// # Safety
    /// The slot must be INITIALISED, and must not be used as an initialised
    /// slot afterwards.
    pub unsafe fn destroy(&self) {
        // SAFETY: the caller pins the slot as initialised; `objc_destroyWeak`
        // removes this address from the target's weak table, after which the
        // runtime holds no pointer into these bytes and they may be freed or
        // moved.
        unsafe { objc_destroyWeak(self.addr()) };
    }

    /// The raw pointer word currently in the slot, WITHOUT consulting the
    /// runtime — a plain read of the eight bytes.
    ///
    /// This is not how to use a weak reference; it is how to OBSERVE one, and
    /// it is what proves the runtime nil'd the slot it was holding. The value
    /// may be a dangling pointer if the slot was never registered at this
    /// address, which is exactly the state `tests/weak.rs` reads back out of a
    /// `memcpy`-moved slot.
    ///
    /// # Safety
    /// Reading is always sound; the pointer that comes back must NOT be
    /// messaged, retained or dereferenced unless the caller has other reason to
    /// know it is live.
    #[must_use]
    pub unsafe fn peek(&self) -> Id {
        // SAFETY: `UnsafeCell::get` yields a valid, aligned pointer to the
        // slot's own pointer word, initialised by `uninit()` at the latest;
        // reading a pointer VALUE never dereferences it.
        unsafe { *self.addr() }
    }
}

impl fmt::Debug for WeakSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Deliberately prints the ADDRESS, not the target: the address is what
        // this type is about, and messaging the target from a `Debug` impl
        // could resurrect an object mid-dealloc.
        write!(f, "WeakSlot@{:p}", self.addr())
    }
}

/// An UNTYPED weak reference — the weak twin of [`Obj`].
///
/// Untyped is the shape the live sites need. `view.rs`'s `_ns_window` is a weak
/// reference to an `NSWindow`, which is an `objc2` BINDING type; this crate
/// deliberately has no bindings, so there is no Rust type to parameterise over
/// and the honest handle is the one that carries an address and an ownership
/// rule and nothing else. [`Weak<T>`] is the typed twin for classes
/// [`crate::declare_class!`] mints.
///
/// # Moving this is free and correct
///
/// The slot is behind a `Box`, so a move copies the box POINTER and the
/// registered address is untouched. That is the whole reason for the
/// allocation: see the module docs, and `tests/weak.rs` for the measurement of
/// what the inline alternative does.
pub struct WeakObj {
    /// Boxed so the ADDRESS the runtime registered survives every move of this
    /// handle. Never replaced, only initialised once and destroyed once.
    slot: Box<WeakSlot>,
}

impl WeakObj {
    /// A weak reference to `obj`, or an empty one if `obj` is nil.
    ///
    /// # Safety
    /// `obj` must be nil or a live object that is not currently deallocating.
    /// In practice every call site holds a strong reference to `obj` across
    /// this call, which discharges both halves.
    #[must_use]
    pub unsafe fn new(obj: Id) -> Self {
        let slot = Box::new(WeakSlot::uninit());
        // SAFETY: the slot is freshly `uninit()` and therefore unregistered; it
        // lives in a `Box` this handle owns, so its address is stable until
        // `Drop` destroys it; the caller pins `obj` as nil-or-live.
        unsafe { slot.init(obj) };
        Self { slot }
    }

    /// A weak reference to an object this crate already owns.
    ///
    /// Safe, because holding an [`Obj`] IS the proof `new`'s contract asks for:
    /// an `Obj` is a live +1 reference, and an object holding a strong
    /// reference is not deallocating.
    #[must_use]
    pub fn from_obj(obj: &Obj) -> Self {
        // SAFETY: `obj` owns a +1 reference to a live object for the duration
        // of this call, so the target is live and not mid-`dealloc`.
        unsafe { Self::new(obj.id()) }
    }

    /// An empty weak reference. Loads as `None` forever unless
    /// [`WeakObj::store`] re-points it.
    #[must_use]
    pub fn empty() -> Self {
        // SAFETY: nil is an explicitly legal `objc_initWeak` value and
        // registers nothing.
        unsafe { Self::new(Id::NIL) }
    }

    /// The target at **+1**, or `None` if it has deallocated.
    ///
    /// This is the load to reach for. The retain happens inside the runtime's
    /// weak lock, so the answer cannot become stale between the runtime's check
    /// and the caller's use.
    #[must_use]
    pub fn load(&self) -> Option<Obj> {
        // SAFETY: the slot was initialised by the constructor and has not been
        // destroyed (that happens only in `Drop`).
        let id = unsafe { self.slot.load_retained() };
        // SAFETY: `objc_loadWeakRetained` answers nil or a +1 reference this
        // handle now owns, which is exactly `Obj::from_owned`'s contract.
        unsafe { Obj::from_owned(id) }
    }

    /// Re-point this weak reference at `obj` (or at nil to empty it).
    ///
    /// The slot does not move; only what it names changes.
    ///
    /// # Safety
    /// `obj` must be nil or a live, non-deallocating object.
    pub unsafe fn store(&mut self, obj: Id) {
        // SAFETY: the slot was initialised by the constructor and not
        // destroyed; the caller pins `obj` as nil-or-live.
        unsafe { self.slot.store(obj) };
    }

    /// A SECOND weak reference to the same object (`objc_copyWeak`).
    ///
    /// Deliberately not a [`Clone`] impl, for [`Obj::clone_retained`]'s reason:
    /// every duplication of a runtime handle should be visible at the call
    /// site. This one is cheaper than it looks — it changes no reference count
    /// — but it does allocate a second slot and register a second address, and
    /// a derive would hide both.
    #[must_use]
    pub fn clone_weak(&self) -> Self {
        let slot = Box::new(WeakSlot::uninit());
        // SAFETY: the new slot is freshly `uninit()`, `self.slot` is
        // initialised, and the new slot's address is stable inside the `Box`
        // this handle returns.
        unsafe { slot.copy_from(&self.slot) };
        Self { slot }
    }

    /// Whether the target is still alive, without keeping it alive.
    ///
    /// Answers by loading and dropping the +1, which is the only honest way to
    /// ask: reading the slot's raw word cannot distinguish "alive" from "nil'd
    /// a nanosecond ago", and the runtime's own check is one lock acquisition.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.load().is_some()
    }

    /// The slot's address — the value the runtime registered.
    ///
    /// Exported so a proof can SHOW the address is unchanged across a move.
    /// Nothing else should need it.
    #[must_use]
    pub fn slot_addr(&self) -> *mut Id {
        self.slot.addr()
    }

    /// The raw word in the slot, without consulting the runtime.
    ///
    /// # Safety
    /// The pointer must not be messaged or dereferenced; it is an observation,
    /// not a reference. See [`WeakSlot::peek`].
    #[must_use]
    pub unsafe fn peek(&self) -> Id {
        // SAFETY: the caller takes on the no-dereference obligation; the read
        // itself is a plain load of an initialised pointer word.
        unsafe { self.slot.peek() }
    }
}

impl Drop for WeakObj {
    /// Unregisters the address before the `Box` frees it.
    ///
    /// Skipping this is not a leak, it is a WILD WRITE: the runtime would still
    /// hold the freed allocation's address and would write `nil` through it
    /// when the target deallocates.
    fn drop(&mut self) {
        // SAFETY: the slot was initialised by every constructor of this type
        // and is destroyed exactly once, here, before the `Box` is freed.
        unsafe { self.slot.destroy() };
    }
}

impl fmt::Debug for WeakObj {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // The ADDRESS and liveness, never the target's `-description`:
        // formatting a weak target would retain it inside a `Debug` impl, and
        // objc2's `Weak` prints a bare "(Weak)" for the neighbouring reason
        // (a cycle is exactly what a weak reference is there to break).
        write!(
            f,
            "WeakObj@{:p}({})",
            self.slot.addr(),
            if self.is_live() { "live" } else { "gone" }
        )
    }
}

/// A TYPED weak reference — the weak twin of [`Retained<T>`].
///
/// Same machinery as [`WeakObj`], plus the class type, for the classes
/// [`crate::declare_class!`] mints. The bound is [`ClassType`], the same trait
/// [`Retained`] carries, so a weak reference is expressible for exactly the
/// classes this crate can name and no others.
pub struct Weak<T: ClassType> {
    inner: WeakObj,
    /// `Weak<T>` does NOT own a `T` — that is the point of it — but it must be
    /// invariant-free in the same way `Retained<T>` is, so it carries the
    /// borrow-shaped marker rather than the owning one.
    _ty: PhantomData<fn() -> T>,
}

impl<T: ClassType> Weak<T> {
    /// A weak reference to a live instance of `T`.
    ///
    /// Safe, for [`WeakObj::from_obj`]'s reason: the caller is holding a +1.
    #[must_use]
    pub fn from_retained(obj: &Retained<T>) -> Self {
        // SAFETY: `obj` owns a live +1 reference for the duration of the call,
        // so its target is live and not mid-`dealloc`.
        Self {
            inner: unsafe { WeakObj::new(obj.as_id()) },
            _ty: PhantomData,
        }
    }

    /// An empty typed weak reference.
    #[must_use]
    pub fn empty() -> Self {
        Self {
            inner: WeakObj::empty(),
            _ty: PhantomData,
        }
    }

    /// The target at +1, or `None`.
    #[must_use]
    pub fn load(&self) -> Option<Retained<T>> {
        let obj = self.inner.load()?;
        // SAFETY: this handle was constructed from a `Retained<T>`, so the
        // target is an instance of `T`'s class; `obj` owns the +1
        // `objc_loadWeakRetained` produced and `into_raw` passes it on.
        unsafe { Retained::from_owned(obj.into_raw()) }
    }

    /// Whether the target is still alive.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.inner.is_live()
    }

    /// A second typed weak reference to the same instance.
    #[must_use]
    pub fn clone_weak(&self) -> Self {
        Self {
            inner: self.inner.clone_weak(),
            _ty: PhantomData,
        }
    }

    /// The untyped handle underneath, borrowed.
    #[must_use]
    pub const fn as_untyped(&self) -> &WeakObj {
        &self.inner
    }
}

impl<T: ClassType> fmt::Debug for Weak<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "Weak<{}>@{:p}({})",
            T::NAME,
            self.inner.slot_addr(),
            if self.is_live() { "live" } else { "gone" }
        )
    }
}
