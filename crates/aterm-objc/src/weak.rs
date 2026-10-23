// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! Weak references, for AppKit's object graph (a window retains its content view, and the view names its window back).

use std::cell::UnsafeCell;
use std::fmt;

use crate::runtime::{Id, Obj};

#[link(name = "objc")]
unsafe extern "C" {
    /// `id objc_initWeak(id *location, id value)` — register `location` in
    /// `value`'s weak table and store `value` through it.
    fn objc_initWeak(location: *mut Id, value: Id) -> Id;
    /// `id objc_loadWeakRetained(id *location)` — the object at +1, or nil,
    /// retained under the runtime's weak lock.
    fn objc_loadWeakRetained(location: *mut Id) -> Id;
    /// `id objc_loadWeak(id *location)` — the object autoreleased (+0), or nil.
    fn objc_loadWeak(location: *mut Id) -> Id;
    /// `id objc_storeWeak(id *location, id value)` — re-point an initialised
    /// slot: unregister it from its old target, register it with `value`.
    fn objc_storeWeak(location: *mut Id, value: Id) -> Id;
    /// `void objc_destroyWeak(id *location)` — unregister an initialised slot,
    /// leaving it uninitialised.
    fn objc_destroyWeak(location: *mut Id);
    /// `void objc_copyWeak(id *to, id *from)` — register `to` at whatever
    /// `from` names.
    fn objc_copyWeak(to: *mut Id, from: *mut Id);
}

/// One weak-reference storage location.
#[repr(transparent)]
pub struct WeakSlot(UnsafeCell<Id>);

impl WeakSlot {
    /// An uninitialised slot holding nil.
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
    /// # Safety
    /// * The slot must be uninitialised (fresh from [`WeakSlot::uninit`], or
    ///   [`WeakSlot::destroy`]ed since).
    /// * Until destroyed, the slot must not move or be dropped: the runtime writes `nil`
    ///   through this address at an unpredictable time.
    /// * `value` must be nil, or a live object not currently deallocating.
    pub unsafe fn init(&self, value: Id) {
        // SAFETY: the slot is uninitialised and immovable until destroyed, and `value` is nil-or-live; `objc_initWeak` records `addr()` in `value`'s weak table.
        unsafe { objc_initWeak(self.addr(), value) };
    }

    /// Re-point an already-registered slot at `value` (`objc_storeWeak`).
    /// # Safety
    /// The slot must be initialised, and `value` nil or a live, non-deallocating object.
    /// [`WeakSlot::init`]'s immovability obligation continues.
    pub(crate) unsafe fn store(&self, value: Id) {
        // SAFETY: the slot is initialised; `objc_storeWeak` moves this address from the old target's weak table to `value`'s under the weak lock.
        unsafe { objc_storeWeak(self.addr(), value) };
    }

    /// The target at +1, or nil (`objc_loadWeakRetained`).
    /// # Safety
    /// The slot must be initialised. The caller owns the +1; wrap it in [`Obj`] or
    /// [`crate::Retained`], as [`WeakObj::load`] does.
    #[must_use]
    pub unsafe fn load_retained(&self) -> Id {
        // SAFETY: the slot is initialised; the runtime reads it under the weak lock and either retains the target or answers nil.
        unsafe { objc_loadWeakRetained(self.addr()) }
    }

    /// The target autoreleased (+0), or nil (`objc_loadWeak`).
    /// # Safety
    /// The slot must be initialised, and the returned pointer is borrowed: valid only until the innermost autorelease pool on this thread (at the time of the call) pops, not necessarily any pool the caller can name.
    #[must_use]
    pub unsafe fn load_autoreleased(&self) -> Id {
        // SAFETY: the slot is initialised; the runtime retains under the weak lock and hands the +1 to the innermost pool, so the +0 pointer lives for that pool's scope.
        unsafe { objc_loadWeak(self.addr()) }
    }

    /// Register this slot at whatever `from` names (`objc_copyWeak`).
    /// # Safety
    /// `self` must be uninitialised and `from` initialised; both carry [`WeakSlot::init`]'s immovability obligation afterwards.
    pub(crate) unsafe fn copy_from(&self, from: &Self) {
        // SAFETY: `self` uninitialised, `from` initialised; `objc_copyWeak` registers `self`'s address with `from`'s target under the weak lock.
        unsafe { objc_copyWeak(self.addr(), from.addr()) };
    }

    /// Unregister this slot (`objc_destroyWeak`).
    /// # Safety
    /// The slot must be initialised, and not used as initialised afterwards.
    pub unsafe fn destroy(&self) {
        // SAFETY: the slot is initialised; after `objc_destroyWeak` the runtime holds no pointer into these bytes, so they may be freed or moved.
        unsafe { objc_destroyWeak(self.addr()) };
    }

    /// The raw pointer word in the slot, read without consulting the runtime.
    /// # Safety
    /// Reading is always sound; the returned pointer must not be messaged, retained or dereferenced unless known live by other means.
    #[must_use]
    pub unsafe fn peek(&self) -> Id {
        // SAFETY: `UnsafeCell::get` yields a valid, aligned pointer to the initialised pointer word; reading a pointer value dereferences nothing.
        unsafe { *self.addr() }
    }
}

impl fmt::Debug for WeakSlot {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Prints the address, not the target: messaging the target from `Debug`
        // could resurrect an object mid-dealloc.
        write!(f, "WeakSlot@{:p}", self.addr())
    }
}

/// An untyped weak reference — the weak twin of [`Obj`].
pub struct WeakObj {
    /// Boxed so the address the runtime registered survives every move of this
    /// handle. Never replaced, only initialised once and destroyed once.
    slot: Box<WeakSlot>,
}

impl WeakObj {
    /// A weak reference to `obj`, or an empty one if `obj` is nil.
    /// # Safety
    /// `obj` must be nil or a live object that is not currently deallocating (holding a strong reference across the call discharges both).
    #[must_use]
    pub unsafe fn new(obj: Id) -> Self {
        let slot = Box::new(WeakSlot::uninit());
        // SAFETY: the slot is fresh and unregistered, boxed so its address is stable until `Drop` destroys it; `obj` is nil-or-live.
        unsafe { slot.init(obj) };
        Self { slot }
    }

    /// A weak reference to an object this crate already owns.
    #[must_use]
    pub fn from_obj(obj: &Obj) -> Self {
        // SAFETY: `obj` owns a +1 reference to a live object for the duration of this call, so the target is live and not mid-`dealloc`.
        unsafe { Self::new(obj.id()) }
    }

    /// An empty weak reference.
    #[must_use]
    pub fn empty() -> Self {
        // SAFETY: nil is an explicitly legal `objc_initWeak` value and registers nothing.
        unsafe { Self::new(Id::NIL) }
    }

    /// The target at +1, or `None` if it has deallocated.
    #[must_use]
    pub fn load(&self) -> Option<Obj> {
        // SAFETY: the slot was initialised by the constructor and has not been destroyed (that happens only in `Drop`).
        let id = unsafe { self.slot.load_retained() };
        // SAFETY: `objc_loadWeakRetained` answers nil or a +1 reference this handle now owns, which is exactly `Obj::from_owned`'s contract.
        unsafe { Obj::from_owned(id) }
    }

    /// Re-point this weak reference at `obj` (or nil to empty it).
    /// # Safety
    /// `obj` must be nil or a live, non-deallocating object.
    pub unsafe fn store(&mut self, obj: Id) {
        // SAFETY: the slot was initialised by the constructor and not destroyed; the caller pins `obj` as nil-or-live.
        unsafe { self.slot.store(obj) };
    }

    /// A second weak reference to the same object (`objc_copyWeak`).
    #[must_use]
    pub fn clone_weak(&self) -> Self {
        let slot = Box::new(WeakSlot::uninit());
        // SAFETY: the new slot is fresh, `self.slot` is initialised, and the new slot's address is stable inside the returned `Box`.
        unsafe { slot.copy_from(&self.slot) };
        Self { slot }
    }

    /// Whether the target is still alive, without keeping it alive.
    #[must_use]
    pub fn is_live(&self) -> bool {
        self.load().is_some()
    }

    /// The slot's address — the value the runtime registered — so a test can show it is unchanged across a move.
    #[must_use]
    pub fn slot_addr(&self) -> *mut Id {
        self.slot.addr()
    }

    /// The raw word in the slot, without consulting the runtime.
    /// # Safety
    /// The pointer must not be messaged or dereferenced.
    #[must_use]
    pub unsafe fn peek(&self) -> Id {
        // SAFETY: the caller takes on the no-dereference obligation; the read itself is a plain load of an initialised pointer word.
        unsafe { self.slot.peek() }
    }
}

impl Drop for WeakObj {
    /// Unregisters the address before the `Box` frees it; otherwise the runtime
    /// would write `nil` into freed memory when the target deallocates.
    fn drop(&mut self) {
        // SAFETY: the slot was initialised by every constructor of this type and is destroyed exactly once, here, before the `Box` is freed.
        unsafe { self.slot.destroy() };
    }
}

impl fmt::Debug for WeakObj {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "WeakObj@{:p}({})",
            self.slot.addr(),
            if self.is_live() { "live" } else { "gone" }
        )
    }
}
