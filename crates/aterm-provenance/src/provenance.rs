// Copyright 2026 Andrew Yates
// SPDX-License-Identifier: Apache-2.0

//! The [`Provenance<T, O>`] `#[repr(transparent)]` wrapper, its six inherent
//! constructors (one per origin marker), the
//! [`DynProvenance<T>`] runtime-tagged fallback, and the
//! [`UnliftableTop`] / [`DynProvenanceJoin`] diagnostic types.

use core::marker::PhantomData;

use crate::metrics::{Subsystem, record_drop_on_top};
use crate::origin::{Ai, ConfigFile, Host, NetworkUntrusted, Origin, OriginTag, Pty, User};

/// A value tagged with its static origin.
///
/// `#[repr(transparent)]` is a load-bearing guarantee:
/// `size_of::<Provenance<T, O>>() == size_of::<T>()` and the layout is
/// identical to `T`. Phase 1 will exploit this to let the parser hand out
/// `&Provenance<[u8], Pty>` references over PTY byte slices without copying.
///
/// `O` is `PhantomData<fn() -> O>` so the struct is *invariant* in `O`.
/// This prevents accidental variance-driven `Provenance<T, Pty>` →
/// `Provenance<T, Host>` upcasts in generic code.
///
/// `Provenance` does not implement `Deref` or any auto-converting trait;
/// consumers must call [`Provenance::as_ref`] or one of the
/// `authorize_*` ceremonies explicitly.
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

impl<T> Provenance<T, ConfigFile> {
    /// Construct a `Provenance<T, ConfigFile>`. Marks `value` as originating
    /// from on-disk configuration.
    #[must_use]
    pub const fn from_config(value: T) -> Self {
        Self {
            value,
            _origin: PhantomData,
        }
    }
}

impl<T> Provenance<T, User> {
    /// Construct a `Provenance<T, User>`. Marks `value` as live user input.
    #[must_use]
    pub const fn from_user(value: T) -> Self {
        Self {
            value,
            _origin: PhantomData,
        }
    }
}

impl<T> Provenance<T, Ai> {
    /// Construct a `Provenance<T, Ai>`. Marks `value` as AI-generated.
    #[must_use]
    pub const fn from_ai(value: T) -> Self {
        Self {
            value,
            _origin: PhantomData,
        }
    }
}

impl<T> Provenance<T, NetworkUntrusted> {
    /// Construct a `Provenance<T, NetworkUntrusted>`. Marks `value` as
    /// out-of-band network data.
    #[must_use]
    pub const fn from_network_untrusted(value: T) -> Self {
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

    /// Mutable borrow of the inner value (origin-preserving).
    ///
    /// Most sinks should consume the `Provenance` by value; this exists for
    /// in-place transformations inside trust-preserving code.
    #[allow(clippy::should_implement_trait)]
    pub fn as_mut(&mut self) -> &mut T {
        &mut self.value
    }
}

impl<T, O: Origin> Provenance<T, O> {
    /// Structural map — preserves origin.
    ///
    /// ```
    /// use aterm_provenance::{Pty, Provenance};
    /// let p = Provenance::<_, Pty>::from_pty(b"hi".to_vec());
    /// let p2: Provenance<usize, Pty> = p.map(|v| v.len());
    /// assert_eq!(*p2.as_ref(), 2);
    /// ```
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Provenance<U, O> {
        Provenance {
            value: f(self.value),
            _origin: PhantomData,
        }
    }

    /// Project to a runtime-tagged [`DynProvenance`]. Useful when writing to
    /// storage (the static type parameter must be erased for serialization).
    #[must_use]
    pub fn into_dyn(self) -> DynProvenance<T> {
        DynProvenance {
            value: self.value,
            // `O::runtime_tag()` is identical to `O::TAG` (each sealed impl
            // returns its tag); the call form keeps the polymorphic MIR free
            // of `OriginTag`-typed unevaluated constants for Trust L0.
            tag: O::runtime_tag(),
            is_top: false,
        }
    }

    /// Deliberately audit-surfaced escape hatch. Consuming crates must have a
    /// `// PROVENANCE-ERASE: <reason>` comment near the call; CI greps for it.
    ///
    /// Prefer an `authorize_*` ceremony or staying inside the `Provenance`
    /// wrapper.
    #[deprecated(note = "use `authorize_*` or keep provenance; erasure is audited")]
    pub fn into_inner_erased(self) -> T {
        self.value
    }
}

impl<T> From<T> for Provenance<T, Host> {
    /// Only `Host` has a `From` impl. Other origins require the explicit
    /// `from_<origin>` constructor so grep-audit can find every tag site.
    fn from(value: T) -> Self {
        Self::from_host(value)
    }
}

// ---------------------------------------------------------------------------
// DynProvenance
// ---------------------------------------------------------------------------

/// Runtime-tagged provenance wrapper. Used at storage / FFI boundaries where
/// the static origin cannot be carried in the type.
///
/// A `DynProvenance` may hold either a concrete [`OriginTag`] or the synthetic
/// `Top` element (mixed-incomparable ancestry, see §3.2). `Top` carriers
/// cannot be lifted — the `authorize_*_dyn` ceremonies and every subsystem
/// consumer enumerated in §7.2 must drop them on sight via
/// [`DynProvenance::drop_if_top`] (increments the appropriate subsystem
/// counter) or reject them via the typed error path.
#[derive(Clone, Debug)]
pub struct DynProvenance<T> {
    value: T,
    tag: OriginTag,
    /// When `true`, this carrier represents the synthetic `Top` lattice
    /// element regardless of `tag`. `tag` is retained so best-effort
    /// diagnostics (logs, FFI reporting) can still describe the underlying
    /// concrete origin that was combined into `Top`.
    is_top: bool,
}

impl<T> DynProvenance<T> {
    /// Construct a new `DynProvenance` with an explicit runtime tag (non-Top).
    pub const fn new(value: T, tag: OriginTag) -> Self {
        Self {
            value,
            tag,
            is_top: false,
        }
    }

    /// Construct a `Top`-tagged `DynProvenance`. `witness_tag` is the concrete
    /// origin that was combined into Top; it is kept for diagnostics but the
    /// carrier will be dropped by every consumer per §7.2.
    #[must_use]
    pub const fn new_top(value: T, witness_tag: OriginTag) -> Self {
        Self {
            value,
            tag: witness_tag,
            is_top: true,
        }
    }

    /// Returns the runtime origin tag. For `Top` carriers, returns the
    /// witness tag (the concrete origin combined into Top). Prefer
    /// [`DynProvenance::is_top`] when the Top / concrete distinction matters.
    pub const fn tag(&self) -> OriginTag {
        self.tag
    }

    /// Returns `true` if this carrier is the synthetic `Top` lattice element.
    #[must_use]
    pub const fn is_top(&self) -> bool {
        self.is_top
    }

    /// Returns the stable byte representation of the carrier's tag, where
    /// `Top` maps to [`crate::TOP_TAG_U8`] (`0xFF`).
    ///
    /// This is the byte the FFI `aterm_get_cell`-family accessors return for
    /// Top per §7.2 so the host UI can render with the `MIXED_UNTRUSTED`
    /// styling if it chooses.
    #[must_use]
    pub const fn tag_byte(&self) -> u8 {
        if self.is_top {
            crate::TOP_TAG_U8
        } else {
            self.tag.as_u8()
        }
    }

    /// Borrow the inner value.
    #[allow(clippy::should_implement_trait)]
    pub const fn as_ref(&self) -> &T {
        &self.value
    }

    /// Consume self, returning the inner value (origin information is lost).
    ///
    /// This is the dynamic-tagged equivalent of
    /// [`Provenance::into_inner_erased`] and is subject to the same audit
    /// policy.
    #[deprecated(note = "use `try_as` or a dynamic-dispatched ceremony; erasure is audited")]
    pub fn into_inner_erased(self) -> T {
        self.value
    }

    /// Refine to a static origin. Returns `Err(self)` on tag mismatch (and
    /// always on `Top`), so the caller can inspect the actual tag and decide
    /// what to do.
    ///
    /// # Errors
    ///
    /// Returns the original `DynProvenance` unchanged when:
    /// * `self.is_top()` — Top is not a valid static `Origin`
    /// * `self.tag() != O::TAG`
    ///
    /// Combine with `.map_err(|d| d.tag())` if only the tag matters.
    pub fn try_as<O: Origin>(self) -> Result<Provenance<T, O>, Self> {
        if !self.is_top && self.tag == O::TAG {
            Ok(Provenance {
                value: self.value,
                _origin: PhantomData,
            })
        } else {
            Err(self)
        }
    }

    /// Drop-on-Top: if the carrier is the synthetic `Top` lattice element,
    /// drop `self` (the inner value) and atomically increment the counter
    /// for `subsystem`. Returns `None` in that case. Otherwise returns
    /// `Some(self)` unchanged.
    ///
    /// This is the §7.2 enforcement point. Every subsystem listed in the
    /// design table MUST funnel its incoming `DynProvenance<T>` through this
    /// helper before doing any further work. The returned `Option` makes the
    /// drop visible at the call site (`if let Some(dp) = dp.drop_if_top(...)
    /// { ... }`) so no code path can silently pass a Top value through.
    #[must_use = "Top values must be dropped; use the returned Option to guard further processing"]
    pub fn drop_if_top(self, subsystem: Subsystem) -> Option<Self> {
        if self.is_top {
            record_drop_on_top(subsystem);
            None
        } else {
            Some(self)
        }
    }
}

/// Result of dynamically joining two [`DynProvenance`]s.
///
/// `Lattice(tag)` wraps a concrete [`OriginTag`] when the join maps to a
/// lattice element. `Top` is returned when the join yields the synthetic
/// `Top` element. In the 6-element lattice as currently specified, every
/// pairwise join collapses to a concrete element (`Pty` is absorbing), so
/// `Top` is produced only by explicit combiners that model disjoint ancestry
/// sets; see design §7.
#[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
pub enum DynProvenanceJoin {
    /// Join resolved to a concrete lattice element.
    Lattice(OriginTag),
    /// Join yielded the synthetic `Top` element (cannot be lifted).
    Top,
}

/// Attempted to lift a `Top`-tagged `DynProvenance` to a static origin.
///
/// `Top` is drop-on-sight per §7: no sink may treat a `Top`-tagged value as a
/// lift opportunity. Authorize ceremonies that take `DynProvenance` and lift
/// to `Host` return this error when the input is `Top`.
#[derive(Debug, aterm_error::Error)]
pub enum UnliftableTop {
    /// The input had the synthetic `Top` tag; cannot be lifted.
    #[error("cannot lift synthetic Top-tagged value (mixed untrusted origin)")]
    Top,
}
