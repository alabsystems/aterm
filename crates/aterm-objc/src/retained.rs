// SPDX-License-Identifier: Apache-2.0
// Copyright 2026 Andrew Yates

//! [`Retained<T>`] — the TYPED twin of [`crate::Obj`].
//!
//! `metal/ffi.rs` needed only one owning wrapper because everything it owned
//! was an opaque protocol object it immediately wrapped in a purpose-built
//! struct (`Device`, `Buffer`, `Texture`). A declared class is different: the
//! Rust type IS the ObjC class, and the owner has to carry that type so
//! `ivars()` and the class's own methods are reachable through it. So this
//! adds a type parameter and a `Deref`, and keeps every other rule identical —
//! including the deliberate absence of `Clone`.

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
///
/// Ownership rules are [`crate::Obj`]'s, verbatim: `from_owned` ADOPTS a +1,
/// `retain` takes a borrowed reference to +1, `Drop` releases exactly once, and
/// there is no `Clone` — a retain is spelled [`Retained::clone_retained`] so it
/// is visible at the call site.
#[repr(transparent)]
pub struct Retained<T: ClassType> {
    ptr: NonNull<T>,
    /// `Retained` OWNS a `T`, so drop-check and variance must treat it as one.
    _owns: PhantomData<T>,
}

impl<T: ClassType> Retained<T> {
    /// Adopt a +1 reference (an `alloc`/`new`/`copy` return).
    ///
    /// # Safety
    /// `id` must be a +1 reference to a live instance of `T`'s class (or a
    /// subclass), or null.
    #[must_use]
    pub unsafe fn from_owned(id: Id) -> Option<Self> {
        NonNull::new(id.cast::<T>()).map(|ptr| Self {
            ptr,
            _owns: PhantomData,
        })
    }

    /// Retain a BORROWED reference to +1.
    ///
    /// # Safety
    /// `id` must be a live instance of `T`'s class (or a subclass), or null.
    #[must_use]
    pub unsafe fn retain(id: Id) -> Option<Self> {
        // SAFETY: caller pins `id` as live-or-null; `crate::Obj::retain` does
        // the null check and the `objc_retain`, and `into_raw` hands the +1 on
        // without releasing it.
        let owned = unsafe { crate::Obj::retain(id) }?;
        // SAFETY: `owned` holds exactly the +1 this `Retained` now adopts.
        unsafe { Self::from_owned(owned.into_raw()) }
    }

    /// The raw `id`, borrowed. Does NOT transfer ownership.
    #[inline]
    #[must_use]
    pub fn as_id(&self) -> Id {
        Id::from_ptr(self.ptr.as_ptr().cast())
    }

    /// A second +1 handle to the SAME object.
    #[must_use]
    pub fn clone_retained(&self) -> Self {
        // SAFETY: `self` holds a live +1 reference, so the pointer is a valid
        // live instance; `objc_retain` is thread-safe and returns the same
        // pointer at +1.
        unsafe { Self::retain(self.as_id()).expect("retaining a live non-null instance") }
    }

    /// Give up ownership WITHOUT releasing — the caller inherits the +1.
    #[must_use]
    pub fn into_raw(self) -> Id {
        let id = self.as_id();
        std::mem::forget(self);
        id
    }

    /// Hand the +1 to the innermost pool and return the now-BORROWED pointer.
    ///
    /// The typed twin of [`crate::Obj::autorelease`], and the ONLY correct
    /// expression for a declared method that returns an object: objc2 spells it
    /// `#[method_id]`, and six real methods in the tree need it —
    /// `toolbar.rs`'s three `NSToolbarDelegate` returns plus three in winit's
    /// macOS backend.
    #[must_use]
    pub fn autorelease(self) -> Id {
        // SAFETY: this holder owns exactly one +1 reference, which `into_raw`
        // hands over without releasing; `autorelease` takes ownership of that
        // reference and returns the same pointer at +0.
        unsafe { crate::runtime::autorelease(self.into_raw()) }
    }
}

impl<T: ClassType> Deref for Retained<T> {
    type Target = T;

    #[inline]
    fn deref(&self) -> &T {
        // SAFETY: `T` is a zero-sized opaque marker by the `ClassType`
        // contract, and `self.ptr` addresses a live instance this holder keeps
        // alive for the borrow. The reference is used only to reach `T`'s
        // inherent methods, which re-derive the object pointer from `self`.
        unsafe { self.ptr.as_ref() }
    }
}

impl<T: ClassType> Drop for Retained<T> {
    /// See [`crate::Obj`]'s `Drop`: the dealloc path may autorelease into the
    /// dropping frame, and holding a pool across the scope is the CALLER's
    /// obligation.
    fn drop(&mut self) {
        // SAFETY: this holder owns exactly one +1 reference to a live object,
        // released exactly once here; `Obj::from_owned` re-adopts that same +1
        // and its own `Drop` performs the single `objc_release`.
        drop(unsafe { crate::Obj::from_owned(self.as_id()) });
    }
}

impl<T: ClassType> fmt::Debug for Retained<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Retained<{}>({:p})", T::NAME, self.ptr.as_ptr())
    }
}

// NOT `Send`/`Sync`. Every declared class in the tree is main-thread-only
// AppKit state (menu targets, views, delegates); the two that objc2 marks
// `InteriorMutable` rather than `MainThreadOnly` are still only ever created
// and messaged on the main thread. Auto-derived `!Send` from `NonNull` is the
// correct default, and a class that genuinely is thread-safe should say so with
// its own wrapper rather than by loosening this one.
