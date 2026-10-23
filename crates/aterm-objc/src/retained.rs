// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Retained<T>`] — the typed twin of [`crate::Obj`].

use std::fmt;
use std::marker::PhantomData;
use std::ops::Deref;
use std::ptr::NonNull;

use crate::runtime::{ClassPtr, Id};

/// A Rust type that names a registered Objective-C class.
///
/// # Safety
/// The implementor must be a zero-sized opaque type that is only ever reached
/// through a reference to a live instance of [`Self::class`], and
/// [`Self::class`] must return that class (registered, non-null). The
/// [`crate::declare_class!`] macro is the only sanctioned implementor.
pub unsafe trait ClassType: Sized {
    /// The Objective-C class name, as registered.
    const NAME: &'static str;
    /// The registered class object. Registers on first call.
    fn class() -> ClassPtr;
}

/// An owned (+1) reference to an instance of `T`.
#[repr(transparent)]
pub struct Retained<T: ClassType> {
    ptr: NonNull<T>,
    /// `Retained` owns a `T`, so drop-check and variance must treat it as one.
    _owns: PhantomData<T>,
}

impl<T: ClassType> Retained<T> {
    /// Adopt a +1 reference (an `alloc`/`new`/`copy` return).
    ///
    /// # Safety
    /// `id` must be a +1 reference to a live instance of `T`'s class (or a
    /// subclass), or null.
    #[inline]
    #[must_use]
    pub unsafe fn from_owned(id: Id) -> Option<Self> {
        NonNull::new(id.cast::<T>()).map(|ptr| Self {
            ptr,
            _owns: PhantomData,
        })
    }

    /// Retain a borrowed reference to +1.
    ///
    /// # Safety
    /// `id` must be a live instance of `T`'s class (or a subclass), or null.
    #[inline]
    #[must_use]
    pub unsafe fn retain(id: Id) -> Option<Self> {
        // SAFETY: `id` is live or null; `crate::Obj::retain` null-checks and
        // retains, and `into_raw` hands the +1 on without releasing.
        let owned = unsafe { crate::Obj::retain(id) }?;
        // SAFETY: `owned` holds exactly the +1 this `Retained` now adopts.
        unsafe { Self::from_owned(owned.into_raw()) }
    }

    /// The raw `id`, borrowed. Does not transfer ownership.
    #[inline]
    #[must_use]
    pub const fn as_id(&self) -> Id {
        Id::from_ptr(self.ptr.as_ptr().cast())
    }

    /// A second +1 handle to the same object.
    #[inline]
    #[must_use]
    pub fn clone_retained(&self) -> Self {
        // SAFETY: `self` holds a live +1, so the pointer is a live instance;
        // `objc_retain` is thread-safe and returns the same pointer at +1.
        unsafe { Self::retain(self.as_id()).expect("retaining a live non-null instance") }
    }

    /// Give up ownership without releasing — the caller inherits the +1.
    #[inline]
    #[must_use]
    pub const fn into_raw(self) -> Id {
        let id = self.as_id();
        std::mem::forget(self);
        id
    }

    /// Hand the +1 to the innermost pool and return the now-borrowed pointer.
    #[inline]
    #[must_use]
    pub fn autorelease(self) -> Id {
        // SAFETY: `into_raw` hands over the one +1 without releasing;
        // `autorelease` takes it and returns the same pointer at +0.
        unsafe { crate::runtime::autorelease(self.into_raw()) }
    }
}

impl<T: ClassType> Deref for Retained<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: `T` is a zero-sized marker (the `ClassType` contract) and
        // `self.ptr` is a live instance kept alive for the borrow. The reference
        // only reaches `T`'s inherent methods, which re-derive the pointer.
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ClassType> Drop for Retained<T> {
    /// See [`crate::Obj`]'s `Drop`: the dealloc path may autorelease into the
    /// dropping frame, and holding a pool across the scope is the caller's job.
    fn drop(&mut self) {
        // SAFETY: this holder owns one +1 to a live object; `Obj::from_owned`
        // re-adopts it and its `Drop` performs the single `objc_release`.
        drop(unsafe { crate::Obj::from_owned(self.as_id()) });
    }
}

impl<T: ClassType> fmt::Debug for Retained<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Retained<{}>({:p})", T::NAME, self.ptr.as_ptr())
    }
}
