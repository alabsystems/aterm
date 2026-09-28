// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The [`Provenance<T, O>`] `#[repr(transparent)]` wrapper and the inherent
//! constructors production mints through: [`Provenance::from_host`] and
//! [`Provenance::from_pty`].

use core::marker::PhantomData;

use crate::origin::{Host, Origin, OriginTag, Pty};

/// A value tagged with its static origin.
///
/// `#[repr(transparent)]` is a load-bearing guarantee:
/// `size_of::<Provenance<T, O>>() == size_of::<T>()` and the layout is
/// identical to `T`. The parser relies on it: [`crate::pty_wrap_ref`] hands
/// out `&Provenance<[u8], Pty>` over PTY byte slices without copying.
///
/// `O` is `PhantomData<fn() -> O>` so the struct is *invariant* in `O`.
/// This prevents accidental variance-driven `Provenance<T, Pty>` →
/// `Provenance<T, Host>` upcasts in generic code.
///
/// `Provenance` does not implement `Deref` or any auto-converting trait;
/// consumers must call [`Provenance::as_ref`] or one of the
/// `authorize_*` ceremonies explicitly.
///
/// # Origins do not convert
///
/// `From<T>` exists for the `Host` origin only, so a value that is already
/// tagged cannot be relabelled with `.into()`. PTY bytes, for a start:
///
/// ```
/// use aterm_provenance::{Host, Provenance, Pty};
/// // Control: the constructors and paths below are valid; only the forge is not.
/// let pty = Provenance::<_, Pty>::from_pty(b"rm -rf /".to_vec());
/// let host: Provenance<Vec<u8>, Host> = b"ls".to_vec().into();
/// # let _ = (pty, host);
/// ```
///
/// ```compile_fail,E0277
/// use aterm_provenance::{Host, Provenance, Pty};
/// let pty = Provenance::<_, Pty>::from_pty(b"rm -rf /".to_vec());
/// let _host: Provenance<Vec<u8>, Host> = pty.into();
/// ```
///
/// ```compile_fail,E0277
/// use aterm_provenance::{Provenance, Pty, User};
/// let pty = Provenance::<_, Pty>::from_pty(String::from("sudo"));
/// let _user: Provenance<String, User> = pty.into();
/// ```
///
/// (The token that [`crate::authorize_pty_to_host`] consumes is feature-sealed
/// behind `internal-mint`; `aterm-core/tests/capability_ceremony.rs` gates that.)
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
            .field("tag", &O::TAG)
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

// Constructors. One inherent impl per origin marker so that constructing
// `Provenance<_, Host>` requires naming `Host` in scope — this is how the
// audit surface stays grep-able (§4.1).

impl<T> Provenance<T, Host> {
    /// Construct a `Provenance<T, Host>`. Marks `value` as host-origin; only
    /// code that legitimately holds a host-trusted value may call this.
    #[must_use]
    pub const fn from_host(value: T) -> Self {
        Self {
            value,
            _origin: PhantomData,
        }
    }
}

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
    /// Returns the runtime [`OriginTag`] for this static origin.
    ///
    /// Implemented via [`Origin::runtime_tag`] (identical to `O::TAG`; each
    /// sealed impl returns its tag) so the polymorphic MIR stays free of
    /// `OriginTag`-typed unevaluated constants, which the Trust L0 verifier
    /// cannot model pre-monomorphization.
    #[must_use]
    pub fn tag(&self) -> OriginTag {
        O::runtime_tag()
    }

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

impl<T> From<T> for Provenance<T, Host> {
    /// Only `Host` has a `From` impl. Other origins require the explicit
    /// `from_<origin>` constructor so grep-audit can find every tag site.
    fn from(value: T) -> Self {
        Self::from_host(value)
    }
}
