// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The [`Provenance<T, O>`] `#[repr(transparent)]` wrapper and its
//! constructor [`Provenance::from_pty`].

use core::marker::PhantomData;

use crate::origin::{Origin, Pty};

/// A value tagged with its static origin.
///
/// `#[repr(transparent)]` is a load-bearing guarantee:
/// `size_of::<Provenance<T, O>>() == size_of::<T>()` and the layout is
/// identical to `T`. The parser relies on it: [`crate::pty_wrap_ref`] hands
/// out `&Provenance<[u8], Pty>` over PTY byte slices without copying.
///
/// `O` is `PhantomData<fn() -> O>` so the struct is *invariant* in `O`.
///
/// `Provenance` does not implement `Deref` or any auto-converting trait;
/// consumers must call [`Provenance::as_ref`] explicitly.
#[repr(transparent)]
pub struct Provenance<T: ?Sized, O: Origin> {
    // `_origin` is placed before `value` so the unsized-trailing layout works
    // for unsized `T` (e.g. `[u8]`, `str`). `PhantomData` is zero-sized, so
    // with `#[repr(transparent)]` the layout is identical to `T`.
    pub(crate) _origin: PhantomData<fn() -> O>,
    pub(crate) value: T,
}

impl<T: Clone, O: Origin> Clone for Provenance<T, O> {
    fn clone(&self) -> Self {
        Self {
            _origin: PhantomData,
            value: self.value.clone(),
        }
    }
}

impl<T: Copy, O: Origin> Copy for Provenance<T, O> {}

impl<T: core::fmt::Debug + ?Sized, O: Origin> core::fmt::Debug for Provenance<T, O> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Provenance")
            .field("origin", &core::any::type_name::<O>())
            .field("value", &&self.value)
            .finish()
    }
}

impl<T: PartialEq + ?Sized, O: Origin> PartialEq for Provenance<T, O> {
    fn eq(&self, other: &Self) -> bool {
        self.value == other.value
    }
}

impl<T: Eq + ?Sized, O: Origin> Eq for Provenance<T, O> {}

impl<T> Provenance<T, Pty> {
    /// Construct a `Provenance<T, Pty>`. Marks `value` as PTY-origin
    /// (adversarial).
    #[must_use]
    pub const fn from_pty(value: T) -> Self {
        Self {
            value,
            _origin: PhantomData,
        }
    }
}

impl<T: ?Sized, O: Origin> Provenance<T, O> {
    /// Borrow the inner value. Preserves origin (the borrow is not tagged,
    /// but the borrow's lifetime is bounded by `self`; most sinks consume
    /// the full `Provenance<_, _>` instead).
    ///
    /// Named `as_ref` to match the design's §4.1 API surface. `Provenance`
    /// deliberately does **not** implement the `AsRef` trait — requiring
    /// the explicit call keeps the audit surface grep-able and prevents
    /// silent deref coercions.
    ///
    /// This method is available for `T: ?Sized` so that the `pty_wrap_ref`
    /// helper can hand out a `&Provenance<[u8], Pty>` whose inner reference
    /// can still be borrowed.
    #[allow(clippy::should_implement_trait)]
    #[must_use]
    pub fn as_ref(&self) -> &T {
        &self.value
    }
}
